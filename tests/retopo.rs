use glam::Vec3;
use polysquish::analyze;
use polysquish::bvh::Bvh;
use polysquish::mesh::{Mesh, NO_VERTEX};
use polysquish::recipe::DecimateOptions;
use polysquish::retopo::{quad_dominant, target_edge_length, RetopoOptions};
use std::collections::HashMap;

/// UV sphere (seg × 2·seg), outward winding, with vertex colours (same as tests/pipeline.rs).
fn make_sphere(seg: u32) -> Mesh {
    let mut m = Mesh::default();
    for i in 0..=seg {
        let v = i as f32 / seg as f32;
        let phi = v * std::f32::consts::PI;
        for j in 0..=seg * 2 {
            let u = j as f32 / (seg * 2) as f32;
            let theta = u * std::f32::consts::TAU;
            let p = Vec3::new(phi.sin() * theta.cos(), phi.cos(), phi.sin() * theta.sin());
            m.positions.push(p);
            m.colors.push([u, v, 0.5, 1.0]);
        }
    }
    let w = seg * 2 + 1;
    for i in 0..seg {
        for j in 0..seg * 2 {
            let a = i * w + j;
            let b = a + 1;
            let c = a + w;
            let d = c + 1;
            m.indices.extend_from_slice(&[a, b, c, b, d, c]);
        }
    }
    m
}

/// Closed, welded sphere (seam and poles merged, pole slivers removed).
fn closed_sphere(seg: u32) -> Mesh {
    let mut m = make_sphere(seg);
    polysquish::clean::weld(&mut m, 1e-5);
    polysquish::clean::remove_degenerate(&mut m);
    m.compute_smooth_normals();
    m
}

/// Noisy high-poly source: subdivide `levels` times and displace.
fn noisy(mut m: Mesh, levels: u32) -> Mesh {
    for _ in 0..levels {
        m = polysquish::synth::subdivide_midpoint(&m);
    }
    polysquish::synth::displace(&mut m, 0.01, 3.0, 7);
    m
}

fn decimate(m: &Mesh, tris: usize) -> Mesh {
    let opts = DecimateOptions { lock_border: true, ..Default::default() };
    let (mut d, _) = polysquish::decimate::simplify_to(m, tris, &opts, 1.0).expect("decimate");
    d.compute_smooth_normals();
    d
}

fn max_deviation(result: &Mesh, source: &Mesh) -> f32 {
    let bvh = Bvh::build(source);
    result
        .positions
        .iter()
        .map(|p| bvh.closest_point(*p, f32::INFINITY).map(|(h, _)| h.t).unwrap_or(f32::INFINITY))
        .fold(0f32, f32::max)
}

fn degenerate_polygons(m: &Mesh) -> usize {
    let mut bad = 0;
    for p in &m.polygons {
        let n = if p[3] == NO_VERTEX { 3 } else { 4 };
        let mut distinct = true;
        for i in 0..n {
            for j in i + 1..n {
                if p[i] == p[j] {
                    distinct = false;
                }
            }
        }
        if !distinct {
            bad += 1;
            continue;
        }
        let pos = |i: usize| m.positions[p[i] as usize];
        let a0 = (pos(1) - pos(0)).cross(pos(2) - pos(0)).length_squared();
        let a1 = if n == 4 { (pos(2) - pos(0)).cross(pos(3) - pos(0)).length_squared() } else { 1.0 };
        if !(a0 > 0.0) || !(a1 > 0.0) {
            bad += 1;
        }
    }
    bad
}

/// Number of closed boundary loops; panics if any boundary vertex is not on exactly 2 boundary edges.
fn boundary_loops(m: &Mesh) -> usize {
    let mut count: HashMap<(u32, u32), u32> = HashMap::new();
    for t in 0..m.triangle_count() {
        let tri = m.tri(t);
        for k in 0..3 {
            let (a, b) = (tri[k], tri[(k + 1) % 3]);
            *count.entry(if a < b { (a, b) } else { (b, a) }).or_insert(0) += 1;
        }
    }
    let mut adj: HashMap<u32, Vec<u32>> = HashMap::new();
    for (&(a, b), &c) in &count {
        if c == 1 {
            adj.entry(a).or_default().push(b);
            adj.entry(b).or_default().push(a);
        }
    }
    for (v, n) in &adj {
        assert_eq!(n.len(), 2, "boundary vertex {v} has {} boundary edges", n.len());
    }
    let mut seen: HashMap<u32, bool> = HashMap::new();
    let mut loops = 0;
    for &v in adj.keys() {
        if seen.contains_key(&v) {
            continue;
        }
        loops += 1;
        let mut stack = vec![v];
        while let Some(x) = stack.pop() {
            if seen.insert(x, true).is_some() {
                continue;
            }
            for &y in &adj[&x] {
                if !seen.contains_key(&y) {
                    stack.push(y);
                }
            }
        }
    }
    loops
}

#[test]
fn quad_dominant_sphere_meets_quality_targets() {
    // 45×90 sphere subdivided twice ≈ 127k noisy triangles.
    let source = noisy(closed_sphere(45), 2);
    assert!(source.triangle_count() > 100_000, "{}", source.triangle_count());
    let start = decimate(&source, 8_000);
    assert!((7_000..=8_100).contains(&start.triangle_count()), "{}", start.triangle_count());
    let target_faces = 3_000;
    let opts = RetopoOptions { target_faces, ..Default::default() };

    let t0 = std::time::Instant::now();
    let (mesh, rep) = quad_dominant(&source, &start, &opts).expect("retopo");
    let secs = t0.elapsed().as_secs_f32();
    eprintln!(
        "retopo sphere: {} faces ({} quads, {} tris, ratio {:.3}), mean edge {:.4}, deviation {:.5}, {:.2}s",
        rep.faces, rep.quads, rep.triangles, rep.quad_ratio, rep.mean_edge_length, rep.max_deviation, secs
    );

    assert!(rep.quad_ratio >= 0.6, "quad ratio {}", rep.quad_ratio);
    let diag = source.bounds().diagonal;
    let dev = max_deviation(&mesh, &source);
    assert!(dev <= 0.01 * diag, "deviation {dev} > 1% of {diag}");
    assert!(rep.max_deviation <= 0.01 * diag, "reported deviation {}", rep.max_deviation);
    let (boundary, non_manifold) = analyze::edge_stats(&mesh);
    assert_eq!(non_manifold, 0);
    assert_eq!(boundary, 0);
    let lo = (target_faces as f32 * 0.7) as usize;
    let hi = (target_faces as f32 * 1.3) as usize;
    assert!((lo..=hi).contains(&rep.faces), "{} faces not within ±30% of {target_faces}", rep.faces);
    let l = target_edge_length(start.surface_area(), target_faces);
    assert!(
        (rep.mean_edge_length - l).abs() <= 0.25 * l,
        "mean edge {} vs target {l}",
        rep.mean_edge_length
    );
    assert_eq!(degenerate_polygons(&mesh), 0);
    assert_eq!(analyze::degenerate_count(&mesh), 0);
    assert_eq!(mesh.triangle_count(), rep.quads * 2 + rep.triangles);
    assert!(mesh.has_colors() && mesh.colors.len() == mesh.vertex_count());
    assert!(mesh.has_normals() && !mesh.has_uvs());
    // Consistent winding: the closed result must enclose a positive volume close to the source's.
    let (vs, vr) = (source.signed_volume(), mesh.signed_volume());
    assert!(vr > 0.0 && (vr - vs).abs() < 0.05 * vs, "volume {vr} vs {vs}");
}

#[test]
fn sphere_with_a_hole_keeps_one_boundary_loop() {
    let mut source = noisy(closed_sphere(32), 1);
    let keep: Vec<bool> = (0..source.triangle_count())
        .map(|t| {
            let [a, b, c] = source.tri(t);
            let y = (source.positions[a as usize].y + source.positions[b as usize].y + source.positions[c as usize].y) / 3.0;
            y < 0.6
        })
        .collect();
    source.retain_triangles(&keep);
    assert_eq!(boundary_loops(&source), 1);
    let start = decimate(&source, 4_000);
    assert_eq!(boundary_loops(&start), 1);
    let opts = RetopoOptions { target_faces: 1_200, ..Default::default() };
    let (mesh, rep) = quad_dominant(&source, &start, &opts).expect("retopo");
    eprintln!("retopo holed sphere: {} faces, ratio {:.3}, deviation {:.5}", rep.faces, rep.quad_ratio, rep.max_deviation);
    let (_, non_manifold) = analyze::edge_stats(&mesh);
    assert_eq!(non_manifold, 0);
    assert_eq!(boundary_loops(&mesh), 1);
    assert_eq!(degenerate_polygons(&mesh), 0);
    assert!(rep.quad_ratio >= 0.5, "quad ratio {}", rep.quad_ratio);
    let diag = source.bounds().diagonal;
    assert!(max_deviation(&mesh, &source) <= 0.01 * diag);
    // The hole must not have grown or shrunk much: boundary vertices stay near y = 0.6.
    let mut count: HashMap<(u32, u32), u32> = HashMap::new();
    for t in 0..mesh.triangle_count() {
        let tri = mesh.tri(t);
        for k in 0..3 {
            let (a, b) = (tri[k], tri[(k + 1) % 3]);
            *count.entry(if a < b { (a, b) } else { (b, a) }).or_insert(0) += 1;
        }
    }
    for (&(a, b), &c) in &count {
        if c == 1 {
            for v in [a, b] {
                let y = mesh.positions[v as usize].y;
                assert!((y - 0.6).abs() < 0.08, "boundary vertex drifted to y = {y}");
            }
        }
    }
}

#[test]
fn box_corners_are_pinned_as_features() {
    // A 2×1×1 box: every edge is a 90° feature; the eight corners must survive.
    let mut m = Mesh::default();
    let (sx, sy, sz) = (1.0f32, 0.5f32, 0.5f32);
    let faces: [([f32; 3], [f32; 3], [f32; 3]); 6] = [
        ([0.0, 0.0, 1.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
        ([0.0, 0.0, -1.0], [0.0, 1.0, 0.0], [1.0, 0.0, 0.0]),
        ([1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]),
        ([-1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 1.0, 0.0]),
        ([0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [1.0, 0.0, 0.0]),
        ([0.0, -1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]),
    ];
    let half = Vec3::new(sx, sy, sz);
    let n = 8u32;
    for (nrm, u, v) in faces {
        let (nrm, u, v) = (Vec3::from(nrm), Vec3::from(u), Vec3::from(v));
        let base = m.positions.len() as u32;
        for i in 0..=n {
            for j in 0..=n {
                let fu = i as f32 / n as f32 * 2.0 - 1.0;
                let fv = j as f32 / n as f32 * 2.0 - 1.0;
                m.positions.push((nrm + u * fu + v * fv) * half);
            }
        }
        for i in 0..n {
            for j in 0..n {
                let a = base + i * (n + 1) + j;
                let b = a + 1;
                let c = a + n + 1;
                let d = c + 1;
                m.indices.extend_from_slice(&[a, c, b, b, c, d]);
            }
        }
    }
    polysquish::clean::weld(&mut m, 1e-5);
    polysquish::clean::fix_winding(&mut m);
    assert!(m.signed_volume() > 0.0);
    m.compute_smooth_normals();
    let source = polysquish::synth::subdivide_midpoint(&polysquish::synth::subdivide_midpoint(&m));
    let opts = RetopoOptions { target_faces: 400, ..Default::default() };
    let (mesh, rep) = quad_dominant(&source, &m, &opts).expect("retopo");
    eprintln!("retopo box: {} faces, ratio {:.3}, deviation {:.5}", rep.faces, rep.quad_ratio, rep.max_deviation);
    let (boundary, non_manifold) = analyze::edge_stats(&mesh);
    assert_eq!((boundary, non_manifold), (0, 0));
    for sx in [-1.0f32, 1.0] {
        for sy in [-0.5f32, 0.5] {
            for sz in [-0.5f32, 0.5] {
                let corner = Vec3::new(sx, sy, sz);
                let d = mesh.positions.iter().map(|p| (*p - corner).length()).fold(f32::INFINITY, f32::min);
                assert!(d < 1e-4, "corner {corner} lost (nearest vertex {d} away)");
            }
        }
    }
    let diag = source.bounds().diagonal;
    assert!(rep.max_deviation <= 0.01 * diag, "deviation {}", rep.max_deviation);
    assert_eq!(degenerate_polygons(&mesh), 0);
}

/// Performance target: a 100k-triangle start mesh should finish in well under 20 s on 4 cores.
/// Run with `cargo test --test retopo -- --ignored --nocapture`.
#[test]
#[ignore]
fn perf_100k_triangle_start() {
    let source = noisy(closed_sphere(45), 2); // ≈127k triangles
    let start = source.clone(); // use the full-resolution mesh itself as the start
    let opts = RetopoOptions { target_faces: 40_000, ..Default::default() };
    let t0 = std::time::Instant::now();
    let (mesh, rep) = quad_dominant(&source, &start, &opts).expect("retopo");
    let secs = t0.elapsed().as_secs_f32();
    eprintln!(
        "retopo perf: start {} tris -> {} faces (ratio {:.3}), mean edge {:.4}, deviation {:.5}, {:.2}s",
        start.triangle_count(), rep.faces, rep.quad_ratio, rep.mean_edge_length, rep.max_deviation, secs
    );
    let (boundary, non_manifold) = analyze::edge_stats(&mesh);
    assert_eq!((boundary, non_manifold), (0, 0));
    assert!(secs < 20.0, "took {secs}s");
}
