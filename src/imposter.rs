//! Imposter / billboard generation: renders a mesh from a grid of directions on an octahedral
//! (hemi-)sphere into an atlas of albedo, world-space normal and depth frames with a small
//! software rasteriser, and builds a classic 3-card (X + horizontal) billboard mesh whose UVs
//! point at the best matching frames so the result works in any engine without a custom shader.
//!
//! # Octahedral convention
//!
//! The atlas is a `frames × frames` grid. Frame `(i, j)` (column `i`, row `j`, rows counted from
//! the *top* of the image, i.e. glTF `v`) has index `j * frames + i` and the grid coordinate
//! `u = i / (frames - 1)`, `v = j / (frames - 1)` (a single frame uses `u = v = 0.5`). The grid
//! therefore includes the boundary of the octahedral square so a shader can bilinearly blend
//! neighbouring frames without wrapping. [`octahedral_dir`] turns `(u, v)` into the *view
//! direction* (unit vector from the mesh pivot towards the camera) and [`octahedral_uv`] is its
//! inverse. Y is up.
//!
//! * `hemisphere = true` (hemi-octahedral): only directions with `y >= 0`. `(0.5, 0.5)` is the
//!   top view (+Y), the four corners are the horizon views `(0,0) = -X`, `(1,0) = +Z`,
//!   `(1,1) = +X`, `(0,1) = -Z`, and every edge is on the horizon.
//! * `hemisphere = false` (full octahedral): `(0.5, 0.5)` is +Y, the edge midpoints are the horizon
//!   views `(1, 0.5) = +X`, `(0.5, 1) = +Z`, `(0, 0.5) = -X`, `(0.5, 0) = -Z` and all four corners
//!   fold to the bottom view (-Y).
//!
//! Every frame is an orthographic render whose camera sits on the view direction, looks at the
//! pivot (centre of the bounding sphere) and is fitted so the bounding sphere exactly fills the
//! cell's inner area (the cell minus a [`GUTTER`] px border on each side). The camera's right/up
//! vectors come from [`view_basis`]. Depth is stored as `(R - dot(p - pivot, view_dir)) / (2R)`:
//! `0` at the near side of the bounding sphere, `1` at the far side. Normals are world space,
//! encoded `n * 0.5 + 0.5`. Alpha of all three maps is the coverage mask; colour and normal are
//! dilated a few pixels past the coverage so bilinear filtering never bleeds in black.

use crate::mesh::{Mesh, Texture};
use anyhow::{bail, Result};
use glam::{Vec2, Vec3, Vec4};
use image::{Rgba, RgbaImage};
use rayon::prelude::*;
use serde_json::{json, Value};

/// Transparent border, in pixels, on every side of each atlas cell.
pub const GUTTER: u32 = 2;
/// How far (in pixels) colour, normal and depth are dilated into uncovered pixels.
pub const DILATE_PX: u32 = 4;

#[derive(Clone, Debug)]
pub struct ImposterOptions {
    /// Atlas width and height in pixels, e.g. 1024.
    pub resolution: u32,
    /// Frames per axis of the octahedral grid; 4 gives 16 views.
    pub frames: u32,
    /// Only render upper-hemisphere views (props standing on the ground).
    pub hemisphere: bool,
}

impl Default for ImposterOptions {
    fn default() -> Self {
        Self { resolution: 1024, frames: 4, hemisphere: true }
    }
}

pub struct ImposterOutput {
    /// RGB = base colour, A = coverage.
    pub albedo: RgbaImage,
    /// World-space normals encoded `n * 0.5 + 0.5` (A = coverage).
    pub normal: RgbaImage,
    /// 8-bit normalised depth, 0 = nearest (A = coverage).
    pub depth: RgbaImage,
    /// Three crossed quads (two vertical, one horizontal) sized to the bounding sphere, UV-mapped to
    /// the front / side / top frames of the atlas. Quads are in `polygons`, triangles in `indices`.
    pub cards: Mesh,
    /// Atlas layout: convention, pivot, radius and per frame `{index, grid, view_dir, uv_rect}`.
    pub frames_json: Value,
}

/// Decode an octahedral grid coordinate `(u, v)` in `0..=1` into a unit view direction (Y up).
/// See the module documentation for the layout. The inverse is [`octahedral_uv`].
pub fn octahedral_dir(u: f32, v: f32, hemisphere: bool) -> Vec3 {
    let x = u * 2.0 - 1.0;
    let y = v * 2.0 - 1.0;
    let d = if hemisphere {
        // Rotate the [-1,1]² square by 45° onto the diamond |x|+|z| <= 1 of the upper octahedron.
        let ox = (x + y) * 0.5;
        let oz = (x - y) * 0.5;
        Vec3::new(ox, (1.0 - ox.abs() - oz.abs()).max(0.0), oz)
    } else {
        let h = 1.0 - x.abs() - y.abs();
        if h < 0.0 {
            // Lower hemisphere: the corners of the square fold inwards.
            let ox = (1.0 - y.abs()) * sign(x);
            let oz = (1.0 - x.abs()) * sign(y);
            Vec3::new(ox, h, oz)
        } else {
            Vec3::new(x, h, y)
        }
    };
    let n = d.normalize_or_zero();
    if n == Vec3::ZERO { Vec3::Y } else { n }
}

/// Inverse of [`octahedral_dir`]: map a direction to its octahedral grid coordinate in `0..=1`.
/// With `hemisphere = true` a direction below the horizon is mirrored to its upper counterpart.
pub fn octahedral_uv(dir: Vec3, hemisphere: bool) -> Vec2 {
    let d = dir.normalize_or_zero();
    let t = d.x.abs() + d.y.abs() + d.z.abs();
    if t <= 0.0 {
        return Vec2::splat(0.5);
    }
    let ox = d.x / t;
    let oz = d.z / t;
    if hemisphere {
        Vec2::new((ox + oz) * 0.5 + 0.5, (ox - oz) * 0.5 + 0.5)
    } else {
        let (px, pz) = if d.y < 0.0 {
            ((1.0 - oz.abs()) * sign(ox), (1.0 - ox.abs()) * sign(oz))
        } else {
            (ox, oz)
        };
        Vec2::new(px * 0.5 + 0.5, pz * 0.5 + 0.5)
    }
}

/// Grid coordinate of frame `(i, j)`; the grid includes the square's boundary.
pub fn frame_uv(i: u32, j: u32, frames: u32) -> Vec2 {
    if frames <= 1 {
        Vec2::splat(0.5)
    } else {
        let n = (frames - 1) as f32;
        Vec2::new(i as f32 / n, j as f32 / n)
    }
}

/// Orthonormal `(right, up)` of the orthographic camera looking along `-view_dir`. World Y is the
/// up hint; for the exact top (bottom) view the hint is -Z (+Z) so the front of the model is at
/// the bottom of the frame.
pub fn view_basis(view_dir: Vec3) -> (Vec3, Vec3) {
    let d = view_dir.normalize_or_zero();
    let hint = if d.y.abs() > 0.999 {
        if d.y > 0.0 { -Vec3::Z } else { Vec3::Z }
    } else {
        Vec3::Y
    };
    let right = hint.cross(d).normalize_or_zero();
    let up = d.cross(right).normalize_or_zero();
    (right, up)
}

#[inline]
fn sign(x: f32) -> f32 {
    if x < 0.0 { -1.0 } else { 1.0 }
}

/// Per-cell render target; `covered` is the true coverage, `filled` additionally includes dilation.
struct FrameBuf {
    color: Vec<Vec3>,
    normal: Vec<Vec3>,
    depth: Vec<f32>,
    covered: Vec<bool>,
    filled: Vec<bool>,
}

enum ColorSource<'a> {
    Texture(&'a Texture),
    Vertex,
    Flat,
}

fn render_frame(
    mesh: &Mesh,
    source: &ColorSource,
    base_color: Vec4,
    pivot: Vec3,
    radius: f32,
    view_dir: Vec3,
    cell: u32,
    gutter: u32,
) -> FrameBuf {
    let n_px = (cell * cell) as usize;
    let mut fb = FrameBuf {
        color: vec![Vec3::ZERO; n_px],
        normal: vec![Vec3::ZERO; n_px],
        depth: vec![f32::INFINITY; n_px],
        covered: vec![false; n_px],
        filled: vec![false; n_px],
    };
    let (right, up) = view_basis(view_dir);
    let inner = (cell - 2 * gutter) as f32;
    let g = gutter as f32;
    let inv_r = 1.0 / radius;
    let inv_2r = 0.5 * inv_r;

    // Project every vertex once: pixel position inside the cell and normalised depth.
    let proj: Vec<(Vec2, f32)> = mesh
        .positions
        .iter()
        .map(|p| {
            let rel = *p - pivot;
            let sx = rel.dot(right) * inv_r;
            let sy = rel.dot(up) * inv_r;
            let px = Vec2::new((sx * 0.5 + 0.5) * inner + g, (0.5 - sy * 0.5) * inner + g);
            let depth = (radius - rel.dot(view_dir)) * inv_2r;
            (px, depth)
        })
        .collect();

    let base_rgb = base_color.truncate();
    let cw = cell as i64;
    for t in 0..mesh.triangle_count() {
        let [ia, ib, ic] = mesh.tri(t);
        let (a, b, c) = (ia as usize, ib as usize, ic as usize);
        let (pa, da) = proj[a];
        let (pb, db) = proj[b];
        let (pc, dc) = proj[c];
        // No back-face culling: AI meshes can have flipped faces.
        let area = (pb - pa).perp_dot(pc - pa);
        if area.abs() < 1e-12 || !area.is_finite() {
            continue;
        }
        let inv_area = 1.0 / area;
        let min = pa.min(pb).min(pc);
        let max = pa.max(pb).max(pc);
        let x0 = (min.x.floor() as i64).clamp(0, cw - 1);
        let y0 = (min.y.floor() as i64).clamp(0, cw - 1);
        let x1 = (max.x.ceil() as i64).clamp(0, cw - 1);
        let y1 = (max.y.ceil() as i64).clamp(0, cw - 1);
        if max.x < 0.0 || max.y < 0.0 || min.x > cell as f32 || min.y > cell as f32 {
            continue;
        }
        let face_n = mesh.face_normal(t);
        let (na, nb, nc) = if mesh.has_normals() {
            (mesh.normals[a], mesh.normals[b], mesh.normals[c])
        } else {
            (face_n, face_n, face_n)
        };
        for y in y0..=y1 {
            for x in x0..=x1 {
                let p = Vec2::new(x as f32 + 0.5, y as f32 + 0.5);
                let w0 = (pb - p).perp_dot(pc - p) * inv_area;
                let w1 = (pc - p).perp_dot(pa - p) * inv_area;
                let w2 = 1.0 - w0 - w1;
                if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                    continue;
                }
                let depth = w0 * da + w1 * db + w2 * dc;
                let i = (y * cw + x) as usize;
                if depth >= fb.depth[i] {
                    continue;
                }
                let mut n = (na * w0 + nb * w1 + nc * w2).normalize_or_zero();
                if n == Vec3::ZERO {
                    n = face_n;
                }
                // Make the normal face the camera so flipped / back faces still light correctly.
                if n.dot(view_dir) < 0.0 {
                    n = -n;
                }
                let rgb = match source {
                    ColorSource::Texture(tex) => {
                        let uv = mesh.uvs[a] * w0 + mesh.uvs[b] * w1 + mesh.uvs[c] * w2;
                        let s = tex.sample(uv);
                        Vec3::new(s[0], s[1], s[2]) * base_rgb
                    }
                    ColorSource::Vertex => {
                        let ca = Vec4::from(mesh.colors[a]);
                        let cb = Vec4::from(mesh.colors[b]);
                        let cc = Vec4::from(mesh.colors[c]);
                        (ca * w0 + cb * w1 + cc * w2).truncate() * base_rgb
                    }
                    ColorSource::Flat => base_rgb,
                };
                fb.depth[i] = depth;
                fb.normal[i] = n;
                fb.color[i] = rgb;
                fb.covered[i] = true;
            }
        }
    }
    fb.filled.copy_from_slice(&fb.covered);
    dilate(&mut fb, cell as usize, DILATE_PX);
    fb
}

/// Grow colour, normal and depth `passes` pixels into unfilled pixels (8-neighbour average).
fn dilate(fb: &mut FrameBuf, w: usize, passes: u32) {
    let h = w;
    let mut next_color = fb.color.clone();
    let mut next_normal = fb.normal.clone();
    let mut next_depth = fb.depth.clone();
    let mut next_filled = fb.filled.clone();
    for _ in 0..passes {
        let mut changed = false;
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                if fb.filled[i] {
                    continue;
                }
                let mut sc = Vec3::ZERO;
                let mut sn = Vec3::ZERO;
                let mut sd = 0.0f32;
                let mut n = 0u32;
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
                        if fb.filled[j] {
                            sc += fb.color[j];
                            sn += fb.normal[j];
                            sd += fb.depth[j];
                            n += 1;
                        }
                    }
                }
                if n > 0 {
                    let inv = 1.0 / n as f32;
                    next_color[i] = sc * inv;
                    next_normal[i] = sn.normalize_or_zero();
                    next_depth[i] = sd * inv;
                    next_filled[i] = true;
                    changed = true;
                }
            }
        }
        fb.color.copy_from_slice(&next_color);
        fb.normal.copy_from_slice(&next_normal);
        fb.depth.copy_from_slice(&next_depth);
        fb.filled.copy_from_slice(&next_filled);
        if !changed {
            break;
        }
    }
}

#[inline]
fn to_u8(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

/// Bounding sphere: centre of the AABB and the largest distance from it to any vertex.
fn bounding_sphere(mesh: &Mesh) -> (Vec3, f32) {
    let pivot = mesh.bounds().center();
    let r2 = mesh
        .positions
        .iter()
        .map(|p| (*p - pivot).length_squared())
        .fold(0.0f32, f32::max);
    (pivot, r2.sqrt().max(1e-6))
}

/// Build one axis-aligned card quad through `pivot` facing `facing` and map it to `uv_rect`
/// (`[u0, v0, u1, v1]`, glTF convention: v down) using the same camera basis as the frame render.
fn push_card(cards: &mut Mesh, pivot: Vec3, radius: f32, facing: Vec3, uv_rect: [f32; 4]) {
    let (right, up) = view_basis(facing);
    let base = cards.positions.len() as u32;
    // Counter-clockwise seen from the facing side: top-left, bottom-left, bottom-right, top-right.
    for (sx, sy) in [(-1.0f32, 1.0f32), (-1.0, -1.0), (1.0, -1.0), (1.0, 1.0)] {
        cards.positions.push(pivot + right * (sx * radius) + up * (sy * radius));
        cards.normals.push(facing);
        let fu = sx * 0.5 + 0.5;
        let fv = 0.5 - sy * 0.5;
        cards.uvs.push(Vec2::new(
            uv_rect[0] + (uv_rect[2] - uv_rect[0]) * fu,
            uv_rect[1] + (uv_rect[3] - uv_rect[1]) * fv,
        ));
    }
    cards.polygons.push([base, base + 1, base + 2, base + 3]);
}

/// Render the imposter atlas and billboard cards for `mesh`.
///
/// Colour comes from `albedo_tex` (when the mesh has UVs and a texture is given), else from vertex
/// colours, else it is flat; in every case it is multiplied by `base_color` (glTF semantics).
pub fn generate(
    mesh: &Mesh,
    albedo_tex: Option<&Texture>,
    base_color: [f32; 4],
    opts: &ImposterOptions,
) -> Result<ImposterOutput> {
    if mesh.triangle_count() == 0 {
        bail!("imposter: mesh has no triangles");
    }
    if opts.frames == 0 {
        bail!("imposter: frames must be >= 1");
    }
    let frames = opts.frames;
    let cell = opts.resolution / frames;
    if cell < 2 * GUTTER + 4 {
        bail!(
            "imposter: resolution {} is too small for {}x{} frames (cell {} px)",
            opts.resolution,
            frames,
            frames,
            cell
        );
    }
    let source = match albedo_tex {
        Some(t) if mesh.has_uvs() => ColorSource::Texture(t),
        _ if mesh.has_colors() => ColorSource::Vertex,
        _ => ColorSource::Flat,
    };
    let base = Vec4::from(base_color);
    let (pivot, radius) = bounding_sphere(mesh);
    let total = (frames * frames) as usize;
    let dirs: Vec<Vec3> = (0..total)
        .map(|f| {
            let (i, j) = (f as u32 % frames, f as u32 / frames);
            let uv = frame_uv(i, j, frames);
            octahedral_dir(uv.x, uv.y, opts.hemisphere)
        })
        .collect();

    let bufs: Vec<FrameBuf> = (0..total)
        .into_par_iter()
        .map(|f| render_frame(mesh, &source, base, pivot, radius, dirs[f], cell, GUTTER))
        .collect();

    let res = opts.resolution;
    let mut albedo = RgbaImage::from_pixel(res, res, Rgba([0, 0, 0, 0]));
    let mut normal = RgbaImage::from_pixel(res, res, Rgba([128, 128, 128, 0]));
    let mut depth = RgbaImage::from_pixel(res, res, Rgba([255, 255, 255, 0]));
    let mut frame_list = Vec::with_capacity(total);
    let mut uv_rects = Vec::with_capacity(total);
    let fres = res as f32;
    for (f, fb) in bufs.iter().enumerate() {
        let (i, j) = (f as u32 % frames, f as u32 / frames);
        let (ox, oy) = (i * cell, j * cell);
        for y in 0..cell {
            for x in 0..cell {
                let k = (y * cell + x) as usize;
                if !fb.filled[k] {
                    continue;
                }
                let a = if fb.covered[k] { 255 } else { 0 };
                let c = fb.color[k];
                let n = fb.normal[k] * 0.5 + 0.5;
                let d = to_u8(fb.depth[k]);
                albedo.put_pixel(ox + x, oy + y, Rgba([to_u8(c.x), to_u8(c.y), to_u8(c.z), a]));
                normal.put_pixel(ox + x, oy + y, Rgba([to_u8(n.x), to_u8(n.y), to_u8(n.z), a]));
                depth.put_pixel(ox + x, oy + y, Rgba([d, d, d, a]));
            }
        }
        let rect = [
            (ox + GUTTER) as f32 / fres,
            (oy + GUTTER) as f32 / fres,
            (ox + cell - GUTTER) as f32 / fres,
            (oy + cell - GUTTER) as f32 / fres,
        ];
        uv_rects.push(rect);
        let grid = frame_uv(i, j, frames);
        let d = dirs[f];
        frame_list.push(json!({
            "index": f,
            "grid": [i, j],
            "grid_uv": [grid.x, grid.y],
            "view_dir": [d.x, d.y, d.z],
            "uv_rect": rect,
        }));
    }

    // 3-card billboard: pick the frame whose view direction best matches each card's facing.
    let best = |target: Vec3| -> usize {
        let mut bi = 0;
        let mut bd = f32::NEG_INFINITY;
        for (k, d) in dirs.iter().enumerate() {
            let dot = d.dot(target);
            if dot > bd {
                bd = dot;
                bi = k;
            }
        }
        bi
    };
    let mut cards = Mesh::default();
    let card_defs = [("front", Vec3::Z), ("side", Vec3::X), ("top", Vec3::Y)];
    let mut cards_json = Vec::new();
    for (name, facing) in card_defs {
        let f = best(facing);
        push_card(&mut cards, pivot, radius, facing, uv_rects[f]);
        cards_json.push(json!({ "name": name, "facing": [facing.x, facing.y, facing.z], "frame": f }));
    }
    cards.triangulate_polygons();

    let frames_json = json!({
        "convention": if opts.hemisphere { "hemi-octahedral" } else { "octahedral" },
        "description": "Frame (i,j) sits at atlas column i, row j (rows from the top of the image, glTF v). \
                        index = j*frames+i; grid_uv = (i,j)/(frames-1) includes the square's boundary. \
                        view_dir = octahedral_dir(grid_uv) is the unit vector from pivot towards the camera; \
                        hemisphere: oct=(x+y, x-y)/2 with (x,y)=grid_uv*2-1, dir=(oct.x, 1-|oct.x|-|oct.z|, oct.z); \
                        full: dir=(x, 1-|x|-|y|, y), corners fold to -Y. Orthographic camera at the pivot looking \
                        along -view_dir, right = cross(up_hint, view_dir), up = cross(view_dir, right), up_hint = +Y \
                        (-Z for the exact top view). The bounding sphere fills uv_rect exactly. depth = \
                        (radius - dot(p - pivot, view_dir)) / (2*radius), 0 = near. normal rgb = world n*0.5+0.5. \
                        Alpha = coverage; colour/normal/depth are dilated 4 px past it.",
        "up_axis": "Y",
        "hemisphere": opts.hemisphere,
        "frames": frames,
        "frame_count": total,
        "resolution": res,
        "cell_px": cell,
        "gutter_px": GUTTER,
        "dilate_px": DILATE_PX,
        "pivot": [pivot.x, pivot.y, pivot.z],
        "radius": radius,
        "depth": { "near": 0.0, "far": 1.0, "range": 2.0 * radius },
        "cards": cards_json,
        "frames_list": frame_list,
    });

    Ok(ImposterOutput { albedo, normal, depth, cards, frames_json })
}
