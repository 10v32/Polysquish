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
