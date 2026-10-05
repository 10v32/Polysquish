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

#[test]
fn rigged_fox_keeps_skin_and_animations() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/Fox.glb");
    if !path.exists() {
        eprintln!("skipping: testdata/Fox.glb missing (run scripts/fetch_testdata.sh)");
        return;
    }
    let scene = polysquish::io::load_scene(&path).unwrap();
    assert!(scene.mesh.has_skin());
    assert_eq!(scene.skeleton.as_ref().map(|s| s.joints.len()), Some(24));
    assert_eq!(scene.animations.len(), 3);
    let mut recipe = Recipe::preset("character").unwrap();
    recipe.decimate.target_triangles = Some(1200);
    recipe.bake.resolution = 256;
    recipe.uv.resolution = 256;
    recipe.bake.ao = false;
    recipe.bake.supersample = 1;
    recipe.lods.count = 1;
    let dir = std::env::temp_dir().join(format!("polysquish-rig-{}", std::process::id()));
    let result = polysquish::pipeline::squish(&scene, &recipe, &dir, "fox", &polysquish::progress::Progress::silent()).unwrap();
    let rig = result.rig.expect("rig info");
    assert_eq!(rig.joints, 24);
    assert_eq!(rig.animations, vec!["Survey", "Walk", "Run"]);
    let back = polysquish::io::load_scene(&dir.join("fox.glb")).unwrap();
    assert!(back.mesh.has_skin(), "exported GLB lost its skin");
    assert_eq!(back.skeleton.map(|s| s.joints.len()), Some(24));
    assert_eq!(back.animations.len(), 3);
    for w in &back.mesh.weights {
        let sum: f32 = w.iter().sum();
        assert!((sum - 1.0).abs() < 1e-3, "weights must be normalised, got {sum}");
    }
    assert!(result.files.iter().any(|f| f.name == "fox.fbx"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn multi_material_exports_udim_tiles_and_primitives() {
    use polysquish::mesh::Material;
    let mut mesh = make_sphere(64);
    polysquish::clean::weld(&mut mesh, 1e-6);
    polysquish::clean::remove_degenerate(&mut mesh);
    mesh.colors.clear();
    mesh.compute_smooth_normals();
    // Upper hemisphere = material 0 (red), lower = material 1 (blue).
    mesh.material_ids = (0..mesh.triangle_count())
        .map(|t| {
            let [a, b, c] = mesh.tri(t);
            let y = mesh.positions[a as usize].y + mesh.positions[b as usize].y + mesh.positions[c as usize].y;
            if y >= 0.0 { 0 } else { 1 }
        })
        .collect();
    let scene = Scene {
        name: "twotone".into(),
        mesh,
        materials: vec![
            Material { name: "red".into(), base_color: [1.0, 0.1, 0.1, 1.0], ..Default::default() },
            Material { name: "blue".into(), base_color: [0.1, 0.2, 1.0, 1.0], ..Default::default() },
        ],
        textures: vec![],
        source_format: "test".into(),
        source_bytes: 0,
        skeleton: None,
        animations: vec![],
    };
    let mut recipe = Recipe::preset("prop").unwrap();
    recipe.decimate.target_triangles = Some(2000);
    recipe.decimate.keep_materials = true;
    recipe.bake.resolution = 128;
    recipe.uv.resolution = 128;
    recipe.bake.ao = false;
    recipe.bake.supersample = 1;
    recipe.lods.count = 1;
    recipe.collision.convex_hull = false;
    let dir = std::env::temp_dir().join(format!("polysquish-udim-{}", std::process::id()));
    let progress = polysquish::progress::Progress::silent();
    let result = polysquish::pipeline::squish(&scene, &recipe, &dir, "twotone", &progress).expect("pipeline");
    let names: Vec<&str> = result.files.iter().map(|f| f.name.as_str()).collect();
    assert!(names.contains(&"twotone_albedo.1001.png"), "{names:?}");
    assert!(names.contains(&"twotone_albedo.1002.png"), "{names:?}");
    let mean = |f: &str| -> [f64; 3] {
        let img = image::open(dir.join(f)).unwrap().to_rgba8();
        let mut s = [0f64; 3];
        let mut n = 0.0;
        for p in img.pixels() {
            for k in 0..3 { s[k] += p.0[k] as f64; }
            n += 1.0;
        }
        [s[0] / n, s[1] / n, s[2] / n]
    };
    let t1 = mean("twotone_albedo.1001.png");
    let t2 = mean("twotone_albedo.1002.png");
    assert!(t1[0] > t1[2] + 40.0, "tile 1001 should be red-ish: {t1:?}");
    assert!(t2[2] > t2[0] + 40.0, "tile 1002 should be blue-ish: {t2:?}");
    // GLB: two materials, LOD0 mesh split into two primitives.
    let (doc, _, _) = gltf::import(dir.join("twotone.glb")).unwrap();
    assert_eq!(doc.materials().count(), 2);
    let lod0 = doc.meshes().next().unwrap();
    assert_eq!(lod0.primitives().count(), 2);
    // OBJ: two usemtl groups referencing the tile textures.
    let obj = std::fs::read_to_string(dir.join("twotone.obj")).unwrap();
    assert_eq!(obj.matches("usemtl ").count(), 2, "expected two material groups");
    let mtl = std::fs::read_to_string(dir.join("twotone.mtl")).unwrap();
    assert!(mtl.contains("twotone_albedo.1001.png") && mtl.contains("twotone_albedo.1002.png"));
    let _ = std::fs::remove_dir_all(&dir);
}
