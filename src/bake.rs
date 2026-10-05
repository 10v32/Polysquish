//! Ray-traced texture baking from the high-poly source onto the low-poly result:
//! tangent-space normals, albedo (vertex colours or source textures), AO and ORM.
//!
//! The tracer is pluggable (`RayTracer`): the CPU BVH or a GPU implementation. Work is done
//! in row bands so memory stays bounded at any resolution.

use crate::bvh::{Bvh, Hit, Ray, RayTracer};
use crate::mesh::{Mesh, Scene};
use crate::progress::{Progress, Stage};
use crate::recipe::{BakeOptions, NormalConvention};
use anyhow::{anyhow, Result};
use glam::{Vec2, Vec3, Vec4};
use image::{Rgba, RgbaImage};
use rayon::prelude::*;

pub struct BakeOutput {
    pub albedo: Option<RgbaImage>,
    pub normal: Option<RgbaImage>,
    pub ao: Option<RgbaImage>,
    /// glTF layout: R = occlusion, G = roughness, B = metallic.
    pub orm: Option<RgbaImage>,
    pub coverage: f32,
    pub width: u32,
    pub height: u32,
    pub tracer: String,
    pub misses: usize,
}

struct TangentGeom<'a> {
    mesh: &'a Mesh,
    tangents: Vec<Vec4>,
}

impl<'a> mikktspace::Geometry for TangentGeom<'a> {
    fn num_faces(&self) -> usize {
        self.mesh.triangle_count()
    }
    fn num_vertices_of_face(&self, _face: usize) -> usize {
        3
    }
    fn position(&self, face: usize, vert: usize) -> [f32; 3] {
        self.mesh.positions[self.mesh.indices[face * 3 + vert] as usize].into()
    }
    fn normal(&self, face: usize, vert: usize) -> [f32; 3] {
        self.mesh.normals[self.mesh.indices[face * 3 + vert] as usize].into()
    }
    fn tex_coord(&self, face: usize, vert: usize) -> [f32; 2] {
        self.mesh.uvs[self.mesh.indices[face * 3 + vert] as usize].into()
    }
    fn set_tangent_encoded(&mut self, tangent: [f32; 4], face: usize, vert: usize) {
        let vi = self.mesh.indices[face * 3 + vert] as usize;
        self.tangents[vi] = Vec4::from(tangent);
    }
}

/// Per-vertex MikkTSpace tangents (xyz + handedness in w).
pub fn compute_tangents(mesh: &Mesh) -> Vec<Vec4> {
    let mut geom = TangentGeom { mesh, tangents: vec![Vec4::new(1.0, 0.0, 0.0, 1.0); mesh.vertex_count()] };
    if !mikktspace::generate_tangents(&mut geom) {
        log::warn!("MikkTSpace tangent generation failed; using fallback tangents");
    }
    for (i, t) in geom.tangents.iter_mut().enumerate() {
        let txyz = t.truncate();
        if !txyz.is_finite() || txyz.length_squared() < 1e-8 {
            let n = mesh.normals[i];
            let helper = if n.x.abs() < 0.9 { Vec3::X } else { Vec3::Y };
            let tt = helper.cross(n).normalize_or_zero();
            *t = Vec4::new(tt.x, tt.y, tt.z, 1.0);
        }
    }
    geom.tangents
}

#[derive(Clone, Copy, Default)]
struct Sample {
    /// Low-poly triangle index + 1 (0 = empty).
    tri: u32,
    b1: f32,
    b2: f32,
}

/// Rasterise the low-poly UV triangles into a sample buffer of size `w*h`.
fn rasterize(low: &Mesh, w: usize, h: usize) -> Vec<Sample> {
    let mut buf = vec![Sample::default(); w * h];
    let (fw, fh) = (w as f32, h as f32);
    for t in 0..low.triangle_count() {
        let [a, b, c] = low.tri(t);
        let ua = low.uvs[a as usize] * Vec2::new(fw, fh);
        let ub = low.uvs[b as usize] * Vec2::new(fw, fh);
        let uc = low.uvs[c as usize] * Vec2::new(fw, fh);
        let area = (ub - ua).perp_dot(uc - ua);
        if area.abs() < 1e-12 {
            continue;
        }
        let inv_area = 1.0 / area;
        let min = ua.min(ub).min(uc);
        let max = ua.max(ub).max(uc);
        let x0 = (min.x.floor() as i64).max(0) as usize;
        let y0 = (min.y.floor() as i64).max(0) as usize;
        let x1 = (max.x.ceil() as i64).min(w as i64 - 1).max(0) as usize;
        let y1 = (max.y.ceil() as i64).min(h as i64 - 1).max(0) as usize;
        if x0 > x1 || y0 > y1 {
            continue;
        }
        let eps = 0.75 / (ub - ua).length().max((uc - ub).length()).max((ua - uc).length()).max(1e-6);
        for y in y0..=y1 {
            for x in x0..=x1 {
                let p = Vec2::new(x as f32 + 0.5, y as f32 + 0.5);
                let w0 = (ub - p).perp_dot(uc - p) * inv_area;
                let w1 = (uc - p).perp_dot(ua - p) * inv_area;
                let w2 = (ua - p).perp_dot(ub - p) * inv_area;
                if w0 >= -eps && w1 >= -eps && w2 >= -eps {
                    let s = &mut buf[y * w + x];
                    let inside = w0 >= 0.0 && w1 >= 0.0 && w2 >= 0.0;
                    if s.tri == 0 || inside {
                        let sum = (w0 + w1 + w2).max(1e-9);
                        *s = Sample { tri: t as u32 + 1, b1: (w1 / sum).clamp(0.0, 1.0), b2: (w2 / sum).clamp(0.0, 1.0) };
                    }
                }
            }
        }
    }
    buf
}

#[inline]
fn hash_u32(mut x: u32) -> u32 {
    x ^= x >> 16;
    x = x.wrapping_mul(0x7feb352d);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846ca68b);
    x ^= x >> 16;
    x
}

#[inline]
fn rand01(seed: u32) -> f32 {
    (hash_u32(seed) >> 8) as f32 / (1u32 << 24) as f32
}

/// Cosine-weighted hemisphere direction around `n`.
#[inline]
fn cosine_dir(n: Vec3, u1: f32, u2: f32) -> Vec3 {
    let r = u1.sqrt();
    let phi = 2.0 * std::f32::consts::PI * u2;
    let x = r * phi.cos();
    let y = r * phi.sin();
    let z = (1.0 - u1).max(0.0).sqrt();
    let helper = if n.x.abs() < 0.9 { Vec3::X } else { Vec3::Y };
    let t = helper.cross(n).normalize_or_zero();
    let b = n.cross(t);
    (t * x + b * y + n * z).normalize_or_zero()
}

/// R2 low-discrepancy sequence point `i`, rotated per texel (Cranley–Patterson).
#[inline]
fn r2(i: usize, shift: (f32, f32)) -> (f32, f32) {
    const G: f64 = 1.324_717_957_244_746;
    let a1 = 1.0 / G;
    let a2 = 1.0 / (G * G);
    let u = ((0.5 + a1 * (i as f64 + 1.0)).fract() as f32 + shift.0).fract();
    let v = ((0.5 + a2 * (i as f64 + 1.0)).fract() as f32 + shift.1).fract();
    (u, v)
}

struct HighHit {
    normal: Vec3,
    albedo: [f32; 4],
    metallic: f32,
    roughness: f32,
    point: Vec3,
}

/// Samples attributes of the source mesh at a hit, using hard-edge aware corner normals.
pub struct HighSampler<'a> {
    pub scene: &'a Scene,
    pub corner_normals: Vec<Vec3>,
}

impl<'a> HighSampler<'a> {
    pub fn new(scene: &'a Scene, hard_edge_angle: f32) -> Self {
        let corner_normals = crate::normals::corner_normals(&scene.mesh, hard_edge_angle);
        Self { scene, corner_normals }
    }

    #[inline]
    fn normal_at(&self, hit: &Hit) -> Vec3 {
        let t = hit.tri as usize;
        let w0 = 1.0 - hit.u - hit.v;
        let n = self.corner_normals[t * 3] * w0 + self.corner_normals[t * 3 + 1] * hit.u + self.corner_normals[t * 3 + 2] * hit.v;
        let n = n.normalize_or_zero();
        if n == Vec3::ZERO {
            self.scene.mesh.face_normal(t)
        } else {
            n
        }
    }

    fn sample(&self, hit: &Hit, ray_dir: Vec3) -> HighHit {
        let m = &self.scene.mesh;
        let [a, b, c] = m.tri(hit.tri as usize);
        let (ia, ib, ic) = (a as usize, b as usize, c as usize);
        let w0 = 1.0 - hit.u - hit.v;
        let (w1, w2) = (hit.u, hit.v);
        let point = m.positions[ia] * w0 + m.positions[ib] * w1 + m.positions[ic] * w2;
        let face_n = m.face_normal(hit.tri as usize);
        let mut normal = self.normal_at(hit);
        if face_n.dot(ray_dir) > 0.0 && normal.dot(ray_dir) > 0.0 {
            normal = -normal;
        }
        let mat = self.scene.material(m.material_of(hit.tri as usize));
        let mut albedo = mat.base_color;
        if let Some(ti) = mat.base_color_tex.and_then(|i| self.scene.textures.get(i)).filter(|_| m.has_uvs()) {
            let uv = m.uvs[ia] * w0 + m.uvs[ib] * w1 + m.uvs[ic] * w2;
            let s = ti.sample(uv);
            for k in 0..4 {
                albedo[k] *= s[k];
            }
        } else if m.has_colors() {
            let col = [0, 1, 2, 3].map(|k| m.colors[ia][k] * w0 + m.colors[ib][k] * w1 + m.colors[ic][k] * w2);
            for k in 0..4 {
                albedo[k] *= col[k];
            }
        }
        let (mut metallic, mut roughness) = (mat.metallic, mat.roughness);
        if let Some(ti) = mat.metallic_roughness_tex.and_then(|i| self.scene.textures.get(i)).filter(|_| m.has_uvs()) {
            let uv = m.uvs[ia] * w0 + m.uvs[ib] * w1 + m.uvs[ic] * w2;
            let s = ti.sample(uv);
            roughness *= s[1];
            metallic *= s[2];
        }
        HighHit { normal, albedo, metallic, roughness, point }
    }
}

/// Fill uncovered texels from their covered neighbours, `passes` texels outward.
fn dilate(img: &mut [Vec4], mask: &mut [bool], w: usize, h: usize, passes: u32) {
    let mut next_img = img.to_vec();
    let mut next_mask = mask.to_vec();
    for _ in 0..passes {
        let mut changed = false;
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                if mask[i] {
                    continue;
                }
                let mut sum = Vec4::ZERO;
                let mut n = 0;
                for dy in -1i64..=1 {
                    for dx in -1i64..=1 {
                        if dx == 0 && dy == 0 {
                            continue;
                        }
                        let (xx, yy) = (x as i64 + dx, y as i64 + dy);
                        if xx < 0 || yy < 0 || xx >= w as i64 || yy >= h as i64 {
                            continue;
                        }
                        let j = yy as usize * w + xx as usize;
                        if mask[j] {
                            sum += img[j];
                            n += 1;
                        }
                    }
                }
                if n > 0 {
                    next_img[i] = sum / n as f32;
                    next_mask[i] = true;
                    changed = true;
                }
            }
        }
        img.copy_from_slice(&next_img);
        mask.copy_from_slice(&next_mask);
        if !changed {
            break;
        }
    }
}

/// Edge-aware (joint bilateral) smoothing of the AO channel, guided by the normal map and
/// the coverage mask so it never bleeds across UV islands or hard edges.
fn denoise_ao(orm: &mut [Vec4], normal: &[Vec4], mask: &[bool], w: usize, h: usize) {
    const R: i64 = 3;
    let src: Vec<f32> = orm.iter().map(|v| v.x).collect();
    let src = &src;
    let out: Vec<f32> = (0..h)
        .into_par_iter()
        .flat_map_iter(|y| {
            (0..w).map(move |x| {
                let i = y * w + x;
                if !mask[i] {
                    return src[i];
                }
                let n0 = normal[i].truncate() * 2.0 - Vec3::ONE;
                let a0 = src[i];
                let mut sum = 0.0;
                let mut wsum = 0.0;
                for dy in -R..=R {
                    for dx in -R..=R {
                        let (xx, yy) = (x as i64 + dx, y as i64 + dy);
                        if xx < 0 || yy < 0 || xx >= w as i64 || yy >= h as i64 {
                            continue;
                        }
                        let j = yy as usize * w + xx as usize;
                        if !mask[j] {
                            continue;
                        }
                        let n1 = normal[j].truncate() * 2.0 - Vec3::ONE;
                        let wn = ((n0.dot(n1) - 1.0) * 8.0).exp(); // normal similarity
                        let ws = (-((dx * dx + dy * dy) as f32) / (2.0 * 2.0 * 2.0)).exp(); // spatial
                        let wa = (-((src[j] - a0) * (src[j] - a0)) / (2.0 * 0.15 * 0.15)).exp(); // range
                        let wgt = wn * ws * wa;
                        sum += src[j] * wgt;
                        wsum += wgt;
                    }
                }
                if wsum > 0.0 {
                    sum / wsum
                } else {
                    a0
                }
            })
        })
        .collect();
    for (v, a) in orm.iter_mut().zip(out) {
        v.x = a;
    }
}

fn to_image(buf: &[Vec4], w: usize, h: usize, with_alpha: bool) -> RgbaImage {
    let mut img = RgbaImage::new(w as u32, h as u32);
    for y in 0..h {
        for x in 0..w {
            let v = buf[y * w + x];
            let f = |c: f32| (c.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            let a = if with_alpha { f(v.w) } else { 255 };
            img.put_pixel(x as u32, y as u32, Rgba([f(v.x), f(v.y), f(v.z), a]));
        }
    }
    img
}

/// Per-vertex maximum search distance: the global `ray_distance`, reduced where the model is
/// thin (another surface close behind) or where another surface sits close in front.
pub fn thickness_limited_distances(low: &Mesh, tracer: &dyn RayTracer, ray_distance: f32) -> Vec<f32> {
    let n = low.vertex_count();
    let mut rays = Vec::with_capacity(n * 2);
    let eps = ray_distance * 0.02;
    for i in 0..n {
        let p = low.positions[i];
        let nn = low.normals[i];
        rays.push(Ray { origin: p - nn * eps, dir: -nn, tmax: ray_distance * 2.5 });
        rays.push(Ray { origin: p + nn * eps, dir: nn, tmax: ray_distance * 2.5 });
    }
    let hits = tracer.closest_hits(&rays);
    (0..n)
        .map(|i| {
            let mut d = ray_distance;
            // Inward: the surface we project onto is typically within a small distance; a *second*
            // surface (the back side of a thin part) must not be reached. We cannot tell them apart
            // from one ray, so use the inward hit distance only when it is clearly a back face.
            if let Some(h) = hits[i * 2] {
                let back = h.t + eps;
                if back > ray_distance * 0.1 {
                    d = d.min(back * 0.45);
                }
            }
            if let Some(h) = hits[i * 2 + 1] {
                let front = h.t + eps;
                if front > ray_distance * 0.1 {
                    d = d.min(front * 0.45);
                }
            }
            d.max(ray_distance * 0.05)
        })
        .collect()
}

/// Bake textures. `ray_distance` is absolute (world units). `tracer` answers ray queries
/// against `high.mesh`; `sampler` carries the corner normals of the same mesh.
pub fn bake(
    high: &Scene,
    low: &Mesh,
    opts: &BakeOptions,
    ray_distance: f32,
    tracer: &dyn RayTracer,
    sampler: &HighSampler,
    progress: &Progress,
) -> Result<BakeOutput> {
    if !low.has_uvs() {
        return Err(anyhow!("low-poly mesh has no UVs to bake into"));
    }
    if !low.has_normals() {
        return Err(anyhow!("low-poly mesh has no normals"));
    }
    let res = opts.resolution.clamp(64, 8192) as usize;
    let mut ss = opts.supersample.clamp(1, 4) as usize;
    while ss > 1 && res * ss > 4096 {
        ss -= 1;
    }
    let (w, h) = (res * ss, res * ss);
    progress.log(format!(
        "Baking {res}×{res} ({ss}× supersampling, {} tracer) from {} source triangles",
        tracer.name(),
        high.mesh.triangle_count()
    ));
    progress.stage(Stage::Bake, 0.02);
    let tangents = compute_tangents(low);
    let samples = rasterize(low, w, h);
    let vdist = thickness_limited_distances(low, tracer, ray_distance);
    progress.stage(Stage::Bake, 0.08);
    progress.check()?;

    let diag = high.mesh.bounds().diagonal.max(1e-6);
    let ao_dist = diag * 0.08;
    let ao_samples = if opts.ao { opts.ao_samples.clamp(4, 256) as usize } else { 0 };
    let flip_green = opts.normal_convention == NormalConvention::DirectX;

    // Final-resolution accumulators.
    let n_px = res * res;
    let mut albedo = vec![Vec4::ZERO; n_px];
    let mut normal = vec![Vec4::ZERO; n_px];
    let mut orm = vec![Vec4::ZERO; n_px];
    let mut count = vec![0u16; n_px];
    let mut misses = 0usize;

    // Per-sample geometry (position, normal, tangent frame) is recomputed per band.
    struct Geo {
        p: Vec3,
        n: Vec3,
        t: Vec3,
        b: Vec3,
        d: f32,
    }
    let geo_of = |s: &Sample| -> Geo {
        let t = (s.tri - 1) as usize;
        let [a, b, c] = low.tri(t);
        let (ia, ib, ic) = (a as usize, b as usize, c as usize);
        let w0 = 1.0 - s.b1 - s.b2;
        let p = low.positions[ia] * w0 + low.positions[ib] * s.b1 + low.positions[ic] * s.b2;
        let n = (low.normals[ia] * w0 + low.normals[ib] * s.b1 + low.normals[ic] * s.b2).normalize_or_zero();
        let n = if n == Vec3::ZERO { low.face_normal(t) } else { n };
        let tg4 = tangents[ia] * w0 + tangents[ib] * s.b1 + tangents[ic] * s.b2;
        let mut tg = tg4.truncate();
        tg = (tg - n * n.dot(tg)).normalize_or_zero();
        if tg == Vec3::ZERO {
            let helper = if n.x.abs() < 0.9 { Vec3::X } else { Vec3::Y };
            tg = helper.cross(n).normalize_or_zero();
        }
        let sign = if tangents[ia].w < 0.0 { -1.0 } else { 1.0 };
        let bt = n.cross(tg) * sign;
        let d = vdist[ia] * w0 + vdist[ib] * s.b1 + vdist[ic] * s.b2;
        Geo { p, n, t: tg, b: bt, d }
    };

    // Band loop: `band_rows` supersampled rows per batch (~1M samples).
    let band_rows = ((1 << 20) / w).max(ss).max(1) / ss * ss;
    let mut y0 = 0usize;
    while y0 < h {
        progress.check()?;
        let y1 = (y0 + band_rows).min(h);
        progress.stage(Stage::Bake, 0.08 + 0.84 * y0 as f32 / h as f32);
        // Collect covered samples of this band.
        let idxs: Vec<usize> = (y0 * w..y1 * w).filter(|&i| samples[i].tri != 0).collect();
        if idxs.is_empty() {
            y0 = y1;
            continue;
        }
        let geos: Vec<Geo> = idxs.par_iter().map(|&i| geo_of(&samples[i])).collect();
        // Primary rays: outward-in and inward-out.
        let mut rays = Vec::with_capacity(geos.len() * 2);
        for g in &geos {
            rays.push(Ray { origin: g.p + g.n * g.d, dir: -g.n, tmax: 2.0 * g.d });
            rays.push(Ray { origin: g.p - g.n * g.d, dir: g.n, tmax: 2.0 * g.d });
        }
        let hits = tracer.closest_hits(&rays);
        // Choose a hit per sample and shade.
        struct Shaded {
            ts_normal: Vec3,
            albedo: Vec4,
            metal: f32,
            rough: f32,
            hit_point: Vec3,
            hit_normal: Vec3,
            hit: bool,
        }
        let shaded: Vec<Shaded> = geos
            .par_iter()
            .enumerate()
            .map(|(k, g)| {
                let mut cands: Vec<(Hit, f32, Vec3)> = Vec::with_capacity(2);
                if let Some(hh) = hits[k * 2] {
                    cands.push((hh, (hh.t - g.d).abs(), -g.n));
                }
                if let Some(hh) = hits[k * 2 + 1] {
                    if !cands.iter().any(|c| c.0 == hh) {
                        cands.push((hh, (hh.t - g.d).abs(), g.n));
                    }
                }
                if cands.is_empty() {
                    return Shaded { ts_normal: Vec3::Z, albedo: Vec4::new(0.8, 0.8, 0.8, 1.0), metal: 0.0, rough: 0.6, hit_point: g.p, hit_normal: g.n, hit: false };
                }
                cands.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
                let mut chosen = None;
                for (hh, _, dir) in &cands {
                    if high.mesh.face_normal(hh.tri as usize).dot(g.n) > 0.0 {
                        chosen = Some((*hh, *dir));
                        break;
                    }
                }
                let (hit, dir) = chosen.unwrap_or((cands[0].0, cands[0].2));
                let hs = sampler.sample(&hit, dir);
                let mut hn = hs.normal;
                if hn.dot(g.n) < 0.0 {
                    hn = -hn;
                }
                let ts = Vec3::new(hn.dot(g.t), hn.dot(g.b), hn.dot(g.n)).normalize_or_zero();
                Shaded { ts_normal: ts, albedo: Vec4::from(hs.albedo), metal: hs.metallic, rough: hs.roughness, hit_point: hs.point, hit_normal: hn, hit: true }
            })
            .collect();
        // Ambient occlusion: one set of rays per *final* texel (first covered subsample of it).
        let mut ao_vals: Vec<f32> = vec![1.0; shaded.len()];
        if ao_samples > 0 {
            // Pick one representative sample per final texel in this band.
            let mut rep: std::collections::HashMap<usize, usize> = std::collections::HashMap::new();
            for (k, &i) in idxs.iter().enumerate() {
                let (x, y) = (i % w, i / w);
                let key = (y / ss) * res + x / ss;
                rep.entry(key).or_insert(k);
            }
            let reps: Vec<(usize, usize)> = rep.into_iter().collect();
            let mut ao_rays = Vec::with_capacity(reps.len() * ao_samples);
            for &(key, k) in &reps {
                let s = &shaded[k];
                let origin = s.hit_point + s.hit_normal * (diag * 1e-4);
                let seed = hash_u32(key as u32 ^ 0x9e3779b9);
                let shift = (rand01(seed), rand01(seed ^ 0x51ed27));
                for j in 0..ao_samples {
                    let (u1, u2) = r2(j, shift);
                    ao_rays.push(Ray { origin, dir: cosine_dir(s.hit_normal, u1, u2), tmax: ao_dist });
                }
            }
            let occ = tracer.any_hits(&ao_rays);
            let mut per_key: std::collections::HashMap<usize, f32> = std::collections::HashMap::with_capacity(reps.len());
            for (r, &(key, _)) in reps.iter().enumerate() {
                let blocked = occ[r * ao_samples..(r + 1) * ao_samples].iter().filter(|&&b| b).count();
                per_key.insert(key, 1.0 - blocked as f32 / ao_samples as f32);
            }
            for (k, &i) in idxs.iter().enumerate() {
                let (x, y) = (i % w, i / w);
                let key = (y / ss) * res + x / ss;
                ao_vals[k] = per_key.get(&key).copied().unwrap_or(1.0);
            }
        }
        // Accumulate into final texels.
        for (k, &i) in idxs.iter().enumerate() {
            let s = &shaded[k];
            let (x, y) = (i % w, i / w);
            let fi = (y / ss) * res + x / ss;
            albedo[fi] += s.albedo;
            normal[fi] += Vec4::new(s.ts_normal.x, s.ts_normal.y, s.ts_normal.z, 0.0);
            orm[fi] += Vec4::new(ao_vals[k], s.rough, s.metal, 0.0);
            count[fi] += 1;
            if !s.hit {
                misses += 1;
            }
        }
        y0 = y1;
    }
    progress.stage(Stage::Bake, 0.93);

    // Resolve averages.
    let mut mask = vec![false; n_px];
    let mut covered = 0usize;
    for i in 0..n_px {
        if count[i] > 0 {
            let inv = 1.0 / count[i] as f32;
            albedo[i] *= inv;
            albedo[i].w = 1.0;
            let nrm = normal[i].truncate().normalize_or_zero();
            let nrm = if nrm == Vec3::ZERO { Vec3::Z } else { nrm };
            let g = if flip_green { -nrm.y } else { nrm.y };
            normal[i] = Vec4::new(nrm.x * 0.5 + 0.5, g * 0.5 + 0.5, nrm.z * 0.5 + 0.5, 1.0);
            orm[i] *= inv;
            orm[i].w = 1.0;
            mask[i] = true;
            covered += 1;
        }
    }
    let coverage = covered as f32 / n_px as f32;
    if misses > 0 {
        progress.log(format!("{misses} texel samples found no source surface within the ray distance (filled with defaults)"));
    }
    if opts.ao && opts.ao_denoise {
        denoise_ao(&mut orm, &normal, &mask, res, res);
    }
    let passes = opts.dilation_px.min(64);
    if passes > 0 {
        let mut m1 = mask.clone();
        dilate(&mut albedo, &mut m1, res, res, passes);
        let mut m2 = mask.clone();
        dilate(&mut normal, &mut m2, res, res, passes);
        dilate(&mut orm, &mut mask, res, res, passes);
    }
    for i in 0..n_px {
        if !mask[i] {
            albedo[i] = Vec4::new(0.5, 0.5, 0.5, 1.0);
            normal[i] = Vec4::new(0.5, 0.5, 1.0, 1.0);
            orm[i] = Vec4::new(1.0, 0.6, 0.0, 1.0);
        }
    }
    progress.stage(Stage::Bake, 0.98);

    let ao_img = if opts.ao {
        let mut img = RgbaImage::new(res as u32, res as u32);
        for y in 0..res {
            for x in 0..res {
                let v = (orm[y * res + x].x.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                img.put_pixel(x as u32, y as u32, Rgba([v, v, v, 255]));
            }
        }
        Some(img)
    } else {
        for px in &mut orm {
            px.x = 1.0;
        }
        None
    };
    Ok(BakeOutput {
        albedo: if opts.albedo { Some(to_image(&albedo, res, res, false)) } else { None },
        normal: if opts.normal_map { Some(to_image(&normal, res, res, false)) } else { None },
        ao: ao_img,
        orm: if opts.metallic_roughness || opts.ao { Some(to_image(&orm, res, res, false)) } else { None },
        coverage,
        width: res as u32,
        height: res as u32,
        tracer: tracer.name().to_string(),
        misses,
    })
}

/// Convenience for callers that only have the scene: builds a CPU BVH and sampler.
pub fn bake_cpu(high: &Scene, low: &Mesh, opts: &BakeOptions, ray_distance: f32, progress: &Progress) -> Result<BakeOutput> {
    let bvh = Bvh::build(&high.mesh);
    let sampler = HighSampler::new(high, opts.hard_edge_angle);
    bake(high, low, opts, ray_distance, &bvh, &sampler, progress)
}
