//! Multi-material support: material-aware vertex splitting, material recovery after decimation,
//! per-material (UDIM tile) UV layouts and texture sets.

use crate::bvh::Bvh;
use crate::mesh::Mesh;
use std::collections::HashMap;

/// Number of distinct material ids referenced by triangles.
pub fn used_materials(mesh: &Mesh) -> usize {
    if mesh.material_ids.is_empty() {
        return 1;
    }
    let mut seen: Vec<u32> = mesh.material_ids.clone();
    seen.sort_unstable();
    seen.dedup();
    seen.len()
}

/// Sorted list of material ids in use.
pub fn material_list(mesh: &Mesh) -> Vec<u32> {
    if mesh.material_ids.is_empty() {
        return vec![0];
    }
    let mut seen: Vec<u32> = mesh.material_ids.clone();
    seen.sort_unstable();
    seen.dedup();
    seen
}

/// Duplicate vertices shared by triangles of different materials so every vertex belongs to
/// exactly one material. Material boundaries then become mesh borders (which decimation locks).
pub fn split_by_material(mesh: &mut Mesh) -> usize {
    if mesh.material_ids.is_empty() || used_materials(mesh) < 2 {
        return 0;
    }
    let n = mesh.vertex_count();
    let mut map: HashMap<(u32, u32), u32> = HashMap::with_capacity(n);
    let mut src_of: Vec<u32> = (0..n as u32).collect();
    let mut first_mat: Vec<u32> = vec![u32::MAX; n];
    let mut new_indices = mesh.indices.clone();
    for t in 0..mesh.triangle_count() {
        let m = mesh.material_ids[t];
        for k in 0..3 {
            let v = mesh.indices[t * 3 + k];
            let idx = if first_mat[v as usize] == u32::MAX || first_mat[v as usize] == m {
                first_mat[v as usize] = m;
                v
            } else {
                *map.entry((v, m)).or_insert_with(|| {
                    src_of.push(v);
                    (src_of.len() - 1) as u32
                })
            };
            new_indices[t * 3 + k] = idx;
        }
    }
    let added = src_of.len() - n;
    if added == 0 {
        return 0;
    }
    fn gather<T: Copy>(src: &[T], src_of: &[u32]) -> Vec<T> {
        if src.is_empty() { Vec::new() } else { src_of.iter().map(|&s| src[s as usize]).collect() }
    }
    mesh.positions = gather(&mesh.positions, &src_of);
    mesh.normals = gather(&mesh.normals, &src_of);
    mesh.uvs = gather(&mesh.uvs, &src_of);
    mesh.colors = gather(&mesh.colors, &src_of);
    mesh.joints = gather(&mesh.joints, &src_of);
    mesh.weights = gather(&mesh.weights, &src_of);
    mesh.indices = new_indices;
    mesh.polygons.clear();
    added
}

/// Give every triangle of `low` the material of the closest source triangle (by centroid).
pub fn assign_by_proximity(low: &mut Mesh, high: &Mesh, bvh: &Bvh) {
    use rayon::prelude::*;
    if high.material_ids.is_empty() {
        low.material_ids.clear();
        return;
    }
    let diag = high.bounds().diagonal.max(1e-9);
    let ids: Vec<u32> = (0..low.triangle_count())
        .into_par_iter()
        .map(|t| {
            let [a, b, c] = low.tri(t);
            let p = (low.positions[a as usize] + low.positions[b as usize] + low.positions[c as usize]) / 3.0;
            bvh.closest_point(p, diag).map(|(h, _)| high.material_ids[h.tri as usize]).unwrap_or(0)
        })
        .collect();
    low.material_ids = ids;
}

/// Extract the triangles of one material as a stand-alone mesh (vertices compacted).
/// Returns the sub-mesh and, per sub-mesh triangle, the index of the triangle in `mesh`.
pub fn submesh(mesh: &Mesh, material: u32) -> (Mesh, Vec<usize>) {
    let mut keep = vec![false; mesh.triangle_count()];
    let mut map = Vec::new();
    for t in 0..mesh.triangle_count() {
        if mesh.material_of(t) == material {
            keep[t] = true;
            map.push(t);
        }
    }
    let mut sub = mesh.clone();
    sub.retain_triangles(&keep);
    (sub, map)
}

/// Unwrap each material separately and lay the results out as UDIM tiles: material `k` (in the
/// order of `material_list`) gets `u` in `[k, k+1)`. Returns the total chart count.
pub fn unwrap_udim(mesh: &mut Mesh, opts: &crate::recipe::UvOptions) -> anyhow::Result<usize> {
    let mats = material_list(mesh);
    if mats.len() < 2 {
        let rep = crate::uv::unwrap(mesh, opts)?;
        return Ok(rep.charts);
    }
    let mut merged = Mesh::default();
    let mut charts = 0usize;
    let no_keep = crate::recipe::UvOptions { keep_existing_if_good: false, ..opts.clone() };
    for (k, &m) in mats.iter().enumerate() {
        let (mut sub, _) = submesh(mesh, m);
        if sub.triangle_count() == 0 {
            continue;
        }
        let rep = crate::uv::unwrap(&mut sub, &no_keep)?;
        charts += rep.charts;
        for uv in &mut sub.uvs {
            uv.x += k as f32;
        }
        sub.material_ids = vec![m; sub.triangle_count()];
        merged.append(&sub, 0);
    }
    *mesh = merged;
    Ok(charts)
}

/// UDIM tile number for the k-th material: 1001, 1002, ...
pub fn udim_tile(k: usize) -> u32 {
    1001 + k as u32
}

/// A copy of `mesh` whose UVs are moved back into `[0,1]` (tile offset removed).
pub fn with_tile_local_uvs(mesh: &Mesh) -> Mesh {
    let mut m = mesh.clone();
    for uv in &mut m.uvs {
        uv.x -= uv.x.floor();
    }
    m
}
