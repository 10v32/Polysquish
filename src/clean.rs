//! Cleanup: welding, degenerate removal, floater removal and winding repair.

use crate::analyze;
use crate::mesh::Mesh;
use crate::recipe::CleanupOptions;
use glam::Vec3;
use std::collections::HashMap;

#[derive(Default, Debug, Clone, serde::Serialize)]
pub struct CleanReport {
    pub welded_vertices: usize,
    pub removed_degenerate: usize,
    pub removed_floaters: usize,
    pub removed_floater_triangles: usize,
    pub flipped_triangles: usize,
    pub messages: Vec<String>,
}

fn fmt(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// Weld vertices whose positions fall within `tolerance` (absolute) of each other,
/// using a uniform grid hash. Attributes of the first vertex in each cell win.
pub fn weld(mesh: &mut Mesh, tolerance: f32) -> usize {
    let n = mesh.vertex_count();
    if n == 0 {
        return 0;
    }
    let inv = if tolerance > 0.0 { 1.0 / tolerance } else { 0.0 };
    let mut map: HashMap<[i64; 3], u32> = HashMap::with_capacity(n);
    let mut remap = vec![u32::MAX; n];
    let mut next = 0u32;
    let key_of = |p: Vec3| -> [i64; 3] {
        if inv == 0.0 {
            [p.x.to_bits() as i64, p.y.to_bits() as i64, p.z.to_bits() as i64]
        } else {
            [
                (p.x * inv).round() as i64,
                (p.y * inv).round() as i64,
                (p.z * inv).round() as i64,
            ]
        }
    };
    for (i, p) in mesh.positions.iter().enumerate() {
        let k = key_of(*p);
        match map.get(&k) {
            Some(&j) => remap[i] = j,
            None => {
                map.insert(k, next);
                remap[i] = next;
                next += 1;
            }
        }
    }
    let welded = n - next as usize;
    if welded > 0 {
        mesh.apply_vertex_remap(&remap, next as usize);
    }
    welded
}

/// Remove degenerate (repeated index or zero-area) triangles.
pub fn remove_degenerate(mesh: &mut Mesh) -> usize {
    let tc = mesh.triangle_count();
    let mut keep = vec![true; tc];
    let mut removed = 0;
    for t in 0..tc {
        let [a, b, c] = mesh.tri(t);
        let bad = a == b || b == c || a == c || !(mesh.face_area2(t).length_squared() > 0.0);
        if bad {
            keep[t] = false;
            removed += 1;
        }
    }
    if removed > 0 {
        mesh.retain_triangles(&keep);
    }
    removed
}

/// Remove connected components smaller than `min_fraction` of the total triangle count
/// AND smaller than `min_fraction_diag` of the bounding diagonal. Returns (components, triangles) removed.
pub fn remove_floaters(mesh: &mut Mesh, min_fraction: f32) -> (usize, usize) {
    let (ids, count) = analyze::components(mesh);
    if count <= 1 {
        return (0, 0);
    }
    let tc = mesh.triangle_count();
    let mut comp_tris = vec![0usize; count];
    let mut comp_min = vec![Vec3::splat(f32::INFINITY); count];
    let mut comp_max = vec![Vec3::splat(f32::NEG_INFINITY); count];
    for t in 0..tc {
        let tri = mesh.tri(t);
        let c = ids[tri[0] as usize] as usize;
        comp_tris[c] += 1;
        for i in tri {
            let p = mesh.positions[i as usize];
            comp_min[c] = comp_min[c].min(p);
            comp_max[c] = comp_max[c].max(p);
        }
    }
    let diag = mesh.bounds().diagonal;
    let tri_threshold = (tc as f32 * min_fraction).max(32.0) as usize;
    let size_threshold = diag * (min_fraction * 10.0).clamp(0.005, 0.2);
    let mut drop = vec![false; count];
    let mut dropped_c = 0;
    let mut dropped_t = 0;
    let largest = comp_tris.iter().copied().max().unwrap_or(0);
    for c in 0..count {
        if comp_tris[c] == largest {
            continue;
        }
        let size = (comp_max[c] - comp_min[c]).length();
        if comp_tris[c] < tri_threshold && size < size_threshold {
            drop[c] = true;
            dropped_c += 1;
            dropped_t += comp_tris[c];
        }
    }
    if dropped_c > 0 {
        let keep: Vec<bool> = (0..tc)
            .map(|t| !drop[ids[mesh.indices[t * 3] as usize] as usize])
            .collect();
        mesh.retain_triangles(&keep);
    }
    (dropped_c, dropped_t)
}

/// Make winding consistent across each connected component by flood fill, then orient
/// each component so its signed volume is positive (normals point outward).
pub fn fix_winding(mesh: &mut Mesh) -> usize {
    let tc = mesh.triangle_count();
    if tc == 0 {
        return 0;
    }
    let diag = mesh.bounds().diagonal.max(1e-9) as f64;
    // Below this, a component is essentially flat and its signed volume says nothing.
    let min_volume = diag * diag * diag * 1e-4;
    let src_normals = if mesh.has_normals() { Some(mesh.normals.clone()) } else { None };
    // Build edge -> triangles adjacency (directed key normalised to (min,max)).
    let mut edge_map: HashMap<(u32, u32), Vec<u32>> = HashMap::with_capacity(tc * 3);
    for t in 0..tc {
        let tri = mesh.tri(t);
        for k in 0..3 {
            let a = tri[k];
            let b = tri[(k + 1) % 3];
            let key = if a < b { (a, b) } else { (b, a) };
            edge_map.entry(key).or_default().push(t as u32);
        }
    }
    let mut visited = vec![false; tc];
    let mut flip = vec![false; tc];
    let mut flipped = 0usize;
    let mut stack: Vec<u32> = Vec::new();
    let mut comp_tris: Vec<u32> = Vec::new();

    let directed = |mesh: &Mesh, t: usize, fl: bool| -> [(u32, u32); 3] {
        let [a, b, c] = mesh.tri(t);
        if fl {
            [(a, c), (c, b), (b, a)]
        } else {
            [(a, b), (b, c), (c, a)]
        }
    };

    for seed in 0..tc {
        if visited[seed] {
            continue;
        }
        visited[seed] = true;
        stack.clear();
        stack.push(seed as u32);
        comp_tris.clear();
        while let Some(t) = stack.pop() {
            let t = t as usize;
            comp_tris.push(t as u32);
            for (a, b) in directed(mesh, t, flip[t]) {
                let key = if a < b { (a, b) } else { (b, a) };
                let Some(neigh) = edge_map.get(&key) else { continue };
                if neigh.len() != 2 {
                    continue; // boundary or non-manifold: don't propagate
                }
                for &nt in neigh {
                    let nt = nt as usize;
                    if nt == t || visited[nt] {
                        continue;
                    }
                    // Neighbour is consistent if it traverses the edge as (b, a).
                    let consistent = directed(mesh, nt, false)
                        .iter()
                        .any(|&(x, y)| x == b && y == a);
                    flip[nt] = !consistent;
                    visited[nt] = true;
                    stack.push(nt as u32);
                }
            }
        }
        // Orientation of the whole component: prefer agreement with the source normals,
        // otherwise positive signed volume == outward.
        let mut vol = 0.0f64;
        let mut agree = 0.0f64;
        for &t in &comp_tris {
            let t = t as usize;
            let [a, b, c] = mesh.tri(t);
            let (a, c) = if flip[t] { (c, a) } else { (a, c) };
            let pa = mesh.positions[a as usize].as_dvec3();
            let pb = mesh.positions[b as usize].as_dvec3();
            let pc = mesh.positions[c as usize].as_dvec3();
            vol += pa.dot(pb.cross(pc));
            if let Some(ns) = &src_normals {
                let fnorm = (pb - pa).cross(pc - pa);
                let vn = (ns[a as usize] + ns[b as usize] + ns[c as usize]).as_dvec3();
                agree += fnorm.dot(vn).signum() * fnorm.length();
            }
        }
        let should_flip = if src_normals.is_some() && agree != 0.0 {
            agree < 0.0
        } else {
            vol.abs() / 6.0 > min_volume && vol < 0.0
        };
        if should_flip {
            for &t in &comp_tris {
                flip[t as usize] = !flip[t as usize];
            }
        }
    }
    for t in 0..tc {
        if flip[t] {
            mesh.indices.swap(t * 3 + 1, t * 3 + 2);
            flipped += 1;
        }
    }
    flipped
}

/// Run the full cleanup pass described by `opts`.
pub fn clean(mesh: &mut Mesh, opts: &CleanupOptions) -> CleanReport {
    let mut rep = CleanReport::default();
    let diag = mesh.bounds().diagonal.max(1e-9);
    if opts.weld {
        let tol = opts.weld_tolerance * diag;
        rep.welded_vertices = weld(mesh, tol);
        if rep.welded_vertices > 0 {
            rep.messages
                .push(format!("Welded {} duplicate vertices", fmt(rep.welded_vertices)));
        }
    }
    if opts.remove_degenerate {
        rep.removed_degenerate = remove_degenerate(mesh);
        if rep.removed_degenerate > 0 {
            rep.messages
                .push(format!("Removed {} zero-area triangles", fmt(rep.removed_degenerate)));
        }
    }
    if opts.remove_floaters {
        let (c, t) = remove_floaters(mesh, opts.floater_min_fraction);
        rep.removed_floaters = c;
        rep.removed_floater_triangles = t;
        if c > 0 {
            rep.messages.push(format!(
                "Removed {} floating fragment{} ({} triangles)",
                fmt(c),
                if c == 1 { "" } else { "s" },
                fmt(t)
            ));
        }
    }
    if opts.fix_winding {
        rep.flipped_triangles = fix_winding(mesh);
        if rep.flipped_triangles > 0 {
            rep.messages.push(format!(
                "Flipped {} inside-out triangles",
                fmt(rep.flipped_triangles)
            ));
        }
    }
    // Normals are recomputed after cleanup so they match the repaired topology.
    mesh.compute_smooth_normals();
    rep
}

/// Remove faces that cannot be seen from outside the model: internal shells and parts buried
/// inside other parts. Visibility is sampled with rays (early-out: most faces escape within the
/// first few). Hidden faces are grouped into connected regions; a region is removed only when it
/// is fully enclosed (no visible neighbour) or when it is a large, deeply hidden patch (an
/// intersecting part). Small hidden patches in crevices are kept. Returns removed triangles.
pub fn remove_hidden(mesh: &mut Mesh, tracer: &dyn crate::bvh::RayTracer, samples: u32) -> usize {
    use crate::bvh::Ray;
    use rayon::prelude::*;
    let tc = mesh.triangle_count();
    if tc < 64 {
        return 0;
    }
    let samples = samples.clamp(8, 256) as usize;
    let diag = mesh.bounds().diagonal.max(1e-9);
    let far = diag * 4.0;
    let eps = diag * 2e-5;
    let face_n: Vec<Vec3> = (0..tc).map(|t| mesh.face_normal(t)).collect();
    // Ray i for face t: cosine-weighted around +n for 3/4 of the samples, around -n otherwise,
    // from one of three points on the face, with a per-face low-discrepancy rotation.
    let ray_for = |t: usize, i: usize| -> Ray {
        let [a, b, c] = mesh.tri(t);
        let (pa, pb, pc) = (mesh.positions[a as usize], mesh.positions[b as usize], mesh.positions[c as usize]);
        let n = if face_n[t] == Vec3::ZERO { Vec3::Y } else { face_n[t] };
        let pts = [(pa + pb + pc) / 3.0, pa * 0.6 + pb * 0.2 + pc * 0.2, pa * 0.2 + pb * 0.2 + pc * 0.6];
        let h = (t as u32).wrapping_mul(0x9e3779b9) ^ 0x85ebca6b;
        let shift = ((h >> 8) as f32 / (1u32 << 24) as f32, ((h.wrapping_mul(0x27d4eb2d)) >> 8) as f32 / (1u32 << 24) as f32);
        const G: f64 = 1.324_717_957_244_746;
        let u1 = ((0.5 + (i as f64 + 1.0) / G).fract() as f32 + shift.0).fract();
        let u2 = ((0.5 + (i as f64 + 1.0) / (G * G)).fract() as f32 + shift.1).fract();
        let side = if i % 4 == 3 { -n } else { n };
        let r = u1.sqrt();
        let phi = std::f32::consts::TAU * u2;
        let helper = if side.x.abs() < 0.9 { Vec3::X } else { Vec3::Y };
        let tv = helper.cross(side).normalize_or_zero();
        let bv = side.cross(tv);
        let d = (tv * (r * phi.cos()) + bv * (r * phi.sin()) + side * (1.0 - u1).max(0.0).sqrt()).normalize_or_zero();
        Ray { origin: pts[i % 3] + side * eps, dir: d, tmax: far }
    };
    // Pass 1: a few rays for every face; pass 2: the rest only for faces still fully blocked.
    let first = samples.min(8);
    let mut visible = vec![false; tc];
    let mut pending: Vec<u32> = (0..tc as u32).collect();
    for (pass, range) in [(0usize, 0..first), (1usize, first..samples)] {
        if range.is_empty() || pending.is_empty() {
            continue;
        }
        let per = range.len();
        const CHUNK: usize = 65_536;
        let mut still: Vec<u32> = Vec::new();
        for chunk in pending.chunks(CHUNK) {
            let rays: Vec<Ray> = chunk.par_iter().flat_map_iter(|&t| range.clone().map(move |i| ray_for(t as usize, i))).collect();
            let blocked = tracer.any_hits(&rays);
            for (k, &t) in chunk.iter().enumerate() {
                if blocked[k * per..(k + 1) * per].iter().any(|&b| !b) {
                    visible[t as usize] = true;
                } else {
                    still.push(t);
                }
            }
        }
        pending = still;
        let _ = pass;
    }
    let hidden_total = pending.len();
    if hidden_total == 0 {
        return 0;
    }
    // Group hidden faces into edge-connected regions and look at their contact with visible faces.
    let mut edge_faces: HashMap<(u32, u32), Vec<u32>> = HashMap::with_capacity(tc * 3);
    for t in 0..tc {
        let tri = mesh.tri(t);
        for k in 0..3 {
            let a = tri[k];
            let b = tri[(k + 1) % 3];
            let key = if a < b { (a, b) } else { (b, a) };
            edge_faces.entry(key).or_default().push(t as u32);
        }
    }
    let mut region = vec![u32::MAX; tc];
    let mut regions: Vec<(usize, usize)> = Vec::new(); // (face count, faces touching a visible face)
    let mut stack: Vec<u32> = Vec::new();
    for &seed in &pending {
        if region[seed as usize] != u32::MAX {
            continue;
        }
        let rid = regions.len() as u32;
        let mut count = 0usize;
        let mut touching = 0usize;
        region[seed as usize] = rid;
        stack.push(seed);
        while let Some(t) = stack.pop() {
            count += 1;
            let tri = mesh.tri(t as usize);
            let mut touches = false;
            for k in 0..3 {
                let a = tri[k];
                let b = tri[(k + 1) % 3];
                let key = if a < b { (a, b) } else { (b, a) };
                if let Some(fs) = edge_faces.get(&key) {
                    for &f in fs {
                        if visible[f as usize] {
                            touches = true;
                        } else if region[f as usize] == u32::MAX {
                            region[f as usize] = rid;
                            stack.push(f);
                        }
                    }
                }
            }
            if touches {
                touching += 1;
            }
        }
        regions.push((count, touching));
    }
    let mut remove_region = vec![false; regions.len()];
    for (i, &(count, touching)) in regions.iter().enumerate() {
        let enclosed = touching == 0;
        let large_and_deep = count >= (tc / 200).max(200) && (touching as f32) < 0.25 * count as f32;
        remove_region[i] = enclosed || large_and_deep;
    }
    let keep: Vec<bool> = (0..tc).map(|t| region[t] == u32::MAX || !remove_region[region[t] as usize]).collect();
    let removed = keep.iter().filter(|&&k| !k).count();
    if removed == 0 || removed as f32 / tc as f32 > 0.9 {
        return 0;
    }
    mesh.retain_triangles(&keep);
    removed
}
