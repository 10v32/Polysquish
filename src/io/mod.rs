//! Model importers and exporters.

pub mod obj;
pub mod ply;
pub mod stl;
pub mod gltf_in;
pub mod gltf_out;
pub mod obj_out;
pub mod pointcloud;

use crate::mesh::Scene;
use anyhow::{bail, Context, Result};
use std::path::Path;

pub const IMPORT_EXTENSIONS: &[&str] = &["obj", "ply", "stl", "glb", "gltf", "splat"];

pub fn extension_of(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default()
}

pub fn is_supported(path: &Path) -> bool {
    IMPORT_EXTENSIONS.contains(&extension_of(path).as_str())
}

/// Load any supported model into a single merged `Scene`.
///
/// A PLY without faces or a `.splat` file yields a point cloud: positions (and colours/normals)
/// but no indices. Check `pointcloud::is_point_cloud` before running mesh-only stages.
pub fn load_scene(path: &Path) -> Result<Scene> {
    let ext = extension_of(path);
    let meta = std::fs::metadata(path).with_context(|| format!("cannot read {}", path.display()))?;
    let mut scene = match ext.as_str() {
        "obj" => obj::load(path)?,
        "ply" => ply::load(path)?,
        "stl" => stl::load(path)?,
        "glb" | "gltf" => gltf_in::load(path)?,
        "splat" => pointcloud::load_splat(path)?,
        other => bail!("unsupported file type .{other} (supported: obj, ply, stl, glb, gltf, splat)"),
    };
    if scene.mesh.positions.is_empty() {
        bail!("the file contains no geometry");
    }
    scene.source_format = ext;
    scene.source_bytes = meta.len();
    if scene.name.is_empty() {
        scene.name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("model")
            .to_string();
    }
    if scene.materials.is_empty() {
        scene.materials.push(Default::default());
    }
    Ok(scene)
}

/// Decode an image file into an RGBA8 texture, returning None (with a log line) on failure.
pub fn load_texture(path: &Path, name: &str) -> Option<crate::mesh::Texture> {
    match image::open(path) {
        Ok(img) => Some(crate::mesh::Texture {
            name: name.to_string(),
            image: img.to_rgba8(),
        }),
        Err(e) => {
            log::warn!("could not load texture {}: {e}", path.display());
            None
        }
    }
}

/// Decode an in-memory image.
pub fn decode_texture(bytes: &[u8], name: &str) -> Option<crate::mesh::Texture> {
    match image::load_from_memory(bytes) {
        Ok(img) => Some(crate::mesh::Texture {
            name: name.to_string(),
            image: img.to_rgba8(),
        }),
        Err(e) => {
            log::warn!("could not decode embedded texture {name}: {e}");
            None
        }
    }
}
