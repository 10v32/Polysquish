//! Wavefront OBJ import (via tobj, with MTL materials and textures).

use crate::mesh::{Material, Mesh, Scene};
use anyhow::{Context, Result};
use glam::{Vec2, Vec3};
use std::collections::HashMap;
use std::path::Path;

pub fn load(path: &Path) -> Result<Scene> {
    let opts = tobj::LoadOptions {
        triangulate: true,
        single_index: true,
        ignore_points: true,
        ignore_lines: true,
    };
    let (models, materials) =
        tobj::load_obj(path, &opts).with_context(|| format!("failed to parse {}", path.display()))?;
    let materials = materials.unwrap_or_default();
    let dir = path.parent().unwrap_or(Path::new("."));

    let mut scene = Scene::default();
    let mut tex_cache: HashMap<String, usize> = HashMap::new();
    let mut load_tex = |scene: &mut Scene, name: &Option<String>| -> Option<usize> {
        let name = name.as_ref()?.trim();
        if name.is_empty() {
            return None;
        }
        if let Some(&i) = tex_cache.get(name) {
            return Some(i);
        }
        let candidate = dir.join(name);
        let p = if candidate.exists() {
            candidate
        } else {
            // Many exporters write absolute or oddly-slashed paths; fall back to the file name.
            dir.join(Path::new(&name.replace('\\', "/")).file_name()?)
        };
        let tex = crate::io::load_texture(&p, name)?;
        scene.textures.push(tex);
        let idx = scene.textures.len() - 1;
        tex_cache.insert(name.to_string(), idx);
        Some(idx)
    };

    for m in &materials {
        let base = m.diffuse.unwrap_or([0.8, 0.8, 0.8]);
        let alpha = m.dissolve.unwrap_or(1.0);
        let mut mat = Material {
            name: m.name.clone(),
            base_color: [base[0], base[1], base[2], alpha],
            roughness: m
                .shininess
                .map(|s| (1.0 - (s / 1000.0).clamp(0.0, 1.0)).sqrt())
                .unwrap_or(0.6),
            ..Default::default()
        };
        mat.base_color_tex = load_tex(&mut scene, &m.diffuse_texture);
        mat.normal_tex = load_tex(&mut scene, &m.normal_texture);
        scene.materials.push(mat);
    }
    if scene.materials.is_empty() {
        scene.materials.push(Material::default());
    }

    let mut merged = Mesh::default();
    for model in &models {
        let m = &model.mesh;
        let n = m.positions.len() / 3;
        let mut part = Mesh {
            positions: (0..n)
                .map(|i| Vec3::new(m.positions[i * 3], m.positions[i * 3 + 1], m.positions[i * 3 + 2]))
                .collect(),
            ..Default::default()
        };
        if m.normals.len() == n * 3 {
            part.normals = (0..n)
                .map(|i| Vec3::new(m.normals[i * 3], m.normals[i * 3 + 1], m.normals[i * 3 + 2]))
                .collect();
        }
        if m.texcoords.len() == n * 2 {
            // OBJ uses v-up; convert to glTF convention (v down) which we use internally.
            part.uvs = (0..n)
                .map(|i| Vec2::new(m.texcoords[i * 2], 1.0 - m.texcoords[i * 2 + 1]))
                .collect();
        }
        if m.vertex_color.len() == n * 3 {
            part.colors = (0..n)
                .map(|i| [m.vertex_color[i * 3], m.vertex_color[i * 3 + 1], m.vertex_color[i * 3 + 2], 1.0])
                .collect();
        }
        part.indices = m.indices.clone();
        let mat_id = m.material_id.map(|i| i as u32).unwrap_or(0);
        merged.append(&part, mat_id);
    }
    scene.mesh = merged;
    Ok(scene)
}
