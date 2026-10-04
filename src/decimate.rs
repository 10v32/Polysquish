//! Decimation via meshoptimizer's attribute-aware edge-collapse simplifier,
//! plus game-ready index/vertex ordering.

use crate::mesh::Mesh;
use crate::recipe::DecimateOptions;
use anyhow::Result;
use meshopt::{SimplifyOptions, VertexDataAdapter};

#[derive(Debug, Clone, serde::Serialize)]
pub struct DecimateReport {
    pub before_triangles: usize,
    pub after_triangles: usize,
    pub error: f32,
}

/// Resolve the target triangle count from the options and the current count.
pub fn resolve_target(tri_count: usize, opts: &DecimateOptions) -> usize {
    let mut target = tri_count;
    if let Some(t) = opts.target_triangles {
        target = target.min(t);
    }
    if let Some(r) = opts.target_ratio {
        target = target.min(((tri_count as f32) * r.clamp(0.0, 1.0)).round() as usize);
    }
    target.max(4)
}

/// Simplify `mesh` to `target_tris` triangles. Attribute-aware: normals, UVs and colours
/// (when present) steer the collapses so seams and shading survive.
pub fn simplify_to(mesh: &Mesh, target_tris: usize, opts: &DecimateOptions, relative_error: f32) -> Result<(Mesh, f32)> {
    let tri_count = mesh.triangle_count();
    if target_tris >= tri_count {
        return Ok((mesh.clone(), 0.0));
    }
    let adapter = VertexDataAdapter::new(mesh.position_bytes(), 12, 0)
        .map_err(|e| anyhow::anyhow!("vertex adapter: {e:?}"))?;
    let target_indices = target_tris * 3;
    let mut flags = SimplifyOptions::None;
    if opts.lock_border {
        flags |= SimplifyOptions::LockBorder;
    }
    let mut err = 0.0f32;
    let new_indices = if opts.aggressive {
        meshopt::simplify_sloppy(&mesh.indices, &adapter, target_indices, relative_error, Some(&mut err))
    } else {
        // Build the attribute stream.
        let mut attrs: Vec<f32> = Vec::new();
        let mut weights: Vec<f32> = Vec::new();
        let use_normals = mesh.has_normals();
        let use_uvs = opts.preserve_uvs && mesh.has_uvs();
        let use_colors = opts.preserve_colors && mesh.has_colors();
        let stride = (if use_normals { 3 } else { 0 }) + (if use_uvs { 2 } else { 0 }) + (if use_colors { 3 } else { 0 });
        if stride > 0 {
            attrs.reserve(mesh.vertex_count() * stride);
            for i in 0..mesh.vertex_count() {
                if use_normals {
                    let n = mesh.normals[i];
                    attrs.extend_from_slice(&[n.x, n.y, n.z]);
                }
                if use_uvs {
                    let uv = mesh.uvs[i];
                    attrs.extend_from_slice(&[uv.x, uv.y]);
                }
                if use_colors {
                    let c = mesh.colors[i];
                    attrs.extend_from_slice(&[c[0], c[1], c[2]]);
                }
            }
            if use_normals {
                weights.extend_from_slice(&[0.5, 0.5, 0.5]);
            }
            if use_uvs {
                weights.extend_from_slice(&[0.5, 0.5]);
            }
            if use_colors {
                weights.extend_from_slice(&[0.6, 0.6, 0.6]);
            }
        }
        let locks = vec![false; mesh.vertex_count()];
        if stride > 0 {
            meshopt::simplify_with_attributes_and_locks(
                &mesh.indices,
                &adapter,
                &attrs,
                &weights,
                stride * 4,
                &locks,
                target_indices,
                relative_error,
                flags,
                Some(&mut err),
            )
        } else {
            meshopt::simplify(&mesh.indices, &adapter, target_indices, relative_error, flags, Some(&mut err))
        }
    };
    let mut out = mesh.clone();
    out.indices = new_indices;
    if !out.material_ids.is_empty() {
        // Material ids cannot be tracked through collapses; most AI assets are single-material.
        // Keep the dominant material for all triangles.
        let dominant = dominant_material(mesh);
        out.material_ids = vec![dominant; out.triangle_count()];
    }
    out.compact();
    // Convert the relative error back into absolute units for callers.
    let scale = meshopt::simplify_scale(&adapter);
    Ok((out, err * scale))
}

pub fn dominant_material(mesh: &Mesh) -> u32 {
    if mesh.material_ids.is_empty() {
        return 0;
    }
    let mut counts: std::collections::HashMap<u32, usize> = Default::default();
    for &m in &mesh.material_ids {
        *counts.entry(m).or_default() += 1;
    }
    counts.into_iter().max_by_key(|(_, c)| *c).map(|(m, _)| m).unwrap_or(0)
}

/// Decimate according to the recipe. Returns the new mesh and a report.
pub fn decimate(mesh: &Mesh, opts: &DecimateOptions) -> Result<(Mesh, DecimateReport)> {
    let before = mesh.triangle_count();
    let target = resolve_target(before, opts);
    // meshoptimizer takes a relative error (fraction of mesh extent); 1.0 = unlimited.
    let rel_err = opts.max_error.map(|e| e.max(1e-6)).unwrap_or(1.0);
    let (mut out, err) = simplify_to(mesh, target, opts, rel_err)?;
    // If the simplifier stopped early (topology constraints), try a sloppy pass to hit the budget.
    if out.triangle_count() > target * 12 / 10 && !opts.aggressive {
        let sloppy = DecimateOptions { aggressive: true, ..opts.clone() };
        if let Ok((o2, _)) = simplify_to(&out, target, &sloppy, 1.0) {
            if o2.triangle_count() < out.triangle_count() {
                out = o2;
            }
        }
    }
    out.compute_smooth_normals();
    let after = out.triangle_count();
    Ok((out, DecimateReport { before_triangles: before, after_triangles: after, error: err }))
}

/// Reorder indices and vertices for GPU cache efficiency (meshoptimizer's standard trio).
pub fn optimize_for_gpu(mesh: &mut Mesh) {
    let vc = mesh.vertex_count();
    if vc == 0 || mesh.indices.is_empty() {
        return;
    }
    meshopt::optimize_vertex_cache_in_place(&mut mesh.indices, vc);
    let pos_bytes: Vec<u8> = mesh.position_bytes().to_vec();
    if let Ok(adapter) = VertexDataAdapter::new(&pos_bytes, 12, 0) {
        meshopt::optimize_overdraw_in_place(&mut mesh.indices, &adapter, 1.05);
    }
    let remap = meshopt::optimize_vertex_fetch_remap(&mesh.indices, vc);
    // remap[old] = new; unused vertices get u32::MAX (~0).
    let new_count = remap.iter().filter(|&&r| r != u32::MAX).count();
    mesh.apply_vertex_remap(&remap, new_count);
}

/// Build an LOD chain from LOD0, each a further simplification of the previous level.
pub fn lod_chain(lod0: &Mesh, ratios: &[f32], opts: &DecimateOptions) -> Vec<Mesh> {
    let mut out = Vec::new();
    let base = lod0.triangle_count();
    let mut prev = lod0.clone();
    for &r in ratios {
        let target = ((base as f32) * r).round().max(4.0) as usize;
        let lod_opts = DecimateOptions {
            target_triangles: Some(target),
            target_ratio: None,
            max_error: None,
            aggressive: false,
            ..opts.clone()
        };
        match simplify_to(&prev, target, &lod_opts, 1.0) {
            Ok((mut m, _)) => {
                if m.triangle_count() > target * 12 / 10 {
                    let sloppy = DecimateOptions { aggressive: true, ..lod_opts.clone() };
                    if let Ok((m2, _)) = simplify_to(&m, target, &sloppy, 1.0) {
                        if m2.triangle_count() < m.triangle_count() {
                            m = m2;
                        }
                    }
                }
                m.compute_smooth_normals();
                optimize_for_gpu(&mut m);
                prev = m.clone();
                out.push(m);
            }
            Err(e) => {
                log::warn!("LOD generation failed: {e}");
                break;
            }
        }
    }
    out
}
