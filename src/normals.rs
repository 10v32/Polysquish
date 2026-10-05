//! Hard-edge aware normals: per-corner normals by dihedral angle, and vertex splitting so a
//! mesh can carry one normal per smoothing group.

use crate::mesh::Mesh;
use glam::Vec3;
use rayon::prelude::*;
use std::collections::HashMap;

/// Vertex → incident faces adjacency in CSR form.
pub struct VertexFaces {
    offsets: Vec<u32>,
    faces: Vec<u32>,
}

impl VertexFaces {
    pub fn build(mesh: &Mesh) -> Self {
        let n = mesh.vertex_count();
        let mut counts = vec![0u32; n + 1];
        for &i in &mesh.indices {
            counts[i as usize + 1] += 1;
        }
        for i in 0..n {
            counts[i + 1] += counts[i];
        }
        let mut fill = counts.clone();
        let mut faces = vec![0u32; mesh.indices.len()];
        for (k, &i) in mesh.indices.iter().enumerate() {
            let slot = &mut fill[i as usize];
            faces[*slot as usize] = (k / 3) as u32;
            *slot += 1;
        }
        Self { offsets: counts, faces }
    }
    #[inline]
    pub fn faces_of(&self, v: u32) -> &[u32] {
        &self.faces[self.offsets[v as usize] as usize..self.offsets[v as usize + 1] as usize]
    }
}

/// Per-corner normals (one per index): the area-weighted average of the normals of the faces
/// around the corner's vertex whose orientation is within `angle_deg` of this face.
pub fn corner_normals(mesh: &Mesh, angle_deg: f32) -> Vec<Vec3> {
    let tc = mesh.triangle_count();
    let face_n2: Vec<Vec3> = (0..tc).map(|t| mesh.face_area2(t)).collect();
    let face_n: Vec<Vec3> = face_n2.iter().map(|n| n.normalize_or_zero()).collect();
    let adj = VertexFaces::build(mesh);
    let cos_t = angle_deg.clamp(0.0, 180.0).to_radians().cos();
    let mut out = vec![Vec3::ZERO; mesh.indices.len()];
    out.par_chunks_mut(3).enumerate().for_each(|(t, corners)| {
        let fnrm = face_n[t];
        for k in 0..3 {
            let v = mesh.indices[t * 3 + k];
            let mut acc = Vec3::ZERO;
            for &f in adj.faces_of(v) {
                let f = f as usize;
                if f == t || face_n[f].dot(fnrm) >= cos_t {
                    acc += face_n2[f];
                }
            }
            let n = acc.normalize_or_zero();
            corners[k] = if n == Vec3::ZERO { if fnrm == Vec3::ZERO { Vec3::Y } else { fnrm } } else { n };
        }
    });
    out
}

/// Split vertices so each has a single normal: corners of one vertex whose corner normals differ
/// by more than a small tolerance become separate vertices. Sets `mesh.normals`. Returns the
/// number of vertices added. Polygons are kept consistent by remapping their corners.
pub fn split_hard_edges(mesh: &mut Mesh, angle_deg: f32) -> usize {
    let corners = corner_normals(mesh, angle_deg);
    let n = mesh.vertex_count();
    // Map (vertex, quantised normal) -> new vertex index.
    let mut map: HashMap<(u32, [i16; 3]), u32> = HashMap::with_capacity(n * 2);
    let mut src_of: Vec<u32> = Vec::with_capacity(n + n / 4);
    let mut new_normals: Vec<Vec3> = Vec::with_capacity(n + n / 4);
    let mut new_indices = vec![0u32; mesh.indices.len()];
    let quant = |v: Vec3| -> [i16; 3] { [(v.x * 2000.0) as i16, (v.y * 2000.0) as i16, (v.z * 2000.0) as i16] };
    // Pre-seed so original vertex ids stay stable when possible (first normal seen wins slot v).
    let mut first_slot_used = vec![false; n];
    for k in 0..mesh.indices.len() {
        let v = mesh.indices[k];
        let key = (v, quant(corners[k]));
        let idx = *map.entry(key).or_insert_with(|| {
            if !first_slot_used[v as usize] {
                first_slot_used[v as usize] = true;
                // reserve slot v
                while src_of.len() <= v as usize {
                    src_of.push(u32::MAX);
                    new_normals.push(Vec3::ZERO);
                }
                src_of[v as usize] = v;
                new_normals[v as usize] = corners[k];
                v
            } else {
                src_of.push(v);
                new_normals.push(corners[k]);
                (src_of.len() - 1) as u32
            }
        });
        new_indices[k] = idx;
    }
    // Vertices never referenced keep their slot with a default normal.
    while src_of.len() < n {
        let v = src_of.len() as u32;
        src_of.push(v);
        new_normals.push(Vec3::Y);
    }
    for v in 0..n {
        if src_of[v] == u32::MAX {
            src_of[v] = v as u32;
            new_normals[v] = Vec3::Y;
        }
    }
    let total = src_of.len();
    let added = total - n;
    {
        fn gather<T: Copy>(src: &[T], src_of: &[u32]) -> Vec<T> {
            if src.is_empty() { Vec::new() } else { src_of.iter().map(|&s| src[s as usize]).collect() }
        }
        mesh.positions = gather(&mesh.positions, &src_of);
        mesh.uvs = gather(&mesh.uvs, &src_of);
        mesh.colors = gather(&mesh.colors, &src_of);
        mesh.joints = gather(&mesh.joints, &src_of);
        mesh.weights = gather(&mesh.weights, &src_of);
        // Polygons: remap each polygon corner using the corner's triangle. Quads were triangulated
        // as (a,b,c),(a,c,d), so polygon p's corners a,b,c come from triangle 2p (or p when all tris).
        if mesh.has_polygons() {
            let mut t = 0usize;
            for p in mesh.polygons.iter_mut() {
                let a = new_indices[t * 3];
                let b = new_indices[t * 3 + 1];
                let c = new_indices[t * 3 + 2];
                if p[3] == crate::mesh::NO_VERTEX {
                    *p = [a, b, c, crate::mesh::NO_VERTEX];
                    t += 1;
                } else {
                    let d = new_indices[(t + 1) * 3 + 2];
                    *p = [a, b, c, d];
                    t += 2;
                }
            }
        }
        mesh.indices = new_indices;
        mesh.normals = new_normals;
    }
    added
}
