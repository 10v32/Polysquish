//! Collision shape generation: bounding box, convex hull and a simplified tri-mesh.

use crate::mesh::Mesh;
use crate::recipe::{CollisionOptions, DecimateOptions};
use glam::Vec3;

#[derive(Default)]
pub struct CollisionSet {
    pub bbox: Option<Mesh>,
    pub convex_hull: Option<Mesh>,
    pub simplified: Option<Mesh>,
}

impl CollisionSet {
    pub fn is_empty(&self) -> bool {
        self.bbox.is_none() && self.convex_hull.is_none() && self.simplified.is_none()
    }
    pub fn shapes(&self) -> Vec<(&'static str, &Mesh)> {
        let mut v = Vec::new();
        if let Some(m) = &self.convex_hull {
            v.push(("hull", m));
        }
        if let Some(m) = &self.bbox {
            v.push(("box", m));
        }
        if let Some(m) = &self.simplified {
            v.push(("simplified", m));
        }
        v
    }
}

pub fn bounding_box(mesh: &Mesh) -> Mesh {
    let b = mesh.bounds();
    let (mn, mx) = (Vec3::from(b.min), Vec3::from(b.max));
    let corners = [
        Vec3::new(mn.x, mn.y, mn.z),
        Vec3::new(mx.x, mn.y, mn.z),
        Vec3::new(mx.x, mx.y, mn.z),
        Vec3::new(mn.x, mx.y, mn.z),
        Vec3::new(mn.x, mn.y, mx.z),
        Vec3::new(mx.x, mn.y, mx.z),
        Vec3::new(mx.x, mx.y, mx.z),
        Vec3::new(mn.x, mx.y, mx.z),
    ];
    // Outward-facing (counter-clockwise) faces.
    let faces: [[u32; 3]; 12] = [
        [0, 2, 1], [0, 3, 2], // -z
        [4, 5, 6], [4, 6, 7], // +z
        [0, 1, 5], [0, 5, 4], // -y
        [3, 7, 6], [3, 6, 2], // +y
        [0, 4, 7], [0, 7, 3], // -x
        [1, 2, 6], [1, 6, 5], // +x
    ];
    let mut m = Mesh {
        positions: corners.to_vec(),
        indices: faces.iter().flatten().copied().collect(),
        ..Default::default()
    };
    m.compute_smooth_normals();
    m
}

pub fn convex_hull(mesh: &Mesh) -> Option<Mesh> {
    // Subsample very dense meshes; the hull of a few hundred thousand points is plenty.
    let step = (mesh.vertex_count() / 200_000).max(1);
    let pts: Vec<Vec3> = mesh.positions.iter().step_by(step).copied().collect();
    if pts.len() < 4 {
        return None;
    }
    let (verts, faces) = std::panic::catch_unwind(|| parry3d::transformation::convex_hull(&pts)).ok()?;
    if faces.is_empty() {
        return None;
    }
    let mut m = Mesh {
        positions: verts,
        indices: faces.iter().flatten().copied().collect(),
        ..Default::default()
    };
    // parry's hull winding is outward; make sure (volume positive) in case of mirror cases.
    if m.signed_volume() < 0.0 {
        for t in m.indices.chunks_mut(3) {
            t.swap(1, 2);
        }
    }
    m.compute_smooth_normals();
    Some(m)
}

pub fn simplified(mesh: &Mesh, triangles: usize) -> Option<Mesh> {
    let opts = DecimateOptions {
        target_triangles: Some(triangles.max(12)),
        target_ratio: None,
        max_error: None,
        lock_border: false,
        preserve_uvs: false,
        preserve_colors: false,
        aggressive: false,
    };
    let mut src = mesh.clone();
    src.uvs.clear();
    src.colors.clear();
    src.normals.clear();
    src.material_ids.clear();
    let (mut m, _) = crate::decimate::simplify_to(&src, opts.target_triangles.unwrap(), &opts, 1.0).ok()?;
    if m.triangle_count() > triangles * 2 {
        let sloppy = DecimateOptions { aggressive: true, ..opts.clone() };
        if let Ok((m2, _)) = crate::decimate::simplify_to(&m, triangles, &sloppy, 1.0) {
            m = m2;
        }
    }
    m.compute_smooth_normals();
    Some(m)
}

pub fn generate(lod0: &Mesh, opts: &CollisionOptions) -> CollisionSet {
    let mut set = CollisionSet::default();
    if opts.bbox {
        set.bbox = Some(bounding_box(lod0));
    }
    if opts.convex_hull {
        set.convex_hull = convex_hull(lod0);
    }
    if opts.simplified_mesh {
        set.simplified = simplified(lod0, opts.simplified_triangles);
    }
    set
}
