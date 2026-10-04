//! Mesh health analysis: the plain-language report shown before and after squishing.

use crate::mesh::{Bounds, Mesh, Scene};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Info,
    Warn,
    Error,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Problem {
    pub id: String,
    pub severity: Severity,
    pub title: String,
    pub detail: String,
    pub fix: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TextureInfo {
    pub kind: String,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HealthReport {
    pub triangles: usize,
    pub vertices: usize,
    pub bounds: Bounds,
    pub components: usize,
    pub largest_component_fraction: f32,
    pub non_manifold_edges: usize,
    pub boundary_edges: usize,
    pub degenerate_triangles: usize,
    pub duplicate_vertices: usize,
    pub watertight: bool,
    pub has_normals: bool,
    pub has_uvs: bool,
    pub has_vertex_colors: bool,
    pub materials: usize,
    pub textures: Vec<TextureInfo>,
    pub units_guess: String,
    pub up_axis_guess: String,
    pub problems: Vec<Problem>,
}

/// Connected components over shared vertex indices. Returns (component id per vertex, count).
pub fn components(mesh: &Mesh) -> (Vec<u32>, usize) {
    let n = mesh.vertex_count();
    let mut parent: Vec<u32> = (0..n as u32).collect();
    fn find(p: &mut [u32], mut x: u32) -> u32 {
        while p[x as usize] != x {
            let gp = p[p[x as usize] as usize];
            p[x as usize] = gp;
            x = gp;
        }
        x
    }
    for t in 0..mesh.triangle_count() {
        let [a, b, c] = mesh.tri(t);
        let ra = find(&mut parent, a);
        let rb = find(&mut parent, b);
        if ra != rb {
            parent[ra as usize] = rb;
        }
        let rb = find(&mut parent, b);
        let rc = find(&mut parent, c);
        if rb != rc {
            parent[rb as usize] = rc;
        }
    }
    let mut ids = vec![u32::MAX; n];
    let mut count = 0u32;
    let mut root_to_id: HashMap<u32, u32> = HashMap::new();
    // Only count vertices that are used by a triangle.
    let mut used = vec![false; n];
    for &i in &mesh.indices {
        used[i as usize] = true;
    }
    for v in 0..n {
        if !used[v] {
            continue;
        }
        let r = find(&mut parent, v as u32);
        let id = *root_to_id.entry(r).or_insert_with(|| {
            count += 1;
            count - 1
        });
        ids[v] = id;
    }
    (ids, count as usize)
}

/// Edge statistics: (boundary edges, non-manifold edges).
pub fn edge_stats(mesh: &Mesh) -> (usize, usize) {
    let mut edges: HashMap<(u32, u32), u32> = HashMap::with_capacity(mesh.indices.len());
    for t in 0..mesh.triangle_count() {
        let tri = mesh.tri(t);
        for k in 0..3 {
            let a = tri[k];
            let b = tri[(k + 1) % 3];
            let key = if a < b { (a, b) } else { (b, a) };
            *edges.entry(key).or_insert(0) += 1;
        }
    }
    let mut boundary = 0;
    let mut non_manifold = 0;
    for (_, c) in edges {
        if c == 1 {
            boundary += 1;
        } else if c > 2 {
            non_manifold += 1;
        }
    }
    (boundary, non_manifold)
}

pub fn degenerate_count(mesh: &Mesh) -> usize {
    let mut n = 0;
    for t in 0..mesh.triangle_count() {
        let [a, b, c] = mesh.tri(t);
        if a == b || b == c || a == c {
            n += 1;
            continue;
        }
        let area2 = mesh.face_area2(t).length_squared();
        if !(area2 > 0.0) {
            n += 1;
        }
    }
    n
}

/// Count vertices that share an identical position with an earlier vertex.
pub fn duplicate_vertex_count(mesh: &Mesh) -> usize {
    let mut seen: HashMap<[u32; 3], ()> = HashMap::with_capacity(mesh.vertex_count());
    let mut dups = 0;
    for p in &mesh.positions {
        let key = [p.x.to_bits(), p.y.to_bits(), p.z.to_bits()];
        if seen.insert(key, ()).is_some() {
            dups += 1;
        }
    }
    dups
}

pub fn guess_units(bounds: &Bounds) -> &'static str {
    let d = bounds.diagonal;
    if d <= 0.0 {
        "unknown"
    } else if d < 0.05 {
        "unknown (tiny)"
    } else if d <= 20.0 {
        "meters"
    } else if d <= 2000.0 {
        "centimeters"
    } else {
        "millimeters"
    }
}

pub fn guess_up_axis(bounds: &Bounds) -> &'static str {
    let s = bounds.size;
    // Heuristic: the tallest axis of a standing object is usually up; prefer Y on ties.
    if s[2] > s[1] * 1.25 && s[2] >= s[0] {
        "Z"
    } else {
        "Y"
    }
}

fn fmt(n: usize) -> String {
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

/// Produce the full health report for a scene.
pub fn analyze(scene: &Scene) -> HealthReport {
    let mesh = &scene.mesh;
    let bounds = mesh.bounds();
    let (comp_ids, comp_count) = components(mesh);
    let mut comp_tris = vec![0usize; comp_count.max(1)];
    for t in 0..mesh.triangle_count() {
        let c = comp_ids[mesh.indices[t * 3] as usize];
        if c != u32::MAX {
            comp_tris[c as usize] += 1;
        }
    }
    let largest = comp_tris.iter().copied().max().unwrap_or(0);
    let largest_fraction = if mesh.triangle_count() > 0 {
        largest as f32 / mesh.triangle_count() as f32
    } else {
        0.0
    };
    let (boundary, non_manifold) = edge_stats(mesh);
    let degenerate = degenerate_count(mesh);
    let duplicates = duplicate_vertex_count(mesh);
    let watertight = boundary == 0 && non_manifold == 0;

    let mut textures = Vec::new();
    for m in &scene.materials {
        let mut push = |kind: &str, idx: Option<usize>| {
            if let Some(i) = idx {
                if let Some(t) = scene.textures.get(i) {
                    textures.push(TextureInfo {
                        kind: kind.into(),
                        width: t.image.width(),
                        height: t.image.height(),
                    });
                }
            }
        };
        push("base_color", m.base_color_tex);
        push("metallic_roughness", m.metallic_roughness_tex);
        push("normal", m.normal_tex);
        push("emissive", m.emissive_tex);
    }

    let mut problems = Vec::new();
    let tri = mesh.triangle_count();
    if tri > 2_000_000 {
        problems.push(Problem {
            id: "huge".into(),
            severity: Severity::Info,
            title: format!("{} triangles", fmt(tri)),
            detail: "This is a very dense mesh. Squishing will take a little longer but works fine.".into(),
            fix: "Decimated to your target budget.".into(),
        });
    }
    if comp_count > 1 {
        let floaters = comp_count - 1;
        let sev = if largest_fraction > 0.95 { Severity::Warn } else { Severity::Info };
        problems.push(Problem {
            id: "floaters".into(),
            severity: sev,
            title: format!(
                "{} disconnected piece{}",
                fmt(floaters),
                if floaters == 1 { "" } else { "s" }
            ),
            detail: if largest_fraction > 0.95 {
                "Small floating fragments that are usually generation noise.".into()
            } else {
                "The model is made of several separate shells. Tiny ones are treated as noise; large ones are kept.".into()
            },
            fix: "Tiny fragments are removed automatically during cleanup.".into(),
        });
    }
    if duplicates > 0 {
        problems.push(Problem {
            id: "duplicates".into(),
            severity: Severity::Info,
            title: format!("{} duplicate vertices", fmt(duplicates)),
            detail: "Vertices sitting on exactly the same spot (common in STL and some OBJ exports).".into(),
            fix: "Welded automatically.".into(),
        });
    }
    if degenerate > 0 {
        problems.push(Problem {
            id: "degenerate".into(),
            severity: Severity::Warn,
            title: format!("{} zero-area triangles", fmt(degenerate)),
            detail: "Triangles with no surface; they cause shading artefacts and break some importers.".into(),
            fix: "Removed automatically.".into(),
        });
    }
    if non_manifold > 0 {
        problems.push(Problem {
            id: "non_manifold".into(),
            severity: Severity::Warn,
            title: format!("{} non-manifold edges", fmt(non_manifold)),
            detail: "Edges shared by more than two triangles. Game engines tolerate this, but it hurts UV unwrapping.".into(),
            fix: "The unwrapper isolates these edges; squishing also removes most of them.".into(),
        });
    }
    if boundary > 0 && !watertight {
        let sev = if boundary as f32 / tri.max(1) as f32 > 0.02 { Severity::Warn } else { Severity::Info };
        problems.push(Problem {
            id: "open_edges".into(),
            severity: sev,
            title: format!("{} open edges", fmt(boundary)),
            detail: "The surface has holes or open borders. Fine for props; matters for printing or boolean ops.".into(),
            fix: "Borders are locked during squishing so holes do not grow.".into(),
        });
    }
    if !mesh.has_uvs() {
        problems.push(Problem {
            id: "no_uvs".into(),
            severity: Severity::Info,
            title: "No UV map".into(),
            detail: "The model has no texture coordinates, which most AI generators skip.".into(),
            fix: "A fresh UV layout is generated automatically.".into(),
        });
    }
    if mesh.has_colors() && textures.is_empty() {
        problems.push(Problem {
            id: "vertex_colors_only".into(),
            severity: Severity::Info,
            title: "Colour stored per vertex".into(),
            detail: "The colour lives on the vertices instead of a texture, so it would be lost when polygons are removed.".into(),
            fix: "Baked into an albedo texture.".into(),
        });
    }
    if !mesh.has_colors() && textures.is_empty() {
        problems.push(Problem {
            id: "no_color".into(),
            severity: Severity::Info,
            title: "No colour information".into(),
            detail: "The model has neither textures nor vertex colours.".into(),
            fix: "A neutral albedo is written so the material is still valid.".into(),
        });
    }
    let units = guess_units(&bounds);
    if units == "millimeters" || units == "centimeters" {
        problems.push(Problem {
            id: "scale".into(),
            severity: Severity::Info,
            title: format!("Model looks like it is in {units}"),
            detail: format!(
                "Bounding box diagonal is {:.1} units. Engines expect metres (Unity, Godot) or centimetres (Unreal).",
                bounds.diagonal
            ),
            fix: "Use the Scale export option if the size is wrong in your engine.".into(),
        });
    }

    HealthReport {
        triangles: tri,
        vertices: mesh.vertex_count(),
        bounds,
        components: comp_count,
        largest_component_fraction: largest_fraction,
        non_manifold_edges: non_manifold,
        boundary_edges: boundary,
        degenerate_triangles: degenerate,
        duplicate_vertices: duplicates,
        watertight,
        has_normals: mesh.has_normals(),
        has_uvs: mesh.has_uvs(),
        has_vertex_colors: mesh.has_colors(),
        materials: scene.materials.len(),
        textures,
        units_guess: units.to_string(),
        up_axis_guess: guess_up_axis(&bounds).to_string(),
        problems,
    }
}
