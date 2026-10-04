//! The squish pipeline: import → analyse → clean → decimate → unwrap → bake → LODs → collision → export.

use crate::analyze::{self, HealthReport};
use crate::io::gltf_out::{GlbMaterial, GlbScene};
use crate::mesh::{Mesh, Scene};
use crate::progress::{Progress, Stage};
use crate::recipe::{NormalConvention, Recipe, Target};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Instant;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileEntry {
    pub name: String,
    pub size_bytes: u64,
    pub kind: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct BeforeStats {
    pub triangles: usize,
    pub vertices: usize,
    pub size_bytes: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LodStat {
    pub level: usize,
    pub triangles: usize,
    pub vertices: usize,
    pub screen_coverage: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct AfterStats {
    pub triangles: usize,
    pub vertices: usize,
    pub texture_size: u32,
    pub size_bytes: u64,
    pub lods: Vec<LodStat>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SquishResult {
    pub name: String,
    pub source_format: String,
    pub output_dir: PathBuf,
    pub files: Vec<FileEntry>,
    pub before: BeforeStats,
    pub after: AfterStats,
    pub problems_fixed: Vec<String>,
    pub report: HealthReport,
    pub source_report: HealthReport,
    pub timings: serde_json::Map<String, serde_json::Value>,
    pub finished_at: String,
    pub normal_convention: String,
    pub main_glb: Option<String>,
    pub main_obj: Option<String>,
}

fn fmt_int(n: usize) -> String {
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

fn now_string() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Simple civil date (UTC) without pulling in chrono.
    let days = secs / 86400;
    let (h, m) = ((secs % 86400) / 3600, (secs % 3600) / 60);
    let (y, mo, d) = civil_from_days(days as i64);
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{m:02} UTC")
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Health check only.
pub fn inspect(scene: &Scene) -> HealthReport {
    analyze::analyze(scene)
}

/// A viewer-friendly version of a mesh: at most `max_tris` triangles, smooth normals.
pub fn preview_mesh(mesh: &Mesh, max_tris: usize) -> Mesh {
    let mut m = mesh.clone();
    if m.triangle_count() > max_tris {
        let opts = crate::recipe::DecimateOptions {
            target_triangles: Some(max_tris),
            aggressive: true,
            ..Default::default()
        };
        if let Ok((d, _)) = crate::decimate::simplify_to(&m, max_tris, &opts, 1.0) {
            m = d;
        }
    }
    if !m.has_normals() {
        m.compute_smooth_normals();
    }
    m
}

/// GLB bytes for the source preview (vertex colours and a downscaled base colour texture kept).
pub fn preview_glb(scene: &Scene, max_tris: usize) -> Result<Vec<u8>> {
    let mesh = preview_mesh(&scene.mesh, max_tris);
    let dominant = crate::decimate::dominant_material(&scene.mesh);
    let mat = scene.material(dominant);
    let mut gm = GlbMaterial {
        name: mat.name.clone(),
        base_color: mat.base_color,
        metallic: mat.metallic,
        roughness: mat.roughness,
        double_sided: true,
        ..Default::default()
    };
    if mesh.has_uvs() {
        if let Some(t) = mat.base_color_tex.and_then(|i| scene.textures.get(i)) {
            let img = if t.image.width() > 1024 || t.image.height() > 1024 {
                image::imageops::resize(&t.image, 1024, 1024, image::imageops::FilterType::Triangle)
            } else {
                t.image.clone()
            };
            gm.albedo = Some(img);
        }
    }
    crate::io::gltf_out::encode_simple(&scene.name, &mesh, gm)
}

struct Timer {
    start: Instant,
}
impl Timer {
    fn new() -> Self {
        Self { start: Instant::now() }
    }
    fn lap(&mut self) -> f64 {
        let e = self.start.elapsed().as_secs_f64();
        self.start = Instant::now();
        (e * 100.0).round() / 100.0
    }
}

fn file_entry(dir: &Path, name: &str, kind: &str) -> FileEntry {
    let size = std::fs::metadata(dir.join(name)).map(|m| m.len()).unwrap_or(0);
    FileEntry { name: name.to_string(), size_bytes: size, kind: kind.to_string() }
}

fn sanitize(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    let s = s.trim_matches('_').to_string();
    if s.is_empty() {
        "model".into()
    } else {
        s
    }
}

/// Run the whole pipeline on an already-loaded scene and write the outputs to `out_dir`.
pub fn squish(scene: &Scene, recipe: &Recipe, out_dir: &Path, name: &str, progress: &Progress) -> Result<SquishResult> {
    let name = sanitize(name);
    std::fs::create_dir_all(out_dir).with_context(|| format!("cannot create {}", out_dir.display()))?;
    let mut timings = serde_json::Map::new();
    let mut timer = Timer::new();
    let mut problems_fixed: Vec<String> = Vec::new();

    // ---- analyse ----
    progress.stage(Stage::Analyze, 0.0);
    let source_report = analyze::analyze(scene);
    progress.log(format!(
        "Source: {} triangles, {} vertices, {} component{}",
        fmt_int(source_report.triangles),
        fmt_int(source_report.vertices),
        source_report.components,
        if source_report.components == 1 { "" } else { "s" }
    ));
    progress.stage(Stage::Analyze, 1.0);
    timings.insert("analyze".into(), timer.lap().into());
    progress.check()?;

    // ---- clean ----
    progress.stage(Stage::Clean, 0.0);
    let mut high = scene.mesh.clone();
    let clean_rep = crate::clean::clean(&mut high, &recipe.cleanup);
    for m in &clean_rep.messages {
        progress.log(m.clone());
    }
    problems_fixed.extend(clean_rep.messages.iter().cloned());
    let high_scene = Scene {
        name: scene.name.clone(),
        mesh: high,
        materials: scene.materials.clone(),
        textures: scene.textures.clone(),
        source_format: scene.source_format.clone(),
        source_bytes: scene.source_bytes,
        skeleton: scene.skeleton.clone(),
        animations: scene.animations.clone(),
    };
    let diag = high_scene.mesh.bounds().diagonal.max(1e-9);
    progress.stage(Stage::Clean, 1.0);
    timings.insert("clean".into(), timer.lap().into());
    progress.check()?;

    // ---- decimate ----
    progress.stage(Stage::Decimate, 0.0);
    let (mut lod0, dec_rep) = crate::decimate::decimate(&high_scene.mesh, &recipe.decimate)?;
    progress.log(format!(
        "Squished {} → {} triangles (max deviation {:.3}% of size)",
        fmt_int(dec_rep.before_triangles),
        fmt_int(dec_rep.after_triangles),
        100.0 * dec_rep.error / diag
    ));
    if dec_rep.before_triangles > dec_rep.after_triangles {
        problems_fixed.push(format!(
            "Reduced {} triangles to {}",
            fmt_int(dec_rep.before_triangles),
            fmt_int(dec_rep.after_triangles)
        ));
    }
    progress.stage(Stage::Decimate, 1.0);
    timings.insert("decimate".into(), timer.lap().into());
    progress.check()?;

    // ---- uv ----
    progress.stage(Stage::Uv, 0.0);
    let mut uv_ok = false;
    if recipe.uv.enabled {
        match crate::uv::unwrap(&mut lod0, &recipe.uv) {
            Ok(rep) => {
                uv_ok = true;
                if rep.kept_existing {
                    progress.log("Existing UVs look good; kept them".to_string());
                } else {
                    progress.log(format!(
                        "Unwrapped into {} charts ({}×{} atlas, {:.0}% used)",
                        rep.charts,
                        rep.atlas_width,
                        rep.atlas_height,
                        rep.utilization * 100.0
                    ));
                    problems_fixed.push(format!("Generated a UV layout with {} charts", rep.charts));
                }
            }
            Err(e) => {
                progress.log(format!("UV unwrap failed ({e}); continuing without textures"));
            }
        }
    } else if lod0.has_uvs() {
        uv_ok = true;
    }
    progress.stage(Stage::Uv, 1.0);
    timings.insert("uv".into(), timer.lap().into());
    progress.check()?;

    // ---- bake ----
    progress.stage(Stage::Bake, 0.0);
    let mut baked: Option<crate::bake::BakeOutput> = None;
    if recipe.bake.enabled && uv_ok && (recipe.bake.albedo || recipe.bake.normal_map || recipe.bake.ao || recipe.bake.metallic_roughness) {
        let auto = (dec_rep.error * 3.0).clamp(diag * 0.004, diag * 0.08);
        let ray_distance = recipe.bake.ray_distance.map(|f| f * diag).unwrap_or(auto);
        match crate::bake::bake(&high_scene, &lod0, &recipe.bake, ray_distance, progress) {
            Ok(b) => {
                progress.log(format!("Baked {}×{} textures ({:.0}% of the atlas covered)", b.width, b.height, b.coverage * 100.0));
                baked = Some(b);
            }
            Err(e) => {
                if progress.cancel.is_cancelled() {
                    return Err(e);
                }
                progress.log(format!("Baking failed ({e}); exporting without textures"));
            }
        }
    } else if recipe.bake.enabled && !uv_ok {
        progress.log("No UVs available, skipping bake".to_string());
    }
    progress.stage(Stage::Bake, 1.0);
    timings.insert("bake".into(), timer.lap().into());
    progress.check()?;

    // ---- lods ----
    progress.stage(Stage::Lods, 0.0);
    crate::decimate::optimize_for_gpu(&mut lod0);
    let ratios: Vec<f32> = recipe.lods.ratios.iter().copied().take(recipe.lods.count).collect();
    let lods = if ratios.is_empty() { Vec::new() } else { crate::decimate::lod_chain(&lod0, &ratios, &recipe.decimate) };
    if !lods.is_empty() {
        progress.log(format!(
            "Built {} LODs: {}",
            lods.len(),
            lods.iter().map(|m| fmt_int(m.triangle_count())).collect::<Vec<_>>().join(", ")
        ));
    }
    progress.stage(Stage::Lods, 1.0);
    timings.insert("lods".into(), timer.lap().into());
    progress.check()?;

    // ---- collision ----
    progress.stage(Stage::Collision, 0.0);
    let collision = crate::collision::generate(&lod0, &recipe.collision);
    if !collision.is_empty() {
        progress.log(format!(
            "Collision: {}",
            collision.shapes().iter().map(|(k, m)| format!("{k} ({} tris)", m.triangle_count())).collect::<Vec<_>>().join(", ")
        ));
    }
    progress.stage(Stage::Collision, 1.0);
    timings.insert("collision".into(), timer.lap().into());
    progress.check()?;

    // ---- export ----
    progress.stage(Stage::Export, 0.0);
    let mut files: Vec<FileEntry> = Vec::new();
    let dominant = crate::decimate::dominant_material(&high_scene.mesh);
    let src_mat = high_scene.material(dominant);
    let open_fraction = source_report.boundary_edges as f32 / source_report.triangles.max(1) as f32;
    let double_sided = open_fraction > 0.05;

    // Textures to disk.
    let mut tex_albedo = None;
    let mut tex_normal = None;
    let mut tex_ao = None;
    let mut tex_orm = None;
    if let Some(b) = &baked {
        let mut save = |img: &Option<image::RgbaImage>, suffix: &str| -> Result<Option<String>> {
            if let Some(img) = img {
                let fname = format!("{name}_{suffix}.png");
                img.save(out_dir.join(&fname))?;
                files.push(file_entry(out_dir, &fname, "texture"));
                Ok(Some(fname))
            } else {
                Ok(None)
            }
        };
        tex_albedo = save(&b.albedo, "albedo")?;
        tex_normal = save(&b.normal, "normal")?;
        tex_ao = save(&b.ao, "ao")?;
        tex_orm = save(&b.orm, "orm")?;
    }
    progress.stage(Stage::Export, 0.3);

    let glb_scale = recipe.export.scale;
    let units_m = source_report.units_guess == "meters";
    let obj_scale = recipe.export.scale * if recipe.export.target.prefers_centimeters() && units_m { 100.0 } else { 1.0 };
    let coverage: Vec<f32> = {
        // Screen coverage thresholds for LOD1..n: halve each step starting from 0.5.
        let mut c = vec![1.0f32];
        let mut v = 0.5;
        for _ in 0..lods.len() {
            c.push(v);
            v *= 0.5;
        }
        c
    };
    let main_glb_name = format!("{name}.glb");
    let mut main_glb = None;
    if recipe.export.glb {
        let material = GlbMaterial {
            name: format!("{name}_material"),
            base_color: if baked.as_ref().map(|b| b.albedo.is_some()).unwrap_or(false) { [1.0; 4] } else { src_mat.base_color },
            metallic: if baked.as_ref().map(|b| b.orm.is_some()).unwrap_or(false) { 1.0 } else { src_mat.metallic },
            roughness: if baked.as_ref().map(|b| b.orm.is_some()).unwrap_or(false) { 1.0 } else { src_mat.roughness },
            albedo: baked.as_ref().and_then(|b| b.albedo.clone()),
            normal: baked.as_ref().and_then(|b| b.normal.clone()),
            orm: baked.as_ref().and_then(|b| b.orm.clone()),
            double_sided,
        };
        let mut lod_refs: Vec<&Mesh> = vec![&lod0];
        lod_refs.extend(lods.iter());
        let mut coll: Vec<(String, &Mesh)> = Vec::new();
        if recipe.export.target == Target::Unreal {
            for (i, (kind, m)) in collision.shapes().iter().enumerate() {
                let prefix = match *kind {
                    "hull" => "UCX",
                    "box" => "UBX",
                    _ => "UCX",
                };
                coll.push((format!("{prefix}_{name}_{:02}", i + 1), *m));
            }
        }
        let glb = crate::io::gltf_out::encode(&GlbScene {
            name: name.clone(),
            lods: lod_refs,
            screen_coverage: coverage.clone(),
            material,
            collision: coll,
            scale: glb_scale,
            generator_note: format!("Squished from {} triangles. Normal map: {}.", fmt_int(source_report.triangles), match recipe.bake.normal_convention { NormalConvention::OpenGL => "OpenGL (+Y)", NormalConvention::DirectX => "DirectX (-Y)" }),
        })?;
        std::fs::write(out_dir.join(&main_glb_name), &glb)?;
        files.push(file_entry(out_dir, &main_glb_name, "glb"));
        main_glb = Some(main_glb_name.clone());
        // Collision as its own GLB for non-Unreal targets.
        if recipe.export.target != Target::Unreal && !collision.is_empty() {
            let coll: Vec<(String, &Mesh)> = collision.shapes().iter().map(|(k, m)| (format!("{name}_collision_{k}"), *m)).collect();
            let first = coll[0].1;
            let glb = crate::io::gltf_out::encode(&GlbScene {
                name: format!("{name}_collision"),
                lods: vec![first],
                screen_coverage: vec![],
                material: GlbMaterial { name: "collision".into(), base_color: [0.4, 1.0, 0.7, 0.5], ..Default::default() },
                collision: coll[1..].to_vec(),
                scale: glb_scale,
                generator_note: "Collision shapes".into(),
            })?;
            let fname = format!("{name}_collision.glb");
            std::fs::write(out_dir.join(&fname), &glb)?;
            files.push(file_entry(out_dir, &fname, "collision"));
        }
    }
    progress.stage(Stage::Export, 0.6);

    let mut main_obj = None;
    if recipe.export.obj {
        let mtl_name = format!("{name}.mtl");
        let mat_name = format!("{name}_material");
        crate::io::obj_out::write_mtl(
            &out_dir.join(&mtl_name),
            &mat_name,
            &crate::io::obj_out::ObjMaterialFiles {
                albedo: tex_albedo.clone(),
                normal: tex_normal.clone(),
                ao: tex_ao.clone(),
                orm: tex_orm.clone(),
                base_color: if tex_albedo.is_some() { [1.0; 4] } else { src_mat.base_color },
                roughness: src_mat.roughness,
                metallic: src_mat.metallic,
            },
        )?;
        files.push(file_entry(out_dir, &mtl_name, "mtl"));
        let obj_name = format!("{name}.obj");
        crate::io::obj_out::write_obj(&out_dir.join(&obj_name), &lod0, &name, Some(&mtl_name), &mat_name, obj_scale)?;
        files.push(file_entry(out_dir, &obj_name, "obj"));
        main_obj = Some(obj_name);
        for (i, l) in lods.iter().enumerate() {
            let fname = format!("{name}_LOD{}.obj", i + 1);
            crate::io::obj_out::write_obj(&out_dir.join(&fname), l, &format!("{name}_LOD{}", i + 1), Some(&mtl_name), &mat_name, obj_scale)?;
            files.push(file_entry(out_dir, &fname, "lod"));
        }
        for (kind, m) in collision.shapes() {
            let fname = format!("{name}_collision_{kind}.obj");
            let oname = if recipe.export.target == Target::Unreal { format!("UCX_{name}_{kind}") } else { format!("{name}_collision_{kind}") };
            crate::io::obj_out::write_obj(&out_dir.join(&fname), m, &oname, None, "collision", obj_scale)?;
            files.push(file_entry(out_dir, &fname, "collision"));
        }
    }
    progress.stage(Stage::Export, 0.8);

    // Weld UV-seam splits back together so seams are not reported as open edges.
    let mut welded = lod0.clone();
    crate::clean::weld(&mut welded, 0.0);
    let final_scene = Scene {
        name: name.clone(),
        mesh: welded,
        materials: vec![Default::default()],
        textures: vec![],
        source_format: scene.source_format.clone(),
        source_bytes: 0,
        skeleton: None,
        animations: vec![],
    };
    let report = analyze::analyze(&final_scene);
    let after_size: u64 = files
        .iter()
        .filter(|f| f.kind == "glb" || (main_glb.is_none() && (f.kind == "obj" || f.kind == "texture")))
        .map(|f| f.size_bytes)
        .sum();
    let mut lod_stats = vec![LodStat { level: 0, triangles: lod0.triangle_count(), vertices: lod0.vertex_count(), screen_coverage: 1.0 }];
    for (i, l) in lods.iter().enumerate() {
        lod_stats.push(LodStat { level: i + 1, triangles: l.triangle_count(), vertices: l.vertex_count(), screen_coverage: coverage[i + 1] });
    }
    timings.insert("export".into(), timer.lap().into());

    let mut result = SquishResult {
        name: name.clone(),
        source_format: scene.source_format.clone(),
        output_dir: out_dir.to_path_buf(),
        files,
        before: BeforeStats { triangles: source_report.triangles, vertices: source_report.vertices, size_bytes: scene.source_bytes },
        after: AfterStats {
            triangles: lod0.triangle_count(),
            vertices: lod0.vertex_count(),
            texture_size: baked.as_ref().map(|b| b.width).unwrap_or(0),
            size_bytes: after_size,
            lods: lod_stats,
        },
        problems_fixed,
        report,
        source_report,
        timings,
        finished_at: now_string(),
        normal_convention: match recipe.bake.normal_convention { NormalConvention::OpenGL => "OpenGL (+Y up)".into(), NormalConvention::DirectX => "DirectX (-Y up)".into() },
        main_glb,
        main_obj,
    };
    if recipe.export.report {
        let recipe_json = serde_json::to_string_pretty(recipe)?;
        let html = crate::report::render(&result, &recipe_json)?;
        std::fs::write(out_dir.join("report.html"), html)?;
        result.files.push(file_entry(out_dir, "report.html", "report"));
        std::fs::write(out_dir.join("recipe.json"), recipe_json)?;
        result.files.push(file_entry(out_dir, "recipe.json", "json"));
    }
    std::fs::write(out_dir.join("result.json"), serde_json::to_string_pretty(&result)?)?;
    progress.stage(Stage::Export, 1.0);
    Ok(result)
}
