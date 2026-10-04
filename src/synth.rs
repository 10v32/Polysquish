//! Synthetic test inputs: subdivide + displace a mesh to millions of triangles and inject the
//! defects AI generators typically produce (floaters, duplicates, degenerate and flipped faces).

use crate::mesh::Mesh;
use glam::Vec3;
use rand::rngs::SmallRng;
use rand::{Rng, SeedableRng};
use std::collections::HashMap;

/// One level of midpoint subdivision (each triangle -> 4). Attributes are interpolated.
pub fn subdivide_midpoint(mesh: &Mesh) -> Mesh {
    let mut out = Mesh {
        positions: mesh.positions.clone(),
        normals: mesh.normals.clone(),
        uvs: mesh.uvs.clone(),
        colors: mesh.colors.clone(),
        ..Default::default()
    };
    let mut edge_mid: HashMap<(u32, u32), u32> = HashMap::with_capacity(mesh.indices.len());
    let mut midpoint = |out: &mut Mesh, a: u32, b: u32| -> u32 {
        let key = if a < b { (a, b) } else { (b, a) };
        if let Some(&m) = edge_mid.get(&key) {
            return m;
        }
        let (ia, ib) = (a as usize, b as usize);
        out.positions.push((mesh.positions[ia] + mesh.positions[ib]) * 0.5);
        if mesh.has_normals() {
            out.normals.push((mesh.normals[ia] + mesh.normals[ib]).normalize_or_zero());
        }
        if mesh.has_uvs() {
            out.uvs.push((mesh.uvs[ia] + mesh.uvs[ib]) * 0.5);
        }
        if mesh.has_colors() {
            let (ca, cb) = (mesh.colors[ia], mesh.colors[ib]);
            out.colors.push([0, 1, 2, 3].map(|k| (ca[k] + cb[k]) * 0.5));
        }
        let m = (out.positions.len() - 1) as u32;
        edge_mid.insert(key, m);
        m
    };
    out.indices.reserve(mesh.indices.len() * 4);
    let has_mats = !mesh.material_ids.is_empty();
    for t in 0..mesh.triangle_count() {
        let [a, b, c] = mesh.tri(t);
        let ab = midpoint(&mut out, a, b);
        let bc = midpoint(&mut out, b, c);
        let ca = midpoint(&mut out, c, a);
        out.indices.extend_from_slice(&[a, ab, ca, ab, b, bc, ca, bc, c, ab, bc, ca]);
        if has_mats {
            let m = mesh.material_ids[t];
            out.material_ids.extend_from_slice(&[m, m, m, m]);
        }
    }
    out
}

#[inline]
fn hash3(x: i32, y: i32, z: i32, seed: u32) -> f32 {
    let mut h = (x as u32).wrapping_mul(0x8da6b343) ^ (y as u32).wrapping_mul(0xd8163841) ^ (z as u32).wrapping_mul(0xcb1ab31f) ^ seed;
    h ^= h >> 13;
    h = h.wrapping_mul(0x5bd1e995);
    h ^= h >> 15;
    (h & 0xffffff) as f32 / 0xffffff as f32
}

/// Smooth value noise in [0,1].
pub fn value_noise(p: Vec3, seed: u32) -> f32 {
    let f = p.floor();
    let t = p - f;
    let t = t * t * (Vec3::splat(3.0) - 2.0 * t);
    let (x, y, z) = (f.x as i32, f.y as i32, f.z as i32);
    let c = |dx: i32, dy: i32, dz: i32| hash3(x + dx, y + dy, z + dz, seed);
    let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t;
    let x00 = lerp(c(0, 0, 0), c(1, 0, 0), t.x);
    let x10 = lerp(c(0, 1, 0), c(1, 1, 0), t.x);
    let x01 = lerp(c(0, 0, 1), c(1, 0, 1), t.x);
    let x11 = lerp(c(0, 1, 1), c(1, 1, 1), t.x);
    let y0 = lerp(x00, x10, t.y);
    let y1 = lerp(x01, x11, t.y);
    lerp(y0, y1, t.z)
}

/// Displace along normals with multi-octave noise; `amplitude` is a fraction of the diagonal.
pub fn displace(mesh: &mut Mesh, amplitude: f32, frequency: f32, seed: u32) {
    if !mesh.has_normals() {
        mesh.compute_smooth_normals();
    }
    let b = mesh.bounds();
    let diag = b.diagonal.max(1e-6);
    let origin = Vec3::from(b.min);
    let normals = mesh.normals.clone();
    for (i, p) in mesh.positions.iter_mut().enumerate() {
        let q = (*p - origin) / diag * frequency;
        let n = value_noise(q, seed) * 0.6 + value_noise(q * 2.3, seed ^ 0x9e37) * 0.3 + value_noise(q * 5.1, seed ^ 0x51ed) * 0.1;
        *p += normals[i] * ((n - 0.5) * 2.0 * amplitude * diag);
    }
    mesh.compute_smooth_normals();
}

/// Paint procedural vertex colours so the albedo bake has something to transfer.
pub fn paint(mesh: &mut Mesh, seed: u32) {
    let b = mesh.bounds();
    let diag = b.diagonal.max(1e-6);
    let origin = Vec3::from(b.min);
    mesh.colors = mesh
        .positions
        .iter()
        .map(|p| {
            let q = (*p - origin) / diag;
            let n1 = value_noise(q * 6.0, seed);
            let n2 = value_noise(q * 13.0 + Vec3::splat(7.0), seed ^ 0xabcd);
            let base = [0.62, 0.36, 0.78];
            let alt = [0.98, 0.45, 0.80];
            let k = (n1 * 1.4 - 0.2).clamp(0.0, 1.0);
            let mut c = [0.0f32; 4];
            for i in 0..3 {
                c[i] = (base[i] + (alt[i] - base[i]) * k) * (0.75 + 0.25 * n2);
            }
            c[3] = 1.0;
            c
        })
        .collect();
}

pub struct DefectOptions {
    pub floaters: usize,
    pub duplicate_fraction: f32,
    pub degenerate: usize,
    pub flipped_fraction: f32,
}

/// Inject typical AI-generation defects.
pub fn add_defects(mesh: &mut Mesh, opts: &DefectOptions, seed: u64) {
    let mut rng = SmallRng::seed_from_u64(seed);
    let b = mesh.bounds();
    let diag = b.diagonal.max(1e-6);
    let has_c = mesh.has_colors();
    let has_n = mesh.has_normals();
    let has_uv = mesh.has_uvs();
    // Floaters: tiny tetrahedra scattered around the surface.
    for _ in 0..opts.floaters {
        let anchor = mesh.positions[rng.random_range(0..mesh.positions.len())];
        let c = anchor + Vec3::new(rng.random::<f32>() - 0.5, rng.random::<f32>() - 0.5, rng.random::<f32>() - 0.5) * diag * 0.05;
        let r = diag * rng.random_range(0.0005..0.004);
        let base = mesh.positions.len() as u32;
        let pts = [
            c + Vec3::new(r, r, r),
            c + Vec3::new(-r, -r, r),
            c + Vec3::new(-r, r, -r),
            c + Vec3::new(r, -r, -r),
        ];
        for p in pts {
            mesh.positions.push(p);
            if has_n {
                mesh.normals.push((p - c).normalize_or_zero());
            }
            if has_c {
                mesh.colors.push([0.9, 0.9, 0.9, 1.0]);
            }
            if has_uv {
                mesh.uvs.push(glam::Vec2::ZERO);
            }
        }
        mesh.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 3, base + 1, base, base + 2, base + 3, base + 1, base + 3, base + 2]);
        if !mesh.material_ids.is_empty() {
            mesh.material_ids.extend_from_slice(&[0, 0, 0, 0]);
        }
    }
    // Duplicate vertices: unshare a fraction of triangle corners.
    let n_tri = mesh.triangle_count();
    let dup_count = (n_tri as f32 * opts.duplicate_fraction) as usize;
    for _ in 0..dup_count {
        let t = rng.random_range(0..n_tri);
        let k = rng.random_range(0..3);
        let old = mesh.indices[t * 3 + k] as usize;
        mesh.positions.push(mesh.positions[old]);
        if has_n {
            mesh.normals.push(mesh.normals[old]);
        }
        if has_c {
            mesh.colors.push(mesh.colors[old]);
        }
        if has_uv {
            mesh.uvs.push(mesh.uvs[old]);
        }
        mesh.indices[t * 3 + k] = (mesh.positions.len() - 1) as u32;
    }
    // Degenerate triangles.
    for _ in 0..opts.degenerate {
        let t = rng.random_range(0..n_tri);
        let [a, _, _] = mesh.tri(t);
        mesh.indices.extend_from_slice(&[a, a, mesh.indices[t * 3 + 1]]);
        if !mesh.material_ids.is_empty() {
            mesh.material_ids.push(0);
        }
    }
    // Flipped triangles.
    let flip_count = (n_tri as f32 * opts.flipped_fraction) as usize;
    for _ in 0..flip_count {
        let t = rng.random_range(0..n_tri);
        mesh.indices.swap(t * 3 + 1, t * 3 + 2);
    }
}
