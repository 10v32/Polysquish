//! Progress reporting and cooperative cancellation shared by every long operation.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Import,
    Analyze,
    Clean,
    Decimate,
    Uv,
    Bake,
    Lods,
    Collision,
    Export,
}

impl Stage {
    pub const ALL: [Stage; 9] = [
        Stage::Import,
        Stage::Analyze,
        Stage::Clean,
        Stage::Decimate,
        Stage::Uv,
        Stage::Bake,
        Stage::Lods,
        Stage::Collision,
        Stage::Export,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Stage::Import => "import",
            Stage::Analyze => "analyze",
            Stage::Clean => "clean",
            Stage::Decimate => "decimate",
            Stage::Uv => "uv",
            Stage::Bake => "bake",
            Stage::Lods => "lods",
            Stage::Collision => "collision",
            Stage::Export => "export",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Stage::Import => "Reading model",
            Stage::Analyze => "Health check",
            Stage::Clean => "Cleaning",
            Stage::Decimate => "Squishing polygons",
            Stage::Uv => "Unwrapping UVs",
            Stage::Bake => "Baking textures",
            Stage::Lods => "Building LODs",
            Stage::Collision => "Collision shapes",
            Stage::Export => "Exporting",
        }
    }

    /// Rough share of total wall time, used to blend stage progress into one bar.
    pub fn weight(self) -> f32 {
        match self {
            Stage::Import => 0.10,
            Stage::Analyze => 0.05,
            Stage::Clean => 0.05,
            Stage::Decimate => 0.10,
            Stage::Uv => 0.15,
            Stage::Bake => 0.40,
            Stage::Lods => 0.05,
            Stage::Collision => 0.03,
            Stage::Export => 0.07,
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("cancelled")]
pub struct Cancelled;

/// Shared cancellation flag.
#[derive(Clone, Default, Debug)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
    pub fn check(&self) -> Result<(), Cancelled> {
        if self.is_cancelled() {
            Err(Cancelled)
        } else {
            Ok(())
        }
    }
}

/// Receives progress events. Implementations must be cheap and thread safe.
pub trait ProgressSink: Send + Sync {
    fn stage(&self, stage: Stage, fraction: f32);
    fn log(&self, message: String);
}

/// A sink that prints to stderr; used by the CLI.
pub struct StderrSink;

impl ProgressSink for StderrSink {
    fn stage(&self, stage: Stage, fraction: f32) {
        if fraction <= 0.0 {
            eprintln!("▶ {}", stage.label());
        } else if fraction >= 1.0 {
            eprintln!("✓ {}", stage.label());
        }
    }
    fn log(&self, message: String) {
        eprintln!("  {message}");
    }
}

/// A sink that discards everything.
pub struct NullSink;

impl ProgressSink for NullSink {
    fn stage(&self, _: Stage, _: f32) {}
    fn log(&self, _: String) {}
}

/// Convenience handle bundling a sink and a cancel token.
#[derive(Clone)]
pub struct Progress {
    pub sink: Arc<dyn ProgressSink>,
    pub cancel: CancelToken,
}

impl Progress {
    pub fn new(sink: Arc<dyn ProgressSink>, cancel: CancelToken) -> Self {
        Self { sink, cancel }
    }
    pub fn stderr() -> Self {
        Self::new(Arc::new(StderrSink), CancelToken::new())
    }
    pub fn silent() -> Self {
        Self::new(Arc::new(NullSink), CancelToken::new())
    }
    pub fn stage(&self, stage: Stage, fraction: f32) {
        self.sink.stage(stage, fraction.clamp(0.0, 1.0));
    }
    pub fn log(&self, message: impl Into<String>) {
        self.sink.log(message.into());
    }
    pub fn check(&self) -> Result<(), Cancelled> {
        self.cancel.check()
    }
}
