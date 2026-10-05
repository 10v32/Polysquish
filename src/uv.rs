//! UV unwrapping via xatlas: charts the low-poly mesh and packs an atlas.

use crate::mesh::Mesh;
use crate::recipe::UvOptions;
use anyhow::{anyhow, Result};
use glam::Vec2;
use xatlas_rs_v2::{ChartOptions, IndexData, MeshData, MeshDecl, PackOptions, Xatlas};

#[derive(Debug, Clone, serde::Serialize)]
pub struct UvReport {
    pub charts: usize,
    pub atlas_width: u32,
    pub atlas_height: u32,
    pub utilization: f32,
    pub kept_existing: bool,
}

/// Decide whether the mesh's existing UVs are usable: present, inside [0,1] mostly, and
/// without gross overlap (checked by a coarse coverage test).
pub fn existing_uvs_usable(mesh: &Mesh) -> bool {
    if !mesh.has_uvs() {
        return false;
    }
    let mut outside = 0usize;
    for uv in &mesh.uvs {
        if !uv.x.is_finite() || !uv.y.is_finite() || uv.x < -0.01 || uv.x > 1.01 || uv.y < -0.01 || uv.y > 1.01 {
            outside += 1;
        }
    }
    if outside as f32 / mesh.vertex_count().max(1) as f32 > 0.02 {
        return false;
    }
    // Coverage / overlap test on a coarse grid.
    const N: usize = 256;
    let mut grid = vec![0u8; N * N];
    let mut covered = 0usize;
    let mut overlapped = 0usize;
    let mut uv_area = 0.0f64;
    for t in 0..mesh.triangle_count() {
        let [a, b, c] = mesh.tri(t);
        let (ua, ub, uc) = (mesh.uvs[a as usize], mesh.uvs[b as usize], mesh.uvs[c as usize]);
        uv_area += (((ub - ua).perp_dot(uc - ua)) as f64).abs() * 0.5;
        let min = ua.min(ub).min(uc) * N as f32;
        let max = ua.max(ub).max(uc) * N as f32;
        let (x0, y0) = (min.x.floor().max(0.0) as usize, min.y.floor().max(0.0) as usize);
        let (x1, y1) = ((max.x.ceil() as usize).min(N - 1), (max.y.ceil() as usize).min(N - 1));
        for y in y0..=y1 {
            for x in x0..=x1 {
                let p = Vec2::new(x as f32 + 0.5, y as f32 + 0.5) / N as f32;
                if point_in_tri(p, ua, ub, uc) {
                    let cell = &mut grid[y * N + x];
                    if *cell == 0 {
                        covered += 1;
                    } else if *cell == 1 {
                        overlapped += 1;
                    }
                    *cell = cell.saturating_add(1);
                }
            }
        }
    }
    if covered == 0 {
        return false;
    }
    let overlap_ratio = overlapped as f32 / covered as f32;
    // Degenerate UVs (all zero) have ~no area.
    uv_area > 0.01 && overlap_ratio < 0.05
}

/// Positions relaxed toward their 1-ring average `iterations` times (boundary vertices fixed).
fn smoothed_positions(mesh: &Mesh, iterations: usize) -> Vec<glam::Vec3> {
    let n = mesh.vertex_count();
    let mut adj: Vec<Vec<u32>> = vec![Vec::new(); n];
    let mut edge_count: std::collections::HashMap<(u32, u32), u8> = std::collections::HashMap::new();
    for t in 0..mesh.triangle_count() {
        let [a, b, c] = mesh.tri(t);
        for (x, y) in [(a, b), (b, c), (c, a)] {
            adj[x as usize].push(y);
            adj[y as usize].push(x);
            let k = if x < y { (x, y) } else { (y, x) };
            *edge_count.entry(k).or_insert(0) += 1;
        }
    }
    let mut boundary = vec![false; n];
    for ((a, b), c) in &edge_count {
        if *c == 1 {
            boundary[*a as usize] = true;
            boundary[*b as usize] = true;
        }
    }
    let mut cur = mesh.positions.clone();
    for _ in 0..iterations {
        let next: Vec<glam::Vec3> = (0..n)
            .map(|i| {
                if boundary[i] || adj[i].is_empty() {
                    return cur[i];
                }
                let mut acc = glam::Vec3::ZERO;
                for &j in &adj[i] {
                    acc += cur[j as usize];
                }
                let avg = acc / adj[i].len() as f32;
                cur[i] + (avg - cur[i]) * 0.5
            })
            .collect();
        cur = next;
    }
    cur
}

/// Normals averaged over the 1-ring `iterations` times (positions untouched).
fn smoothed_normals(mesh: &Mesh, iterations: usize) -> Vec<glam::Vec3> {
    let n = mesh.vertex_count();
    let mut adj: Vec<Vec<u32>> = vec![Vec::new(); n];
    for t in 0..mesh.triangle_count() {
        let [a, b, c] = mesh.tri(t);
        for (x, y) in [(a, b), (b, c), (c, a)] {
            adj[x as usize].push(y);
            adj[y as usize].push(x);
        }
    }
    let mut cur = mesh.normals.clone();
    for _ in 0..iterations {
        let next: Vec<glam::Vec3> = (0..n)
            .map(|i| {
                let mut acc = cur[i] * 2.0;
                for &j in &adj[i] {
                    acc += cur[j as usize];
                }
                let v = acc.normalize_or_zero();
                if v == glam::Vec3::ZERO { cur[i] } else { v }
            })
            .collect();
        cur = next;
    }
    cur
}

/// xatlas keeps face order but re-indexes vertices; quads (triangulated as consecutive
/// `(a,b,c),(a,c,d)`) are re-paired when both halves still share the diagonal, else kept as triangles.
fn rebuild_polygons(old: &[[u32; 4]], new_indices: &[u32]) -> Vec<[u32; 4]> {
    use crate::mesh::NO_VERTEX;
    let mut out = Vec::with_capacity(old.len());
    let mut t = 0usize;
    for p in old {
        if t * 3 + 3 > new_indices.len() {
            break;
        }
        let (a, b, c) = (new_indices[t * 3], new_indices[t * 3 + 1], new_indices[t * 3 + 2]);
        if p[3] == NO_VERTEX {
            out.push([a, b, c, NO_VERTEX]);
            t += 1;
        } else {
            if (t + 1) * 3 + 3 > new_indices.len() {
                out.push([a, b, c, NO_VERTEX]);
                break;
            }
            let (a2, c2, d) = (new_indices[(t + 1) * 3], new_indices[(t + 1) * 3 + 1], new_indices[(t + 1) * 3 + 2]);
            if a2 == a && c2 == c {
                out.push([a, b, c, d]);
            } else {
                out.push([a, b, c, NO_VERTEX]);
                out.push([a2, c2, d, NO_VERTEX]);
            }
            t += 2;
        }
    }
    out
}

#[inline]
fn point_in_tri(p: Vec2, a: Vec2, b: Vec2, c: Vec2) -> bool {
    let d1 = (p - b).perp_dot(a - b);
    let d2 = (p - c).perp_dot(b - c);
    let d3 = (p - a).perp_dot(c - a);
    let has_neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let has_pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(has_neg && has_pos)
}

/// Unwrap the mesh in place. Vertices get split along chart boundaries as needed.
pub fn unwrap(mesh: &mut Mesh, opts: &UvOptions) -> Result<UvReport> {
    if opts.keep_existing_if_good && existing_uvs_usable(mesh) {
        return Ok(UvReport {
            charts: 0,
            atlas_width: opts.resolution,
            atlas_height: opts.resolution,
            utilization: 0.0,
            kept_existing: true,
        });
    }
    if !mesh.has_normals() {
        mesh.compute_smooth_normals();
    }
    // Charting sees a lightly smoothed copy of the geometry (positions and normals): decimation
    // noise otherwise fragments charts. The UVs are applied to the real mesh afterwards (same topology).
    let smooth_pos = smoothed_positions(mesh, 3);
    let positions: Vec<f32> = smooth_pos.iter().flat_map(|p| [p.x, p.y, p.z]).collect();
    // Charting sees smoothed normals so decimation noise does not fragment charts; hard edges
    // (already split into separate vertices upstream) still break charts naturally.
    let smoothed = smoothed_normals(mesh, 4);
    let normals: Vec<f32> = smoothed.iter().flat_map(|n| [n.x, n.y, n.z]).collect();
    let materials: Vec<u32> = if mesh.material_ids.is_empty() {
        Vec::new()
    } else {
        mesh.material_ids.clone()
    };
    let indices = mesh.indices.clone();
    let (new, charts, w, h, utilization) = {
    let decl = MeshDecl {
        vertex_position_data: MeshData::Contiguous(&positions),
        vertex_normal_data: Some(MeshData::Contiguous(&normals)),
        vertex_uv_data: None,
        face_ignore_data: None,
        face_material_data: if materials.is_empty() { None } else { Some(&materials) },
        face_vertex_count: None,
        index_data: Some(IndexData::U32(&indices)),
        face_count: mesh.triangle_count() as u32,
        epsilon: 1.192092896e-07,
    };
    let mut atlas = Xatlas::new();
    atlas
        .add_mesh(&decl)
        .map_err(|e| anyhow!("xatlas rejected the mesh: {e:?}"))?;
    let chart_opts = ChartOptions {
        max_iterations: 3,
        max_cost: 8.0,
        normal_deviation_weight: 1.0,
        roundness_weight: 0.005,
        straightness_weight: 3.0,
        normal_seam_weight: 6.0,
        texture_seam_weight: 0.5,
        ..Default::default()
    };
    let pack_opts = PackOptions {
        resolution: opts.resolution,
        padding: opts.padding,
        bilinear: true,
        block_align: false,
        brute_force: false,
        rotate_charts: true,
        rotate_charts_to_axis: true,
        ..Default::default()
    };
    atlas.generate(&chart_opts, &pack_opts);
    let (w, h) = (atlas.width(), atlas.height());
    if w == 0 || h == 0 {
        return Err(anyhow!("xatlas produced an empty atlas"));
    }
    let utilization = atlas.utilization().and_then(|u| u.first().copied()).unwrap_or(0.0);
    let charts = atlas.chart_count() as usize;
    let meshes = atlas.meshes();
    let out_mesh = meshes.first().ok_or_else(|| anyhow!("xatlas returned no mesh"))?;

    // Rebuild the vertex arrays from xatlas' split vertices.
    let mut new = Mesh::default();
    let nv = out_mesh.vertex_array.len();
    new.positions.reserve(nv);
    new.uvs.reserve(nv);
    let has_colors = mesh.has_colors();
    let has_skin = mesh.has_skin();
    for v in &out_mesh.vertex_array {
        let src = v.xref as usize;
        new.positions.push(mesh.positions[src]);
        new.normals.push(mesh.normals[src]);
        if has_colors {
            new.colors.push(mesh.colors[src]);
        }
        if has_skin {
            new.joints.push(mesh.joints[src]);
            new.weights.push(mesh.weights[src]);
        }
        new.uvs.push(Vec2::new(v.uv[0] / w as f32, v.uv[1] / h as f32));
    }
    new.indices = out_mesh.index_array.to_vec();
    // xatlas keeps face order, so material ids carry over directly.
    if !mesh.material_ids.is_empty() && new.triangle_count() == mesh.triangle_count() {
        new.material_ids = mesh.material_ids.clone();
    } else if !mesh.material_ids.is_empty() {
        let dominant = crate::decimate::dominant_material(mesh);
        new.material_ids = vec![dominant; new.triangle_count()];
    }
    (new, charts, w, h, utilization)
    };
    let mut new = new;
    if mesh.has_polygons() {
        new.polygons = rebuild_polygons(&mesh.polygons, &new.indices);
    }
    *mesh = new;
    Ok(UvReport {
        charts,
        atlas_width: w,
        atlas_height: h,
        utilization,
        kept_existing: false,
    })
}
