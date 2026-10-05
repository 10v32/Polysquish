//! Quad-dominant retopology: Botsch–Kobbelt style isotropic remeshing of a decimated mesh
//! (split / collapse / flip / tangential smoothing / projection onto the high-poly source),
//! followed by greedy pairing of adjacent triangles into quads.
//!
//! The remesher works on a plain vertex array + triangle list; adjacency is rebuilt with hash
//! maps for every pass. Feature edges (dihedral angle above `feature_angle_deg` in the start
//! mesh) and boundary loops are tracked explicitly through every topological operation; their
//! vertices only slide along the feature polyline, and feature corners stay pinned.
//!
//! The result is *not* field-aligned: edge flow only follows curvature through the isotropic
//! smoothing, so quads align with features and boundaries but not with principal directions.

use crate::bvh::Bvh;
use crate::mesh::{Mesh, NO_VERTEX};
use anyhow::{bail, Result};
use glam::Vec3;
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasherDefault, Hasher};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RetopoOptions {
    /// Approximate number of output polygons (quads + leftover triangles).
    pub target_faces: usize,
    /// Remeshing iterations (each: split, collapse, flip, smooth, project).
    pub iterations: u32,
    /// Edges whose dihedral angle (degrees) in the start mesh is at least this are features.
    pub feature_angle_deg: f32,
    /// Pair triangles into quads. When false the result is a pure isotropic triangle mesh.
    pub quad_pairing: bool,
}

impl Default for RetopoOptions {
    fn default() -> Self {
        Self { target_faces: 5_000, iterations: 5, feature_angle_deg: 50.0, quad_pairing: true }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct RetopoReport {
    pub faces: usize,
    pub quads: usize,
    pub triangles: usize,
    pub quad_ratio: f32,
    pub mean_edge_length: f32,
    /// Largest distance from a result vertex or polygon centroid to the source surface.
    pub max_deviation: f32,
}

/// Target edge length for `target_faces` mostly-quad polygons over `area`. A quad covers
/// about two equilateral triangles, so the remesher aims for ~1.7×`target_faces` triangles.
pub fn target_edge_length(area: f64, target_faces: usize) -> f32 {
    let tris = 1.7 * target_faces.max(1) as f64;
    // Equilateral triangle area = sqrt(3)/4 * L^2.
    ((4.0 * area) / (3f64.sqrt() * tris)).sqrt() as f32
}

// ---------------------------------------------------------------------------------------------
// Small fast hasher (FxHash) for the per-pass adjacency maps.

#[derive(Default, Clone, Copy)]
struct FxHasher(u64);

impl Hasher for FxHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0.rotate_left(5) ^ b as u64).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
        }
    }
    #[inline]
    fn write_u32(&mut self, i: u32) {
        self.0 = (self.0.rotate_left(5) ^ i as u64).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }
    #[inline]
    fn write_u64(&mut self, i: u64) {
        self.0 = (self.0.rotate_left(5) ^ i).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }
    #[inline]
    fn write_usize(&mut self, i: usize) {
        self.write_u64(i as u64);
    }
    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }
}

type FxBuild = BuildHasherDefault<FxHasher>;
type FxMap<K, V> = HashMap<K, V, FxBuild>;
type FxSet<K> = HashSet<K, FxBuild>;

#[inline]
fn edge_key(a: u32, b: u32) -> (u32, u32) {
    if a < b {
        (a, b)
    } else {
        (b, a)
    }
}

// ---------------------------------------------------------------------------------------------
// Working mesh

const FREE: u8 = 0;
/// On a feature polyline or boundary: slides along it.
const CURVE: u8 = 1;
/// Feature corner / polyline end: pinned.
const CORNER: u8 = 2;

struct Work {
    pos: Vec<Vec3>,
    tris: Vec<[u32; 3]>,
    /// Feature + boundary edges, as sorted vertex pairs.
    features: FxSet<(u32, u32)>,
    kind: Vec<u8>,
}

impl Work {
    fn from_mesh(start: &Mesh, feature_angle_deg: f32) -> Result<Work> {
        if start.triangle_count() == 0 || start.positions.is_empty() {
            bail!("retopo: start mesh is empty");
        }
        let pos = start.positions.clone();
        let mut tris: Vec<[u32; 3]> = Vec::with_capacity(start.triangle_count());
        for t in 0..start.triangle_count() {
            let tri = start.tri(t);
            if tri[0] == tri[1] || tri[1] == tri[2] || tri[0] == tri[2] {
                continue;
            }
            if tri.iter().any(|&i| i as usize >= pos.len()) {
                bail!("retopo: start mesh index out of range");
            }
            let n = (pos[tri[1] as usize] - pos[tri[0] as usize]).cross(pos[tri[2] as usize] - pos[tri[0] as usize]);
            if !(n.length_squared() > 0.0) {
                continue;
            }
            tris.push(tri);
        }
        if tris.is_empty() {
            bail!("retopo: start mesh has no valid triangles");
        }
        // Feature edges: dihedral angle >= threshold, plus every boundary / non-manifold edge.
        let cos_thresh = feature_angle_deg.to_radians().cos();
        let mut edge_tris: FxMap<(u32, u32), (u32, u32, u8)> = FxMap::default();
        for (t, tri) in tris.iter().enumerate() {
            for k in 0..3 {
                let e = edge_key(tri[k], tri[(k + 1) % 3]);
                let ent = edge_tris.entry(e).or_insert((u32::MAX, u32::MAX, 0));
                match ent.2 {
                    0 => ent.0 = t as u32,
                    1 => ent.1 = t as u32,
                    _ => {}
                }
                ent.2 = ent.2.saturating_add(1);
            }
        }
        let normals: Vec<Vec3> = tris
            .iter()
            .map(|t| (pos[t[1] as usize] - pos[t[0] as usize]).cross(pos[t[2] as usize] - pos[t[0] as usize]).normalize_or_zero())
            .collect();
        let mut features = FxSet::default();
        for (e, (t0, t1, c)) in &edge_tris {
            if *c != 2 {
                features.insert(*e);
                continue;
            }
            if normals[*t0 as usize].dot(normals[*t1 as usize]) <= cos_thresh {
                features.insert(*e);
            }
        }
        let mut w = Work { pos, tris, features, kind: Vec::new() };
        w.update_kinds();
        Ok(w)
    }

    /// Classify vertices from the number of incident feature edges.
    fn update_kinds(&mut self) {
        let mut count = vec![0u8; self.pos.len()];
        for &(a, b) in &self.features {
            count[a as usize] = count[a as usize].saturating_add(1);
            count[b as usize] = count[b as usize].saturating_add(1);
        }
        self.kind = count.iter().map(|&c| match c {
            0 => FREE,
            2 => CURVE,
            _ => CORNER,
        }).collect();
    }

    #[inline]
    fn tri_normal(&self, t: &[u32; 3]) -> Vec3 {
        (self.pos[t[1] as usize] - self.pos[t[0] as usize]).cross(self.pos[t[2] as usize] - self.pos[t[0] as usize])
    }

    fn surface_area(&self) -> f64 {
        self.tris.iter().map(|t| self.tri_normal(t).length() as f64 * 0.5).sum()
    }

    // ------------------------------------------------------------------ split

    fn split_long_edges(&mut self, max_len: f32) {
        let max2 = max_len * max_len;
        let mut mid: FxMap<(u32, u32), u32> = FxMap::default();
        for tri in &self.tris {
            for k in 0..3 {
                let (a, b) = (tri[k], tri[(k + 1) % 3]);
                let e = edge_key(a, b);
                if mid.contains_key(&e) {
                    continue;
                }
                if (self.pos[a as usize] - self.pos[b as usize]).length_squared() > max2 {
                    let m = self.pos.len() as u32;
                    self.pos.push((self.pos[a as usize] + self.pos[b as usize]) * 0.5);
                    mid.insert(e, m);
                }
            }
        }
        if mid.is_empty() {
            return;
        }
        // Propagate feature edges through the split.
        for (&(a, b), &m) in &mid {
            if self.features.remove(&(a, b)) {
                self.features.insert(edge_key(a, m));
                self.features.insert(edge_key(m, b));
            }
        }
        let old = std::mem::take(&mut self.tris);
        let mut out: Vec<[u32; 3]> = Vec::with_capacity(old.len() + mid.len() * 2);
        for tri in old {
            let m = [
                mid.get(&edge_key(tri[0], tri[1])).copied(),
                mid.get(&edge_key(tri[1], tri[2])).copied(),
                mid.get(&edge_key(tri[2], tri[0])).copied(),
            ];
            let n = m.iter().filter(|x| x.is_some()).count();
            match n {
                0 => out.push(tri),
                3 => {
                    let (m01, m12, m20) = (m[0].unwrap(), m[1].unwrap(), m[2].unwrap());
                    out.push([tri[0], m01, m20]);
                    out.push([m01, tri[1], m12]);
                    out.push([m20, m12, tri[2]]);
                    out.push([m01, m12, m20]);
                }
                1 => {
                    // Rotate so the split edge is (a,b).
                    let k = m.iter().position(|x| x.is_some()).unwrap();
                    let (a, b, c) = (tri[k], tri[(k + 1) % 3], tri[(k + 2) % 3]);
                    let mm = m[k].unwrap();
                    out.push([a, mm, c]);
                    out.push([mm, b, c]);
                }
                _ => {
                    // Two split edges: rotate so the unsplit edge is (c,a): split edges (a,b),(b,c).
                    let k = m.iter().position(|x| x.is_none()).unwrap(); // unsplit edge index
                    let (a, b, c) = (tri[(k + 1) % 3], tri[(k + 2) % 3], tri[k]);
                    let m1 = m[(k + 1) % 3].unwrap(); // on (a,b)
                    let m2 = m[(k + 2) % 3].unwrap(); // on (b,c)
                    out.push([m1, b, m2]);
                    let d1 = (self.pos[a as usize] - self.pos[m2 as usize]).length_squared();
                    let d2 = (self.pos[m1 as usize] - self.pos[c as usize]).length_squared();
                    if d1 <= d2 {
                        out.push([a, m1, m2]);
                        out.push([a, m2, c]);
                    } else {
                        out.push([a, m1, c]);
                        out.push([m1, m2, c]);
                    }
                }
            }
        }
        self.tris = out;
        self.update_kinds();
    }

    // --------------------------------------------------------------- collapse

    fn collapse_short_edges(&mut self, min_len: f32, max_len: f32) {
        let min2 = min_len * min_len;
        let max2 = max_len * max_len;
        // Minimum squared length of the (doubled-area) normal of an acceptable triangle.
        let eps_area2 = (1e-6 * min2).powi(2);
        let nv = self.pos.len();
        let mut vtris: Vec<Vec<u32>> = vec![Vec::new(); nv];
        for (t, tri) in self.tris.iter().enumerate() {
            for &v in tri {
                vtris[v as usize].push(t as u32);
            }
        }
        let mut alive = vec![true; self.tris.len()];
        // Candidate edges, shortest first.
        let mut seen: FxSet<(u32, u32)> = FxSet::default();
        let mut cands: Vec<(f32, u32, u32)> = Vec::new();
        for tri in &self.tris {
            for k in 0..3 {
                let e = edge_key(tri[k], tri[(k + 1) % 3]);
                let d2 = (self.pos[e.0 as usize] - self.pos[e.1 as usize]).length_squared();
                if d2 < min2 && seen.insert(e) {
                    cands.push((d2, e.0, e.1));
                }
            }
        }
        cands.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        let mut vdead = vec![false; nv];
        let mut ring_u: Vec<u32> = Vec::new();
        let mut ring_v: Vec<u32> = Vec::new();

        'outer: for &(_, a, b) in &cands {
            if vdead[a as usize] || vdead[b as usize] {
                continue;
            }
            // Still an edge, still short?
            let d2 = (self.pos[a as usize] - self.pos[b as usize]).length_squared();
            if d2 >= min2 {
                continue;
            }
            // Pick which vertex survives: `u` stays, `v` is removed.
            let (ka, kb) = (self.kind[a as usize], self.kind[b as usize]);
            let is_feature = self.features.contains(&edge_key(a, b));
            let (u, v) = match (ka, kb) {
                (FREE, FREE) => (a, b),
                (FREE, _) => (b, a),
                (_, FREE) => (a, b),
                _ => {
                    // Both constrained: only along a shared feature, never removing a corner.
                    if !is_feature {
                        continue;
                    }
                    match (ka, kb) {
                        (CURVE, CURVE) => (a, b),
                        (CORNER, CURVE) => (a, b),
                        (CURVE, CORNER) => (b, a),
                        _ => continue,
                    }
                }
            };
            let (ku, kv) = (self.kind[u as usize], self.kind[v as usize]);
            // A free vertex may only be pulled onto a constrained one; a constrained vertex onto a
            // constrained one along the feature. A free `v` with a constrained `u` must not have
            // other constrained neighbours through this edge's triangles (would pinch the feature).
            let new_pos = if ku == FREE && kv == FREE {
                (self.pos[u as usize] + self.pos[v as usize]) * 0.5
            } else if ku == CURVE && kv == CURVE {
                (self.pos[u as usize] + self.pos[v as usize]) * 0.5
            } else {
                self.pos[u as usize]
            };
            // Shared triangles and link condition.
            let tv = &vtris[v as usize];
            let tu = &vtris[u as usize];
            let mut shared = 0usize;
            ring_u.clear();
            ring_v.clear();
            for &t in tu.iter() {
                if !alive[t as usize] {
                    continue;
                }
                let tri = self.tris[t as usize];
                if tri.contains(&v) {
                    shared += 1;
                }
                for &w in &tri {
                    if w != u && w != v && !ring_u.contains(&w) {
                        ring_u.push(w);
                    }
                }
            }
            if shared == 0 || shared > 2 {
                continue;
            }
            for &t in tv.iter() {
                if !alive[t as usize] {
                    continue;
                }
                for &w in &self.tris[t as usize] {
                    if w != u && w != v && !ring_v.contains(&w) {
                        ring_v.push(w);
                    }
                }
            }
            let common = ring_u.iter().filter(|w| ring_v.contains(w)).count();
            if common != shared {
                continue; // would create a non-manifold configuration
            }
            // New edges must not exceed the split threshold (prevents split/collapse ping-pong).
            for &w in ring_v.iter() {
                if (self.pos[w as usize] - new_pos).length_squared() > max2 {
                    continue 'outer;
                }
            }
            if new_pos != self.pos[u as usize] {
                for &w in ring_u.iter() {
                    if (self.pos[w as usize] - new_pos).length_squared() > max2 {
                        continue 'outer;
                    }
                }
            }
            // Flip / degeneracy check on every surviving triangle around u and v.
            for &t in tv.iter().chain(tu.iter()) {
                if !alive[t as usize] {
                    continue;
                }
                let tri = self.tris[t as usize];
                if tri.contains(&u) && tri.contains(&v) {
                    continue; // removed
                }
                let old_n = self.tri_normal(&tri);
                let p = |i: u32| if i == u || i == v { new_pos } else { self.pos[i as usize] };
                let new_n = (p(tri[1]) - p(tri[0])).cross(p(tri[2]) - p(tri[0]));
                let nl = new_n.length_squared();
                if !(nl > eps_area2) || new_n.dot(old_n) <= 0.0 {
                    continue 'outer;
                }
                // Reject results whose normal swings away strongly from the old one.
                if new_n.dot(old_n) < 0.2 * (nl.sqrt() * old_n.length()) {
                    continue 'outer;
                }
            }
            // Perform the collapse v -> u.
            let tv = std::mem::take(&mut vtris[v as usize]);
            for &t in &tv {
                if !alive[t as usize] {
                    continue;
                }
                let tri = &mut self.tris[t as usize];
                if tri.contains(&u) {
                    alive[t as usize] = false;
                    for &w in tri.iter() {
                        if w != v {
                            if let Some(i) = vtris[w as usize].iter().position(|&x| x == t) {
                                vtris[w as usize].swap_remove(i);
                            }
                        }
                    }
                } else {
                    for x in tri.iter_mut() {
                        if *x == v {
                            *x = u;
                        }
                    }
                    vtris[u as usize].push(t);
                }
            }
            self.pos[u as usize] = new_pos;
            vdead[v as usize] = true;
            // Re-route feature edges of v to u.
            if kv != FREE {
                let fv: Vec<(u32, u32)> = self
                    .features
                    .iter()
                    .copied()
                    .filter(|&(x, y)| x == v || y == v)
                    .collect();
                for e in fv {
                    self.features.remove(&e);
                    let w = if e.0 == v { e.1 } else { e.0 };
                    if w != u {
                        self.features.insert(edge_key(u, w));
                    }
                }
                let cnt = self.features.iter().filter(|&&(x, y)| x == u || y == u).count();
                self.kind[u as usize] = match cnt {
                    0 => FREE,
                    2 => CURVE,
                    _ => CORNER,
                };
            }
        }
        // Compact: drop dead triangles and unreferenced vertices.
        let mut remap = vec![u32::MAX; nv];
        let mut next = 0u32;
        let mut new_tris = Vec::with_capacity(self.tris.len());
        for (t, tri) in self.tris.iter().enumerate() {
            if !alive[t] {
                continue;
            }
            for &v in tri {
                if remap[v as usize] == u32::MAX {
                    remap[v as usize] = next;
                    next += 1;
                }
            }
            new_tris.push(tri.map(|v| remap[v as usize]));
        }
        let mut new_pos = vec![Vec3::ZERO; next as usize];
        for (old, &new) in remap.iter().enumerate() {
            if new != u32::MAX {
                new_pos[new as usize] = self.pos[old];
            }
        }
        self.features = self
            .features
            .iter()
            .filter_map(|&(a, b)| {
                let (ra, rb) = (remap[a as usize], remap[b as usize]);
                if ra == u32::MAX || rb == u32::MAX || ra == rb {
                    None
                } else {
                    Some(edge_key(ra, rb))
                }
            })
            .collect();
        self.pos = new_pos;
        self.tris = new_tris;
        self.update_kinds();
    }

    // ------------------------------------------------------------------- flip

    /// Build (edge -> (tri0, tri1)) with u32::MAX for a missing second triangle.
    fn edge_map(&self) -> FxMap<(u32, u32), (u32, u32)> {
        let mut em: FxMap<(u32, u32), (u32, u32)> = FxMap::with_capacity_and_hasher(self.tris.len() * 3 / 2 + 1, FxBuild::default());
        for (t, tri) in self.tris.iter().enumerate() {
            for k in 0..3 {
                let e = edge_key(tri[k], tri[(k + 1) % 3]);
                let ent = em.entry(e).or_insert((u32::MAX, u32::MAX));
                if ent.0 == u32::MAX {
                    ent.0 = t as u32;
                } else if ent.1 == u32::MAX {
                    ent.1 = t as u32;
                } else {
                    // non-manifold: mark so it is never flipped
                    ent.1 = u32::MAX - 1;
                }
            }
        }
        em
    }

    fn flip_edges(&mut self) {
        let em = self.edge_map();
        let nv = self.pos.len();
        let mut valence = vec![0i32; nv];
        let mut boundary = vec![false; nv];
        for (&(a, b), &(_, t1)) in &em {
            valence[a as usize] += 1;
            valence[b as usize] += 1;
            if t1 == u32::MAX {
                boundary[a as usize] = true;
                boundary[b as usize] = true;
            }
        }
        let target = |v: usize| -> i32 { if boundary[v] { 4 } else { 6 } };
        let mut edges: FxSet<(u32, u32)> = em.keys().copied().collect();
        let mut touched = vec![false; self.tris.len()];
        let mut keys: Vec<(u32, u32)> = em.keys().copied().collect();
        keys.sort_unstable();
        for e in keys {
            let (t0, t1) = em[&e];
            if t1 >= u32::MAX - 1 {
                continue;
            }
            if touched[t0 as usize] || touched[t1 as usize] {
                continue;
            }
            if self.features.contains(&e) {
                continue;
            }
            let (a, b) = e;
            let tri0 = self.tris[t0 as usize];
            let tri1 = self.tris[t1 as usize];
            // Opposite vertices and orientation: in exactly one of the triangles the edge runs a->b.
            let c = tri0.iter().copied().find(|&x| x != a && x != b).unwrap();
            let d = tri1.iter().copied().find(|&x| x != a && x != b).unwrap();
            if c == d || edges.contains(&edge_key(c, d)) {
                continue;
            }
            let dev = |v: u32, delta: i32| -> i32 {
                let x = valence[v as usize] + delta - target(v as usize);
                x * x
            };
            let before = dev(a, 0) + dev(b, 0) + dev(c, 0) + dev(d, 0);
            let after = dev(a, -1) + dev(b, -1) + dev(c, 1) + dev(d, 1);
            if after >= before {
                continue;
            }
            // Keep a minimum valence of 3.
            if valence[a as usize] <= 3 || valence[b as usize] <= 3 {
                continue;
            }
            // New triangles preserving orientation. Find which triangle has a->b.
            let ab_in_0 = (0..3).any(|k| tri0[k] == a && tri0[(k + 1) % 3] == b);
            let (x, y) = if ab_in_0 { (c, d) } else { (d, c) };
            // tri with a->b has opposite x; tri with b->a has opposite y.
            // Original: (a,b,x) and (b,a,y). New: (x,a,y)?? Check: boundary a->b,b->x,x->a | b->a,a->y,y->b.
            // New must contain b->x, x->a, a->y, y->b: (x,a,y) gives x->a,a->y,y->x; (y,b,x) gives y->b,b->x,x->y.
            let n0 = [x, a, y];
            let n1 = [y, b, x];
            let len_old_min = (self.pos[a as usize] - self.pos[b as usize]).length_squared();
            let old_n = self.tri_normal(&tri0).normalize_or_zero() + self.tri_normal(&tri1).normalize_or_zero();
            let nn0 = self.tri_normal(&n0);
            let nn1 = self.tri_normal(&n1);
            let eps_area2 = (1e-6 * len_old_min).powi(2);
            if !(nn0.length_squared() > eps_area2) || !(nn1.length_squared() > eps_area2) {
                continue;
            }
            if nn0.dot(old_n) <= 0.0 || nn1.dot(old_n) <= 0.0 {
                continue;
            }
            // Avoid producing very thin triangles (convexity check via normal alignment).
            let ol = old_n.length();
            if nn0.dot(old_n) < 0.3 * nn0.length() * ol || nn1.dot(old_n) < 0.3 * nn1.length() * ol {
                continue;
            }
            // Avoid creating an edge longer than the longest removed edge by a large factor.
            let len_new = (self.pos[c as usize] - self.pos[d as usize]).length_squared();
            let len_old = (self.pos[a as usize] - self.pos[b as usize]).length_squared();
            if len_new > len_old * 2.5 {
                continue;
            }
            self.tris[t0 as usize] = n0;
            self.tris[t1 as usize] = n1;
            touched[t0 as usize] = true;
            touched[t1 as usize] = true;
            valence[a as usize] -= 1;
            valence[b as usize] -= 1;
            valence[c as usize] += 1;
            valence[d as usize] += 1;
            edges.remove(&e);
            edges.insert(edge_key(c, d));
        }
    }

    // ----------------------------------------------------------------- smooth

    fn smooth(&mut self, lambda: f32) {
        let nv = self.pos.len();
        // Neighbour lists (CSR).
        let mut pairs: Vec<(u32, u32)> = Vec::with_capacity(self.tris.len() * 6);
        for tri in &self.tris {
            for k in 0..3 {
                let (a, b) = (tri[k], tri[(k + 1) % 3]);
                pairs.push((a, b));
                pairs.push((b, a));
            }
        }
        pairs.sort_unstable();
        pairs.dedup();
        let mut start = vec![0u32; nv + 1];
        for &(a, _) in &pairs {
            start[a as usize + 1] += 1;
        }
        for i in 0..nv {
            start[i + 1] += start[i];
        }
        let nbr: Vec<u32> = pairs.iter().map(|p| p.1).collect();
        // Area-weighted vertex normals.
        let mut normals = vec![Vec3::ZERO; nv];
        for tri in &self.tris {
            let n = self.tri_normal(tri);
            for &v in tri {
                normals[v as usize] += n;
            }
        }
        let pos = &self.pos;
        let kind = &self.kind;
        let features = &self.features;
        let new_pos: Vec<Vec3> = (0..nv)
            .into_par_iter()
            .with_min_len(512)
            .map(|v| {
                let p = pos[v];
                match kind[v] {
                    FREE => {
                        let (s, e) = (start[v] as usize, start[v + 1] as usize);
                        if e <= s {
                            return p;
                        }
                        let mut c = Vec3::ZERO;
                        for &w in &nbr[s..e] {
                            c += pos[w as usize];
                        }
                        c /= (e - s) as f32;
                        let n = normals[v].normalize_or_zero();
                        let d = c - p;
                        let d = d - n * n.dot(d);
                        p + d * lambda
                    }
                    CURVE => {
                        // Slide along the feature polyline: 1D Laplacian projected on the tangent.
                        let (s, e) = (start[v] as usize, start[v + 1] as usize);
                        let mut fn_: [Option<u32>; 2] = [None, None];
                        let mut cnt = 0;
                        for &w in &nbr[s..e] {
                            if features.contains(&edge_key(v as u32, w)) {
                                if cnt < 2 {
                                    fn_[cnt] = Some(w);
                                }
                                cnt += 1;
                            }
                        }
                        if cnt != 2 {
                            return p;
                        }
                        let (p0, p1) = (pos[fn_[0].unwrap() as usize], pos[fn_[1].unwrap() as usize]);
                        let t = (p1 - p0).normalize_or_zero();
                        let d = (p0 + p1) * 0.5 - p;
                        p + t * t.dot(d) * lambda
                    }
                    _ => p,
                }
            })
            .collect();
        self.pos = new_pos;
    }

    // ---------------------------------------------------------------- project

    fn project(&mut self, bvh: &Bvh, max_dist: f32) {
        let kind = &self.kind;
        self.pos.par_iter_mut().with_min_len(256).enumerate().for_each(|(v, p)| {
            if kind[v] == CORNER {
                return;
            }
            if let Some((_, q)) = bvh.closest_point(*p, max_dist) {
                *p = q;
            }
        });
    }
}

// ---------------------------------------------------------------------------------------------
// Quad pairing

struct Candidate {
    score: f32,
    t0: u32,
    t1: u32,
    /// Quad corners in counter-clockwise order; (a,b,c) = t0, (a,c,d) = t1.
    quad: [u32; 4],
}

fn angle_at(p: Vec3, q: Vec3, r: Vec3) -> f32 {
    // Angle at p between (q-p) and (r-p).
    let a = (q - p).normalize_or_zero();
    let b = (r - p).normalize_or_zero();
    a.dot(b).clamp(-1.0, 1.0).acos()
}

/// Score for merging the two triangles across the edge (lower is better); None when the quad
/// would be unacceptable (folded, strongly non-planar or concave).
fn quad_candidate(w: &Work, t0: u32, t1: u32, e: (u32, u32)) -> Option<Candidate> {
    let tri0 = w.tris[t0 as usize];
    let tri1 = w.tris[t1 as usize];
    // In tri0 find the directed shared edge x_k -> x_{k+1}; set c = x_k, a = x_{k+1}, b = x_{k+2}.
    let k = (0..3).find(|&k| edge_key(tri0[k], tri0[(k + 1) % 3]) == e)?;
    let (c, a, b) = (tri0[k], tri0[(k + 1) % 3], tri0[(k + 2) % 3]);
    let d = tri1.iter().copied().find(|&x| x != a && x != c)?;
    // tri1 must contain the directed edge a -> c for a consistent orientation.
    if !(0..3).any(|k| tri1[k] == a && tri1[(k + 1) % 3] == c) {
        return None;
    }
    if d == b {
        return None;
    }
    let (pa, pb, pc, pd) = (w.pos[a as usize], w.pos[b as usize], w.pos[c as usize], w.pos[d as usize]);
    let n0 = (pb - pa).cross(pc - pa);
    let n1 = (pc - pa).cross(pd - pa);
    let (l0, l1) = (n0.length(), n1.length());
    if !(l0 > 0.0) || !(l1 > 0.0) {
        return None;
    }
    let cos_n = (n0.dot(n1) / (l0 * l1)).clamp(-1.0, 1.0);
    let normal_angle = cos_n.acos();
    if normal_angle > 60f32.to_radians() {
        return None;
    }
    // Interior angles of the quad a,b,c,d.
    let ang_a = angle_at(pa, pd, pb);
    let ang_b = angle_at(pb, pa, pc);
    let ang_c = angle_at(pc, pb, pd);
    let ang_d = angle_at(pd, pc, pa);
    // Angles at a and c are sums across the diagonal; keep the quad convex.
    let ang_a2 = angle_at(pa, pb, pc) + angle_at(pa, pc, pd);
    let ang_c2 = angle_at(pc, pa, pb) + angle_at(pc, pd, pa);
    let max_sum = 165f32.to_radians();
    if ang_a2 > max_sum || ang_c2 > max_sum {
        return None;
    }
    let right = std::f32::consts::FRAC_PI_2;
    let ang_dev = [ang_a, ang_b, ang_c, ang_d].iter().map(|x| (x - right).abs()).sum::<f32>() / (4.0 * right);
    let sides = [(pb - pa).length(), (pc - pb).length(), (pd - pc).length(), (pa - pd).length()];
    let (smin, smax) = sides.iter().fold((f32::INFINITY, 0f32), |(lo, hi), &s| (lo.min(s), hi.max(s)));
    if !(smin > 0.0) {
        return None;
    }
    let aspect = 1.0 - smin / smax;
    // Also penalise a diagonal much longer than the other (skewed quads).
    let diag = (pc - pa).length() / (pd - pb).length().max(1e-12);
    let skew = (diag.max(1.0 / diag) - 1.0).min(2.0) / 2.0;
    let planarity = normal_angle / (std::f32::consts::FRAC_PI_2);
    let score = 1.0 * planarity + 2.0 * ang_dev + 0.7 * aspect + 0.5 * skew;
    Some(Candidate { score, t0, t1, quad: [a, b, c, d] })
}

fn pair_quads(w: &Work) -> Vec<[u32; 4]> {
    let em = w.edge_map();
    let mut cands: Vec<Candidate> = Vec::with_capacity(em.len());
    let mut keys: Vec<(u32, u32)> = em.keys().copied().collect();
    keys.sort_unstable();
    for e in keys {
        let (t0, t1) = em[&e];
        if t1 >= u32::MAX - 1 {
            continue;
        }
        if let Some(c) = quad_candidate(w, t0, t1, e) {
            cands.push(c);
        }
    }
    cands.sort_by(|x, y| x.score.partial_cmp(&y.score).unwrap_or(std::cmp::Ordering::Equal));
    let nt = w.tris.len();
    // partner[t] = index into cands, or u32::MAX.
    let mut partner = vec![u32::MAX; nt];
    for (i, c) in cands.iter().enumerate() {
        if partner[c.t0 as usize] == u32::MAX && partner[c.t1 as usize] == u32::MAX {
            partner[c.t0 as usize] = i as u32;
            partner[c.t1 as usize] = i as u32;
        }
    }
    // Candidates per triangle (sorted by score since cands is sorted).
    let mut per_tri: Vec<Vec<u32>> = vec![Vec::new(); nt];
    for (i, c) in cands.iter().enumerate() {
        per_tri[c.t0 as usize].push(i as u32);
        per_tri[c.t1 as usize].push(i as u32);
    }
    let other = |c: &Candidate, t: u32| if c.t0 == t { c.t1 } else { c.t0 };
    // Improvement pass: an unmatched triangle t steals neighbour n from its partner m when m can
    // re-pair with another unmatched triangle o. Each such move pairs two leftover triangles.
    for t in 0..nt as u32 {
        if partner[t as usize] != u32::MAX {
            continue;
        }
        let mut best: Option<(f32, u32, u32)> = None; // (score sum, cand t-n, cand m-o)
        for &ci in &per_tri[t as usize] {
            let c = &cands[ci as usize];
            let n = other(c, t);
            let mi = partner[n as usize];
            if mi == u32::MAX {
                // Neighbour is free: pair directly.
                let sum = c.score;
                if best.map_or(true, |b| sum < b.0) {
                    best = Some((sum, ci, u32::MAX));
                }
                continue;
            }
            let m = other(&cands[mi as usize], n);
            for &cj in &per_tri[m as usize] {
                let c2 = &cands[cj as usize];
                let o = other(c2, m);
                if o == n || o == t || partner[o as usize] != u32::MAX {
                    continue;
                }
                let sum = c.score + c2.score;
                if best.map_or(true, |b| sum < b.0) {
                    best = Some((sum, ci, cj));
                }
            }
        }
        if let Some((_, ci, cj)) = best {
            let c = &cands[ci as usize];
            let n = other(c, t);
            if cj != u32::MAX {
                let c2 = &cands[cj as usize];
                partner[c2.t0 as usize] = cj;
                partner[c2.t1 as usize] = cj;
            }
            partner[t as usize] = ci;
            partner[n as usize] = ci;
        }
    }
    // Emit polygons.
    let mut out: Vec<[u32; 4]> = Vec::with_capacity(nt);
    let mut done = vec![false; nt];
    for t in 0..nt {
        if done[t] {
            continue;
        }
        let ci = partner[t];
        if ci == u32::MAX {
            let tri = w.tris[t];
            out.push([tri[0], tri[1], tri[2], NO_VERTEX]);
            done[t] = true;
        } else {
            let c = &cands[ci as usize];
            out.push(c.quad);
            done[c.t0 as usize] = true;
            done[c.t1 as usize] = true;
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------

/// Quad-dominant retopology. `source` is the clean high-poly mesh used for projection; `start`
/// is a decimated version (about 2–4× `target_faces` triangles) that seeds the remesher.
pub fn quad_dominant(source: &Mesh, start: &Mesh, opts: &RetopoOptions) -> Result<(Mesh, RetopoReport)> {
    if opts.target_faces == 0 {
        bail!("retopo: target_faces must be positive");
    }
    if source.triangle_count() == 0 {
        bail!("retopo: source mesh is empty");
    }
    let mut w = Work::from_mesh(start, opts.feature_angle_deg)?;
    let area = w.surface_area();
    if !(area > 0.0) {
        bail!("retopo: start mesh has zero area");
    }
    let target_len = target_edge_length(area, opts.target_faces);
    let max_len = target_len * 4.0 / 3.0;
    let min_len = target_len * 4.0 / 5.0;
    let bvh = Bvh::build(source);
    let diag = source.bounds().diagonal.max(1e-6);
    let proj_dist = diag; // generous: always find the closest point

    let iterations = opts.iterations.max(1);
    for it in 0..iterations {
        w.split_long_edges(max_len);
        w.collapse_short_edges(min_len, max_len);
        w.flip_edges();
        // Two tangential relaxation rounds per iteration, re-projecting each time.
        let rounds = if it + 1 == iterations { 3 } else { 2 };
        for _ in 0..rounds {
            w.smooth(0.8);
            w.project(&bvh, proj_dist);
        }
    }
    // A final valence pass after the last relaxation so the pairing sees a regular mesh.
    w.flip_edges();
    w.smooth(0.5);
    w.project(&bvh, proj_dist);

    // Polygons.
    let polygons: Vec<[u32; 4]> = if opts.quad_pairing {
        pair_quads(&w)
    } else {
        w.tris.iter().map(|t| [t[0], t[1], t[2], NO_VERTEX]).collect()
    };

    let mut mesh = Mesh { positions: w.pos.clone(), polygons, ..Default::default() };
    mesh.triangulate_polygons();
    mesh.compact();
    mesh.compute_smooth_normals();
    if !source.material_ids.is_empty() {
        let dominant = crate::decimate::dominant_material(source);
        mesh.material_ids = vec![dominant; mesh.triangle_count()];
    }

    // Colour transfer and deviation measurement.
    let hits: Vec<Option<(crate::bvh::Hit, Vec3)>> =
        mesh.positions.par_iter().with_min_len(256).map(|p| bvh.closest_point(*p, f32::INFINITY)).collect();
    if source.has_colors() {
        mesh.colors = hits
            .iter()
            .map(|h| match h {
                Some((hit, _)) => {
                    let [a, b, c] = source.tri(hit.tri as usize);
                    let (ca, cb, cc) = (source.colors[a as usize], source.colors[b as usize], source.colors[c as usize]);
                    let w0 = 1.0 - hit.u - hit.v;
                    [0, 1, 2, 3].map(|k| ca[k] * w0 + cb[k] * hit.u + cc[k] * hit.v)
                }
                None => [1.0, 1.0, 1.0, 1.0],
            })
            .collect();
    }
    let vert_dev = hits.iter().map(|h| h.as_ref().map_or(0.0, |(hit, _)| hit.t)).fold(0f32, f32::max);
    let centroid_dev = mesh
        .polygons
        .par_iter()
        .with_min_len(256)
        .map(|p| {
            let n = if p[3] == NO_VERTEX { 3 } else { 4 };
            let mut c = Vec3::ZERO;
            for &i in &p[..n] {
                c += mesh.positions[i as usize];
            }
            c /= n as f32;
            bvh.closest_point(c, f32::INFINITY).map_or(0.0, |(h, _)| h.t)
        })
        .reduce(|| 0f32, f32::max);
    let max_deviation = vert_dev.max(centroid_dev);

    // Mean polygon edge length (quad diagonals excluded).
    let mut seen: FxSet<(u32, u32)> = FxSet::default();
    let mut sum = 0f64;
    for p in &mesh.polygons {
        let n = if p[3] == NO_VERTEX { 3 } else { 4 };
        for k in 0..n {
            let e = edge_key(p[k], p[(k + 1) % n]);
            if seen.insert(e) {
                sum += (mesh.positions[e.0 as usize] - mesh.positions[e.1 as usize]).length() as f64;
            }
        }
    }
    let mean_edge_length = if seen.is_empty() { 0.0 } else { (sum / seen.len() as f64) as f32 };
    let faces = mesh.polygons.len();
    let quads = mesh.quad_count();
    let report = RetopoReport {
        faces,
        quads,
        triangles: faces - quads,
        quad_ratio: if faces > 0 { quads as f32 / faces as f32 } else { 0.0 },
        mean_edge_length,
        max_deviation,
    };
    Ok((mesh, report))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_length_matches_area() {
        // 1.7 * 1000 equilateral triangles of edge L must tile the area.
        let l = target_edge_length(100.0, 1000);
        let tri_area = 3f64.sqrt() / 4.0 * (l as f64) * (l as f64);
        assert!(((100.0 / tri_area) - 1700.0).abs() < 1.0);
    }
}
