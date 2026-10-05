use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use polysquish::progress::Progress;
use polysquish::recipe::{Recipe, Target};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "polysquish", version, about = "Squish multi-million-polygon AI models into game-ready meshes")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Start the desktop UI (default when run without arguments)
    Ui {
        #[arg(long, default_value_t = 7777)]
        port: u16,
        /// Where squished models are written
        #[arg(long)]
        output_root: Option<PathBuf>,
        /// Serve the UI from this folder instead of the embedded copy (development)
        #[arg(long)]
        ui_dir: Option<PathBuf>,
        /// Do not open the browser automatically
        #[arg(long)]
        no_open: bool,
    },
    /// Squish a model from the command line
    Squish {
        input: PathBuf,
        /// Output directory (default: ./<name>_squished)
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Preset: hero, prop, mobile, dcc
        #[arg(short, long, default_value = "hero")]
        preset: String,
        /// Override the triangle budget
        #[arg(short = 't', long)]
        target_tris: Option<usize>,
        /// Override the texture size (512..8192)
        #[arg(long)]
        texture: Option<u32>,
        /// Target: generic, unity, unreal, godot, blender, maya, c4d
        #[arg(long)]
        target: Option<String>,
        /// Skip ambient occlusion baking (faster)
        #[arg(long)]
        no_ao: bool,
        /// Skip baking entirely
        #[arg(long)]
        no_bake: bool,
        /// Load a full recipe JSON (overrides preset)
        #[arg(long)]
        recipe: Option<PathBuf>,
        /// Output name (default: input file stem)
        #[arg(short, long)]
        name: Option<String>,
    },
    /// Print the health report of a model as JSON
    Inspect { input: PathBuf },
    /// List presets (or dump one as JSON with --dump)
    Presets {
        #[arg(long)]
        dump: Option<String>,
    },
    /// Generate a heavy synthetic test model from a smaller one (dev tool)
    Synth {
        input: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
        /// Midpoint subdivision levels (each level multiplies triangles by 4)
        #[arg(long, default_value_t = 2)]
        levels: u32,
        /// Noise displacement amplitude as a fraction of the model size
        #[arg(long, default_value_t = 0.003)]
        noise: f32,
        /// Number of floating fragments to add
        #[arg(long, default_value_t = 25)]
        floaters: usize,
        /// Skip procedural vertex colours
        #[arg(long)]
        no_paint: bool,
    },
    /// Run the regression corpus and compare against a baseline (dev tool)
    Bench {
        /// Corpus manifest
        #[arg(long, default_value = "corpus/manifest.json")]
        manifest: PathBuf,
        /// Baseline bench.json to compare against (and to update with --update-baseline)
        #[arg(long, default_value = "corpus/baseline.json")]
        baseline: PathBuf,
        /// Output root (bench.json, bench.md and one folder per run)
        #[arg(long, default_value = "bench_out")]
        out: PathBuf,
        /// Only entries whose id contains, or whose tags equal, one of these comma-separated tokens
        #[arg(long)]
        filter: Option<String>,
        /// Run only these presets (repeatable) instead of the ones each entry lists
        #[arg(long = "preset")]
        presets: Vec<String>,
        /// Write the results into the baseline file (merged by id and preset)
        #[arg(long)]
        update_baseline: bool,
        /// Relative growth of time or deviation that counts as a regression
        #[arg(long, default_value_t = 0.15)]
        tolerance: f32,
        /// Smaller textures (<= 512 px), no ambient occlusion
        #[arg(long)]
        quick: bool,
    },
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let cli = Cli::parse();
    match cli.command.unwrap_or(Command::Ui { port: 7777, output_root: None, ui_dir: None, no_open: false }) {
        Command::Ui { port, output_root, ui_dir, no_open } => {
            let root = output_root.unwrap_or_else(polysquish::server::default_output_root);
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(polysquish::server::serve(port, root, ui_dir, !no_open))
        }
        Command::Squish { input, output, preset, target_tris, texture, target, no_ao, no_bake, recipe, name } => {
            let mut rec = match recipe {
                Some(p) => serde_json::from_str(&std::fs::read_to_string(&p)?).context("invalid recipe JSON")?,
                None => Recipe::preset(&preset).with_context(|| format!("unknown preset '{preset}' (try: hero, prop, mobile, dcc)"))?,
            };
            if let Some(t) = target_tris {
                rec.decimate.target_triangles = Some(t);
            }
            if let Some(t) = texture {
                rec.uv.resolution = t;
                rec.bake.resolution = t;
            }
            if let Some(t) = target {
                rec.export.target = match t.to_lowercase().as_str() {
                    "unity" => Target::Unity,
                    "unreal" | "ue" | "ue5" => Target::Unreal,
                    "godot" => Target::Godot,
                    "blender" => Target::Blender,
                    "maya" => Target::Maya,
                    "c4d" | "cinema4d" => Target::C4d,
                    _ => Target::Generic,
                };
            }
            if no_ao {
                rec.bake.ao = false;
            }
            if no_bake {
                rec.bake.enabled = false;
            }
            let name = name.unwrap_or_else(|| input.file_stem().and_then(|s| s.to_str()).unwrap_or("model").to_string());
            let out = output.unwrap_or_else(|| PathBuf::from(format!("{name}_squished")));
            let progress = Progress::stderr();
            let t0 = std::time::Instant::now();
            eprintln!("▶ Reading {}", input.display());
            let scene = polysquish::io::load_scene(&input)?;
            let result = polysquish::pipeline::squish(&scene, &rec, &out, &name, &progress)?;
            eprintln!(
                "\nDone in {:.1}s: {} → {} triangles, written to {}",
                t0.elapsed().as_secs_f64(),
                result.before.triangles,
                result.after.triangles,
                out.display()
            );
            for f in &result.files {
                eprintln!("  {:<40} {:>10} bytes  {}", f.name, f.size_bytes, f.kind);
            }
            Ok(())
        }
        Command::Inspect { input } => {
            let scene = polysquish::io::load_scene(&input)?;
            let report = polysquish::pipeline::inspect(&scene);
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
        Command::Presets { dump } => {
            if let Some(id) = dump {
                let r = Recipe::preset(&id).with_context(|| format!("unknown preset {id}"))?;
                println!("{}", serde_json::to_string_pretty(&r)?);
            } else {
                for p in polysquish::recipe::presets() {
                    println!("{:<8} {:<34} {:>7} tris  {:>4}px  {}", p.id, p.name, p.target_triangles, p.texture_size, p.tagline);
                }
            }
            Ok(())
        }
        Command::Synth { input, output, levels, noise, floaters, no_paint } => {
            let scene = polysquish::io::load_scene(&input)?;
            let mut mesh = scene.mesh;
            eprintln!("Loaded {} triangles", mesh.triangle_count());
            for _ in 0..levels {
                mesh = polysquish::synth::subdivide_midpoint(&mesh);
            }
            eprintln!("Subdivided to {} triangles", mesh.triangle_count());
            if noise > 0.0 {
                polysquish::synth::displace(&mut mesh, noise, 24.0, 7);
            }
            if !no_paint {
                polysquish::synth::paint(&mut mesh, 3);
            }
            polysquish::synth::add_defects(
                &mut mesh,
                &polysquish::synth::DefectOptions { floaters, duplicate_fraction: 0.002, degenerate: 50, flipped_fraction: 0.001 },
                42,
            );
            mesh.compute_smooth_normals();
            match polysquish::io::extension_of(&output).as_str() {
                "ply" => polysquish::io::ply::save_binary(&output, &mesh)?,
                "obj" => polysquish::io::obj_out::write_obj(&output, &mesh, "synth", None, "default", 1.0)?,
                other => anyhow::bail!("synth output must be .ply or .obj (got .{other})"),
            }
            eprintln!("Wrote {} triangles to {}", mesh.triangle_count(), output.display());
            Ok(())
        }
        Command::Bench { manifest, baseline, out, filter, presets, update_baseline, tolerance, quick } => {
            let args = polysquish::bench::BenchArgs {
                manifest,
                baseline: Some(baseline),
                out,
                filter,
                presets: if presets.is_empty() { None } else { Some(presets) },
                update_baseline,
                tolerance,
                quick,
            };
            let ok = polysquish::bench::run(&args)?;
            if !ok {
                std::process::exit(1);
            }
            Ok(())
        }
    }
}
