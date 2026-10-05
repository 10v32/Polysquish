//! Binary FBX writer round-trip tests, verified with the independent `ufbx` parser.

use glam::{Mat4, Quat, Vec2, Vec3};
use polysquish::io::fbx_out::{self, FbxLod, FbxMaterial, FbxScene};
use polysquish::mesh::{Animation, AnimationChannel, Joint, Mesh, Skeleton, NO_VERTEX};

/// UV sphere with normals, UVs and a quad-dominant `polygons` list (poles are triangles).
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
            m.normals.push(p.normalize_or_zero());
            m.uvs.push(Vec2::new(u, v));
        }
    }
    let w = seg * 2 + 1;
    for i in 0..seg {
        for j in 0..seg * 2 {
            let a = i * w + j;
            let b = a + 1;
            let c = a + w;
            let d = c + 1;
            if i == 0 {
                m.polygons.push([a, d, c, NO_VERTEX]);
            } else if i == seg - 1 {
                m.polygons.push([a, b, d, NO_VERTEX]);
            } else {
                m.polygons.push([a, b, d, c]);
            }
        }
    }
    m.triangulate_polygons();
    m
}

fn make_box(half: f32) -> Mesh {
    let mut m = Mesh::default();
    for z in [-1.0, 1.0] {
        for y in [-1.0, 1.0] {
            for x in [-1.0, 1.0] {
                m.positions.push(Vec3::new(x, y, z) * half);
            }
        }
    }
    m.indices = vec![0, 2, 1, 1, 2, 3, 4, 5, 6, 5, 7, 6, 0, 1, 5, 0, 5, 4, 2, 6, 7, 2, 7, 3, 0, 4, 6, 0, 6, 2, 1, 3, 7, 1, 7, 5];
    m
}

fn tmp_path(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("polysquish_fbx_test_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

fn load(path: &std::path::Path) -> ufbx::SceneRoot {
    let data = std::fs::read(path).unwrap();
    assert!(data.starts_with(b"Kaydara FBX Binary  \0\x1a\0"), "magic header");
    assert_eq!(u32::from_le_bytes(data[23..27].try_into().unwrap()), 7400, "version field");
    match ufbx::load_memory(&data, ufbx::LoadOpts::default()) {
        Ok(s) => s,
        Err(e) => panic!("ufbx failed to load {}: {:?} ({})", path.display(), e.type_, e.description),
    }
}

fn find_node<'a>(scene: &'a ufbx::Scene, name: &str) -> &'a ufbx::Node {
    scene.nodes.iter().find(|n| n.element.name == name).unwrap_or_else(|| {
        let names: Vec<String> = scene.nodes.iter().map(|n| n.element.name.to_string()).collect();
        panic!("node {name:?} not found; have {names:?}")
    })
}

#[test]
fn lods_collision_material_round_trip() {
    let lod0 = make_sphere(12);
    let lod1 = make_sphere(6);
    let coll = make_box(1.1);
    let scale = 100.0f32;

    let scene = FbxScene {
        name: "ball".into(),
        lods: vec![FbxLod { name: "ball_LOD0".into(), mesh: &lod0 }, FbxLod { name: "ball_LOD1".into(), mesh: &lod1 }],
        material: FbxMaterial {
            name: "ball_material".into(),
            base_color: [0.25, 0.5, 0.75, 1.0],
            roughness: 0.4,
            metallic: 0.0,
            albedo_file: Some("test_albedo.png".into()),
            normal_file: Some("textures/test_normal.png".into()),
            ao_file: None,
            orm_file: None,
        },
        collision: vec![("UCX_ball_01".into(), &coll)],
        scale,
        skeleton: None,
        animations: &[],
    };
    let path = tmp_path("ball.fbx");
    fbx_out::write(&path, &scene).unwrap();
    let s = load(&path);

    assert_eq!(s.metadata.version, 7400);
    assert!(s.metadata.creator.starts_with("Polysquish"), "creator = {}", s.metadata.creator);
    assert!((s.settings.unit_meters - 0.01).abs() < 1e-9, "UnitScaleFactor 1.0 means centimetres");

    // Nodes: root + LodGroup + 2 LOD meshes + collision = 5.
    assert_eq!(s.nodes.len(), 5, "node count");
    assert_eq!(s.meshes.len(), 3, "mesh count");
    assert_eq!(s.lod_groups.len(), 1, "lod group count");
    assert_eq!(s.materials.len(), 1);
    assert_eq!(s.textures.len(), 2);
    assert_eq!(s.videos.len(), 2);

    // LOD group hierarchy and thresholds.
    let group = find_node(&s, "ball");
    assert_eq!(group.attrib_type, ufbx::ElementType::LodGroup);
    assert_eq!(group.children.len(), 2);
    assert_eq!(group.children[0].element.name, "ball_LOD0");
    assert_eq!(group.children[1].element.name, "ball_LOD1");
    let lg = &s.lod_groups[0];
    assert!(lg.relative_distances);
    assert_eq!(lg.lod_levels.len(), 2);
    assert!((lg.lod_levels[1].distance - 50.0).abs() < 1e-9, "LOD1 threshold {}", lg.lod_levels[1].distance);

    // LOD0 geometry.
    let n0 = find_node(&s, "ball_LOD0");
    let m0 = n0.mesh.as_ref().expect("LOD0 has a mesh");
    assert_eq!(m0.num_vertices, lod0.vertex_count(), "vertex count");
    assert_eq!(m0.num_faces, lod0.polygons.len(), "face count (polygons)");
    assert_eq!(m0.num_triangles, lod0.triangle_count(), "triangle count after triangulation");
    let quads = m0.faces.iter().filter(|f| f.num_indices == 4).count();
    let tris = m0.faces.iter().filter(|f| f.num_indices == 3).count();
    assert_eq!(quads, lod0.quad_count(), "quads preserved");
    assert_eq!(tris, lod0.polygons.len() - lod0.quad_count());
    assert!(m0.vertex_normal.exists, "normals present");
    assert!(m0.vertex_uv.exists, "uvs present");
    assert_eq!(m0.uv_sets.len(), 1);
    assert!(!m0.vertex_color.exists);

    // Positions round-trip with the scale applied, and in the same vertex order.
    for (i, p) in lod0.positions.iter().enumerate() {
        let v = m0.vertices[i];
        let e = (*p * scale).as_dvec3();
        assert!((v.x - e.x).abs() < 1e-3 && (v.y - e.y).abs() < 1e-3 && (v.z - e.z).abs() < 1e-3, "vertex {i}: {v:?} vs {e:?}");
    }
    // Polygon corners reference the same vertices; UV v is flipped; normals match.
    for (fi, poly) in lod0.polygons.iter().enumerate() {
        let f = m0.faces[fi];
        for k in 0..f.num_indices as usize {
            let ix = (f.index_begin as usize) + k;
            assert_eq!(m0.vertex_indices[ix], poly[k], "face {fi} corner {k}");
            let uv = m0.vertex_uv[ix];
            let src = lod0.uvs[poly[k] as usize];
            assert!((uv.x - src.x as f64).abs() < 1e-6 && (uv.y - (1.0 - src.y) as f64).abs() < 1e-6, "uv of face {fi} corner {k}");
            let n = m0.vertex_normal[ix];
            let sn = lod0.normals[poly[k] as usize];
            assert!((n.x - sn.x as f64).abs() < 1e-6 && (n.y - sn.y as f64).abs() < 1e-6 && (n.z - sn.z as f64).abs() < 1e-6);
        }
    }

    // LOD1 is a different, smaller mesh.
    let m1 = find_node(&s, "ball_LOD1").mesh.as_ref().unwrap();
    assert_eq!(m1.num_vertices, lod1.vertex_count());
    assert_eq!(m1.num_faces, lod1.polygons.len());

    // Material + textures.
    assert_eq!(m0.materials.len(), 1);
    let mat = &m0.materials[0];
    assert_eq!(mat.element.name, "ball_material");
    assert_eq!(mat.shading_model_name, "phong");
    let dc = mat.fbx.diffuse_color.value_vec4;
    assert!((dc.x - 0.25).abs() < 1e-6 && (dc.y - 0.5).abs() < 1e-6 && (dc.z - 0.75).abs() < 1e-6, "diffuse {dc:?}");
    let albedo = mat.fbx.diffuse_color.texture.as_ref().expect("diffuse texture connected");
    assert_eq!(albedo.relative_filename, "test_albedo.png");
    assert_eq!(albedo.filename, "test_albedo.png");
    assert!(albedo.video.is_some());
    let normal = mat.fbx.normal_map.texture.as_ref().expect("normal map connected");
    assert_eq!(normal.relative_filename, "test_normal.png", "relative file name only");
    assert_eq!(mat.textures.len(), 2);

    // Collision box: plain mesh at the root with no material.
    let c = find_node(&s, "UCX_ball_01");
    assert!(c.parent.as_ref().map(|p| p.is_root).unwrap_or(false), "collision at the root");
    let cm = c.mesh.as_ref().unwrap();
    assert_eq!(cm.num_vertices, 8);
    assert_eq!(cm.num_faces, 12);
    assert!(cm.materials.is_empty());
    for (i, p) in coll.positions.iter().enumerate() {
        let v = cm.vertices[i];
        assert!((v.x - (p.x * scale) as f64).abs() < 1e-3 && (v.y - (p.y * scale) as f64).abs() < 1e-3);
    }
}

#[test]
fn single_lod_vertex_colors_and_triangles() {
    let mut m = make_box(0.5);
    m.compute_smooth_normals();
    m.colors = (0..8).map(|i| [i as f32 / 8.0, 0.5, 1.0 - i as f32 / 8.0, 1.0]).collect();
    let scene = FbxScene {
        name: "cube".into(),
        lods: vec![FbxLod { name: "cube".into(), mesh: &m }],
        material: FbxMaterial::default(),
        collision: vec![],
        scale: 1.0,
        skeleton: None,
        animations: &[],
    };
    let bytes = fbx_out::encode(&scene).unwrap();
    let s = ufbx::load_memory(&bytes, ufbx::LoadOpts::default()).unwrap_or_else(|e| panic!("{:?}", e.description));
    assert_eq!(s.nodes.len(), 2, "root + cube");
    assert!(s.lod_groups.is_empty());
    let mesh = find_node(&s, "cube").mesh.as_ref().unwrap();
    assert_eq!(mesh.num_faces, 12);
    assert_eq!(mesh.num_triangles, 12);
    assert!(mesh.faces.iter().all(|f| f.num_indices == 3));
    assert!(mesh.vertex_color.exists);
    for t in 0..12 {
        for k in 0..3 {
            let ix = t * 3 + k;
            let c = mesh.vertex_color[ix];
            let src = m.colors[m.indices[ix] as usize];
            assert!((c.x - src[0] as f64).abs() < 1e-6 && (c.z - src[2] as f64).abs() < 1e-6);
        }
    }
    assert_eq!(mesh.materials.len(), 1);
    assert_eq!(mesh.materials[0].element.name, "Material");
    assert!(mesh.materials[0].fbx.diffuse_color.texture.is_none());
}

#[test]
fn skin_and_animation_round_trip() {
    // Two-bone chain along +Y, a box skinned half to each bone.
    let mut m = make_box(0.5);
    m.compute_smooth_normals();
    let root_local = Mat4::from_translation(Vec3::new(0.0, -0.5, 0.0));
    let child_local = Mat4::from_rotation_translation(Quat::from_rotation_z(30f32.to_radians()), Vec3::new(0.0, 1.0, 0.0));
    let root_world = root_local;
    let child_world = root_world * child_local;
    let skel = Skeleton {
        joints: vec![
            Joint { name: "root".into(), parent: None, local: root_local.to_cols_array_2d(), inverse_bind: root_world.inverse().to_cols_array_2d() },
            Joint { name: "tip".into(), parent: Some(0), local: child_local.to_cols_array_2d(), inverse_bind: child_world.inverse().to_cols_array_2d() },
        ],
    };
    m.joints = (0..8).map(|i| if m.positions[i].y > 0.0 { [1, 0, 0, 0] } else { [0, 0, 0, 0] }).collect();
    m.weights = (0..8).map(|i| if m.positions[i].y > 0.0 { [0.75, 0.25, 0.0, 0.0] } else { [1.0, 0.0, 0.0, 0.0] }).collect();
    assert!(m.has_skin());

    let q0 = Quat::IDENTITY;
    let q1 = Quat::from_rotation_x(90f32.to_radians());
    let anims = vec![Animation {
        name: "wave".into(),
        channels: vec![
            AnimationChannel { joint: 1, path: "rotation".into(), interpolation: "LINEAR".into(), times: vec![0.0, 1.0], values: vec![q0.x, q0.y, q0.z, q0.w, q1.x, q1.y, q1.z, q1.w] },
            AnimationChannel { joint: 0, path: "translation".into(), interpolation: "LINEAR".into(), times: vec![0.0, 0.5, 2.0], values: vec![0.0, -0.5, 0.0, 0.0, 0.0, 0.0, 0.0, 0.5, 0.0] },
        ],
    }];
    let scale = 10.0f32;
    let scene = FbxScene {
        name: "rig".into(),
        lods: vec![FbxLod { name: "rig".into(), mesh: &m }],
        material: FbxMaterial::default(),
        collision: vec![],
        scale,
        skeleton: Some(&skel),
        animations: &anims,
    };
    let path = tmp_path("rig.fbx");
    fbx_out::write(&path, &scene).unwrap();
    let s = load(&path);

    // Hierarchy: root, rig mesh, root bone, tip bone.
    assert_eq!(s.nodes.len(), 4);
    assert_eq!(s.bones.len(), 2);
    let root_b = find_node(&s, "root");
    let tip_b = find_node(&s, "tip");
    assert_eq!(root_b.attrib_type, ufbx::ElementType::Bone);
    assert_eq!(tip_b.parent.as_ref().unwrap().element.name, "root");
    let t = root_b.local_transform.translation;
    assert!((t.y + 0.5 * scale as f64).abs() < 1e-4, "root translation scaled: {t:?}");
    let r = tip_b.local_transform.rotation;
    let expect = Quat::from_rotation_z(30f32.to_radians());
    let dot = (r.x * expect.x as f64 + r.y * expect.y as f64 + r.z * expect.z as f64 + r.w * expect.w as f64).abs();
    assert!(dot > 0.9999, "tip rotation Euler->quat mismatch: {r:?} vs {expect:?}");

    // Skin.
    assert_eq!(s.skin_deformers.len(), 1);
    assert_eq!(s.skin_clusters.len(), 2);
    let mesh = find_node(&s, "rig").mesh.as_ref().unwrap();
    assert_eq!(mesh.skin_deformers.len(), 1);
    let skin = &mesh.skin_deformers[0];
    assert_eq!(skin.vertices.len(), 8);
    for (vi, sv) in skin.vertices.iter().enumerate() {
        let mut total = 0.0;
        for k in 0..sv.num_weights as usize {
            let w = skin.weights[sv.weight_begin as usize + k];
            let cluster = &skin.clusters[w.cluster_index as usize];
            let bone = cluster.bone_node.as_ref().unwrap().element.name.to_string();
            let joint = if bone == "root" { 0 } else { 1 };
            let src = (0..4).find(|&c| m.joints[vi][c] as usize == joint && m.weights[vi][c] > 0.0).map(|c| m.weights[vi][c]).unwrap_or(0.0);
            assert!((w.weight - src as f64).abs() < 1e-6, "vertex {vi} bone {bone}: {} vs {src}", w.weight);
            total += w.weight;
        }
        assert!((total - 1.0).abs() < 1e-6);
    }
    // Cluster bind matrices: TransformLink is the (scaled) bone world matrix.
    let tip_cluster = s.skin_clusters.iter().find(|c| c.bone_node.as_ref().unwrap().element.name == "tip").unwrap();
    let bw = tip_cluster.bind_to_world;
    assert!((bw.m03 - (child_world.w_axis.x * scale) as f64).abs() < 1e-4);
    assert!((bw.m13 - (child_world.w_axis.y * scale) as f64).abs() < 1e-4);
    assert!(s.poses.len() == 1 && s.poses[0].is_bind_pose);

    // Animation.
    assert_eq!(s.anim_stacks.len(), 1);
    let stack = &s.anim_stacks[0];
    assert_eq!(stack.element.name, "wave");
    assert!((stack.time_end - 2.0).abs() < 1e-6, "stack end {}", stack.time_end);
    assert_eq!(stack.layers.len(), 1);
    assert_eq!(s.anim_values.len(), 2);
    assert_eq!(s.anim_curves.len(), 6);
    // Evaluate the tip rotation half-way: a 45 degree X rotation composed onto the rest pose.
    let tr = ufbx::evaluate_transform(&stack.anim, tip_b, 0.5);
    let got = Quat::from_xyzw(tr.rotation.x as f32, tr.rotation.y as f32, tr.rotation.z as f32, tr.rotation.w as f32);
    let want = Quat::from_rotation_x(45f32.to_radians());
    assert!(got.dot(want).abs() > 0.999, "tip at t=0.5: {got:?} vs {want:?}");
    let tr_end = ufbx::evaluate_transform(&stack.anim, tip_b, 1.0);
    let got_end = Quat::from_xyzw(tr_end.rotation.x as f32, tr_end.rotation.y as f32, tr_end.rotation.z as f32, tr_end.rotation.w as f32);
    assert!(got_end.dot(q1).abs() > 0.999, "tip at t=1: {got_end:?}");
    // Root translation keys (scaled).
    let rt = ufbx::evaluate_transform(&stack.anim, root_b, 2.0);
    assert!((rt.translation.y - 0.5 * scale as f64).abs() < 1e-3, "root translation at t=2: {:?}", rt.translation);
    let rt_mid = ufbx::evaluate_transform(&stack.anim, root_b, 0.25);
    assert!((rt_mid.translation.y + 0.25 * scale as f64).abs() < 1e-3, "linear interpolation: {:?}", rt_mid.translation);
}

#[test]
fn hundred_thousand_triangles_load() {
    // 224 segments -> 224 * 448 = 100,352 quads / triangle polygons (~200k triangles).
    let mut m = make_sphere(224);
    m.polygons.clear(); // plain triangle path
    assert!(m.triangle_count() >= 100_000);
    let scene = FbxScene {
        name: "big".into(),
        lods: vec![FbxLod { name: "big".into(), mesh: &m }],
        material: FbxMaterial::default(),
        collision: vec![],
        scale: 1.0,
        skeleton: None,
        animations: &[],
    };
    let start = std::time::Instant::now();
    let bytes = fbx_out::encode(&scene).unwrap();
    let encode_time = start.elapsed();
    let s = ufbx::load_memory(&bytes, ufbx::LoadOpts::default()).unwrap_or_else(|e| panic!("{:?}", e.description));
    let mesh = find_node(&s, "big").mesh.as_ref().unwrap();
    assert_eq!(mesh.num_triangles, m.triangle_count());
    assert_eq!(mesh.num_vertices, m.vertex_count());
    assert!(mesh.vertex_uv.exists && mesh.vertex_normal.exists);
    assert!(encode_time.as_secs() < 20, "encoding took {encode_time:?}");
}
