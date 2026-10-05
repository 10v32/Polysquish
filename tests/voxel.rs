use glam::Vec3;
use polysquish::analyze::edge_stats;
use polysquish::io::pointcloud;
use polysquish::mesh::Mesh;
use polysquish::voxel::{voxel_remesh, VoxelOptions};
use rand::rngs::SmallRng;
use rand::{Rng, SeedableRng};
use std::time::Instant;

const SPHERE_VOLUME: f64 = 4.0 / 3.0 * std::f64::consts::PI;

/// UV sphere of radius 1 centred at `center`, outward winding, with vertex colours.
fn make_sphere(seg: u32, center: Vec3) -> Mesh {
    let mut m = Mesh::default();
    for i in 0..=seg {
        let v = i as f32 / seg as f32;
        let phi = v * std::f32::consts::PI;
        for j in 0..=seg * 2 {
            let u = j as f32 / (seg * 2) as f32;
            let theta = u * std::f32::consts::TAU;
            let p = Vec3::new(phi.sin() * theta.cos(), phi.cos(), phi.sin() * theta.sin());
            m.positions.push(p + center);
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

/// Two unit spheres whose centres are `d` apart, as one triangle soup, plus the union volume.
fn two_spheres(seg: u32, d: f32) -> (Mesh, f64) {
    let mut m = make_sphere(seg, Vec3::ZERO);
    m.append(&make_sphere(seg, Vec3::new(d, 0.0, 0.0)), 0);
    let d = d as f64;
    let lens = std::f64::consts::PI * (4.0 + d) * (2.0 - d).powi(2) / 12.0;
    (m, 2.0 * SPHERE_VOLUME - lens)
}

fn assert_watertight(m: &Mesh, what: &str) {
    assert!(m.triangle_count() > 0, "{what}: empty mesh");
    let (boundary, non_manifold) = edge_stats(m);
    assert_eq!((boundary, non_manifold), (0, 0), "{what}: boundary/non-manifold edges");
}

#[test]
fn two_overlapping_spheres_remesh_to_a_watertight_union() {
    let (soup, union_volume) = two_spheres(48, 0.8);
    assert!(soup.has_colors());
    let t = Instant::now();
    let out = voxel_remesh(&soup, &VoxelOptions { resolution: 96, smooth_iterations: 3, close_holes: true }).expect("remesh");
    eprintln!("two spheres @96: {:.2?}, {} quads, {} tris", t.elapsed(), out.quad_count(), out.triangle_count());
    assert_watertight(&out, "two spheres @96");
    assert!(out.quad_count() > 0, "surface nets should emit quads");
    assert_eq!(out.indices.len(), out.polygons.iter().map(|p| if p[3] == u32::MAX { 3 } else { 6 }).sum::<usize>());
    let vol = out.signed_volume();
    assert!(vol > 0.0, "normals must point outward (signed volume {vol})");
    let err = (vol - union_volume).abs() / union_volume;
    assert!(err < 0.15, "volume {vol:.3} vs union {union_volume:.3} ({:.1}% off)", err * 100.0);
    // Colours were transferred and lie in the source range.
    assert_eq!(out.colors.len(), out.positions.len());
    let bad: Vec<_> = out.colors.iter().filter(|c| !((c[2] - 0.5).abs() < 1e-3 && (c[3] - 1.0).abs() < 1e-5)).take(5).collect();
    assert!(bad.is_empty(), "{} bad colours, e.g. {bad:?}", out.colors.iter().filter(|c| !((c[2] - 0.5).abs() < 1e-3)).count());
    assert!(out.has_normals());
    // The result is a single shell.
    let (_, components) = polysquish::analyze::components(&out);
    assert_eq!(components, 1);
}

#[test]
fn sphere_with_a_hole_remeshes_watertight() {
    let mut sphere = make_sphere(40, Vec3::ZERO);
    // Remove the cap above y = 0.85 (hole radius ~0.53, far wider than the band).
    let keep: Vec<bool> = (0..sphere.triangle_count())
        .map(|t| {
            let [a, b, c] = sphere.tri(t);
            let cy = (sphere.positions[a as usize].y + sphere.positions[b as usize].y + sphere.positions[c as usize].y) / 3.0;
            cy < 0.85
        })
        .collect();
    sphere.retain_triangles(&keep);
    assert!(edge_stats(&sphere).0 > 0, "the input must actually be open");
    let out = voxel_remesh(&sphere, &VoxelOptions { resolution: 64, smooth_iterations: 3, close_holes: true }).expect("remesh");
    assert_watertight(&out, "holed sphere @64");
    let vol = out.signed_volume();
    // The cap (h = 0.15) removes ~1.6% of the volume; the lid is flat so a bit more is lost.
    assert!(vol > 0.85 * SPHERE_VOLUME && vol < 1.1 * SPHERE_VOLUME, "volume {vol}");
}

#[test]
fn point_cloud_reconstruct_sphere() {
    let mut rng = SmallRng::seed_from_u64(7);
    let mut cloud = Mesh::default();
    for _ in 0..20_000 {
        let v = loop {
            let v = Vec3::new(rng.random::<f32>() * 2.0 - 1.0, rng.random::<f32>() * 2.0 - 1.0, rng.random::<f32>() * 2.0 - 1.0);
            let l = v.length();
            if l > 1e-3 && l <= 1.0 {
                break v / l;
            }
        };
        cloud.positions.push(v);
        cloud.colors.push([(v.x + 1.0) * 0.5, (v.y + 1.0) * 0.5, (v.z + 1.0) * 0.5, 1.0]);
    }
    let t = Instant::now();
    let out = pointcloud::reconstruct(&cloud, 64).expect("reconstruct");
    eprintln!("reconstruct 20k @64: {:.2?}, {} quads", t.elapsed(), out.quad_count());
    assert_watertight(&out, "reconstructed sphere");
    assert!(out.quad_count() > 0);
    let vol = out.signed_volume();
    let err = (vol - SPHERE_VOLUME).abs() / SPHERE_VOLUME;
    assert!(vol > 0.0 && err < 0.25, "volume {vol:.3} vs {SPHERE_VOLUME:.3} ({:.1}% off)", err * 100.0);
    // Colours follow position; allow for the voxel quantisation.
    assert_eq!(out.colors.len(), out.positions.len());
    let mut worst = 0.0f32;
    for (p, c) in out.positions.iter().zip(&out.colors) {
        let n = p.normalize_or_zero();
        let expect = [(n.x + 1.0) * 0.5, (n.y + 1.0) * 0.5, (n.z + 1.0) * 0.5];
        for k in 0..3 {
            worst = worst.max((c[k] - expect[k]).abs());
        }
    }
    assert!(worst < 0.2, "colour error {worst}");
    // Without colours there are none in the output either.
    cloud.colors.clear();
    let plain = pointcloud::reconstruct(&cloud, 32).expect("reconstruct");
    assert!(!plain.has_colors());
    assert_watertight(&plain, "reconstructed sphere @32");
}

#[test]
fn splat_file_round_trips_positions_and_colours() {
    let mut m = Mesh::default();
    for i in 0..5 {
        let f = i as f32;
        m.positions.push(Vec3::new(f * 0.25, -f, 1.5 + f * f));
        m.colors.push([f / 4.0, 1.0 - f / 4.0, 0.25, 1.0]);
    }
    let path = std::env::temp_dir().join(format!("polysquish-splat-{}.splat", std::process::id()));
    pointcloud::save_splat(&path, &m).unwrap();
    assert_eq!(std::fs::metadata(&path).unwrap().len(), 5 * 32);
    let scene = polysquish::io::load_scene(&path).expect("load .splat");
    let _ = std::fs::remove_file(&path);
    assert!(pointcloud::is_point_cloud(&scene));
    assert_eq!(scene.source_format, "splat");
    assert_eq!(scene.mesh.positions, m.positions);
    assert_eq!(scene.mesh.colors.len(), 5);
    for (a, b) in scene.mesh.colors.iter().zip(&m.colors) {
        for k in 0..3 {
            assert!((a[k] - b[k]).abs() <= 1.0 / 255.0, "{a:?} vs {b:?}");
        }
    }
    assert!(polysquish::io::is_supported(&path));
}

#[test]
fn faceless_ply_loads_as_point_cloud_and_splat_ply_is_recognised() {
    let mut m = make_sphere(6, Vec3::ZERO);
    m.indices.clear();
    let dir = std::env::temp_dir();
    let p = dir.join(format!("polysquish-cloud-{}.ply", std::process::id()));
    polysquish::io::ply::save_binary(&p, &m).unwrap();
    let scene = polysquish::io::load_scene(&p).expect("point cloud ply");
    let _ = std::fs::remove_file(&p);
    assert!(pointcloud::is_point_cloud(&scene));
    assert_eq!(scene.mesh.positions.len(), m.positions.len());
    assert!(scene.mesh.has_colors());

    // A minimal ASCII Gaussian-splat PLY: one opaque splat, one transparent, one oversized.
    let p = dir.join(format!("polysquish-gs-{}.ply", std::process::id()));
    let mut s = String::from("ply\nformat ascii 1.0\nelement vertex 3\nproperty float x\nproperty float y\nproperty float z\n");
    for n in ["f_dc_0", "f_dc_1", "f_dc_2", "opacity", "scale_0", "scale_1", "scale_2", "rot_0", "rot_1", "rot_2", "rot_3"] {
        s.push_str(&format!("property float {n}\n"));
    }
    s.push_str("end_header\n");
    // colour = 0.5 + 0.2821 * f_dc  ->  (1.0, 0.5, 0.0) for (1.772, 0, -1.772)
    s.push_str("0 0 0  1.7725 0 -1.7725  4.0  -6 -6 -6  1 0 0 0\n"); // opaque, tiny -> kept
    s.push_str("1 0 0  0 0 0  -4.0  -6 -6 -6  1 0 0 0\n"); // sigmoid(-4) = 0.018 -> dropped
    s.push_str("0 1 0  0 0 0  4.0  2 -6 -6  1 0 0 0\n"); // exp(2) = 7.4 >> 5% of diagonal -> dropped
    std::fs::write(&p, s).unwrap();
    let scene = polysquish::io::load_scene(&p).expect("splat ply");
    let _ = std::fs::remove_file(&p);
    assert!(pointcloud::is_point_cloud(&scene));
    assert_eq!(scene.mesh.positions.len(), 1);
    assert_eq!(scene.mesh.positions[0], Vec3::ZERO);
    let c = scene.mesh.colors[0];
    assert!((c[0] - 1.0).abs() < 1e-3 && (c[1] - 0.5).abs() < 1e-3 && c[2].abs() < 1e-3, "{c:?}");
}

/// Timing only: `cargo test --test voxel -- --ignored --nocapture`.
#[test]
#[ignore]
fn two_spheres_timing_at_96_and_256() {
    let (soup, union_volume) = two_spheres(48, 0.8);
    for res in [96u32, 256] {
        let t = Instant::now();
        let out = voxel_remesh(&soup, &VoxelOptions { resolution: res, smooth_iterations: 3, close_holes: true }).expect("remesh");
        let dt = t.elapsed();
        let vol = out.signed_volume();
        let (b, nm) = edge_stats(&out);
        eprintln!(
            "two spheres @{res}: {dt:.2?}, {} verts, {} quads, volume {vol:.4} (union {union_volume:.4}, {:+.2}%), boundary {b}, non-manifold {nm}",
            out.vertex_count(),
            out.quad_count(),
            (vol / union_volume - 1.0) * 100.0
        );
        assert_eq!((b, nm), (0, 0));
    }
}
