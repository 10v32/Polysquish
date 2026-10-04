//! STL import (binary and ASCII). STL has no shared vertices, so the result is unindexed
//! until the cleanup stage welds it.

use crate::mesh::{Mesh, Scene};
use anyhow::{Context, Result};
use glam::Vec3;
use std::path::Path;

pub fn load(path: &Path) -> Result<Scene> {
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .open(path)
        .with_context(|| format!("cannot open {}", path.display()))?;
    let stl = stl_io::read_stl(&mut file).with_context(|| format!("failed to parse {}", path.display()))?;
    let mut mesh = Mesh {
        positions: stl
            .vertices
            .iter()
            .map(|v| Vec3::new(v[0], v[1], v[2]))
            .collect(),
        ..Default::default()
    };
    mesh.indices.reserve(stl.faces.len() * 3);
    for f in &stl.faces {
        for &i in &f.vertices {
            mesh.indices.push(i as u32);
        }
    }
    Ok(Scene {
        mesh,
        ..Default::default()
    })
}
