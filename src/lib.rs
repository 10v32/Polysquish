//! Polysquish core library: import, analyse, clean, decimate, unwrap, bake and export meshes.

pub mod mesh;
pub mod io;
pub mod analyze;
pub mod clean;
pub mod decimate;
pub mod uv;
pub mod bvh;
pub mod gpu;
pub mod bake;
pub mod collision;
pub mod imposter;
pub mod recipe;
pub mod pipeline;
pub mod report;
pub mod synth;
pub mod retopo;
pub mod voxel;
pub mod progress;
pub mod normals;
pub mod metrics;
pub mod server;

pub use mesh::{Material, Mesh, Scene, Texture};
pub use recipe::Recipe;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
