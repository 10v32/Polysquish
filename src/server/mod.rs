//! Local HTTP server: serves the embedded web UI and the JSON job API (see docs/API.md).

use crate::analyze::HealthReport;
use crate::mesh::Scene;
use crate::progress::{CancelToken, Progress, ProgressSink, Stage};
use crate::recipe::Recipe;
use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Multipart, Path as AxPath, State};
use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

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
    pub state: Mutex<JobState>,
    pub cancel: CancelToken,
    pub dir: PathBuf,
    pub output_dir: Mutex<Option<PathBuf>>,
}

impl Job {
    fn new(id: String, kind: &'static str, dir: PathBuf, stages: &[Stage]) -> Self {
        Job {
            id,
            kind,
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
        }
    }

    fn snapshot(&self, with_log: bool) -> Value {
        let mut st = self.state.lock().unwrap();
        st.elapsed = st.started.elapsed().as_secs_f64();
        let mut v = serde_json::to_value(&*st).unwrap_or(json!({}));
        v["id"] = json!(self.id);
        v["kind"] = json!(self.kind);
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

pub struct AppState {
    pub uploads: Mutex<HashMap<String, Arc<Upload>>>,
    pub jobs: Mutex<Vec<Arc<Job>>>,
    pub output_root: PathBuf,
    pub work_dir: PathBuf,
    pub ui_dir: Option<PathBuf>,
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

fn start_job(s: &Shared, kind: &'static str, stages: &[Stage]) -> Arc<Job> {
    let id = new_id("j");
    let dir = s.work_dir.join(&id);
    let _ = std::fs::create_dir_all(&dir);
    let job = Arc::new(Job::new(id, kind, dir, stages));
    s.jobs.lock().unwrap().push(job.clone());
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
    let job = start_job(&s, "inspect", &[Stage::Import, Stage::Analyze]);
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
    let name = req
        .name
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| up.main_path.file_stem().and_then(|s| s.to_str()).unwrap_or("model").to_string());
    let out_dir = match req.output_dir.filter(|d| !d.trim().is_empty()) {
        Some(d) => PathBuf::from(d),
        None => s.output_root.join(&name),
    };
    let job = start_job(&s, "squish", &Stage::ALL);
    *job.output_dir.lock().unwrap() = Some(out_dir.clone());
    let j = job.clone();
    let recipe = req.recipe;
    std::thread::spawn(move || {
        {
            j.state.lock().unwrap().status = "running";
        }
        let sink = Arc::new(JobSink { job: j.clone(), weights: Stage::ALL.iter().map(|s| (*s, s.weight())).collect() });
        let progress = Progress::new(sink, j.cancel.clone());
        let res = (|| -> anyhow::Result<Value> {
            progress.stage(Stage::Import, 0.0);
            let scene = load_scene_cached(&up)?;
            progress.log(format!("Loaded {} triangles", scene.mesh.triangle_count()));
            progress.stage(Stage::Import, 1.0);
            let result = crate::pipeline::squish(&scene, &recipe, &out_dir, &name, &progress)?;
            // Previews
            let src = crate::pipeline::preview_glb(&scene, 250_000)?;
            std::fs::write(j.dir.join("source.glb"), src)?;
            if let Some(glb) = &result.main_glb {
                let _ = std::fs::copy(out_dir.join(glb), j.dir.join("result.glb"));
            }
            let mut v = serde_json::to_value(&result)?;
            v["preview"] = json!({
                "source": format!("/api/jobs/{}/preview/source.glb", j.id),
                "result": if result.main_glb.is_some() { Value::String(format!("/api/jobs/{}/preview/result.glb", j.id)) } else { Value::Null },
            });
            v["download_zip"] = json!(format!("/api/jobs/{}/zip", j.id));
            Ok(v)
        })();
        finish_job(&j, res);
    });
    Ok(Json(json!({ "job_id": job.id })))
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
    if !matches!(file.as_str(), "source.glb" | "result.glb") {
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
    let work_dir = std::env::temp_dir().join(format!("polysquish-{}", std::process::id()));
    std::fs::create_dir_all(&work_dir)?;
    std::fs::create_dir_all(&output_root)?;
    let state = Arc::new(AppState { uploads: Mutex::new(HashMap::new()), jobs: Mutex::new(Vec::new()), output_root, work_dir: work_dir.clone(), ui_dir });
    let app = router(state);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    let addr = listener.local_addr()?;
    let url = format!("http://{addr}");
    eprintln!("Polysquish is running at {url}");
    if open_browser {
        let _ = open::that_detached(&url);
    }
    let result = axum::serve(listener, app).await;
    let _ = std::fs::remove_dir_all(&work_dir);
    result.map_err(Into::into)
}
