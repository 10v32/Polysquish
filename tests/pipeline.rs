use glam::Vec3;
use polysquish::mesh::{Mesh, Scene};
use polysquish::recipe::{CleanupOptions, Recipe};

fn make_sphere(seg: u32) -> Mesh {
    // UV sphere, outward winding, with vertex colours.
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

#[test]
fn sphere_is_outward_and_winding_fix_is_a_noop() {
    let mut m = make_sphere(24);
    assert!(m.signed_volume() > 0.0, "test sphere should wind outward");
    m.normals.clear();
    let flipped = polysquish::clean::fix_winding(&mut m);
    assert_eq!(flipped, 0);
    // Flip everything, then repair.
    for t in m.indices.chunks_mut(3) {
        t.swap(1, 2);
    }
    assert!(m.signed_volume() < 0.0);
    let flipped = polysquish::clean::fix_winding(&mut m);
    assert_eq!(flipped, m.triangle_count());
    assert!(m.signed_volume() > 0.0);
}

#[test]
fn weld_merges_duplicates_and_degenerates_are_removed() {
    let mut m = make_sphere(8);
    let before = m.vertex_count();
    let dup = m.positions[5];
    m.positions.push(dup);
    m.colors.push([0.0; 4]);
    m.indices.extend_from_slice(&[0, 1, (m.positions.len() - 1) as u32]);
    m.indices.extend_from_slice(&[2, 2, 3]); // degenerate
    let rep = polysquish::clean::clean(&mut m, &CleanupOptions::default());
    assert!(rep.welded_vertices >= 1);
    assert!(rep.removed_degenerate >= 1); // the explicit one plus collapsed pole triangles
    assert!(m.vertex_count() <= before);
}

#[test]
fn floaters_are_removed() {
    let mut m = make_sphere(16);
    let base = m.positions.len() as u32;
    for p in [Vec3::new(3.0, 0.0, 0.0), Vec3::new(3.01, 0.0, 0.0), Vec3::new(3.0, 0.01, 0.0), Vec3::new(3.0, 0.0, 0.01)] {
        m.positions.push(p);
        m.colors.push([1.0; 4]);
    }
    m.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 3, base + 1, base, base + 2, base + 3, base + 1, base + 3, base + 2]);
    let (c, t) = polysquish::clean::remove_floaters(&mut m, 0.001);
    assert_eq!((c, t), (1, 4));
}

#[test]
fn bvh_hits_the_sphere() {
    let m = make_sphere(32);
    let bvh = polysquish::bvh::Bvh::build(&m);
    let hit = bvh.intersect(Vec3::new(0.0, 0.0, -5.0), Vec3::Z, 10.0).expect("ray should hit");
    assert!((hit.t - 4.0).abs() < 0.02, "t = {}", hit.t);
    assert!(bvh.intersect(Vec3::new(0.0, 5.0, -5.0), Vec3::Z, 10.0).is_none());
    assert!(bvh.occluded(Vec3::ZERO, Vec3::X, 2.0));
}

#[test]
fn full_pipeline_runs_on_a_sphere() {
    let mut mesh = make_sphere(96); // ~36k triangles
    mesh.compute_smooth_normals();
    let scene = Scene { name: "sphere".into(), mesh, materials: vec![Default::default()], textures: vec![], source_format: "test".into(), source_bytes: 0, skeleton: None, animations: vec![] };
    let mut recipe = Recipe::preset("mobile").unwrap();
    recipe.bake.resolution = 256;
    recipe.uv.resolution = 256;
    recipe.bake.ao_samples = 4;
    recipe.bake.supersample = 1;
    let dir = std::env::temp_dir().join(format!("polysquish-test-{}", std::process::id()));
    let progress = polysquish::progress::Progress::silent();
    let result = polysquish::pipeline::squish(&scene, &recipe, &dir, "sphere", &progress).expect("pipeline");
    assert!(result.after.triangles <= 1_600 && result.after.triangles >= 1_000, "{}", result.after.triangles);
    assert!(result.files.iter().any(|f| f.name == "sphere.glb"));
    assert!(result.files.iter().any(|f| f.name == "sphere_albedo.png"));
    assert!(result.files.iter().any(|f| f.name == "sphere_normal.png"));
    assert_eq!(result.after.lods.len(), 3);
    // Re-import what we wrote and check it is sane.
    let back = polysquish::io::load_scene(&dir.join("sphere.glb")).expect("re-import glb");
    assert!(back.mesh.has_uvs());
    assert_eq!(back.mesh.triangle_count(), result.after.triangles);
    let obj = polysquish::io::load_scene(&dir.join("sphere.obj")).expect("re-import obj");
    assert_eq!(obj.mesh.triangle_count(), result.after.triangles);
    // The baked normal map of a sphere projected onto a coarser sphere must be mostly flat blue.
    let img = image::open(dir.join("sphere_normal.png")).unwrap().to_rgba8();
    let mut sum = [0u64; 3];
    for p in img.pixels() {
        for k in 0..3 {
            sum[k] += p.0[k] as u64;
        }
    }
    let n = img.pixels().len() as u64;
    let mean: Vec<f64> = sum.iter().map(|s| *s as f64 / n as f64).collect();
    assert!((mean[0] - 128.0).abs() < 8.0 && (mean[1] - 128.0).abs() < 8.0 && mean[2] > 235.0, "mean normal {mean:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ply_roundtrip_keeps_colours() {
    let m = make_sphere(10);
    let dir = std::env::temp_dir();
    let p = dir.join(format!("polysquish-rt-{}.ply", std::process::id()));
    polysquish::io::ply::save_binary(&p, &m).unwrap();
    let back = polysquish::io::load_scene(&p).unwrap();
    assert_eq!(back.mesh.triangle_count(), m.triangle_count());
    assert!(back.mesh.has_colors());
    let _ = std::fs::remove_file(&p);
}

#[test]
fn chunked_decimation_stitches_back_into_one_shell() {
    let mut m = make_sphere(120); // ~57k triangles
    polysquish::clean::weld(&mut m, 1e-6);
    polysquish::clean::remove_degenerate(&mut m);
    m.compute_smooth_normals();
    let (b0, _) = polysquish::analyze::edge_stats(&m);
    assert_eq!(b0, 0, "welded sphere should be closed");
    let opts = polysquish::recipe::DecimateOptions { target_triangles: Some(3000), ..Default::default() };
    let (out, rep, chunks) = polysquish::decimate::decimate_chunked(&m, &opts, 8000).unwrap();
    assert!(chunks >= 2, "expected several chunks, got {chunks}");
    let (_, count) = polysquish::analyze::components(&out);
    let (boundary, non_manifold) = polysquish::analyze::edge_stats(&out);
    assert_eq!(count, 1, "chunks were not stitched back together");
    assert_eq!(boundary, 0, "stitch left open edges");
    assert_eq!(non_manifold, 0);
    assert!(out.triangle_count() <= 3600 && out.triangle_count() >= 2000, "{}", out.triangle_count());
    assert!(rep.error < 0.05, "deviation {}", rep.error);
}


#[test]
fn hidden_inner_shell_is_removed_but_outer_kept() {
    let mut outer = make_sphere(48);
    let mut inner = make_sphere(24);
    for p in &mut inner.positions {
        *p *= 0.5;
    }
    let inner_tris = inner.triangle_count();
    outer.append(&inner, 0);
    polysquish::clean::weld(&mut outer, 1e-6);
    polysquish::clean::remove_degenerate(&mut outer);
    let before = outer.triangle_count();
    let bvh = polysquish::bvh::Bvh::build(&outer);
    let removed = polysquish::clean::remove_hidden(&mut outer, &bvh, 32);
    assert!(removed > 0, "inner shell should be detected");
    assert!(removed <= inner_tris, "removed {removed} > inner {inner_tris}");
    assert!(removed as f32 > inner_tris as f32 * 0.9, "removed only {removed} of {inner_tris}");
    assert_eq!(outer.triangle_count(), before - removed);
    let (_, comps) = polysquish::analyze::components(&outer);
    assert_eq!(comps, 1);
}

#[test]
fn hard_edge_split_keeps_geometry_consistent() {
    // A cube with 90° edges: every corner gets 3 normals, positions must be unchanged per index.
    let mut m = polysquish::collision::bounding_box(&make_sphere(8));
    let before = m.clone();
    let added = polysquish::normals::split_hard_edges(&mut m, 60.0);
    assert_eq!(m.vertex_count(), 24, "cube should split into 24 vertices (added {added})");
    assert_eq!(m.triangle_count(), 12);
    for t in 0..m.triangle_count() {
        let [a, b, c] = m.tri(t);
        let [oa, ob, oc] = before.tri(t);
        assert_eq!(m.positions[a as usize], before.positions[oa as usize]);
        assert_eq!(m.positions[b as usize], before.positions[ob as usize]);
        assert_eq!(m.positions[c as usize], before.positions[oc as usize]);
        // Vertex normal must match the face normal exactly on a cube.
        let fnrm = m.face_normal(t);
        assert!(m.normals[a as usize].dot(fnrm) > 0.999);
    }
}
