//! Decimation via meshoptimizer's attribute-aware edge-collapse simplifier,
//! plus game-ready index/vertex ordering.

use crate::mesh::Mesh;
use crate::recipe::DecimateOptions;
use anyhow::Result;
use meshopt::{SimplifyOptions, VertexDataAdapter};
use std::collections::HashMap;

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
    out.polygons.clear(); // collapses only track triangles
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
    let (mut out, mut err) = simplify_to(mesh, target, opts, rel_err)?;
    // If the simplifier stopped early because of locked borders, retry with borders free; only
    // then consider the sloppy clusterer, and only with a bounded error so it cannot shred the mesh.
    if out.triangle_count() > target * 12 / 10 && !opts.aggressive {
        if opts.lock_border {
            let free = DecimateOptions { lock_border: false, ..opts.clone() };
            if let Ok((o2, e2)) = simplify_to(&out, target, &free, rel_err) {
                if o2.triangle_count() < out.triangle_count() {
                    out = o2;
                    err = err.max(e2);
                }
            }
        }
        if out.triangle_count() > target * 12 / 10 {
            let sloppy = DecimateOptions { aggressive: true, ..opts.clone() };
            if let Ok((o2, e2)) = simplify_to(&out, target, &sloppy, 0.02) {
                if o2.triangle_count() < out.triangle_count() {
                    out = o2;
                    err = err.max(e2);
                }
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

/// Chunked parallel decimation for very large meshes: the triangles are split spatially into
/// roughly `chunk_size` pieces, each simplified in parallel with its border locked, then stitched
/// back and simplified once more globally to the final target (this second pass also removes the
/// dense seams). Returns the decimated mesh and the number of chunks used.
pub fn decimate_chunked(mesh: &Mesh, opts: &DecimateOptions, chunk_size: usize) -> Result<(Mesh, DecimateReport, usize)> {
    use rayon::prelude::*;
    let tc = mesh.triangle_count();
    let target = resolve_target(tc, opts);
    let n_chunks = ((tc + chunk_size - 1) / chunk_size).max(2);
    // Recursive median split on triangle centroids along the longest axis.
    let centroids: Vec<glam::Vec3> = (0..tc)
        .map(|t| {
            let [a, b, c] = mesh.tri(t);
            (mesh.positions[a as usize] + mesh.positions[b as usize] + mesh.positions[c as usize]) / 3.0
        })
        .collect();
    let mut groups: Vec<Vec<u32>> = vec![(0..tc as u32).collect()];
    while groups.len() < n_chunks {
        // Split the largest group.
        let (gi, _) = groups.iter().enumerate().max_by_key(|(_, g)| g.len()).unwrap();
        let mut g = groups.swap_remove(gi);
        if g.len() < 2 {
            groups.push(g);
            break;
        }
        let mut mn = glam::Vec3::splat(f32::INFINITY);
        let mut mx = glam::Vec3::splat(f32::NEG_INFINITY);
        for &t in &g {
            mn = mn.min(centroids[t as usize]);
            mx = mx.max(centroids[t as usize]);
        }
        let ext = mx - mn;
        let axis = if ext.x >= ext.y && ext.x >= ext.z { 0 } else if ext.y >= ext.z { 1 } else { 2 };
        let mid = g.len() / 2;
        g.select_nth_unstable_by(mid, |a, b| centroids[*a as usize][axis].partial_cmp(&centroids[*b as usize][axis]).unwrap_or(std::cmp::Ordering::Equal));
        let right = g.split_off(mid);
        groups.push(g);
        groups.push(right);
    }
    let per_chunk_ratio = (target as f32 / tc as f32 * 1.6).min(1.0);
    let lock_opts = DecimateOptions { lock_border: true, aggressive: false, ..opts.clone() };
    let parts: Vec<Result<Mesh>> = groups
        .par_iter()
        .map(|g| {
            // Extract the sub-mesh.
            let mut remap: HashMap<u32, u32> = HashMap::with_capacity(g.len() * 2);
            let mut sub = Mesh::default();
            let has_mats = !mesh.material_ids.is_empty();
            for &t in g {
                let tri = mesh.tri(t as usize);
                for v in tri {
                    let nv = *remap.entry(v).or_insert_with(|| {
                        sub.positions.push(mesh.positions[v as usize]);
                        if mesh.has_normals() { sub.normals.push(mesh.normals[v as usize]); }
                        if mesh.has_uvs() { sub.uvs.push(mesh.uvs[v as usize]); }
                        if mesh.has_colors() { sub.colors.push(mesh.colors[v as usize]); }
                        (sub.positions.len() - 1) as u32
                    });
                    sub.indices.push(nv);
                }
                if has_mats {
                    sub.material_ids.push(mesh.material_ids[t as usize]);
                }
            }
            let t_target = ((sub.triangle_count() as f32) * per_chunk_ratio).round().max(4.0) as usize;
            let (m, _) = simplify_to(&sub, t_target, &lock_opts, 1.0)?;
            Ok(m)
        })
        .collect();
    let mut stitched = Mesh::default();
    for p in parts {
        let p = p?;
        stitched.append(&p, 0);
    }
    // Border vertices were locked and are bit-identical across chunks: an exact weld rejoins them.
    crate::clean::weld(&mut stitched, 0.0);
    let (mut out, rep) = decimate(&stitched, opts)?;
    out.compute_smooth_normals();
    let rep = DecimateReport { before_triangles: tc, after_triangles: rep.after_triangles, error: rep.error };
    Ok((out, rep, groups.len()))
}
