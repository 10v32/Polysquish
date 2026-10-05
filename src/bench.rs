//! `polysquish bench`: squish every model of the regression corpus with every preset it lists,
//! record timings, output size and geometric fidelity, and compare against a baseline.
//!
//! The corpus is described by `corpus/manifest.json` (see `corpus/README.md`). Inputs are either
//! downloaded (sha256-verified) or synthesised from another entry with [`crate::synth`], and cached
//! next to the manifest in `cache/`.

use crate::bvh::Bvh;
use crate::mesh::Mesh;
use crate::progress::Progress;
use crate::recipe::Recipe;
use anyhow::{anyhow, bail, Context, Result};
use glam::Vec3;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Options for [`run`].
#[derive(Clone, Debug)]
pub struct BenchArgs {
    /// `corpus/manifest.json`.
    pub manifest: PathBuf,
    /// A previous `bench.json` to compare against (missing file = no comparison).
    pub baseline: Option<PathBuf>,
    /// Output root; squished models go to `out/<id>/<preset>/`.
    pub out: PathBuf,
    /// Comma-separated tokens; an entry runs when its id contains a token or one of its tags equals it.
    pub filter: Option<String>,
    /// Run only these presets (for every entry) instead of the ones each entry lists.
    pub presets: Option<Vec<String>>,
    /// Merge the new rows into the baseline file.
    pub update_baseline: bool,
    /// Relative growth of total time or deviation that counts as a regression (default 0.15).
    pub tolerance: f32,
    /// Smaller textures (<= 512 px), no ambient occlusion, no supersampling.
    pub quick: bool,
}

impl Default for BenchArgs {
    fn default() -> Self {
        Self {
            manifest: PathBuf::from("corpus/manifest.json"),
            baseline: Some(PathBuf::from("corpus/baseline.json")),
            out: PathBuf::from("bench_out"),
            filter: None,
            presets: None,
            update_baseline: false,
            tolerance: 0.15,
            quick: false,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Manifest
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SynthSpec {
    /// Id of the entry to derive from.
    pub from: String,
    #[serde(default = "default_levels")]
    pub levels: u32,
    #[serde(default = "default_noise")]
    pub noise: f32,
    #[serde(default = "default_floaters")]
    pub floaters: usize,
    #[serde(default = "default_true")]
    pub paint: bool,
    #[serde(default = "default_seed")]
    pub seed: u64,
}

fn default_levels() -> u32 {
    2
}
fn default_noise() -> f32 {
    0.003
}
fn default_floaters() -> usize {
    25
}
fn default_true() -> bool {
    true
}
fn default_seed() -> u64 {
    42
}
fn default_presets() -> Vec<String> {
    vec!["hero".into(), "prop".into(), "mobile".into()]
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Source {
    Url {
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sha256: Option<String>,
    },
    Synth {
        synth: SynthSpec,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub source: Source,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default = "default_presets")]
    pub presets: Vec<String>,
    #[serde(default)]
    pub notes: String,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ManifestFile {
    List(Vec<Entry>),
    Object { entries: Vec<Entry> },
}

/// Read and validate a manifest.
pub fn load_manifest(path: &Path) -> Result<Vec<Entry>> {
    let text = std::fs::read_to_string(path).with_context(|| format!("cannot read manifest {}", path.display()))?;
    let file: ManifestFile = serde_json::from_str(&text).with_context(|| format!("invalid manifest {}", path.display()))?;
    let entries = match file {
        ManifestFile::List(e) => e,
        ManifestFile::Object { entries } => entries,
    };
    let mut seen = std::collections::HashSet::new();
    for e in &entries {
        if e.id.is_empty() || !e.id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
            bail!("manifest id {:?} must be non-empty [A-Za-z0-9_-]", e.id);
        }
        if !seen.insert(e.id.clone()) {
            bail!("duplicate manifest id {:?}", e.id);
        }
        if let Source::Synth { synth } = &e.source {
            if !entries.iter().any(|o| o.id == synth.from) {
                bail!("entry {:?} derives from unknown entry {:?}", e.id, synth.from);
            }
        }
    }
    Ok(entries)
}

// ---------------------------------------------------------------------------------------------
// Results
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InputStats {
    pub path: String,
    pub triangles: usize,
    pub vertices: usize,
    pub bytes: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileSize {
    pub name: String,
    pub bytes: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OutputStats {
    pub triangles: usize,
    pub vertices: usize,
    pub texture_size: u32,
    /// Triangle counts of LOD1..n.
    pub lods: Vec<usize>,
    pub lod_count: usize,
    pub files: Vec<FileSize>,
    pub total_bytes: u64,
    /// Size of the main GLB (or OBJ + textures when no GLB is written).
    pub main_bytes: u64,
}

/// Distances as fractions of the source bounding diagonal.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Deviation {
    pub mean: f32,
    pub p95: f32,
    pub max: f32,
    pub samples: usize,
    /// Source vertices projected onto the result (informational: floater removal inflates it).
    pub source_to_result_mean: f32,
    pub source_to_result_p95: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Edges {
    pub open: usize,
    pub non_manifold: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunRow {
    pub id: String,
    pub preset: String,
    pub tags: Vec<String>,
    /// "ok" or "error".
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<InputStats>,
    /// Seconds spent in `io::load_scene`.
    pub load_s: f64,
    /// Seconds spent in `pipeline::squish`.
    pub pipeline_s: f64,
    /// `load_s + pipeline_s`; the number gated against the baseline.
    pub total_s: f64,
    /// Per-stage seconds from `SquishResult::timings`.
    pub timings: BTreeMap<String, f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<OutputStats>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deviation: Option<Deviation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edges: Option<Edges>,
    /// Peak resident set size sampled during the run (Linux only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peak_rss_mb: Option<f64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Host {
    pub os: String,
    pub arch: String,
    pub cpus: usize,
    pub rayon_threads: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BenchReport {
    pub polysquish_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_commit: Option<String>,
    pub timestamp: String,
    pub host: Host,
    pub quick: bool,
    pub tolerance: f32,
    pub results: Vec<RunRow>,
}

// ---------------------------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------------------------

/// Run the corpus. Returns `Ok(true)` when nothing regressed against the baseline.
pub fn run(args: &BenchArgs) -> Result<bool> {
    let entries = load_manifest(&args.manifest)?;
    let manifest_dir = args.manifest.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
    let cache = manifest_dir.join("cache");
    std::fs::create_dir_all(&cache).with_context(|| format!("cannot create {}", cache.display()))?;
    std::fs::create_dir_all(&args.out).with_context(|| format!("cannot create {}", args.out.display()))?;

    let tokens: Vec<String> = args
        .filter
        .as_deref()
        .map(|f| f.split(',').map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty()).collect())
        .unwrap_or_default();
    let selected: Vec<&Entry> = entries
        .iter()
        .filter(|e| {
            tokens.is_empty()
                || tokens.iter().any(|t| e.id.to_lowercase().contains(t) || e.tags.iter().any(|g| g.to_lowercase() == *t))
        })
        .collect();
    if selected.is_empty() {
        bail!("no manifest entry matches filter {:?}", args.filter.as_deref().unwrap_or(""));
    }

    let mut jobs: Vec<(&Entry, String)> = Vec::new();
    for e in &selected {
        let presets = args.presets.clone().unwrap_or_else(|| e.presets.clone());
        for p in presets {
            if Recipe::preset(&p).is_none() {
                bail!("entry {:?} lists unknown preset {:?}", e.id, p);
            }
            jobs.push((e, p));
        }
    }

    let baseline_path = args.baseline.clone().or_else(|| if args.update_baseline { Some(manifest_dir.join("baseline.json")) } else { None });
    let baseline: Option<BenchReport> = match &baseline_path {
        Some(p) if p.exists() => {
            let text = std::fs::read_to_string(p)?;
            Some(serde_json::from_str(&text).with_context(|| format!("invalid baseline {}", p.display()))?)
        }
        _ => None,
    };

    eprintln!(
        "polysquish bench: {} run{} ({} entr{}), {} threads{}{}",
        jobs.len(),
        if jobs.len() == 1 { "" } else { "s" },
        selected.len(),
        if selected.len() == 1 { "y" } else { "ies" },
        rayon::current_num_threads(),
        if args.quick { ", quick" } else { "" },
        match (&baseline, &baseline_path) {
            (Some(_), Some(p)) => format!(", baseline {}", p.display()),
            (None, Some(p)) => format!(", no baseline yet at {}", p.display()),
            _ => String::new(),
        }
    );

    let mut rows: Vec<RunRow> = Vec::with_capacity(jobs.len());
    let mut all_ok = true;
    for (i, (entry, preset)) in jobs.iter().enumerate() {
        eprint!("[{}/{}] {} × {} … ", i + 1, jobs.len(), entry.id, preset);
        let row = match run_one(entry, preset, &entries, &cache, &args.out, args.quick) {
            Ok(r) => r,
            Err(e) => RunRow {
                id: entry.id.clone(),
                preset: preset.clone(),
                tags: entry.tags.clone(),
                status: "error".into(),
                error: Some(format!("{e:#}")),
                input: None,
                load_s: 0.0,
                pipeline_s: 0.0,
                total_s: 0.0,
                timings: BTreeMap::new(),
                output: None,
                deviation: None,
                edges: None,
                peak_rss_mb: None,
            },
        };
        let base_row = baseline.as_ref().and_then(|b| b.results.iter().find(|r| r.id == row.id && r.preset == row.preset));
        let verdict = compare(&row, base_row, args.tolerance);
        if matches!(verdict, Verdict::Regression(_)) {
            all_ok = false;
        }
        eprintln!("{}", describe(&row, &verdict));
        rows.push(row);
    }

    let report = BenchReport {
        polysquish_version: crate::VERSION.to_string(),
        git_commit: git_commit(&manifest_dir),
        timestamp: now_string(),
        host: Host {
            os: std::env::consts::OS.into(),
            arch: std::env::consts::ARCH.into(),
            cpus: std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1),
            rayon_threads: rayon::current_num_threads(),
        },
        quick: args.quick,
        tolerance: args.tolerance,
        results: rows,
    };

    std::fs::write(args.out.join("bench.json"), serde_json::to_string_pretty(&report)?)?;
    std::fs::write(args.out.join("bench.md"), render_markdown(&report, baseline.as_ref()))?;

    // Summary.
    let n_err = report.results.iter().filter(|r| r.status != "ok").count();
    let n_reg = report
        .results
        .iter()
        .filter(|r| {
            let b = baseline.as_ref().and_then(|b| b.results.iter().find(|x| x.id == r.id && x.preset == r.preset));
            matches!(compare(r, b, args.tolerance), Verdict::Regression(_))
        })
        .count();
    eprintln!();
    eprintln!("Wrote {} and {}", args.out.join("bench.json").display(), args.out.join("bench.md").display());
    if all_ok {
        eprintln!("PASS: {} run{}, no regression", report.results.len(), if report.results.len() == 1 { "" } else { "s" });
    } else {
        eprintln!("REGRESSION: {n_reg} of {} run{} regressed ({n_err} failed)", report.results.len(), if report.results.len() == 1 { "" } else { "s" });
    }

    if args.update_baseline {
        let path = baseline_path.ok_or_else(|| anyhow!("update_baseline needs a baseline path"))?;
        let merged = merge_baseline(baseline, &report);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, serde_json::to_string_pretty(&merged)?)?;
        eprintln!("Baseline updated: {} ({} rows)", path.display(), merged.results.len());
    }
    Ok(all_ok)
}

fn merge_baseline(old: Option<BenchReport>, new: &BenchReport) -> BenchReport {
    let mut rows: Vec<RunRow> = old.map(|b| b.results).unwrap_or_default();
    for r in &new.results {
        if r.status != "ok" {
            continue; // never bake a failure into the baseline
        }
        rows.retain(|x| !(x.id == r.id && x.preset == r.preset));
        rows.push(r.clone());
    }
    rows.sort_by(|a, b| a.id.cmp(&b.id).then(a.preset.cmp(&b.preset)));
    BenchReport { results: rows, ..new.clone() }
}

// ---------------------------------------------------------------------------------------------
// One run
// ---------------------------------------------------------------------------------------------

fn run_one(entry: &Entry, preset: &str, all: &[Entry], cache: &Path, out_root: &Path, quick: bool) -> Result<RunRow> {
    let input = ensure_input(entry, all, cache, 0)?;
    let mut recipe = Recipe::preset(preset).ok_or_else(|| anyhow!("unknown preset {preset}"))?;
    if quick {
        recipe.uv.resolution = recipe.uv.resolution.min(512);
        recipe.bake.resolution = recipe.bake.resolution.min(512);
        recipe.bake.ao = false;
        recipe.bake.supersample = 1;
    }
    let out_dir = out_root.join(&entry.id).join(preset);
    if out_dir.exists() {
        std::fs::remove_dir_all(&out_dir).ok();
    }
    std::fs::create_dir_all(&out_dir)?;

    let rss = RssSampler::start();
    let t0 = Instant::now();
    let scene = crate::io::load_scene(&input).with_context(|| format!("loading {}", input.display()))?;
    let load_s = t0.elapsed().as_secs_f64();
    let t1 = Instant::now();
    let result = crate::pipeline::squish(&scene, &recipe, &out_dir, &entry.id, &Progress::silent()).context("pipeline")?;
    let pipeline_s = t1.elapsed().as_secs_f64();
    let peak_rss_mb = rss.finish();

    let timings: BTreeMap<String, f64> = result.timings.iter().filter_map(|(k, v)| v.as_f64().map(|f| (k.clone(), f))).collect();
    let files: Vec<FileSize> = result.files.iter().map(|f| FileSize { name: f.name.clone(), bytes: f.size_bytes }).collect();
    let output = OutputStats {
        triangles: result.after.triangles,
        vertices: result.after.vertices,
        texture_size: result.after.texture_size,
        lods: result.after.lods.iter().filter(|l| l.level > 0).map(|l| l.triangles).collect(),
        lod_count: result.after.lods.iter().filter(|l| l.level > 0).count(),
        total_bytes: files.iter().map(|f| f.bytes).sum(),
        main_bytes: result.after.size_bytes,
        files,
    };

    // Geometric deviation: load LOD0 back from disk (OBJ is LOD0 only; GLB merges every LOD).
    let lod0 = load_lod0(&result, &recipe, &out_dir)?;
    let diag = scene.mesh.bounds().diagonal.max(1e-9);
    let deviation = measure_deviation(&scene.mesh, &lod0, diag);

    Ok(RunRow {
        id: entry.id.clone(),
        preset: preset.to_string(),
        tags: entry.tags.clone(),
        status: "ok".into(),
        error: None,
        input: Some(InputStats {
            path: input.display().to_string(),
            triangles: result.before.triangles,
            vertices: result.before.vertices,
            bytes: result.before.size_bytes,
        }),
        load_s: round3(load_s),
        pipeline_s: round3(pipeline_s),
        total_s: round3(load_s + pipeline_s),
        timings,
        output: Some(output),
        deviation: Some(deviation),
        edges: Some(Edges { open: result.report.boundary_edges, non_manifold: result.report.non_manifold_edges }),
        peak_rss_mb,
    })
}

fn round3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

fn load_lod0(result: &crate::pipeline::SquishResult, recipe: &Recipe, out_dir: &Path) -> Result<Mesh> {
    if let Some(obj) = &result.main_obj {
        let mut mesh = read_obj_geometry(&out_dir.join(obj)).context("reading LOD0 OBJ")?;
        // Undo the export scale so the result lives in source units.
        let units_m = result.source_report.units_guess == "meters";
        let scale = recipe.export.scale * if recipe.export.target.prefers_centimeters() && units_m { 100.0 } else { 1.0 };
        if (scale - 1.0).abs() > 1e-6 && scale > 0.0 {
            mesh.scale_translate(1.0 / scale, Vec3::ZERO);
        }
        return Ok(mesh);
    }
    if let Some(glb) = &result.main_glb {
        let mut mesh = crate::io::load_scene(&out_dir.join(glb)).context("reading LOD0 GLB")?.mesh;
        if (recipe.export.scale - 1.0).abs() > 1e-6 && recipe.export.scale > 0.0 {
            mesh.scale_translate(1.0 / recipe.export.scale, Vec3::ZERO);
        }
        return Ok(mesh);
    }
    bail!("the recipe writes neither OBJ nor GLB; nothing to measure")
}

/// Positions and (fan-triangulated) faces of an OBJ file, ignoring materials and textures. The
/// full importer would also decode every baked texture referenced by the MTL, which is wasted work
/// for a geometry comparison.
fn read_obj_geometry(path: &Path) -> Result<Mesh> {
    use std::io::BufRead;
    let f = std::io::BufReader::new(std::fs::File::open(path).with_context(|| format!("cannot open {}", path.display()))?);
    let mut mesh = Mesh::default();
    let mut face: Vec<u32> = Vec::with_capacity(4);
    for line in f.lines() {
        let line = line?;
        let mut it = line.split_ascii_whitespace();
        match it.next() {
            Some("v") => {
                let mut c = [0f32; 3];
                for v in c.iter_mut() {
                    *v = it.next().and_then(|t| t.parse().ok()).ok_or_else(|| anyhow!("bad vertex line {line:?}"))?;
                }
                mesh.positions.push(Vec3::from(c));
            }
            Some("f") => {
                face.clear();
                for tok in it {
                    let idx: i64 = tok.split('/').next().and_then(|t| t.parse().ok()).ok_or_else(|| anyhow!("bad face line {line:?}"))?;
                    let n = mesh.positions.len() as i64;
                    let i = if idx < 0 { n + idx } else { idx - 1 };
                    if i < 0 || i >= n {
                        bail!("face index out of range in {line:?}");
                    }
                    face.push(i as u32);
                }
                for k in 1..face.len().saturating_sub(1) {
                    mesh.indices.extend_from_slice(&[face[0], face[k], face[k + 1]]);
                }
            }
            _ => {}
        }
    }
    if mesh.indices.is_empty() {
        bail!("{} contains no faces", path.display());
    }
    Ok(mesh)
}

/// Distances from the result's vertices and triangle centroids to the source surface, and the
/// reverse direction on a subsample of source vertices.
pub fn measure_deviation(source: &Mesh, result: &Mesh, diag: f32) -> Deviation {
    let src_bvh = Bvh::build(source);
    let mut pts: Vec<Vec3> = result.positions.clone();
    pts.extend((0..result.triangle_count()).map(|t| {
        let [a, b, c] = result.tri(t);
        (result.positions[a as usize] + result.positions[b as usize] + result.positions[c as usize]) / 3.0
    }));
    let mut d: Vec<f32> = pts
        .par_iter()
        .with_min_len(512)
        .map(|p| src_bvh.closest_point(*p, diag).map(|(h, _)| h.t).unwrap_or(diag) / diag)
        .collect();
    drop(src_bvh);
    let (mean, p95, max) = stats(&mut d);

    let res_bvh = Bvh::build(result);
    let n = source.positions.len();
    let stride = (n / 100_000).max(1);
    let mut r: Vec<f32> = source
        .positions
        .par_iter()
        .step_by(stride)
        .with_min_len(512)
        .map(|p| res_bvh.closest_point(*p, diag).map(|(h, _)| h.t).unwrap_or(diag) / diag)
        .collect();
    let (rmean, rp95, _) = stats(&mut r);
    Deviation { mean, p95, max, samples: d.len(), source_to_result_mean: rmean, source_to_result_p95: rp95 }
}

fn stats(d: &mut [f32]) -> (f32, f32, f32) {
    if d.is_empty() {
        return (0.0, 0.0, 0.0);
    }
    d.sort_by(f32::total_cmp);
    let mean = (d.iter().map(|&x| x as f64).sum::<f64>() / d.len() as f64) as f32;
    let p95 = d[((d.len() - 1) as f32 * 0.95).round() as usize];
    (mean, p95, d[d.len() - 1])
}

// ---------------------------------------------------------------------------------------------
// Inputs
// ---------------------------------------------------------------------------------------------

/// Make sure the entry's input file exists in the cache and return its path.
pub fn ensure_input(entry: &Entry, all: &[Entry], cache: &Path, depth: usize) -> Result<PathBuf> {
    if depth > 8 {
        bail!("synth chain for {:?} is too deep (cycle?)", entry.id);
    }
    match &entry.source {
        Source::Url { url, sha256 } => ensure_download(&entry.id, url, sha256.as_deref(), cache),
        Source::Synth { synth } => {
            let dest = cache.join(format!("{}.ply", entry.id));
            if dest.exists() {
                return Ok(dest);
            }
            let from = all.iter().find(|e| e.id == synth.from).ok_or_else(|| anyhow!("unknown synth source {:?}", synth.from))?;
            let from_path = ensure_input(from, all, cache, depth + 1)?;
            eprint!("(synthesising {} from {} … ", entry.id, synth.from);
            let t = Instant::now();
            let scene = crate::io::load_scene(&from_path)?;
            let mut mesh = scene.mesh;
            for _ in 0..synth.levels {
                mesh = crate::synth::subdivide_midpoint(&mesh);
            }
            if synth.noise > 0.0 {
                crate::synth::displace(&mut mesh, synth.noise, 24.0, 7);
            }
            if synth.paint {
                crate::synth::paint(&mut mesh, 3);
            }
            crate::synth::add_defects(
                &mut mesh,
                &crate::synth::DefectOptions { floaters: synth.floaters, duplicate_fraction: 0.002, degenerate: 50, flipped_fraction: 0.001 },
                synth.seed,
            );
            mesh.compute_smooth_normals();
            let tmp = cache.join(format!("{}.ply.part", entry.id));
            crate::io::ply::save_binary(&tmp, &mesh)?;
            std::fs::rename(&tmp, &dest)?;
            eprint!("{} triangles in {:.1}s) ", mesh.triangle_count(), t.elapsed().as_secs_f64());
            Ok(dest)
        }
    }
}

fn ensure_download(id: &str, url: &str, sha256: Option<&str>, cache: &Path) -> Result<PathBuf> {
    let fname = url
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty() && !s.contains('?'))
        .map(str::to_string)
        .unwrap_or_else(|| format!("{id}.bin"));
    let dest = cache.join(&fname);
    if !dest.exists() {
        // Reuse a copy from testdata/ (next to the corpus directory) when there is one.
        let local = cache.parent().and_then(Path::parent).map(|root| root.join("testdata").join(&fname)).filter(|p| p.exists());
        if let Some(local) = local {
            if std::fs::hard_link(&local, &dest).is_err() {
                std::fs::copy(&local, &dest).with_context(|| format!("copying {}", local.display()))?;
            }
        } else {
            eprint!("(downloading {fname} … ");
            let tmp = cache.join(format!("{fname}.part"));
            let status = std::process::Command::new("curl")
                .args(["-fsSL", "--retry", "3", "-o"])
                .arg(&tmp)
                .arg(url)
                .status()
                .context("running curl (is it installed?)")?;
            if !status.success() {
                std::fs::remove_file(&tmp).ok();
                bail!("curl failed for {url} ({status})");
            }
            std::fs::rename(&tmp, &dest)?;
            eprint!("done) ");
        }
    }
    if let Some(expected) = sha256 {
        let actual = sha256_file(&dest)?;
        if !actual.eq_ignore_ascii_case(expected.trim()) {
            bail!("sha256 mismatch for {}: expected {expected}, got {actual} (delete the file to re-download)", dest.display());
        }
    }
    Ok(dest)
}

// ---------------------------------------------------------------------------------------------
// Comparison and reporting
// ---------------------------------------------------------------------------------------------

/// Absolute slowdown (seconds) below which a relative timing regression is ignored.
const TIME_FLOOR_S: f64 = 0.5;

#[derive(Debug)]
enum Verdict {
    Pass,
    New,
    Regression(Vec<String>),
}

fn compare(row: &RunRow, base: Option<&RunRow>, tol: f32) -> Verdict {
    if row.status != "ok" {
        return Verdict::Regression(vec![format!("failed: {}", row.error.as_deref().unwrap_or("unknown error"))]);
    }
    let Some(b) = base else { return Verdict::New };
    if b.status != "ok" {
        return Verdict::New;
    }
    let tol = tol as f64;
    let mut why = Vec::new();
    if b.total_s > 0.0 {
        // Sub-second rows are dominated by scheduler and disk noise; require a real difference.
        let rel = (row.total_s - b.total_s) / b.total_s;
        if rel > tol && row.total_s - b.total_s > TIME_FLOOR_S {
            why.push(format!("time {:.2}s → {:.2}s (+{:.0}%)", b.total_s, row.total_s, rel * 100.0));
        }
    }
    if let (Some(d), Some(bd)) = (&row.deviation, &b.deviation) {
        // Ignore differences below 1e-5 of the diagonal: that is float noise, not a regression.
        for (label, new, old) in [("mean", d.mean, bd.mean), ("p95", d.p95, bd.p95)] {
            let (new, old) = (new as f64, old as f64);
            if new - old > 1e-5 && old > 0.0 && (new - old) / old > tol {
                why.push(format!("deviation {label} {:.4}% → {:.4}%", old * 100.0, new * 100.0));
            }
        }
    }
    if let (Some(o), Some(bo)) = (&row.output, &b.output) {
        if o.triangles != bo.triangles {
            why.push(format!("triangles {} → {}", bo.triangles, o.triangles));
        }
    }
    if why.is_empty() {
        Verdict::Pass
    } else {
        Verdict::Regression(why)
    }
}

fn describe(row: &RunRow, v: &Verdict) -> String {
    let mut s = String::new();
    if row.status == "ok" {
        s.push_str(&format!("{:.1}s", row.total_s));
        if let Some(o) = &row.output {
            s.push_str(&format!("  {} tris", fmt_int(o.triangles)));
        }
        if let Some(d) = &row.deviation {
            s.push_str(&format!("  dev {:.4}% (p95 {:.4}%)", d.mean * 100.0, d.p95 * 100.0));
        }
        s.push_str("  ");
    }
    match v {
        Verdict::Pass => s.push_str("PASS"),
        Verdict::New => s.push_str("PASS (new, no baseline row)"),
        Verdict::Regression(why) => {
            s.push_str("REGRESSION: ");
            s.push_str(&why.join("; "));
        }
    }
    s
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

fn render_markdown(report: &BenchReport, baseline: Option<&BenchReport>) -> String {
    let mut md = String::new();
    md.push_str(&format!(
        "# polysquish bench\n\n{} · v{}{} · {} {} · {} CPUs / {} threads · {}{}\n\n",
        report.timestamp,
        report.polysquish_version,
        report.git_commit.as_deref().map(|c| format!(" ({c})")).unwrap_or_default(),
        report.host.os,
        report.host.arch,
        report.host.cpus,
        report.host.rayon_threads,
        if report.quick { "quick mode" } else { "full mode" },
        baseline.map(|b| format!(" · baseline {} ({} rows)", b.timestamp, b.results.len())).unwrap_or_else(|| " · no baseline".into())
    ));
    md.push_str("| id | preset | in tris | out tris | LODs | total s | load | clean | decimate | uv | bake | lods | dev mean % | dev p95 % | open | non-manifold | peak RSS MB | main bytes | status |\n");
    md.push_str("|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|\n");
    for r in &report.results {
        let b = baseline.and_then(|b| b.results.iter().find(|x| x.id == r.id && x.preset == r.preset));
        let v = compare(r, b, report.tolerance);
        let status = match &v {
            Verdict::Pass => "PASS".to_string(),
            Verdict::New => "NEW".to_string(),
            Verdict::Regression(why) => format!("**REGRESSION** {}", why.join("; ")),
        };
        if r.status != "ok" {
            md.push_str(&format!("| {} | {} | | | | | | | | | | | | | | | | | {} |\n", r.id, r.preset, status));
            continue;
        }
        let t = |k: &str| r.timings.get(k).map(|v| format!("{v:.2}")).unwrap_or_default();
        let o = r.output.as_ref();
        let d = r.deviation.as_ref();
        let e = r.edges.as_ref();
        md.push_str(&format!(
            "| {} | {} | {} | {} | {} | {:.2} | {:.2} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
            r.id,
            r.preset,
            r.input.as_ref().map(|i| fmt_int(i.triangles)).unwrap_or_default(),
            o.map(|o| fmt_int(o.triangles)).unwrap_or_default(),
            o.map(|o| o.lods.iter().map(|n| fmt_int(*n)).collect::<Vec<_>>().join(" / ")).unwrap_or_default(),
            r.total_s,
            r.load_s,
            t("clean"),
            t("decimate"),
            t("uv"),
            t("bake"),
            t("lods"),
            d.map(|d| format!("{:.4}", d.mean * 100.0)).unwrap_or_default(),
            d.map(|d| format!("{:.4}", d.p95 * 100.0)).unwrap_or_default(),
            e.map(|e| e.open.to_string()).unwrap_or_default(),
            e.map(|e| e.non_manifold.to_string()).unwrap_or_default(),
            r.peak_rss_mb.map(|m| format!("{m:.0}")).unwrap_or_default(),
            o.map(|o| fmt_int(o.main_bytes as usize)).unwrap_or_default(),
            status
        ));
    }
    md.push_str(&format!(
        "\nRegression rule: total time or deviation (mean / p95) up by more than {:.0}% relative, or any change in LOD0 triangle count. Deviation is the distance from LOD0 vertices and centroids to the source surface as a percentage of the source bounding diagonal.\n",
        report.tolerance * 100.0
    ));
    md
}

// ---------------------------------------------------------------------------------------------
// Helpers: RSS sampler, git, time, sha256
// ---------------------------------------------------------------------------------------------

/// Samples `VmRSS` from `/proc/self/status` every 50 ms on a background thread.
struct RssSampler {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<Option<u64>>>,
}

impl RssSampler {
    fn start() -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let handle = std::thread::Builder::new()
            .name("bench-rss".into())
            .spawn(move || {
                let mut peak: Option<u64> = None;
                loop {
                    if let Some(kb) = read_vm_rss_kb() {
                        peak = Some(peak.map_or(kb, |p| p.max(kb)));
                    } else {
                        return None;
                    }
                    if flag.load(Ordering::Relaxed) {
                        return peak;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            })
            .ok();
        Self { stop, handle }
    }

    fn finish(mut self) -> Option<f64> {
        self.stop.store(true, Ordering::Relaxed);
        self.handle.take().and_then(|h| h.join().ok()).flatten().map(|kb| (kb as f64 / 1024.0 * 10.0).round() / 10.0)
    }
}

fn read_vm_rss_kb() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    status
        .lines()
        .find(|l| l.starts_with("VmRSS:"))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|v| v.parse().ok())
}

fn git_commit(dir: &Path) -> Option<String> {
    let out = std::process::Command::new("git").args(["rev-parse", "--short", "HEAD"]).current_dir(dir).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

fn now_string() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let days = (secs / 86400) as i64;
    let (h, m, s) = ((secs % 86400) / 3600, (secs % 3600) / 60, secs % 60);
    // Howard Hinnant's civil-from-days.
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if mo <= 2 { y + 1 } else { y };
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

/// SHA-256 of a file as lowercase hex (small pure-Rust implementation; no extra dependency).
pub fn sha256_file(path: &Path) -> Result<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finish().iter().map(|b| format!("{b:02x}")).collect())
}

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3,
    0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
    0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208,
    0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

struct Sha256 {
    state: [u32; 8],
    buf: [u8; 64],
    buf_len: usize,
    total: u64,
}

impl Sha256 {
    fn new() -> Self {
        Self {
            state: [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19],
            buf: [0; 64],
            buf_len: 0,
            total: 0,
        }
    }

    fn update(&mut self, mut data: &[u8]) {
        self.total += data.len() as u64;
        if self.buf_len > 0 {
            let take = (64 - self.buf_len).min(data.len());
            self.buf[self.buf_len..self.buf_len + take].copy_from_slice(&data[..take]);
            self.buf_len += take;
            data = &data[take..];
            if self.buf_len == 64 {
                let block = self.buf;
                self.block(&block);
                self.buf_len = 0;
            }
        }
        while data.len() >= 64 {
            let (b, rest) = data.split_at(64);
            self.block(b.try_into().unwrap());
            data = rest;
        }
        if !data.is_empty() {
            self.buf[..data.len()].copy_from_slice(data);
            self.buf_len = data.len();
        }
    }

    fn block(&mut self, b: &[u8; 64]) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([b[4 * i], b[4 * i + 1], b[4 * i + 2], b[4 * i + 3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let [mut a, mut bb, mut c, mut d, mut e, mut f, mut g, mut h] = self.state;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = h.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & bb) ^ (a & c) ^ (bb & c);
            let t2 = s0.wrapping_add(maj);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = bb;
            bb = a;
            a = t1.wrapping_add(t2);
        }
        for (s, v) in self.state.iter_mut().zip([a, bb, c, d, e, f, g, h]) {
            *s = s.wrapping_add(v);
        }
    }

    fn finish(mut self) -> [u8; 32] {
        let bits = self.total.wrapping_mul(8);
        self.update(&[0x80]);
        while self.buf_len != 56 {
            self.update(&[0]);
        }
        // `update` counted the padding; the length field must use the original byte count.
        self.update(&bits.to_be_bytes());
        let mut out = [0u8; 32];
        for (i, s) in self.state.iter().enumerate() {
            out[4 * i..4 * i + 4].copy_from_slice(&s.to_be_bytes());
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(data: &[u8]) -> String {
        let mut h = Sha256::new();
        h.update(data);
        h.finish().iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn sha256_vectors() {
        assert_eq!(hex(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(
            hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        // Multi-block streaming in odd chunk sizes.
        let data = vec![0x61u8; 1000];
        let mut h = Sha256::new();
        for c in data.chunks(37) {
            h.update(c);
        }
        let streamed: String = h.finish().iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(streamed, hex(&data));
    }

    #[test]
    fn manifest_parses_both_shapes() {
        let list: ManifestFile = serde_json::from_str(r#"[{"id":"a","source":{"url":"https://x/y.obj"}}]"#).unwrap();
        let obj: ManifestFile = serde_json::from_str(r#"{"entries":[{"id":"b","source":{"synth":{"from":"a"}}}]}"#).unwrap();
        match (list, obj) {
            (ManifestFile::List(l), ManifestFile::Object { entries }) => {
                assert_eq!(l[0].presets, default_presets());
                assert!(matches!(entries[0].source, Source::Synth { .. }));
            }
            _ => panic!("wrong shapes"),
        }
    }

    #[test]
    fn regression_rules() {
        let mk = |t: f64, tris: usize, dev: f32| RunRow {
            id: "x".into(),
            preset: "hero".into(),
            tags: vec![],
            status: "ok".into(),
            error: None,
            input: None,
            load_s: 0.0,
            pipeline_s: t,
            total_s: t,
            timings: BTreeMap::new(),
            output: Some(OutputStats { triangles: tris, vertices: 0, texture_size: 0, lods: vec![], lod_count: 0, files: vec![], total_bytes: 0, main_bytes: 0 }),
            deviation: Some(Deviation { mean: dev, p95: dev, max: dev, samples: 1, source_to_result_mean: 0.0, source_to_result_p95: 0.0 }),
            edges: None,
            peak_rss_mb: None,
        };
        let base = mk(10.0, 100, 0.01);
        assert!(matches!(compare(&mk(11.0, 100, 0.01), Some(&base), 0.15), Verdict::Pass));
        assert!(matches!(compare(&mk(12.0, 100, 0.01), Some(&base), 0.15), Verdict::Regression(_)));
        assert!(matches!(compare(&mk(10.0, 101, 0.01), Some(&base), 0.15), Verdict::Regression(_)));
        assert!(matches!(compare(&mk(10.0, 100, 0.02), Some(&base), 0.15), Verdict::Regression(_)));
        assert!(matches!(compare(&mk(10.0, 100, 0.0101), Some(&base), 0.15), Verdict::Pass));
        assert!(matches!(compare(&mk(10.0, 100, 0.01), None, 0.15), Verdict::New));
    }
}
