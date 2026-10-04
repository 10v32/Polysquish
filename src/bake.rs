//! CPU ray-traced texture baking from the high-poly source onto the low-poly result:
//! tangent-space normals, albedo (vertex colours or source textures), AO and ORM.

use crate::bvh::Bvh;
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
    let mut geom = TangentGeom {
        mesh,
        tangents: vec![Vec4::new(1.0, 0.0, 0.0, 1.0); mesh.vertex_count()],
    };
    if !mikktspace::generate_tangents(&mut geom) {
        log::warn!("MikkTSpace tangent generation failed; using fallback tangents");
    }
    // Fallback for any vertex mikktspace left untouched (degenerate UVs).
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
        // Conservative half-texel expansion so neighbouring triangles leave no cracks.
        let eps = 0.75 / (ub - ua).length().max((uc - ub).length()).max((ua - uc).length()).max(1e-6);
        for y in y0..=y1 {
            for x in x0..=x1 {
                let p = Vec2::new(x as f32 + 0.5, y as f32 + 0.5);
                let w0 = (ub - p).perp_dot(uc - p) * inv_area;
                let w1 = (uc - p).perp_dot(ua - p) * inv_area;
                let w2 = (ua - p).perp_dot(ub - p) * inv_area;
                if w0 >= -eps && w1 >= -eps && w2 >= -eps {
                    let s = &mut buf[y * w + x];
                    // Prefer samples strictly inside a triangle over expanded-edge samples.
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

struct HighHit {
    normal: Vec3,
    albedo: [f32; 4],
    metallic: f32,
    roughness: f32,
    point: Vec3,
}

struct HighSampler<'a> {
    scene: &'a Scene,
}

impl<'a> HighSampler<'a> {
    fn sample(&self, hit: &crate::bvh::Hit, ray_dir: Vec3) -> HighHit {
        let m = &self.scene.mesh;
        let [a, b, c] = m.tri(hit.tri as usize);
        let (ia, ib, ic) = (a as usize, b as usize, c as usize);
        let w0 = 1.0 - hit.u - hit.v;
        let (w1, w2) = (hit.u, hit.v);
        let point = m.positions[ia] * w0 + m.positions[ib] * w1 + m.positions[ic] * w2;
        let face_n = m.face_normal(hit.tri as usize);
        let mut normal = if m.has_normals() {
            (m.normals[ia] * w0 + m.normals[ib] * w1 + m.normals[ic] * w2).normalize_or_zero()
        } else {
            face_n
        };
        if normal == Vec3::ZERO {
            normal = face_n;
        }
        // Double-sided surfaces: make the normal face the ray.
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

fn to_image(buf: &[Vec4], w: usize, h: usize, srgb_like: bool) -> RgbaImage {
    let mut img = RgbaImage::new(w as u32, h as u32);
    for y in 0..h {
        for x in 0..w {
            let v = buf[y * w + x];
            let f = |c: f32| (c.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            let a = if srgb_like { f(v.w) } else { 255 };
            img.put_pixel(x as u32, y as u32, Rgba([f(v.x), f(v.y), f(v.z), a]));
        }
    }
    img
}

/// Bake textures. `ray_distance` is absolute (world units).
pub fn bake(high: &Scene, low: &Mesh, opts: &BakeOptions, ray_distance: f32, progress: &Progress) -> Result<BakeOutput> {
    if !low.has_uvs() {
        return Err(anyhow!("low-poly mesh has no UVs to bake into"));
    }
    let res = opts.resolution.clamp(64, 8192) as usize;
    // Keep the supersampled buffer at or below 4096² samples.
    let mut ss = opts.supersample.clamp(1, 4) as usize;
    while ss > 1 && res * ss > 4096 {
        ss -= 1;
    }
    let (w, h) = (res * ss, res * ss);
    progress.log(format!(
        "Baking {res}×{res} ({ss}× supersampling) from {} source triangles",
        high.mesh.triangle_count()
    ));
    progress.stage(Stage::Bake, 0.02);
    let bvh = Bvh::build(&high.mesh);
    progress.check()?;
    progress.stage(Stage::Bake, 0.10);

    let tangents = compute_tangents(low);
    let samples = rasterize(low, w, h);
    progress.stage(Stage::Bake, 0.15);
    progress.check()?;

    let sampler = HighSampler { scene: high };
    let diag = high.mesh.bounds().diagonal.max(1e-6);
    let ao_dist = diag * 0.08;
    let ao_samples = if opts.ao { opts.ao_samples.clamp(4, 256) as usize } else { 0 };
    let ao_per_sample = (ao_samples / (ss * ss)).max(1);
    let flip_green = opts.normal_convention == NormalConvention::DirectX;

    // Per supersample results.
    struct Px {
        normal: Vec3,
        albedo: Vec4,
        ao: f32,
        metal: f32,
        rough: f32,
        hit: bool,
    }
    let rows_done = std::sync::atomic::AtomicUsize::new(0);
    let (samples, tangents, bvh, sampler) = (&samples, &tangents, &bvh, &sampler);
    let rows_done = &rows_done;
    let progress_ref = progress;
    let cancel = progress.cancel.clone();
    let out: Vec<Option<Px>> = (0..h)
        .into_par_iter()
        .flat_map_iter(|y| {
            let done = rows_done.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if done % 64 == 0 {
                progress_ref.stage(Stage::Bake, 0.15 + 0.75 * done as f32 / h as f32);
            }
            let cancelled = cancel.is_cancelled();
            (0..w).map(move |x| {
                if cancelled {
                    return None;
                }
                let s = samples[y * w + x];
                if s.tri == 0 {
                    return None;
                }
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

                // Two-sided search along the smooth normal; prefer a front-facing hit.
                let d = ray_distance;
                let outward = bvh.intersect(p + n * d, -n, 2.0 * d).map(|hh| (hh, (hh.t - d).abs(), -n));
                let inward = bvh.intersect(p - n * d, n, 2.0 * d).map(|hh| (hh, (hh.t - d).abs(), n));
                let mut candidates: Vec<(crate::bvh::Hit, f32, Vec3)> = Vec::with_capacity(2);
                if let Some(o) = outward {
                    candidates.push(o);
                }
                if let Some(i) = inward {
                    if !candidates.iter().any(|c| c.0 == i.0) {
                        candidates.push(i);
                    }
                }
                if candidates.is_empty() {
                    return Some(Px { normal: Vec3::Z, albedo: Vec4::new(0.8, 0.8, 0.8, 1.0), ao: 1.0, metal: 0.0, rough: 0.6, hit: false });
                }
                candidates.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
                let mut chosen = None;
                for (hh, _, dir) in &candidates {
                    let fn_ = high.mesh.face_normal(hh.tri as usize);
                    if fn_.dot(n) > 0.0 {
                        chosen = Some((*hh, *dir));
                        break;
                    }
                }
                let (hit, dir) = chosen.unwrap_or((candidates[0].0, candidates[0].2));
                let hs = sampler.sample(&hit, dir);
                let mut hn = hs.normal;
                if hn.dot(n) < 0.0 {
                    hn = -hn;
                }
                let ts = Vec3::new(hn.dot(tg), hn.dot(bt), hn.dot(n)).normalize_or_zero();

                let mut ao = 1.0;
                if ao_per_sample > 0 && ao_samples > 0 {
                    let mut occl = 0usize;
                    let origin = hs.point + hn * (diag * 1e-4);
                    let base_seed = hash_u32((x as u32).wrapping_mul(73856093) ^ (y as u32).wrapping_mul(19349663));
                    for k in 0..ao_per_sample {
                        let u1 = rand01(base_seed.wrapping_add(k as u32 * 2 + 1));
                        let u2 = rand01(base_seed.wrapping_add(k as u32 * 2 + 2));
                        let dirr = cosine_dir(hn, u1, u2);
                        if bvh.occluded(origin, dirr, ao_dist) {
                            occl += 1;
                        }
                    }
                    ao = 1.0 - occl as f32 / ao_per_sample as f32;
                }
                Some(Px {
                    normal: ts,
                    albedo: Vec4::from(hs.albedo),
                    ao,
                    metal: hs.metallic,
                    rough: hs.roughness,
                    hit: true,
                })
            })
        })
        .collect();
    progress.check()?;
    progress.stage(Stage::Bake, 0.92);

    // Downsample to the final resolution.
    let n_px = res * res;
    let mut albedo = vec![Vec4::ZERO; n_px];
    let mut normal = vec![Vec4::ZERO; n_px];
    let mut orm = vec![Vec4::ZERO; n_px];
    let mut mask = vec![false; n_px];
    let mut covered = 0usize;
    let mut misses = 0usize;
    for y in 0..res {
        for x in 0..res {
            let mut cnt = 0;
            let mut a = Vec4::ZERO;
            let mut nn = Vec3::ZERO;
            let mut ao = 0.0;
            let mut me = 0.0;
            let mut ro = 0.0;
            for sy in 0..ss {
                for sx in 0..ss {
                    if let Some(px) = &out[(y * ss + sy) * w + (x * ss + sx)] {
                        cnt += 1;
                        a += px.albedo;
                        nn += px.normal;
                        ao += px.ao;
                        me += px.metal;
                        ro += px.rough;
                        if !px.hit {
                            misses += 1;
                        }
                    }
                }
            }
            if cnt > 0 {
                let i = y * res + x;
                let inv = 1.0 / cnt as f32;
                albedo[i] = a * inv;
                let nrm = nn.normalize_or_zero();
                let nrm = if nrm == Vec3::ZERO { Vec3::Z } else { nrm };
                let g = if flip_green { -nrm.y } else { nrm.y };
                normal[i] = Vec4::new(nrm.x * 0.5 + 0.5, g * 0.5 + 0.5, nrm.z * 0.5 + 0.5, 1.0);
                orm[i] = Vec4::new(ao * inv, ro * inv, me * inv, 1.0);
                mask[i] = true;
                covered += 1;
            }
        }
    }
    let coverage = covered as f32 / n_px as f32;
    if misses > 0 {
        progress.log(format!(
            "{} of {} texel samples found no source surface within the ray distance (filled with defaults)",
            misses,
            covered * ss * ss
        ));
    }
    // Dilation.
    let passes = opts.dilation_px.min(64);
    if passes > 0 {
        let mut m1 = mask.clone();
        dilate(&mut albedo, &mut m1, res, res, passes);
        let mut m2 = mask.clone();
        dilate(&mut normal, &mut m2, res, res, passes);
        dilate(&mut orm, &mut mask, res, res, passes);
    }
    // Background for still-empty texels.
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
        albedo: if opts.albedo { Some(to_image(&albedo, res, res, true)) } else { None },
        normal: if opts.normal_map { Some(to_image(&normal, res, res, false)) } else { None },
        ao: ao_img,
        orm: if opts.metallic_roughness || opts.ao { Some(to_image(&orm, res, res, false)) } else { None },
        coverage,
        width: res as u32,
        height: res as u32,
    })
}
