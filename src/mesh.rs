//! Core indexed triangle mesh and scene containers.

use glam::{Vec2, Vec3};
use serde::{Deserialize, Serialize};

/// An indexed triangle mesh with optional per-vertex attributes.
///
/// Attribute vectors are either empty (attribute absent) or exactly `positions.len()` long.
/// `material_ids` is either empty (single material 0) or `triangle_count()` long.
#[derive(Clone, Debug, Default)]
pub struct Mesh {
    pub positions: Vec<Vec3>,
    pub normals: Vec<Vec3>,
    pub uvs: Vec<Vec2>,
    pub colors: Vec<[f32; 4]>,
    pub indices: Vec<u32>,
    pub material_ids: Vec<u32>,
}

#[derive(Clone, Debug, Copy, PartialEq, Serialize, Deserialize)]
pub struct Bounds {
    pub min: [f32; 3],
    pub max: [f32; 3],
    pub size: [f32; 3],
    pub diagonal: f32,
}

impl Bounds {
    pub fn center(&self) -> Vec3 {
        (Vec3::from(self.min) + Vec3::from(self.max)) * 0.5
    }
}

impl Mesh {
    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }
    pub fn vertex_count(&self) -> usize {
        self.positions.len()
    }
    pub fn has_normals(&self) -> bool {
        !self.normals.is_empty()
    }
    pub fn has_uvs(&self) -> bool {
        !self.uvs.is_empty()
    }
    pub fn has_colors(&self) -> bool {
        !self.colors.is_empty()
    }
    #[inline]
    pub fn tri(&self, t: usize) -> [u32; 3] {
        [self.indices[t * 3], self.indices[t * 3 + 1], self.indices[t * 3 + 2]]
    }
    #[inline]
    pub fn material_of(&self, t: usize) -> u32 {
        if self.material_ids.is_empty() {
            0
        } else {
            self.material_ids[t]
        }
    }

    pub fn bounds(&self) -> Bounds {
        let mut min = Vec3::splat(f32::INFINITY);
        let mut max = Vec3::splat(f32::NEG_INFINITY);
        for p in &self.positions {
            min = min.min(*p);
            max = max.max(*p);
        }
        if self.positions.is_empty() {
            min = Vec3::ZERO;
            max = Vec3::ZERO;
        }
        let size = max - min;
        Bounds {
            min: min.into(),
            max: max.into(),
            size: size.into(),
            diagonal: size.length(),
        }
    }

    #[inline]
    pub fn face_normal(&self, t: usize) -> Vec3 {
        let [a, b, c] = self.tri(t);
        let (pa, pb, pc) = (
            self.positions[a as usize],
            self.positions[b as usize],
            self.positions[c as usize],
        );
        (pb - pa).cross(pc - pa).normalize_or_zero()
    }

    #[inline]
    pub fn face_area2(&self, t: usize) -> Vec3 {
        let [a, b, c] = self.tri(t);
        let (pa, pb, pc) = (
            self.positions[a as usize],
            self.positions[b as usize],
            self.positions[c as usize],
        );
        (pb - pa).cross(pc - pa)
    }

    /// Area-weighted smooth vertex normals.
    pub fn compute_smooth_normals(&mut self) {
        let mut normals = vec![Vec3::ZERO; self.positions.len()];
        for t in 0..self.triangle_count() {
            let n = self.face_area2(t);
            for i in self.tri(t) {
                normals[i as usize] += n;
            }
        }
        for n in &mut normals {
            *n = n.normalize_or_zero();
            if *n == Vec3::ZERO {
                *n = Vec3::Y;
            }
        }
        self.normals = normals;
    }

    /// Signed volume (positive when faces wind outward, counter-clockwise).
    pub fn signed_volume(&self) -> f64 {
        let mut v = 0.0f64;
        for t in 0..self.triangle_count() {
            let [a, b, c] = self.tri(t);
            let pa = self.positions[a as usize].as_dvec3();
            let pb = self.positions[b as usize].as_dvec3();
            let pc = self.positions[c as usize].as_dvec3();
            v += pa.dot(pb.cross(pc));
        }
        v / 6.0
    }

    pub fn surface_area(&self) -> f64 {
        (0..self.triangle_count())
            .map(|t| self.face_area2(t).length() as f64 * 0.5)
            .sum()
    }

    /// Keep only the triangles for which `keep[t]` is true, then drop unused vertices.
    pub fn retain_triangles(&mut self, keep: &[bool]) {
        let mut new_indices = Vec::with_capacity(self.indices.len());
        let mut new_mats = Vec::new();
        let has_mats = !self.material_ids.is_empty();
        for t in 0..self.triangle_count() {
            if keep[t] {
                new_indices.extend_from_slice(&self.indices[t * 3..t * 3 + 3]);
                if has_mats {
                    new_mats.push(self.material_ids[t]);
                }
            }
        }
        self.indices = new_indices;
        self.material_ids = new_mats;
        self.compact();
    }

    /// Remove vertices not referenced by any triangle and renumber.
    pub fn compact(&mut self) {
        let n = self.positions.len();
        let mut remap = vec![u32::MAX; n];
        let mut next = 0u32;
        for &i in &self.indices {
            let slot = &mut remap[i as usize];
            if *slot == u32::MAX {
                *slot = next;
                next += 1;
            }
        }
        if next as usize == n {
            return;
        }
        self.apply_vertex_remap(&remap, next as usize);
    }

    /// Apply a vertex remap (`u32::MAX` = drop) producing `new_count` vertices.
    /// When several old vertices map to one new vertex the first one wins.
    pub fn apply_vertex_remap(&mut self, remap: &[u32], new_count: usize) {
        fn gather<T: Copy + Default>(src: &[T], remap: &[u32], new_count: usize) -> Vec<T> {
            if src.is_empty() {
                return Vec::new();
            }
            let mut out = vec![T::default(); new_count];
            let mut filled = vec![false; new_count];
            for (old, &new) in remap.iter().enumerate() {
                if new != u32::MAX && !filled[new as usize] {
                    out[new as usize] = src[old];
                    filled[new as usize] = true;
                }
            }
            out
        }
        self.positions = gather(&self.positions, remap, new_count);
        self.normals = gather(&self.normals, remap, new_count);
        self.uvs = gather(&self.uvs, remap, new_count);
        self.colors = gather(&self.colors, remap, new_count);
        for i in &mut self.indices {
            *i = remap[*i as usize];
        }
    }

    /// Transform all positions (and normals by the rotation part) by a uniform scale + translation.
    pub fn scale_translate(&mut self, scale: f32, offset: Vec3) {
        for p in &mut self.positions {
            *p = *p * scale + offset;
        }
    }

    /// Append another mesh (used by importers to merge primitives).
    pub fn append(&mut self, other: &Mesh, material_offset: u32) {
        let base = self.positions.len() as u32;
        let tri_before = self.triangle_count();
        // Harmonise attribute presence: if one side lacks an attribute, fill with defaults.
        fn merge<T: Copy + Default>(a: &mut Vec<T>, b: &[T], a_len: usize, b_len: usize) {
            if a.is_empty() && b.is_empty() {
                return;
            }
            if a.is_empty() {
                a.resize(a_len, T::default());
            }
            if b.is_empty() {
                a.extend(std::iter::repeat(T::default()).take(b_len));
            } else {
                a.extend_from_slice(b);
            }
        }
        let (al, bl) = (self.positions.len(), other.positions.len());
        merge(&mut self.normals, &other.normals, al, bl);
        merge(&mut self.uvs, &other.uvs, al, bl);
        merge(&mut self.colors, &other.colors, al, bl);
        self.positions.extend_from_slice(&other.positions);
        self.indices.extend(other.indices.iter().map(|i| i + base));
        if !self.material_ids.is_empty() || !other.material_ids.is_empty() || material_offset != 0 {
            if self.material_ids.is_empty() {
                self.material_ids = vec![0; tri_before];
            }
            if other.material_ids.is_empty() {
                self.material_ids
                    .extend(std::iter::repeat(material_offset).take(other.triangle_count()));
            } else {
                self.material_ids
                    .extend(other.material_ids.iter().map(|m| m + material_offset));
            }
        }
    }

    /// Byte view of positions for meshoptimizer.
    pub fn position_bytes(&self) -> &[u8] {
        bytemuck::cast_slice(&self.positions)
    }
}

/// An RGBA8 texture.
#[derive(Clone, Debug)]
pub struct Texture {
    pub name: String,
    pub image: image::RgbaImage,
}

impl Texture {
    /// Bilinear sample with wrap-around; `uv` in glTF convention (v down).
    #[inline]
    pub fn sample(&self, uv: Vec2) -> [f32; 4] {
        let (w, h) = (self.image.width() as f32, self.image.height() as f32);
        let x = (uv.x.rem_euclid(1.0)) * w - 0.5;
        let y = (uv.y.rem_euclid(1.0)) * h - 0.5;
        let x0 = x.floor();
        let y0 = y.floor();
        let fx = x - x0;
        let fy = y - y0;
        let px = |xi: f32, yi: f32| -> [f32; 4] {
            let xi = (xi as i64).rem_euclid(w as i64) as u32;
            let yi = (yi as i64).rem_euclid(h as i64) as u32;
            let p = self.image.get_pixel(xi, yi).0;
            [
                p[0] as f32 / 255.0,
                p[1] as f32 / 255.0,
                p[2] as f32 / 255.0,
                p[3] as f32 / 255.0,
            ]
        };
        let a = px(x0, y0);
        let b = px(x0 + 1.0, y0);
        let c = px(x0, y0 + 1.0);
        let d = px(x0 + 1.0, y0 + 1.0);
        let mut out = [0.0; 4];
        for i in 0..4 {
            let top = a[i] + (b[i] - a[i]) * fx;
            let bot = c[i] + (d[i] - c[i]) * fx;
            out[i] = top + (bot - top) * fy;
        }
        out
    }
}

/// PBR metal/rough material. Texture fields index `Scene::textures`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Material {
    pub name: String,
    pub base_color: [f32; 4],
    pub base_color_tex: Option<usize>,
    pub metallic: f32,
    pub roughness: f32,
    pub metallic_roughness_tex: Option<usize>,
    pub normal_tex: Option<usize>,
    pub emissive: [f32; 3],
    pub emissive_tex: Option<usize>,
}

impl Default for Material {
    fn default() -> Self {
        Self {
            name: "Material".into(),
            base_color: [0.8, 0.8, 0.8, 1.0],
            base_color_tex: None,
            metallic: 0.0,
            roughness: 0.6,
            metallic_roughness_tex: None,
            normal_tex: None,
            emissive: [0.0; 3],
            emissive_tex: None,
        }
    }
}

/// A loaded model: one merged mesh plus its materials and textures.
#[derive(Clone, Debug, Default)]
pub struct Scene {
    pub name: String,
    pub mesh: Mesh,
    pub materials: Vec<Material>,
    pub textures: Vec<Texture>,
    pub source_format: String,
    pub source_bytes: u64,
}

impl Scene {
    pub fn material(&self, id: u32) -> Material {
        self.materials
            .get(id as usize)
            .cloned()
            .unwrap_or_default()
    }
    pub fn has_any_texture(&self) -> bool {
        !self.textures.is_empty()
    }
}
