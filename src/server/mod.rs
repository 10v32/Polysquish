//! Local HTTP server: serves the embedded web UI and the JSON job API (see docs/API.md).

use crate::analyze::HealthReport;
use crate::mesh::Scene;
use crate::progress::{CancelToken, Progress, ProgressSink, Stage};
use crate::pipeline::{SquishContext, StageCache};
use crate::recipe::Recipe;
use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Multipart, Path as AxPath, State};
use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(rust_embed::Embed)]
#[folder = "ui/"]
#[exclude = "screenshots/*"]
#[exclude = "node_modules/*"]
struct UiAssets;

pub struct Upload {
    pub id: String,
    pub main_path: PathBuf,
    pub files: Vec<String>,
    pub size_bytes: u64,
    pub scene: Mutex<Option<Arc<Scene>>>,
}

#[derive(Clone, Debug, Serialize)]
pub struct StageState {
    pub id: &'static str,
    pub label: &'static str,
    pub status: &'static str,
    pub seconds: Option<f64>,
    #[serde(skip)]
    started: Option<Instant>,
}

#[derive(Serialize)]
pub struct JobState {
    pub status: &'static str,
    pub progress: f32,
    pub stage: Option<&'static str>,
    pub stage_label: Option<&'static str>,
    pub stage_progress: f32,
    pub stages: Vec<StageState>,
    pub log: Vec<String>,
    pub error: Option<String>,
    pub result: Option<Value>,
    pub elapsed: f64,
    #[serde(skip)]
    started: Instant,
}

pub struct Job {
    pub id: String,
    pub kind: &'static str,
    pub name: String,
    pub state: Mutex<JobState>,
    pub cancel: CancelToken,
    pub dir: PathBuf,
    pub output_dir: Mutex<Option<PathBuf>>,
    pub tags: Vec<String>,
}

impl Job {
    fn new(id: String, kind: &'static str, name: String, dir: PathBuf, stages: &[Stage]) -> Self {
        Job {
            id,
            kind,
            name,
            state: Mutex::new(JobState {
                status: "queued",
                progress: 0.0,
                stage: None,
                stage_label: None,
                stage_progress: 0.0,
                stages: stages
                    .iter()
                    .map(|s| StageState { id: s.id(), label: s.label(), status: "pending", seconds: None, started: None })
                    .collect(),
                log: Vec::new(),
                error: None,
                result: None,
                elapsed: 0.0,
                started: Instant::now(),
            }),
            cancel: CancelToken::new(),
            dir,
            output_dir: Mutex::new(None),
            tags: Vec::new(),
        }
    }

    fn snapshot(&self, with_log: bool) -> Value {
        let mut st = self.state.lock().unwrap();
        st.elapsed = st.started.elapsed().as_secs_f64();
        let mut v = serde_json::to_value(&*st).unwrap_or(json!({}));
        v["id"] = json!(self.id);
        v["kind"] = json!(self.kind);
        v["name"] = json!(self.name);
        v["tags"] = json!(self.tags);
        if !with_log {
            v.as_object_mut().map(|o| o.remove("log"));
        }
        v
    }
}

struct JobSink {
    job: Arc<Job>,
    weights: Vec<(Stage, f32)>,
}

impl ProgressSink for JobSink {
    fn stage(&self, stage: Stage, fraction: f32) {
        let mut st = self.job.state.lock().unwrap();
        let total: f32 = self.weights.iter().map(|(_, w)| w).sum();
        let mut acc = 0.0;
        for (s, w) in &self.weights {
            if *s == stage {
                acc += w * fraction;
                break;
            }
            acc += w;
        }
        st.progress = (acc / total.max(1e-6)).clamp(0.0, 1.0);
        st.stage = Some(stage.id());
        st.stage_label = Some(stage.label());
        st.stage_progress = fraction;
        let now = Instant::now();
        for s in &mut st.stages {
            if s.id == stage.id() {
                if s.started.is_none() {
                    s.started = Some(now);
                }
                if fraction >= 1.0 {
                    s.status = "done";
                    s.seconds = s.started.map(|t| (t.elapsed().as_secs_f64() * 100.0).round() / 100.0);
                } else {
                    s.status = "running";
                }
            } else if s.status == "running" {
                s.status = "done";
                s.seconds = s.started.map(|t| (t.elapsed().as_secs_f64() * 100.0).round() / 100.0);
            }
        }
    }
    fn log(&self, message: String) {
        let mut st = self.job.state.lock().unwrap();
        st.log.push(message);
        if st.log.len() > 500 {
            st.log.remove(0);
        }
    }
}

pub struct Batch {
    pub id: String,
    pub job_ids: Vec<String>,
}

pub struct Watch {
    pub id: String,
    pub folder: PathBuf,
    pub output_dir: PathBuf,
    pub preset: String,
    pub recipe: Recipe,
    pub job_ids: Mutex<Vec<String>>,
    pub processed: Mutex<HashSet<String>>,
    pub active: AtomicBool,
}

type QueueItem = Box<dyn FnOnce() + Send>;

pub struct AppState {
    pub uploads: Mutex<HashMap<String, Arc<Upload>>>,
    pub jobs: Mutex<Vec<Arc<Job>>>,
    pub batches: Mutex<Vec<Arc<Batch>>>,
    pub watches: Mutex<Vec<Arc<Watch>>>,
    pub output_root: PathBuf,
    pub work_dir: PathBuf,
    pub ui_dir: Option<PathBuf>,
    pub cache: Arc<StageCache>,
    pub queue: Mutex<mpsc::Sender<QueueItem>>,
    pub gpu: Option<String>,
}

type Shared = Arc<AppState>;

fn new_id(prefix: &str) -> String {
    let u = uuid::Uuid::new_v4().simple().to_string();
    format!("{prefix}_{}", &u[..8])
}

struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

fn bad(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, msg.into())
}
fn not_found(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::NOT_FOUND, msg.into())
}
fn internal(e: impl std::fmt::Display) -> ApiError {
    ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

pub fn default_output_root() -> PathBuf {
    dirs::document_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Polysquish")
}

pub fn router(state: Shared) -> Router {
    Router::new()
        .route("/api/health", get(health))
        .route("/api/presets", get(presets))
        .route("/api/upload", post(upload))
        .route("/api/upload/path", post(upload_path))
        .route("/api/inspect", post(inspect))
        .route("/api/squish", post(squish))
        .route("/api/jobs", get(list_jobs))
        .route("/api/jobs/{id}", get(get_job))
        .route("/api/jobs/{id}/cancel", post(cancel_job))
        .route("/api/jobs/{id}/preview/{file}", get(preview_file))
        .route("/api/jobs/{id}/files/{file}", get(output_file))
        .route("/api/jobs/{id}/zip", get(zip_outputs))
        .route("/api/batch", post(create_batch))
        .route("/api/batch/{id}", get(get_batch))
        .route("/api/watch", post(create_watch).get(list_watches))
        .route("/api/watch/{id}", axum::routing::delete(delete_watch))
        .route("/api/fs", get(fs_list))
        .route("/api/open-folder", post(open_folder))
        .fallback(static_file)
        .layer(DefaultBodyLimit::disable())
        .with_state(state)
}

async fn health(State(s): State<Shared>) -> Json<Value> {
    Json(json!({
        "version": crate::VERSION,
        "threads": rayon::current_num_threads(),
        "output_root": s.output_root,
        "gpu": { "available": s.gpu.is_some(), "name": s.gpu },
    }))
}

async fn presets() -> Json<Value> {
    Json(serde_json::to_value(crate::recipe::presets()).unwrap_or(json!([])))
}

fn upload_json(u: &Upload) -> Value {
    json!({
        "upload_id": u.id,
        "main_file": u.main_path.file_name().and_then(|s| s.to_str()).unwrap_or(""),
        "files": u.files,
        "size_bytes": u.size_bytes,
    })
}

fn safe_file_name(name: &str) -> String {
    let base = Path::new(name).file_name().and_then(|s| s.to_str()).unwrap_or("upload");
    let s: String = base.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ' ') { c } else { '_' }).collect();
    if s.trim().is_empty() {
        "upload".into()
    } else {
        s
    }
}

async fn upload(State(s): State<Shared>, mut mp: Multipart) -> Result<Json<Value>, ApiError> {
    let id = new_id("u");
    let dir = s.work_dir.join(&id);
    std::fs::create_dir_all(&dir).map_err(internal)?;
    let mut files: Vec<String> = Vec::new();
    let mut total = 0u64;
    while let Some(field) = mp.next_field().await.map_err(|e| bad(format!("upload error: {e}")))? {
        let Some(fname) = field.file_name().map(|f| f.to_string()) else { continue };
        let fname = safe_file_name(&fname);
        let path = dir.join(&fname);
        let mut f = tokio::fs::File::create(&path).await.map_err(internal)?;
        let mut field = field;
        use tokio::io::AsyncWriteExt;
        while let Some(chunk) = field.chunk().await.map_err(|e| bad(format!("upload error: {e}")))? {
            total += chunk.len() as u64;
            f.write_all(&chunk).await.map_err(internal)?;
        }
        f.flush().await.map_err(internal)?;
        files.push(fname);
    }
    if files.is_empty() {
        return Err(bad("no files were uploaded"));
    }
    let main = files
        .iter()
        .find(|f| crate::io::is_supported(Path::new(f)))
        .cloned()
        .ok_or_else(|| bad("none of the uploaded files is a supported model (.obj .ply .stl .glb .gltf)"))?;
    let up = Arc::new(Upload { id: id.clone(), main_path: dir.join(&main), files, size_bytes: total, scene: Mutex::new(None) });
    s.uploads.lock().unwrap().insert(id, up.clone());
    Ok(Json(upload_json(&up)))
}

#[derive(Deserialize)]
struct PathReq {
    path: String,
}

async fn upload_path(State(s): State<Shared>, Json(req): Json<PathReq>) -> Result<Json<Value>, ApiError> {
    let p = PathBuf::from(req.path.trim().trim_matches('"'));
    if !p.is_file() {
        return Err(bad(format!("{} is not a file", p.display())));
    }
    if !crate::io::is_supported(&p) {
        return Err(bad("unsupported file type (use .obj .ply .stl .glb .gltf)"));
    }
    let size = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
    let id = new_id("u");
    let up = Arc::new(Upload {
        id: id.clone(),
        main_path: p.clone(),
        files: vec![p.file_name().and_then(|s| s.to_str()).unwrap_or("").to_string()],
        size_bytes: size,
        scene: Mutex::new(None),
    });
    s.uploads.lock().unwrap().insert(id, up.clone());
    Ok(Json(upload_json(&up)))
}

fn get_upload(s: &Shared, id: &str) -> Result<Arc<Upload>, ApiError> {
    s.uploads.lock().unwrap().get(id).cloned().ok_or_else(|| not_found("unknown upload_id"))
}

fn load_scene_cached(up: &Upload) -> anyhow::Result<Arc<Scene>> {
    if let Some(s) = up.scene.lock().unwrap().clone() {
        return Ok(s);
    }
    let scene = Arc::new(crate::io::load_scene(&up.main_path)?);
    *up.scene.lock().unwrap() = Some(scene.clone());
    Ok(scene)
}

fn start_job(s: &Shared, kind: &'static str, name: &str, stages: &[Stage]) -> Arc<Job> {
    let id = new_id("j");
    let dir = s.work_dir.join(&id);
    let _ = std::fs::create_dir_all(&dir);
    let job = Arc::new(Job::new(id, kind, name.to_string(), dir, stages));
    s.jobs.lock().unwrap().push(job.clone());
    job
}

fn upload_stem(up: &Upload) -> String {
    up.main_path.file_stem().and_then(|s| s.to_str()).unwrap_or("model").to_string()
}

/// Create a squish job for an upload and put it on the (sequential) work queue.
fn enqueue_squish(s: &Shared, up: Arc<Upload>, recipe: Recipe, name: String, out_dir: PathBuf, tags: Vec<String>) -> Arc<Job> {
    let mut job = Job::new(new_id("j"), "squish", name.clone(), s.work_dir.join("pending"), &Stage::ALL);
    job.dir = s.work_dir.join(&job.id);
    job.tags = tags;
    let _ = std::fs::create_dir_all(&job.dir);
    let job = Arc::new(job);
    *job.output_dir.lock().unwrap() = Some(out_dir.clone());
    s.jobs.lock().unwrap().push(job.clone());
    let j = job.clone();
    let cache = s.cache.clone();
    let work: QueueItem = Box::new(move || {
        if j.cancel.is_cancelled() {
            finish_job(&j, Err(anyhow::anyhow!("cancelled")));
            return;
        }
        {
            let mut st = j.state.lock().unwrap();
            st.status = "running";
            st.started = Instant::now();
        }
        let sink = Arc::new(JobSink { job: j.clone(), weights: Stage::ALL.iter().map(|s| (*s, s.weight())).collect() });
        let progress = Progress::new(sink, j.cancel.clone());
        let res = (|| -> anyhow::Result<Value> {
            progress.stage(Stage::Import, 0.0);
            let scene = load_scene_cached(&up)?;
            progress.log(format!("Loaded {} triangles", scene.mesh.triangle_count()));
            progress.stage(Stage::Import, 1.0);
            let ctx = SquishContext { cache: Some(cache), cache_key: up.id.clone() };
            let result = crate::pipeline::squish_with(&scene, &recipe, &out_dir, &name, &progress, &ctx)?;
            let src = crate::pipeline::preview_glb(&scene, 250_000)?;
            std::fs::write(j.dir.join("source.glb"), src)?;
            if let Some(glb) = &result.main_glb {
                let _ = std::fs::copy(out_dir.join(glb), j.dir.join("result.glb"));
            }
            let mut preview = json!({
                "source": format!("/api/jobs/{}/preview/source.glb", j.id),
                "result": if result.main_glb.is_some() { Value::String(format!("/api/jobs/{}/preview/result.glb", j.id)) } else { Value::Null },
                "heatmap_deviation": Value::Null,
                "heatmap_density": Value::Null,
            });
            for (fname, bytes) in &result.previews {
                if std::fs::write(j.dir.join(fname), bytes).is_ok() {
                    let key = fname.trim_end_matches(".glb");
                    preview[key] = json!(format!("/api/jobs/{}/preview/{}", j.id, fname));
                }
            }
            let mut v = serde_json::to_value(&result)?;
            v["preview"] = preview;
            v["download_zip"] = json!(format!("/api/jobs/{}/zip", j.id));
            Ok(v)
        })();
        finish_job(&j, res);
    });
    if s.queue.lock().unwrap().send(work).is_err() {
        finish_job(&job, Err(anyhow::anyhow!("work queue is not running")));
    }
    job
}

fn finish_job(job: &Job, result: anyhow::Result<Value>) {
    let mut st = job.state.lock().unwrap();
    match result {
        Ok(v) => {
            st.status = "done";
            st.progress = 1.0;
            st.result = Some(v);
            for s in &mut st.stages {
                if s.status == "running" {
                    s.status = "done";
                }
            }
        }
        Err(e) => {
            if job.cancel.is_cancelled() {
                st.status = "cancelled";
                st.error = Some("Cancelled".into());
            } else {
                st.status = "error";
                st.error = Some(format!("{e:#}"));
            }
            for s in &mut st.stages {
                if s.status == "running" {
                    s.status = "error";
                }
            }
        }
    }
}

#[derive(Deserialize)]
struct InspectReq {
    upload_id: String,
}

async fn inspect(State(s): State<Shared>, Json(req): Json<InspectReq>) -> Result<Json<Value>, ApiError> {
    let up = get_upload(&s, &req.upload_id)?;
    let job = start_job(&s, "inspect", &upload_stem(&up), &[Stage::Import, Stage::Analyze]);
    let j = job.clone();
    std::thread::spawn(move || {
        {
            j.state.lock().unwrap().status = "running";
        }
        let sink = Arc::new(JobSink { job: j.clone(), weights: vec![(Stage::Import, 0.7), (Stage::Analyze, 0.3)] });
        let progress = Progress::new(sink, j.cancel.clone());
        let res = (|| -> anyhow::Result<Value> {
            progress.stage(Stage::Import, 0.0);
            let scene = load_scene_cached(&up)?;
            progress.log(format!("Loaded {} triangles from {}", scene.mesh.triangle_count(), up.main_path.file_name().and_then(|s| s.to_str()).unwrap_or("")));
            progress.stage(Stage::Import, 1.0);
            progress.stage(Stage::Analyze, 0.0);
            let report: HealthReport = crate::pipeline::inspect(&scene);
            progress.stage(Stage::Analyze, 0.5);
            let glb = crate::pipeline::preview_glb(&scene, 250_000)?;
            std::fs::write(j.dir.join("source.glb"), glb)?;
            progress.stage(Stage::Analyze, 1.0);
            Ok(json!({
                "report": report,
                "preview_url": format!("/api/jobs/{}/preview/source.glb", j.id),
            }))
        })();
        finish_job(&j, res);
    });
    Ok(Json(json!({ "job_id": job.id })))
}

#[derive(Deserialize)]
struct SquishReq {
    upload_id: String,
    recipe: Recipe,
    name: Option<String>,
    output_dir: Option<String>,
}

async fn squish(State(s): State<Shared>, Json(req): Json<SquishReq>) -> Result<Json<Value>, ApiError> {
    let up = get_upload(&s, &req.upload_id)?;
    let name = req.name.filter(|n| !n.trim().is_empty()).unwrap_or_else(|| upload_stem(&up));
    let out_dir = match req.output_dir.filter(|d| !d.trim().is_empty()) {
        Some(d) => PathBuf::from(d),
        None => s.output_root.join(&name),
    };
    let job = enqueue_squish(&s, up, req.recipe, name, out_dir, vec![]);
    Ok(Json(json!({ "job_id": job.id })))
}

#[derive(Deserialize)]
struct BatchReq {
    upload_ids: Vec<String>,
    recipe: Recipe,
    output_dir: Option<String>,
}

async fn create_batch(State(s): State<Shared>, Json(req): Json<BatchReq>) -> Result<Json<Value>, ApiError> {
    if req.upload_ids.is_empty() {
        return Err(bad("upload_ids is empty"));
    }
    let batch_id = new_id("b");
    let mut job_ids = Vec::new();
    for uid in &req.upload_ids {
        let up = get_upload(&s, uid)?;
        let name = upload_stem(&up);
        let out_dir = match req.output_dir.as_ref().filter(|d| !d.trim().is_empty()) {
            Some(d) => PathBuf::from(d).join(&name),
            None => s.output_root.join(&name),
        };
        let job = enqueue_squish(&s, up, req.recipe.clone(), name, out_dir, vec![format!("batch:{batch_id}")]);
        job_ids.push(job.id.clone());
    }
    s.batches.lock().unwrap().push(Arc::new(Batch { id: batch_id.clone(), job_ids: job_ids.clone() }));
    Ok(Json(json!({ "batch_id": batch_id, "job_ids": job_ids })))
}

fn batch_json(s: &Shared, b: &Batch) -> Value {
    let jobs = s.jobs.lock().unwrap();
    let mut done = 0;
    let mut status = "done";
    let mut any_running = false;
    let mut any_error = false;
    for id in &b.job_ids {
        if let Some(j) = jobs.iter().find(|j| &j.id == id) {
            let st = j.state.lock().unwrap().status;
            match st {
                "done" => done += 1,
                "error" | "cancelled" => {
                    done += 1;
                    any_error = true;
                }
                "running" => any_running = true,
                _ => {}
            }
        }
    }
    if done < b.job_ids.len() {
        status = if any_running { "running" } else { "queued" };
    } else if any_error {
        status = "error";
    }
    json!({ "id": b.id, "job_ids": b.job_ids, "done": done, "total": b.job_ids.len(), "status": status })
}

async fn get_batch(State(s): State<Shared>, AxPath(id): AxPath<String>) -> Result<Json<Value>, ApiError> {
    let b = s.batches.lock().unwrap().iter().find(|b| b.id == id).cloned().ok_or_else(|| not_found("unknown batch"))?;
    Ok(Json(batch_json(&s, &b)))
}

#[derive(Deserialize)]
struct WatchReq {
    folder: String,
    output_dir: Option<String>,
    recipe: Option<Recipe>,
    preset: Option<String>,
}

fn watch_json(w: &Watch) -> Value {
    let jobs = w.job_ids.lock().unwrap().clone();
    json!({
        "id": w.id,
        "folder": w.folder,
        "output_dir": w.output_dir,
        "preset": w.preset,
        "processed": w.processed.lock().unwrap().len(),
        "queued": jobs.len(),
        "job_ids": jobs,
        "active": w.active.load(Ordering::SeqCst),
    })
}

async fn create_watch(State(s): State<Shared>, Json(req): Json<WatchReq>) -> Result<Json<Value>, ApiError> {
    let folder = PathBuf::from(req.folder.trim());
    if !folder.is_dir() {
        return Err(bad(format!("{} is not a folder", folder.display())));
    }
    let preset = req.preset.clone().unwrap_or_else(|| "prop".into());
    let recipe = match req.recipe {
        Some(r) => r,
        None => Recipe::preset(&preset).ok_or_else(|| bad(format!("unknown preset {preset}")))?,
    };
    let output_dir = req.output_dir.filter(|d| !d.trim().is_empty()).map(PathBuf::from).unwrap_or_else(|| s.output_root.clone());
    std::fs::create_dir_all(&output_dir).map_err(internal)?;
    let w = Arc::new(Watch {
        id: new_id("w"),
        folder: folder.clone(),
        output_dir,
        preset,
        recipe,
        job_ids: Mutex::new(Vec::new()),
        processed: Mutex::new(HashSet::new()),
        active: AtomicBool::new(true),
    });
    s.watches.lock().unwrap().push(w.clone());
    let st = s.clone();
    std::thread::spawn(move || watch_loop(st, w));
    Ok(Json(json!({ "watch_id": s.watches.lock().unwrap().last().unwrap().id })))
}

/// Polls a folder: a supported model whose size has been stable for two polls and which has no
/// output yet is queued for squishing.
fn watch_loop(s: Shared, w: Arc<Watch>) {
    let mut sizes: HashMap<String, u64> = HashMap::new();
    while w.active.load(Ordering::SeqCst) {
        if let Ok(rd) = std::fs::read_dir(&w.folder) {
            for entry in rd.flatten() {
                let path = entry.path();
                if !path.is_file() || !crate::io::is_supported(&path) {
                    continue;
                }
                let fname = entry.file_name().to_string_lossy().to_string();
                if w.processed.lock().unwrap().contains(&fname) {
                    continue;
                }
                let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                let stable = sizes.get(&fname) == Some(&size) && size > 0;
                sizes.insert(fname.clone(), size);
                if !stable {
                    continue;
                }
                let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("model").to_string();
                let out_dir = w.output_dir.join(&stem);
                w.processed.lock().unwrap().insert(fname.clone());
                if out_dir.join("result.json").exists() {
                    continue; // already squished in an earlier session
                }
                let id = new_id("u");
                let up = Arc::new(Upload { id: id.clone(), main_path: path.clone(), files: vec![fname.clone()], size_bytes: size, scene: Mutex::new(None) });
                s.uploads.lock().unwrap().insert(id, up.clone());
                let job = enqueue_squish(&s, up, w.recipe.clone(), stem, out_dir, vec![format!("watch:{}", w.id)]);
                w.job_ids.lock().unwrap().push(job.id.clone());
            }
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

async fn list_watches(State(s): State<Shared>) -> Json<Value> {
    let ws = s.watches.lock().unwrap();
    Json(Value::Array(ws.iter().filter(|w| w.active.load(Ordering::SeqCst)).map(|w| watch_json(w)).collect()))
}

async fn delete_watch(State(s): State<Shared>, AxPath(id): AxPath<String>) -> Result<Json<Value>, ApiError> {
    let ws = s.watches.lock().unwrap();
    let w = ws.iter().find(|w| w.id == id).ok_or_else(|| not_found("unknown watch"))?;
    w.active.store(false, Ordering::SeqCst);
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct FsQuery {
    path: Option<String>,
}

async fn fs_list(axum::extract::Query(q): axum::extract::Query<FsQuery>) -> Result<Json<Value>, ApiError> {
    let path = match q.path.filter(|p| !p.trim().is_empty()) {
        Some(p) => PathBuf::from(p),
        None => dirs::home_dir().unwrap_or_else(|| PathBuf::from("/")),
    };
    let path = std::fs::canonicalize(&path).map_err(|_| not_found(format!("{} does not exist", path.display())))?;
    if !path.is_dir() {
        return Err(not_found(format!("{} is not a folder", path.display())));
    }
    let mut dirs_v: Vec<String> = Vec::new();
    let mut files: Vec<Value> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&path) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            match e.file_type() {
                Ok(t) if t.is_dir() => dirs_v.push(name),
                Ok(t) if t.is_file() => {
                    let supported = crate::io::is_supported(&e.path());
                    let size = e.metadata().map(|m| m.len()).unwrap_or(0);
                    files.push(json!({ "name": name, "size_bytes": size, "supported": supported }));
                }
                _ => {}
            }
        }
    }
    dirs_v.sort_by_key(|d| d.to_lowercase());
    files.sort_by_key(|f| f["name"].as_str().unwrap_or("").to_lowercase());
    Ok(Json(json!({
        "path": path,
        "parent": path.parent().map(|p| p.to_path_buf()),
        "dirs": dirs_v,
        "files": files,
    })))
}

fn find_job(s: &Shared, id: &str) -> Result<Arc<Job>, ApiError> {
    s.jobs.lock().unwrap().iter().find(|j| j.id == id).cloned().ok_or_else(|| not_found("unknown job"))
}

async fn list_jobs(State(s): State<Shared>) -> Json<Value> {
    let jobs = s.jobs.lock().unwrap().clone();
    Json(Value::Array(jobs.iter().map(|j| j.snapshot(false)).collect()))
}

async fn get_job(State(s): State<Shared>, AxPath(id): AxPath<String>) -> Result<Json<Value>, ApiError> {
    let job = find_job(&s, &id)?;
    Ok(Json(job.snapshot(true)))
}

async fn cancel_job(State(s): State<Shared>, AxPath(id): AxPath<String>) -> Result<Json<Value>, ApiError> {
    let job = find_job(&s, &id)?;
    job.cancel.cancel();
    Ok(Json(json!({ "ok": true })))
}

fn serve_path(path: &Path, download_name: Option<&str>) -> Result<Response, ApiError> {
    let bytes = std::fs::read(path).map_err(|_| not_found("file not found"))?;
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    let mut resp = Response::builder().header(header::CONTENT_TYPE, mime.as_ref()).header(header::CACHE_CONTROL, "no-store");
    if let Some(n) = download_name {
        resp = resp.header(header::CONTENT_DISPOSITION, format!("inline; filename=\"{n}\""));
    }
    resp.body(Body::from(bytes)).map_err(internal)
}

async fn preview_file(State(s): State<Shared>, AxPath((id, file)): AxPath<(String, String)>) -> Result<Response, ApiError> {
    let job = find_job(&s, &id)?;
    if !matches!(file.as_str(), "source.glb" | "result.glb" | "heatmap_deviation.glb" | "heatmap_density.glb") {
        return Err(not_found("unknown preview"));
    }
    serve_path(&job.dir.join(file), None)
}

async fn output_file(State(s): State<Shared>, AxPath((id, file)): AxPath<(String, String)>) -> Result<Response, ApiError> {
    let job = find_job(&s, &id)?;
    let out = job.output_dir.lock().unwrap().clone().ok_or_else(|| not_found("job has no outputs"))?;
    let safe = safe_file_name(&file);
    if safe != file {
        return Err(bad("invalid file name"));
    }
    serve_path(&out.join(&safe), Some(&safe))
}

async fn zip_outputs(State(s): State<Shared>, AxPath(id): AxPath<String>) -> Result<Response, ApiError> {
    let job = find_job(&s, &id)?;
    let out = job.output_dir.lock().unwrap().clone().ok_or_else(|| not_found("job has no outputs"))?;
    let name = out.file_name().and_then(|s| s.to_str()).unwrap_or("polysquish").to_string();
    let bytes = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<u8>> {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut zw = zip::ZipWriter::new(&mut buf);
            let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
            for entry in std::fs::read_dir(&out)? {
                let entry = entry?;
                if entry.file_type()?.is_file() {
                    let fname = entry.file_name().to_string_lossy().to_string();
                    zw.start_file(fname, opts)?;
                    let data = std::fs::read(entry.path())?;
                    std::io::Write::write_all(&mut zw, &data)?;
                }
            }
            zw.finish()?;
        }
        Ok(buf.into_inner())
    })
    .await
    .map_err(internal)?
    .map_err(internal)?;
    Response::builder()
        .header(header::CONTENT_TYPE, "application/zip")
        .header(header::CONTENT_DISPOSITION, format!("attachment; filename=\"{name}.zip\""))
        .body(Body::from(bytes))
        .map_err(internal)
}

async fn open_folder(State(s): State<Shared>, Json(req): Json<PathReq>) -> Result<Json<Value>, ApiError> {
    let p = PathBuf::from(&req.path);
    // Only allow opening folders we produced.
    if !p.starts_with(&s.output_root) && !s.jobs.lock().unwrap().iter().any(|j| j.output_dir.lock().unwrap().as_deref() == Some(p.as_path())) {
        return Err(bad("refusing to open a folder Polysquish did not create"));
    }
    open::that_detached(&p).map_err(internal)?;
    Ok(Json(json!({ "ok": true })))
}

async fn static_file(State(s): State<Shared>, uri: Uri) -> Response {
    let mut path = uri.path().trim_start_matches('/').to_string();
    if path.is_empty() {
        path = "index.html".into();
    }
    if let Some(dir) = &s.ui_dir {
        let p = dir.join(&path);
        if p.is_file() {
            return serve_path(&p, None).unwrap_or_else(|e| e.into_response());
        }
    }
    match UiAssets::get(&path) {
        Some(f) => {
            let mime = mime_guess::from_path(&path).first_or_octet_stream();
            Response::builder()
                .header(header::CONTENT_TYPE, mime.as_ref())
                .body(Body::from(f.data.into_owned()))
                .unwrap()
        }
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

/// Run the server until the process exits.
pub async fn serve(port: u16, output_root: PathBuf, ui_dir: Option<PathBuf>, open_browser: bool) -> anyhow::Result<()> {
    serve_with_ready(port, output_root, ui_dir, move |addr| {
        if open_browser {
            let _ = open::that_detached(format!("http://{addr}"));
        }
    })
    .await
}

/// Run the server; `on_ready` receives the bound address once listening.
pub async fn serve_with_ready(
    port: u16,
    output_root: PathBuf,
    ui_dir: Option<PathBuf>,
    on_ready: impl FnOnce(std::net::SocketAddr) + Send + 'static,
) -> anyhow::Result<()> {
    let work_dir = std::env::temp_dir().join(format!("polysquish-{}", std::process::id()));
    std::fs::create_dir_all(&work_dir)?;
    std::fs::create_dir_all(&output_root)?;
    let (tx, rx) = mpsc::channel::<QueueItem>();
    std::thread::Builder::new()
        .name("polysquish-queue".into())
        .spawn(move || {
            for item in rx {
                item();
            }
        })?;
    let gpu = crate::gpu::probe();
    let state = Arc::new(AppState {
        uploads: Mutex::new(HashMap::new()),
        jobs: Mutex::new(Vec::new()),
        batches: Mutex::new(Vec::new()),
        watches: Mutex::new(Vec::new()),
        output_root,
        work_dir: work_dir.clone(),
        ui_dir,
        cache: Arc::new(StageCache::default()),
        queue: Mutex::new(tx),
        gpu,
    });
    let app = router(state);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    let addr = listener.local_addr()?;
    eprintln!("Polysquish is running at http://{addr}");
    on_ready(addr);
    let result = axum::serve(listener, app).await;
    let _ = std::fs::remove_dir_all(&work_dir);
    result.map_err(Into::into)
}
