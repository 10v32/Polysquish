use glam::Vec3;
use polysquish::imposter::{generate, octahedral_dir, octahedral_uv, ImposterOptions, ImposterOutput};
use polysquish::mesh::{Mesh, NO_VERTEX};

fn make_sphere(seg: u32) -> Mesh {
    // UV sphere, outward winding, with vertex colours (same helper as tests/pipeline.rs).
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

fn frame_dir(out: &ImposterOutput, f: usize) -> Vec3 {
    let v = &out.frames_json["frames_list"][f]["view_dir"];
    Vec3::new(v[0].as_f64().unwrap() as f32, v[1].as_f64().unwrap() as f32, v[2].as_f64().unwrap() as f32)
}

fn uv_rect(out: &ImposterOutput, f: usize) -> [f32; 4] {
    let r = &out.frames_json["frames_list"][f]["uv_rect"];
    let g = |k: usize| r[k].as_f64().unwrap() as f32;
    [g(0), g(1), g(2), g(3)]
}

fn centre_px(out: &ImposterOutput, f: usize) -> (u32, u32) {
    let r = uv_rect(out, f);
    let res = out.albedo.width() as f32;
    (((r[0] + r[2]) * 0.5 * res) as u32, ((r[1] + r[3]) * 0.5 * res) as u32)
}

#[test]
fn octahedral_mapping_round_trips() {
    for &hemi in &[true, false] {
        for i in 0..=10 {
            for j in 0..=10 {
                let (u, v) = (i as f32 / 10.0, j as f32 / 10.0);
                let d = octahedral_dir(u, v, hemi);
                assert!((d.length() - 1.0).abs() < 1e-4);
                if hemi {
                    assert!(d.y >= -1e-6, "hemisphere dir below horizon: {d}");
                }
                let back = octahedral_dir(octahedral_uv(d, hemi).x, octahedral_uv(d, hemi).y, hemi);
                assert!(back.dot(d) > 0.9999, "u={u} v={v} hemi={hemi}: {d} -> {back}");
            }
        }
    }
    // Documented landmarks.
    assert!(octahedral_dir(0.5, 0.5, true).dot(Vec3::Y) > 0.9999);
    assert!(octahedral_dir(1.0, 0.0, true).dot(Vec3::Z) > 0.9999);
    assert!(octahedral_dir(1.0, 1.0, true).dot(Vec3::X) > 0.9999);
    assert!(octahedral_dir(0.0, 1.0, true).dot(-Vec3::Z) > 0.9999);
    assert!(octahedral_dir(0.0, 0.0, true).dot(-Vec3::X) > 0.9999);
    assert!(octahedral_dir(0.5, 0.5, false).dot(Vec3::Y) > 0.9999);
    assert!(octahedral_dir(1.0, 0.5, false).dot(Vec3::X) > 0.9999);
    assert!(octahedral_dir(0.5, 1.0, false).dot(Vec3::Z) > 0.9999);
    assert!(octahedral_dir(0.0, 0.0, false).dot(-Vec3::Y) > 0.9999);
    assert!(octahedral_dir(1.0, 1.0, false).dot(-Vec3::Y) > 0.9999);
}

#[test]
fn sphere_imposter_atlas_and_cards() {
    let mut mesh = make_sphere(32);
    mesh.compute_smooth_normals();
    let opts = ImposterOptions { resolution: 512, frames: 4, hemisphere: true };
    let out = generate(&mesh, None, [1.0; 4], &opts).expect("generate");
    assert_eq!(out.albedo.dimensions(), (512, 512));
    assert_eq!(out.normal.dimensions(), (512, 512));
    assert_eq!(out.depth.dimensions(), (512, 512));
    assert_eq!(out.frames_json["frames_list"].as_array().unwrap().len(), 16);
    assert_eq!(out.frames_json["frame_count"], 16);

    // Every frame cell has coverage at its centre, and uv rects are sane.
    for f in 0..16 {
        let r = uv_rect(&out, f);
        assert!(r.iter().all(|v| (0.0..=1.0).contains(v)) && r[0] < r[2] && r[1] < r[3], "{r:?}");
        let (x, y) = centre_px(&out, f);
        assert!(out.albedo.get_pixel(x, y)[3] > 0, "frame {f} albedo centre is empty");
        assert!(out.normal.get_pixel(x, y)[3] > 0, "frame {f} normal centre is empty");
        assert!(out.depth.get_pixel(x, y)[3] > 0, "frame {f} depth centre is empty");
        // Depth at the centre is the near side of the bounding sphere.
        assert!(out.depth.get_pixel(x, y)[0] < 20, "frame {f} depth centre {}", out.depth.get_pixel(x, y)[0]);
        // Pivot/radius in frames_json describe the unit sphere.
        let d = frame_dir(&out, f);
        assert!((d.length() - 1.0).abs() < 1e-3 && d.y >= -1e-6);
    }
    assert!((out.frames_json["radius"].as_f64().unwrap() - 1.0).abs() < 1e-3);
    let pivot = &out.frames_json["pivot"];
    assert!(pivot[0].as_f64().unwrap().abs() < 1e-3 && pivot[1].as_f64().unwrap().abs() < 1e-3);

    // Front frame: the view direction closest to +Z (exactly +Z for a hemi grid corner).
    let front = (0..16).max_by(|&a, &b| frame_dir(&out, a).z.partial_cmp(&frame_dir(&out, b).z).unwrap()).unwrap();
    let fd = frame_dir(&out, front);
    assert!(fd.z > 0.999, "front dir {fd}");
    let (cx, cy) = centre_px(&out, front);
    // The sphere point (0,0,1) is at theta = pi/2 (u = 0.25), phi = pi/2 (v = 0.5): colour (0.25, 0.5, 0.5).
    let px = out.albedo.get_pixel(cx, cy).0;
    assert!((px[0] as i32 - 64).abs() <= 12 && (px[1] as i32 - 128).abs() <= 12 && (px[2] as i32 - 128).abs() <= 12, "front albedo {px:?}");
    assert_eq!(px[3], 255);
    // The normal at the centre of the front frame points towards the camera.
    let n = out.normal.get_pixel(cx, cy).0;
    let nd = Vec3::new(n[0] as f32 / 255.0 * 2.0 - 1.0, n[1] as f32 / 255.0 * 2.0 - 1.0, n[2] as f32 / 255.0 * 2.0 - 1.0);
    assert!(nd.z > 0.9 && nd.dot(fd) > 0.9, "front normal {nd}");

    // Dilation: just outside the silhouette alpha is 0 but colour is not black.
    let r = uv_rect(&out, front);
    let edge_x = (r[2] * 512.0) as u32; // right edge of the inner cell, on the sphere equator row
    let p = out.albedo.get_pixel(edge_x, cy).0;
    assert_eq!(p[3], 0, "gutter pixel should be transparent: {p:?}");
    assert!(p[0] as u32 + p[1] as u32 + p[2] as u32 > 0, "gutter pixel should carry dilated colour: {p:?}");

    // Cards: three quads / six triangles, UVs inside 0..1, normals present.
    let cards = &out.cards;
    assert_eq!(cards.polygons.len(), 3);
    assert_eq!(cards.quad_count(), 3);
    assert!(cards.polygons.iter().all(|p| p[3] != NO_VERTEX));
    assert_eq!(cards.triangle_count(), 6);
    assert_eq!(cards.vertex_count(), 12);
    assert_eq!(cards.normals.len(), 12);
    assert_eq!(cards.uvs.len(), 12);
    assert!(cards.uvs.iter().all(|uv| (0.0..=1.0).contains(&uv.x) && (0.0..=1.0).contains(&uv.y)));
    assert!(cards.indices.iter().all(|&i| (i as usize) < 12));
    for (t, facing) in [Vec3::Z, Vec3::Z, Vec3::X, Vec3::X, Vec3::Y, Vec3::Y].iter().enumerate() {
        assert!(cards.face_normal(t).dot(*facing) > 0.999, "card tri {t} winding");
    }
    assert!(cards.positions.iter().all(|p| p.abs().max_element() <= 1.0 + 1e-4));
    assert_eq!(out.frames_json["cards"].as_array().unwrap().len(), 3);

    // The atlases are valid PNG files.
    let dir = std::env::temp_dir().join(format!("polysquish-imposter-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (name, img) in [("albedo", &out.albedo), ("normal", &out.normal), ("depth", &out.depth)] {
        let path = dir.join(format!("{name}.png"));
        img.save(&path).expect("save png");
        let back = image::open(&path).expect("reload png").to_rgba8();
        assert_eq!(back.dimensions(), (512, 512));
        assert_eq!(back.get_pixel(cx, cy), img.get_pixel(cx, cy));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn full_sphere_and_textured_paths() {
    let mut mesh = make_sphere(16);
    mesh.compute_smooth_normals();
    let out = generate(&mesh, None, [1.0; 4], &ImposterOptions { resolution: 256, frames: 3, hemisphere: false }).unwrap();
    for f in 0..9 {
        let (x, y) = centre_px(&out, f);
        assert!(out.albedo.get_pixel(x, y)[3] > 0, "frame {f}");
    }
    // Textured: give the sphere UVs and a flat green texture; base colour halves it.
    mesh.uvs = mesh.positions.iter().map(|p| glam::Vec2::new(p.x * 0.5 + 0.5, p.z * 0.5 + 0.5)).collect();
    let tex = polysquish::mesh::Texture { name: "t".into(), image: image::RgbaImage::from_pixel(8, 8, image::Rgba([0, 255, 0, 255])) };
    let out = generate(&mesh, Some(&tex), [1.0, 0.5, 1.0, 1.0], &ImposterOptions { resolution: 256, frames: 2, hemisphere: true }).unwrap();
    let (x, y) = centre_px(&out, 0);
    let p = out.albedo.get_pixel(x, y).0;
    assert!(p[0] == 0 && (p[1] as i32 - 128).abs() <= 1 && p[2] == 0 && p[3] == 255, "{p:?}");
    // Bad options are rejected.
    assert!(generate(&mesh, None, [1.0; 4], &ImposterOptions { resolution: 16, frames: 8, hemisphere: true }).is_err());
    assert!(generate(&Mesh::default(), None, [1.0; 4], &ImposterOptions::default()).is_err());
}

#[test]
fn large_atlas_is_fast_enough() {
    let mut mesh = make_sphere(71); // 71 * 142 * 2 = 20_164 triangles
    mesh.compute_smooth_normals();
    assert!(mesh.triangle_count() >= 20_000);
    let start = std::time::Instant::now();
    let out = generate(&mesh, None, [1.0; 4], &ImposterOptions { resolution: 1024, frames: 4, hemisphere: true }).unwrap();
    let elapsed = start.elapsed();
    assert_eq!(out.albedo.dimensions(), (1024, 1024));
    assert!(elapsed.as_secs_f32() < 5.0, "imposter generation took {elapsed:?}");
}
