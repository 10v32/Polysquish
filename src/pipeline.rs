//! The squish pipeline: import → analyse → clean → decimate → unwrap → bake → LODs → collision → export.

use crate::analyze::{self, HealthReport};
use crate::bvh::{Bvh, RayTracer};
use crate::io::gltf_out::{ExtraNode, GlbMaterial, GlbScene};
use crate::mesh::{Mesh, Scene, Texture};
use crate::metrics::Metrics;
use crate::progress::{Progress, Stage};
use crate::recipe::{NormalConvention, Recipe, RetopoMode, Target};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
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
    pub main_fbx: Option<String>,
    pub metrics: Metrics,
    pub rig: Option<RigInfo>,
    /// Stages served from the cache (reported with zero seconds).
    pub cached_stages: Vec<String>,
    /// Viewer-only previews (name, GLB bytes); not written to the output folder.
    #[serde(skip)]
    pub previews: Vec<(String, Vec<u8>)>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RigInfo {
    pub joints: usize,
    pub animations: Vec<String>,
}

// ---------------------------------------------------------------------------------------------
// Stage cache
// ---------------------------------------------------------------------------------------------

/// Result of the import + analyse + clean stages.
pub struct CleanedState {
    pub high: Arc<Scene>,
    pub source_report: HealthReport,
    pub messages: Vec<String>,
    pub hidden_removed: usize,
    pub bvh: Arc<Bvh>,
    pub reconstructed_points: bool,
    /// Voxel mode: the rebuilt surface that decimation starts from (baking still uses `high`).
    pub decimate_source: Option<Arc<Mesh>>,
}

/// Result of the decimate/retopo + UV stages.
pub struct LowState {
    pub lod0: Mesh,
    pub dec_before: usize,
    pub dec_after: usize,
    pub dec_error: f32,
    pub messages: Vec<String>,
    pub uv_charts: usize,
    pub uv_ok: bool,
    pub quads: usize,
}

#[derive(Clone)]
enum CacheItem {
    Cleaned(Arc<CleanedState>),
    Low(Arc<LowState>),
}

/// Small in-memory cache of intermediate stages keyed by (input, options) so re-baking with
/// different texture settings does not repeat cleanup, decimation and unwrapping.
#[derive(Default)]
pub struct StageCache {
    entries: Mutex<Vec<(String, CacheItem)>>,
}

impl StageCache {
    const CAP: usize = 6;
    fn get(&self, key: &str) -> Option<CacheItem> {
        let mut e = self.entries.lock().unwrap();
        if let Some(pos) = e.iter().position(|(k, _)| k == key) {
            let item = e.remove(pos);
            let v = item.1.clone();
            e.push(item);
            Some(v)
        } else {
            None
        }
    }
    fn put(&self, key: String, item: CacheItem) {
        let mut e = self.entries.lock().unwrap();
        e.retain(|(k, _)| k != &key);
        e.push((key, item));
        while e.len() > Self::CAP {
            e.remove(0);
        }
    }
    pub fn clear(&self) {
        self.entries.lock().unwrap().clear();
    }
}

/// Per-run context: optional cache and a key prefix identifying the input.
#[derive(Default, Clone)]
pub struct SquishContext {
    pub cache: Option<Arc<StageCache>>,
    pub cache_key: String,
}

fn hash_json<T: Serialize>(v: &T) -> u64 {
    let s = serde_json::to_string(v).unwrap_or_default();
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
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


/// Pick the ray tracer for baking and visibility: GPU when requested and available, else the BVH.
fn make_tracer(high: &Mesh, bvh: Arc<Bvh>, want_gpu: bool, progress: &Progress) -> Arc<dyn RayTracer> {
    if want_gpu {
        match crate::gpu::GpuTracer::new(high) {
            Ok(g) => {
                progress.log(format!("Using GPU ray tracer ({})", g.adapter_name()));
                return Arc::new(g);
            }
            Err(e) => {
                log::debug!("GPU tracer unavailable: {e}");
            }
        }
    }
    bvh
}

/// Run the whole pipeline on an already-loaded scene and write the outputs to `out_dir`.
pub fn squish(scene: &Scene, recipe: &Recipe, out_dir: &Path, name: &str, progress: &Progress) -> Result<SquishResult> {
    squish_with(scene, recipe, out_dir, name, progress, &SquishContext::default())
}

/// Like [`squish`], with a stage cache.
pub fn squish_with(scene: &Scene, recipe: &Recipe, out_dir: &Path, name: &str, progress: &Progress, ctx: &SquishContext) -> Result<SquishResult> {
    let name = sanitize(name);
    std::fs::create_dir_all(out_dir).with_context(|| format!("cannot create {}", out_dir.display()))?;
    let mut timings = serde_json::Map::new();
    let mut timer = Timer::new();
    let mut problems_fixed: Vec<String> = Vec::new();
    let mut cached_stages: Vec<String> = Vec::new();
    let cache = ctx.cache.clone();
    let key_a = format!("{}|clean:{:x}|retopo:{:x}", ctx.cache_key, hash_json(&recipe.cleanup), hash_json(&(recipe.retopo.mode == RetopoMode::Voxel, recipe.retopo.voxel_resolution)));
    let key_b = format!("{key_a}|dec:{:x}|retopo:{:x}|uv:{:x}|hard:{}", hash_json(&recipe.decimate), hash_json(&recipe.retopo), hash_json(&recipe.uv), recipe.bake.hard_edge_angle as i32);

    // ---- analyse + clean (cached) ----
    let cleaned: Arc<CleanedState> = match cache.as_ref().and_then(|c| c.get(&key_a)) {
        Some(CacheItem::Cleaned(c)) if !ctx.cache_key.is_empty() => {
            progress.stage(Stage::Analyze, 1.0);
            progress.stage(Stage::Clean, 1.0);
            cached_stages.push("analyze".into());
            cached_stages.push("clean".into());
            timings.insert("analyze".into(), 0.0.into());
            timings.insert("clean".into(), 0.0.into());
            for m in &c.messages {
                progress.log(m.clone());
            }
            c
        }
        _ => {
            progress.stage(Stage::Analyze, 0.0);
            let mut messages: Vec<String> = Vec::new();
            let mut reconstructed = false;
            // Point clouds and splats are reconstructed into a surface first.
            let mut source_mesh = scene.mesh.clone();
            if crate::io::pointcloud::is_point_cloud(scene) {
                let res = recipe.retopo.voxel_resolution.clamp(32, 256);
                progress.log(format!("Input is a point cloud ({} points); reconstructing a surface at {res}³", fmt_int(scene.mesh.vertex_count())));
                source_mesh = crate::io::pointcloud::reconstruct(&scene.mesh, res)?;
                source_mesh.compute_smooth_normals();
                reconstructed = true;
                messages.push(format!("Reconstructed a surface from {} points", fmt_int(scene.mesh.vertex_count())));
            }
            let pre_scene = Scene { mesh: source_mesh, ..scene_without_mesh(scene) };
            let source_report = analyze::analyze(&pre_scene);
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

            progress.stage(Stage::Clean, 0.0);
            let mut high = pre_scene.mesh;
            let clean_rep = crate::clean::clean(&mut high, &recipe.cleanup);
            for m in &clean_rep.messages {
                progress.log(m.clone());
            }
            messages.extend(clean_rep.messages.iter().cloned());
            progress.stage(Stage::Clean, 0.4);
            // Voxel rebuild: a watertight re-extraction becomes the decimation source, while
            // textures are still baked from the original surface (which carries the UVs/colours).
            let mut decimate_source: Option<Arc<Mesh>> = None;
            if recipe.retopo.mode == RetopoMode::Voxel {
                let res = recipe.retopo.voxel_resolution.clamp(32, 512);
                progress.log(format!("Voxel rebuild at {res}³"));
                match crate::voxel::voxel_remesh(&high, &crate::voxel::VoxelOptions { resolution: res, smooth_iterations: 3, close_holes: true }) {
                    Ok(mut v) => {
                        v.compute_smooth_normals();
                        let msg = format!("Rebuilt as a watertight surface ({} faces)", fmt_int(v.polygons.len().max(v.triangle_count())));
                        progress.log(msg.clone());
                        messages.push(msg);
                        decimate_source = Some(Arc::new(v));
                    }
                    Err(e) => progress.log(format!("Voxel rebuild failed ({e}); continuing with the cleaned mesh")),
                }
            }
            progress.stage(Stage::Clean, 0.5);
            let mut bvh = Arc::new(Bvh::build(&high));
            let mut hidden_removed = 0usize;
            let single_closed = source_report.components == 1 && source_report.watertight && clean_rep.removed_floaters == 0;
            if recipe.cleanup.remove_hidden && !single_closed && recipe.retopo.mode != RetopoMode::Voxel {
                hidden_removed = crate::clean::remove_hidden(&mut high, bvh.as_ref(), recipe.cleanup.hidden_samples);
                if hidden_removed > 0 {
                    let msg = format!("Removed {} hidden interior faces", fmt_int(hidden_removed));
                    progress.log(msg.clone());
                    messages.push(msg);
                    bvh = Arc::new(Bvh::build(&high));
                }
            }
            let high_scene = Arc::new(Scene { mesh: high, ..scene_without_mesh(scene) });
            progress.stage(Stage::Clean, 1.0);
            timings.insert("clean".into(), timer.lap().into());
            let st = Arc::new(CleanedState { high: high_scene, source_report, messages, hidden_removed, bvh, reconstructed_points: reconstructed, decimate_source });
            if let Some(c) = &cache {
                if !ctx.cache_key.is_empty() {
                    c.put(key_a.clone(), CacheItem::Cleaned(st.clone()));
                }
            }
            st
        }
    };
    progress.check()?;
    let high_scene = cleaned.high.clone();
    let source_report = cleaned.source_report.clone();
    problems_fixed.extend(cleaned.messages.iter().cloned());
    let diag = high_scene.mesh.bounds().diagonal.max(1e-9);
    let bvh = cleaned.bvh.clone();

    // ---- decimate / retopo + uv (cached) ----
    let low: Arc<LowState> = match cache.as_ref().and_then(|c| c.get(&key_b)) {
        Some(CacheItem::Low(l)) if !ctx.cache_key.is_empty() => {
            progress.stage(Stage::Decimate, 1.0);
            progress.stage(Stage::Uv, 1.0);
            cached_stages.push("decimate".into());
            cached_stages.push("uv".into());
            timings.insert("decimate".into(), 0.0.into());
            timings.insert("uv".into(), 0.0.into());
            for m in &l.messages {
                progress.log(m.clone());
            }
            l
        }
        _ => {
            let mut messages: Vec<String> = Vec::new();
            progress.stage(Stage::Decimate, 0.0);
            let high: &Mesh = cleaned.decimate_source.as_deref().unwrap_or(&high_scene.mesh);
            let target = crate::decimate::resolve_target(high.triangle_count(), &recipe.decimate);
            let chunk_threshold = recipe.decimate.chunk_threshold.max(100_000);
            let decimate_to = |m: &Mesh, t: usize, progress: &Progress| -> Result<(Mesh, crate::decimate::DecimateReport)> {
                let opts = crate::recipe::DecimateOptions { target_triangles: Some(t), target_ratio: None, ..recipe.decimate.clone() };
                if m.triangle_count() > chunk_threshold {
                    let (o, r, chunks) = crate::decimate::decimate_chunked(m, &opts, chunk_threshold / 2)?;
                    progress.log(format!("Decimated in {chunks} parallel chunks"));
                    Ok((o, r))
                } else {
                    crate::decimate::decimate(m, &opts)
                }
            };
            let (mut lod0, dec_rep, quads) = match recipe.retopo.mode {
                RetopoMode::QuadDominant => {
                    let start_target = (target * 3).max(2000);
                    let (start, _) = decimate_to(high, start_target, progress)?;
                    progress.stage(Stage::Decimate, 0.4);
                    let faces = (target as f32 / 1.8).round().max(50.0) as usize;
                    match crate::retopo::quad_dominant(high, &start, &crate::retopo::RetopoOptions { target_faces: faces, ..Default::default() }) {
                        Ok((m, rep)) => {
                            let msg = format!("Retopologised into {} faces ({:.0}% quads)", fmt_int(rep.faces), rep.quad_ratio * 100.0);
                            progress.log(msg.clone());
                            messages.push(msg);
                            (m, crate::decimate::DecimateReport { before_triangles: high.triangle_count(), after_triangles: rep.triangles + rep.quads * 2, error: rep.max_deviation }, rep.quads)
                        }
                        Err(e) => {
                            progress.log(format!("Quad retopology failed ({e}); using triangles"));
                            let (m, r) = decimate_to(high, target, progress)?;
                            (m, r, 0)
                        }
                    }
                }
                _ => {
                    let (m, r) = decimate_to(high, target, progress)?;
                    (m, r, 0)
                }
            };
            let msg = format!(
                "Squished {} → {} triangles (max deviation {:.3}% of size)",
                fmt_int(dec_rep.before_triangles),
                fmt_int(lod0.triangle_count()),
                100.0 * dec_rep.error / diag
            );
            progress.log(msg);
            if dec_rep.before_triangles > lod0.triangle_count() {
                messages.push(format!("Reduced {} triangles to {}", fmt_int(dec_rep.before_triangles), fmt_int(lod0.triangle_count())));
            }
            // Hard edges become split normals (and UV seams) before unwrapping.
            if recipe.bake.hard_edge_angle < 179.0 {
                let added = crate::normals::split_hard_edges(&mut lod0, recipe.bake.hard_edge_angle);
                if added > 0 {
                    progress.log(format!("Split {} vertices along hard edges", fmt_int(added)));
                }
            } else {
                lod0.compute_smooth_normals();
            }
            progress.stage(Stage::Decimate, 1.0);
            timings.insert("decimate".into(), timer.lap().into());
            progress.check()?;

            progress.stage(Stage::Uv, 0.0);
            let mut uv_ok = false;
            let mut uv_charts = 0usize;
            if recipe.uv.enabled {
                match crate::uv::unwrap(&mut lod0, &recipe.uv) {
                    Ok(rep) => {
                        uv_ok = true;
                        uv_charts = rep.charts;
                        if rep.kept_existing {
                            progress.log("Existing UVs look good; kept them".to_string());
                        } else {
                            progress.log(format!("Unwrapped into {} charts ({}×{} atlas, {:.0}% used)", rep.charts, rep.atlas_width, rep.atlas_height, rep.utilization * 100.0));
                            messages.push(format!("Generated a UV layout with {} charts", rep.charts));
                        }
                    }
                    Err(e) => progress.log(format!("UV unwrap failed ({e}); continuing without textures")),
                }
            } else if lod0.has_uvs() {
                uv_ok = true;
            }
            progress.stage(Stage::Uv, 1.0);
            timings.insert("uv".into(), timer.lap().into());
            let st = Arc::new(LowState { quads: if lod0.has_polygons() { lod0.quad_count() } else { quads }, lod0, dec_before: dec_rep.before_triangles, dec_after: dec_rep.after_triangles, dec_error: dec_rep.error, messages, uv_charts, uv_ok });
            if let Some(c) = &cache {
                if !ctx.cache_key.is_empty() {
                    c.put(key_b.clone(), CacheItem::Low(st.clone()));
                }
            }
            st
        }
    };
    progress.check()?;
    problems_fixed.extend(low.messages.iter().cloned());
    let mut lod0 = low.lod0.clone();
    let uv_ok = low.uv_ok;

    // ---- bake ----
    progress.stage(Stage::Bake, 0.0);
    let mut baked: Option<crate::bake::BakeOutput> = None;
    let mut tracer_name = String::from("cpu");
    if recipe.bake.enabled && uv_ok && (recipe.bake.albedo || recipe.bake.normal_map || recipe.bake.ao || recipe.bake.metallic_roughness) {
        let auto = (low.dec_error * 3.0).clamp(diag * 0.004, diag * 0.08);
        let ray_distance = recipe.bake.ray_distance.map(|f| f * diag).unwrap_or(auto);
        let tracer = make_tracer(&high_scene.mesh, bvh.clone(), recipe.bake.gpu, progress);
        tracer_name = tracer.name().to_string();
        let sampler = crate::bake::HighSampler::new(&high_scene, recipe.bake.hard_edge_angle);
        match crate::bake::bake(&high_scene, &lod0, &recipe.bake, ray_distance, tracer.as_ref(), &sampler, progress) {
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

    // ---- lods (+ imposter) ----
    progress.stage(Stage::Lods, 0.0);
    crate::decimate::optimize_for_gpu(&mut lod0);
    let ratios: Vec<f32> = recipe.lods.ratios.iter().copied().take(recipe.lods.count).collect();
    let lods = if ratios.is_empty() { Vec::new() } else { crate::decimate::lod_chain(&lod0, &ratios, &recipe.decimate) };
    if !lods.is_empty() {
        progress.log(format!("Built {} LODs: {}", lods.len(), lods.iter().map(|m| fmt_int(m.triangle_count())).collect::<Vec<_>>().join(", ")));
    }
    let dominant = crate::decimate::dominant_material(&high_scene.mesh);
    let src_mat = high_scene.material(dominant);
    let mut imposter: Option<crate::imposter::ImposterOutput> = None;
    if recipe.lods.imposter {
        let albedo_tex = baked.as_ref().and_then(|b| b.albedo.clone()).map(|img| Texture { name: "albedo".into(), image: img });
        let base = if albedo_tex.is_some() { [1.0; 4] } else { src_mat.base_color };
        match crate::imposter::generate(&lod0, albedo_tex.as_ref(), base, &crate::imposter::ImposterOptions { resolution: recipe.lods.imposter_resolution.clamp(256, 4096), frames: 4, hemisphere: true }) {
            Ok(i) => {
                progress.log(format!("Generated a {}px imposter atlas (16 views)", recipe.lods.imposter_resolution));
                imposter = Some(i);
            }
            Err(e) => progress.log(format!("Imposter generation failed ({e})")),
        }
    }
    progress.stage(Stage::Lods, 1.0);
    timings.insert("lods".into(), timer.lap().into());
    progress.check()?;

    // ---- collision ----
    progress.stage(Stage::Collision, 0.0);
    let collision = crate::collision::generate(&lod0, &recipe.collision);
    if !collision.is_empty() {
        progress.log(format!("Collision: {}", collision.shapes().iter().map(|(k, m)| format!("{k} ({} tris)", m.triangle_count())).collect::<Vec<_>>().join(", ")));
    }
    progress.stage(Stage::Collision, 1.0);
    timings.insert("collision".into(), timer.lap().into());
    progress.check()?;

    // ---- metrics + heat-maps ----
    let (dev_stats, dev_per_vertex) = crate::metrics::deviation(&lod0, bvh.as_ref(), diag);
    let density = crate::metrics::texel_density(&lod0, recipe.bake.resolution);
    let mut previews: Vec<(String, Vec<u8>)> = Vec::new();
    {
        let hm = crate::metrics::heatmap_vertices(&lod0, &dev_per_vertex, (dev_stats.heatmap_max * diag).max(1e-9));
        if let Ok(glb) = crate::io::gltf_out::encode_simple(&format!("{name}_deviation"), &hm, GlbMaterial { name: "heat".into(), base_color: [1.0; 4], double_sided: true, ..Default::default() }) {
            previews.push(("heatmap_deviation.glb".into(), glb));
        }
        if let Some((ds, per_tri)) = &density {
            let hm = crate::metrics::heatmap_triangles(&lod0, per_tri, ds.min, ds.max);
            if let Ok(glb) = crate::io::gltf_out::encode_simple(&format!("{name}_density"), &hm, GlbMaterial { name: "heat".into(), base_color: [1.0; 4], double_sided: true, ..Default::default() }) {
                previews.push(("heatmap_density.glb".into(), glb));
            }
        }
    }
    progress.log(format!("Deviation from source: mean {:.3}%, max {:.3}% of size", dev_stats.mean * 100.0, dev_stats.max * 100.0));

    // ---- export ----
    progress.stage(Stage::Export, 0.0);
    let mut files: Vec<FileEntry> = Vec::new();
    let open_fraction = source_report.boundary_edges as f32 / source_report.triangles.max(1) as f32;
    let double_sided = open_fraction > 0.05;
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
    if let Some(i) = &imposter {
        for (img, suffix) in [(&i.albedo, "imposter_albedo"), (&i.normal, "imposter_normal"), (&i.depth, "imposter_depth")] {
            let fname = format!("{name}_{suffix}.png");
            img.save(out_dir.join(&fname))?;
            files.push(file_entry(out_dir, &fname, "imposter"));
        }
        let fname = format!("{name}_imposter.json");
        std::fs::write(out_dir.join(&fname), serde_json::to_string_pretty(&i.frames_json)?)?;
        files.push(file_entry(out_dir, &fname, "imposter"));
    }
    progress.stage(Stage::Export, 0.3);

    let glb_scale = recipe.export.scale;
    let units_m = source_report.units_guess == "meters";
    let dcc_scale = recipe.export.scale * if recipe.export.target.prefers_centimeters() && units_m { 100.0 } else { 1.0 };
    let coverage: Vec<f32> = {
        let mut c = vec![1.0f32];
        let mut v = 0.5;
        for _ in 0..lods.len() {
            c.push(v);
            v *= 0.5;
        }
        c
    };
    let has_albedo = baked.as_ref().map(|b| b.albedo.is_some()).unwrap_or(false);
    let has_orm = baked.as_ref().map(|b| b.orm.is_some()).unwrap_or(false);
    let main_glb_name = format!("{name}.glb");
    let mut main_glb = None;
    if recipe.export.glb {
        let material = GlbMaterial {
            name: format!("{name}_material"),
            base_color: if has_albedo { [1.0; 4] } else { src_mat.base_color },
            metallic: if has_orm { 1.0 } else { src_mat.metallic },
            roughness: if has_orm { 1.0 } else { src_mat.roughness },
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
                let prefix = if *kind == "box" { "UBX" } else { "UCX" };
                coll.push((format!("{prefix}_{name}_{:02}", i + 1), *m));
            }
        }
        let mut extras = Vec::new();
        if let Some(i) = &imposter {
            extras.push(ExtraNode {
                name: format!("{name}_IMPOSTER"),
                mesh: &i.cards,
                material: GlbMaterial { name: format!("{name}_imposter"), base_color: [1.0; 4], albedo: Some(i.albedo.clone()), double_sided: true, roughness: 0.9, ..Default::default() },
                alpha_mask: true,
                as_last_lod: true,
            });
        }
        let glb = crate::io::gltf_out::encode(&GlbScene {
            name: name.clone(),
            lods: lod_refs,
            screen_coverage: coverage.clone(),
            material,
            collision: coll,
            scale: glb_scale,
            generator_note: format!("Squished from {} triangles. Normal map: {}.", fmt_int(source_report.triangles), match recipe.bake.normal_convention { NormalConvention::OpenGL => "OpenGL (+Y)", NormalConvention::DirectX => "DirectX (-Y)" }),
            extras,
        })?;
        std::fs::write(out_dir.join(&main_glb_name), &glb)?;
        files.push(file_entry(out_dir, &main_glb_name, "glb"));
        main_glb = Some(main_glb_name.clone());
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
                extras: vec![],
            })?;
            let fname = format!("{name}_collision.glb");
            std::fs::write(out_dir.join(&fname), &glb)?;
            files.push(file_entry(out_dir, &fname, "collision"));
        }
    }
    progress.stage(Stage::Export, 0.55);

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
        crate::io::obj_out::write_obj(&out_dir.join(&obj_name), &lod0, &name, Some(&mtl_name), &mat_name, dcc_scale)?;
        files.push(file_entry(out_dir, &obj_name, "obj"));
        main_obj = Some(obj_name);
        for (i, l) in lods.iter().enumerate() {
            let fname = format!("{name}_LOD{}.obj", i + 1);
            crate::io::obj_out::write_obj(&out_dir.join(&fname), l, &format!("{name}_LOD{}", i + 1), Some(&mtl_name), &mat_name, dcc_scale)?;
            files.push(file_entry(out_dir, &fname, "lod"));
        }
        if let Some(i) = &imposter {
            let fname = format!("{name}_imposter.obj");
            crate::io::obj_out::write_obj(&out_dir.join(&fname), &i.cards, &format!("{name}_IMPOSTER"), None, "imposter", dcc_scale)?;
            files.push(file_entry(out_dir, &fname, "imposter"));
        }
        for (kind, m) in collision.shapes() {
            let fname = format!("{name}_collision_{kind}.obj");
            let oname = if recipe.export.target == Target::Unreal { format!("UCX_{name}_{kind}") } else { format!("{name}_collision_{kind}") };
            crate::io::obj_out::write_obj(&out_dir.join(&fname), m, &oname, None, "collision", dcc_scale)?;
            files.push(file_entry(out_dir, &fname, "collision"));
        }
    }
    progress.stage(Stage::Export, 0.7);

    let mut main_fbx = None;
    if recipe.export.fbx {
        let mut fbx_lods: Vec<crate::io::fbx_out::FbxLod> = vec![crate::io::fbx_out::FbxLod { name: format!("{name}_LOD0"), mesh: &lod0 }];
        for (i, l) in lods.iter().enumerate() {
            fbx_lods.push(crate::io::fbx_out::FbxLod { name: format!("{name}_LOD{}", i + 1), mesh: l });
        }
        if fbx_lods.len() == 1 {
            fbx_lods[0].name = name.clone();
        }
        let mut coll: Vec<(String, &Mesh)> = Vec::new();
        for (i, (kind, m)) in collision.shapes().iter().enumerate() {
            let prefix = if *kind == "box" { "UBX" } else { "UCX" };
            coll.push((format!("{prefix}_{name}_{:02}", i + 1), *m));
        }
        let skel = if recipe.export.skin { high_scene.skeleton.as_ref() } else { None };
        let anims: &[crate::mesh::Animation] = if recipe.export.skin { &high_scene.animations } else { &[] };
        let fbx_scene = crate::io::fbx_out::FbxScene {
            name: name.clone(),
            lods: fbx_lods,
            material: crate::io::fbx_out::FbxMaterial {
                name: format!("{name}_material"),
                base_color: if tex_albedo.is_some() { [1.0; 4] } else { src_mat.base_color },
                roughness: src_mat.roughness,
                metallic: src_mat.metallic,
                albedo_file: tex_albedo.clone(),
                normal_file: tex_normal.clone(),
                ao_file: tex_ao.clone(),
                orm_file: tex_orm.clone(),
            },
            collision: coll,
            scale: dcc_scale,
            skeleton: skel,
            animations: anims,
        };
        let fname = format!("{name}.fbx");
        match crate::io::fbx_out::write(&out_dir.join(&fname), &fbx_scene) {
            Ok(()) => {
                files.push(file_entry(out_dir, &fname, "fbx"));
                main_fbx = Some(fname);
            }
            Err(e) => progress.log(format!("FBX export failed ({e})")),
        }
    }
    progress.stage(Stage::Export, 0.85);

    // Final report on a seam-welded copy so UV splits are not counted as open edges.
    let mut welded = lod0.clone();
    crate::clean::weld(&mut welded, 0.0);
    let final_scene = Scene { name: name.clone(), mesh: welded, materials: vec![Default::default()], textures: vec![], source_format: scene.source_format.clone(), source_bytes: 0, skeleton: None, animations: vec![] };
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
    if let Some(i) = &imposter {
        lod_stats.push(LodStat { level: lods.len() + 1, triangles: i.cards.triangle_count(), vertices: i.cards.vertex_count(), screen_coverage: coverage.last().copied().unwrap_or(0.5) * 0.5 });
    }
    timings.insert("export".into(), timer.lap().into());
    let rig = high_scene.skeleton.as_ref().filter(|_| lod0.has_skin()).map(|s| RigInfo { joints: s.joints.len(), animations: high_scene.animations.iter().map(|a| a.name.clone()).collect() });
    let metrics = Metrics {
        deviation: dev_stats,
        texel_density: density.map(|(d, _)| d),
        uv_charts: low.uv_charts,
        quads: low.quads,
        polygons: if lod0.has_polygons() { lod0.polygons.len() } else { lod0.triangle_count() },
        watertight: report.watertight,
        hidden_faces_removed: cleaned.hidden_removed,
        tracer: tracer_name,
    };

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
        main_fbx,
        metrics,
        rig,
        cached_stages,
        previews,
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
    let _ = cleaned.reconstructed_points;
    Ok(result)
}

fn scene_without_mesh(scene: &Scene) -> Scene {
    Scene {
        name: scene.name.clone(),
        mesh: Mesh::default(),
        materials: scene.materials.clone(),
        textures: scene.textures.clone(),
        source_format: scene.source_format.clone(),
        source_bytes: scene.source_bytes,
        skeleton: scene.skeleton.clone(),
        animations: scene.animations.clone(),
    }
}
