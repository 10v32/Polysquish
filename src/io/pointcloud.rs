//! Point clouds and Gaussian splats.
//!
//! * A PLY without faces loads as a `Scene` whose mesh has positions (plus normals/colours when
//!   present) and no indices; [`is_point_cloud`] tells such a scene apart.
//! * 3D Gaussian splat PLYs (vertex properties `f_dc_0..2`, `opacity`, `scale_0..2`,
//!   `rot_0..3`) are recognised by the PLY loader, which converts the DC spherical-harmonic
//!   term to a colour and hands opacity/scale to [`filter_splats`] so transparent splats and
//!   oversized "haze" splats are dropped.
//! * The `.splat` binary format (32 bytes per splat) is read by [`load_splat`] and written by
//!   [`save_splat`].
//! * [`reconstruct`] turns a point cloud into a closed triangle mesh. **This is an experimental
//!   occupancy-based reconstruction, not Poisson**: points are splatted into a voxel grid, the
//!   occupancy is morphologically closed (dilate, flood-fill the outside, erode back), a smooth
//!   indicator field is produced by a few Jacobi passes and the surface is extracted with the
//!   surface-nets extractor from `crate::voxel`. Point normals are not used. Expect blobby
//!   results at low resolutions and no reconstruction of features thinner than a cell.

use crate::mesh::{Mesh, Scene};
use crate::voxel::{self, Grid};
use anyhow::{bail, Context, Result};
use glam::Vec3;
use rayon::prelude::*;
use std::collections::HashMap;
use std::path::Path;

/// Zeroth spherical-harmonic basis constant used by 3DGS to store the DC colour.
pub const SH_C0: f32 = 0.282_094_79;
/// Splats with a sigmoid opacity below this are dropped.
pub const MIN_OPACITY: f32 = 0.3;
/// Splats whose largest axis exceeds this fraction of the bounding diagonal are dropped.
pub const MAX_SCALE_FRACTION: f32 = 0.05;
/// Upper resolution for [`reconstruct`] (dense grids: the field is `(res + 7)^3` floats).
pub const MAX_RECONSTRUCT_RESOLUTION: u32 = 256;

/// A scene with positions but no triangles.
pub fn is_point_cloud(scene: &Scene) -> bool {
    scene.mesh.indices.is_empty() && !scene.mesh.positions.is_empty()
}

#[inline]
pub fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

/// Convert the 3DGS DC spherical-harmonic coefficients to an opaque RGBA colour.
#[inline]
pub fn sh_dc_to_color(f_dc: [f32; 3]) -> [f32; 4] {
    let c = |v: f32| (0.5 + SH_C0 * v).clamp(0.0, 1.0);
    [c(f_dc[0]), c(f_dc[1]), c(f_dc[2]), 1.0]
}

/// Per-splat data the PLY loader collects alongside the vertices (parallel to `positions`).
#[derive(Default, Debug, Clone)]
pub struct SplatAux {
    /// Opacity after the sigmoid, in 0..1.
    pub opacity: Vec<f32>,
    /// Largest linear (post-`exp`) scale of the splat.
    pub max_scale: Vec<f32>,
}

/// Keep only the points for which `keep[i]` is true (point clouds have no indices to fix up).
pub fn retain_points(mesh: &mut Mesh, keep: &[bool]) {
    fn filter<T: Copy>(v: &mut Vec<T>, keep: &[bool]) {
        if v.is_empty() {
            return;
        }
        let mut i = 0;
        v.retain(|_| {
            let k = keep[i];
            i += 1;
            k
        });
    }
    filter(&mut mesh.positions, keep);
    filter(&mut mesh.normals, keep);
    filter(&mut mesh.uvs, keep);
    filter(&mut mesh.colors, keep);
    filter(&mut mesh.joints, keep);
    filter(&mut mesh.weights, keep);
}

/// Drop splats with opacity below [`MIN_OPACITY`] or a scale above [`MAX_SCALE_FRACTION`] of the
/// bounding diagonal (floaters and haze). Returns the number of points removed.
pub fn filter_splats(mesh: &mut Mesh, aux: &SplatAux) -> usize {
    let n = mesh.positions.len();
    if n == 0 || aux.opacity.len() != n {
        return 0;
    }
    let diag = mesh.bounds().diagonal.max(1e-9);
    let max_scale = diag * MAX_SCALE_FRACTION;
    let keep: Vec<bool> = (0..n)
        .map(|i| {
            let s = aux.max_scale.get(i).copied().unwrap_or(0.0);
            aux.opacity[i] >= MIN_OPACITY && s <= max_scale
        })
        .collect();
    let dropped = keep.iter().filter(|k| !**k).count();
    if dropped > 0 {
        retain_points(mesh, &keep);
    }
    dropped
}

// ---------------------------------------------------------------------------------------------
// .splat binary format (antimatter15 layout): per splat
//   position f32x3, scale f32x3 (linear), rgba u8x4 (a = opacity), rotation u8x4 (q*128+128)
// ---------------------------------------------------------------------------------------------

const SPLAT_STRIDE: usize = 32;

/// Load a `.splat` file as a point cloud with colours, applying the same opacity/scale filter
/// as for Gaussian-splat PLYs.
pub fn load_splat(path: &Path) -> Result<Scene> {
    let data = std::fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
    if data.len() % SPLAT_STRIDE != 0 {
        bail!(".splat file size {} is not a multiple of {SPLAT_STRIDE} bytes", data.len());
    }
    let n = data.len() / SPLAT_STRIDE;
    let mut mesh = Mesh::default();
    let mut aux = SplatAux::default();
    mesh.positions.reserve(n);
    mesh.colors.reserve(n);
    let f = |b: &[u8], o: usize| f32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
    for rec in data.chunks_exact(SPLAT_STRIDE) {
        mesh.positions.push(Vec3::new(f(rec, 0), f(rec, 4), f(rec, 8)));
        let scale = f(rec, 12).max(f(rec, 16)).max(f(rec, 20));
        let rgba = [rec[24], rec[25], rec[26], rec[27]];
        mesh.colors.push([rgba[0] as f32 / 255.0, rgba[1] as f32 / 255.0, rgba[2] as f32 / 255.0, 1.0]);
        aux.opacity.push(rgba[3] as f32 / 255.0);
        aux.max_scale.push(scale.abs());
    }
    let dropped = filter_splats(&mut mesh, &aux);
    if dropped > 0 {
        log::info!("{}: dropped {dropped} of {n} splats (transparent or oversized)", path.display());
    }
    Ok(Scene { mesh, ..Default::default() })
}

/// Write positions and colours as a `.splat` file (opaque, tiny isotropic scale, identity
/// rotation). Alpha of the colour is used as opacity when present.
pub fn save_splat(path: &Path, mesh: &Mesh) -> Result<()> {
    use std::io::Write;
    let diag = mesh.bounds().diagonal;
    let scale = if diag > 0.0 { diag * 1e-3 } else { 1e-3 };
    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
    for (i, p) in mesh.positions.iter().enumerate() {
        for v in [p.x, p.y, p.z, scale, scale, scale] {
            f.write_all(&v.to_le_bytes())?;
        }
        let c = mesh.colors.get(i).copied().unwrap_or([1.0, 1.0, 1.0, 1.0]);
        let to_u8 = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        f.write_all(&[to_u8(c[0]), to_u8(c[1]), to_u8(c[2]), to_u8(c[3])])?;
        // Quaternion (1, 0, 0, 0) encoded as q * 128 + 128, clamped.
        f.write_all(&[255, 128, 128, 128])?;
    }
    f.flush()?;
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Reconstruction
// ---------------------------------------------------------------------------------------------

/// Separable cube dilation (`max`) or erosion (`min`) of a 0/1 volume by `r` cells.
fn morph(src: &[u8], grid: &Grid, r: usize, dilate: bool) -> Vec<u8> {
    let [nx, ny, _] = grid.dims;
    let mut cur = src.to_vec();
    let mut next = vec![0u8; src.len()];
    for axis in 0..3 {
        let dim = grid.dims[axis];
        let stride = match axis {
            0 => 1,
            1 => nx,
            _ => nx * ny,
        };
        next.par_chunks_mut(nx * ny).enumerate().for_each(|(k, slab)| {
            for j in 0..ny {
                for i in 0..nx {
                    let c = [i, j, k];
                    let base = grid.index(i, j, k);
                    let lo = c[axis].saturating_sub(r);
                    let hi = (c[axis] + r).min(dim - 1);
                    let mut acc = cur[base];
                    for q in lo..=hi {
                        let idx = base + q * stride - c[axis] * stride;
                        let v = cur[idx];
                        acc = if dilate { acc.max(v) } else { acc.min(v) };
                    }
                    slab[j * nx + i] = acc;
                }
            }
        });
        std::mem::swap(&mut cur, &mut next);
    }
    cur
}

/// 1 for every empty node reachable from the grid border without entering `solid`.
fn flood_outside(solid: &[u8], grid: &Grid) -> Vec<u8> {
    let [nx, ny, nz] = grid.dims;
    let mut outside = vec![0u8; solid.len()];
    let mut stack: Vec<u32> = Vec::new();
    let seed = |idx: usize, outside: &mut Vec<u8>, stack: &mut Vec<u32>| {
        if solid[idx] == 0 && outside[idx] == 0 {
            outside[idx] = 1;
            stack.push(idx as u32);
        }
    };
    for k in 0..nz {
        for j in 0..ny {
            seed(grid.index(0, j, k), &mut outside, &mut stack);
            seed(grid.index(nx - 1, j, k), &mut outside, &mut stack);
        }
        for i in 0..nx {
            seed(grid.index(i, 0, k), &mut outside, &mut stack);
            seed(grid.index(i, ny - 1, k), &mut outside, &mut stack);
        }
    }
    for j in 0..ny {
        for i in 0..nx {
            seed(grid.index(i, j, 0), &mut outside, &mut stack);
            seed(grid.index(i, j, nz - 1), &mut outside, &mut stack);
        }
    }
    while let Some(idx) = stack.pop() {
        let idx = idx as usize;
        let [i, j, k] = grid.coords(idx);
        let mut visit = |m: usize| {
            if solid[m] == 0 && outside[m] == 0 {
                outside[m] = 1;
                stack.push(m as u32);
            }
        };
        if i > 0 {
            visit(idx - 1);
        }
        if i + 1 < nx {
            visit(idx + 1);
        }
        if j > 0 {
            visit(idx - nx);
        }
        if j + 1 < ny {
            visit(idx + nx);
        }
        if k > 0 {
            visit(idx - nx * ny);
        }
        if k + 1 < nz {
            visit(idx + nx * ny);
        }
    }
    outside
}

/// One Jacobi pass of 7-point box smoothing that never flips a node's sign.
fn jacobi_pass(f: &[f32], grid: &Grid) -> Vec<f32> {
    let [nx, ny, nz] = grid.dims;
    let mut out = vec![0f32; f.len()];
    out.par_chunks_mut(nx * ny).enumerate().for_each(|(k, slab)| {
        for j in 0..ny {
            for i in 0..nx {
                let idx = grid.index(i, j, k);
                let v = f[idx];
                if i == 0 || j == 0 || k == 0 || i + 1 == nx || j + 1 == ny || k + 1 == nz {
                    slab[j * nx + i] = v;
                    continue;
                }
                let sum = v + f[idx - 1] + f[idx + 1] + f[idx - nx] + f[idx + nx] + f[idx - nx * ny] + f[idx + nx * ny];
                let mut s = sum / 7.0;
                if (s < 0.0) != (v < 0.0) || s == 0.0 {
                    s = v.signum() * 0.05;
                }
                slab[j * nx + i] = s;
            }
        }
    });
    out
}

/// Nearest-point colour transfer through grid buckets. Vertices farther than three cells from
/// any point get white.
fn transfer_point_colors(points: &Mesh, grid: &Grid, out: &mut Mesh) {
    if !points.has_colors() {
        return;
    }
    let mut buckets: HashMap<u32, Vec<u32>> = HashMap::new();
    for (pi, p) in points.positions.iter().enumerate() {
        let [i, j, k] = grid.nearest_node(*p);
        buckets.entry(grid.index(i, j, k) as u32).or_default().push(pi as u32);
    }
    let [nx, ny, nz] = grid.dims;
    out.colors = out
        .positions
        .par_iter()
        .with_min_len(512)
        .map(|&q| {
            let [ci, cj, ck] = grid.nearest_node(q);
            let mut best: Option<(f32, u32)> = None;
            let mut found_ring = usize::MAX;
            for ring in 0..=3usize {
                if found_ring != usize::MAX && ring > found_ring + 1 {
                    break;
                }
                let r = ring as i64;
                for dz in -r..=r {
                    for dy in -r..=r {
                        for dx in -r..=r {
                            if dx.abs().max(dy.abs()).max(dz.abs()) != r {
                                continue;
                            }
                            let (x, y, z) = (ci as i64 + dx, cj as i64 + dy, ck as i64 + dz);
                            if x < 0 || y < 0 || z < 0 || x >= nx as i64 || y >= ny as i64 || z >= nz as i64 {
                                continue;
                            }
                            let Some(list) = buckets.get(&(grid.index(x as usize, y as usize, z as usize) as u32)) else {
                                continue;
                            };
                            for &pi in list {
                                let d = points.positions[pi as usize].distance_squared(q);
                                if best.map_or(true, |(bd, _)| d < bd) {
                                    best = Some((d, pi));
                                    found_ring = found_ring.min(ring);
                                }
                            }
                        }
                    }
                }
            }
            match best {
                Some((_, pi)) => points.colors[pi as usize],
                None => [1.0; 4],
            }
        })
        .collect();
}

/// Experimental occupancy-based reconstruction of a point cloud into a closed triangle mesh
/// (see the module docs; not Poisson). `resolution` is the number of cells along the longest
/// axis, clamped to `16..=MAX_RECONSTRUCT_RESOLUTION`. Colours are transferred from the nearest
/// point when the input has them; the output carries quads in `polygons` and smooth normals.
pub fn reconstruct(points: &Mesh, resolution: u32) -> Result<Mesh> {
    if points.positions.len() < 4 {
        bail!("point cloud reconstruction needs at least 4 points");
    }
    let resolution = resolution.clamp(16, MAX_RECONSTRUCT_RESOLUTION);
    let t0 = std::time::Instant::now();
    let b = points.bounds();
    if !(b.diagonal > 0.0) {
        bail!("point cloud has a degenerate bounding box");
    }
    let grid = Grid::fit(b.min.into(), b.max.into(), resolution, 3);
    let n = grid.node_count();

    // Occupancy: one node per point.
    let mut occ = vec![0u8; n];
    for p in &points.positions {
        let [i, j, k] = grid.nearest_node(*p);
        occ[grid.index(i, j, k)] = 1;
    }

    // Morphological closing with leak detection: grow the dilation radius until the shell
    // encloses something (or give up at r = 3, which still fills gaps of sheets).
    let mut radius = 1;
    let inside = loop {
        let shell = morph(&occ, &grid, radius, true);
        let outside = flood_outside(&shell, &grid);
        let enclosed_empty = shell.iter().zip(&outside).filter(|(s, o)| **s == 0 && **o == 0).count();
        let filled: Vec<u8> = outside.iter().map(|&o| 1 - o).collect();
        let eroded = morph(&filled, &grid, radius, false);
        if enclosed_empty > 0 || radius >= 3 {
            log::debug!("reconstruct: dilation radius {radius}, {enclosed_empty} enclosed empty nodes");
            break eroded;
        }
        radius += 1;
    };
    drop(occ);

    // Smooth indicator field (negative inside), scaled to world units.
    let mut f: Vec<f32> = inside.iter().map(|&v| if v == 1 { -1.0 } else { 1.0 }).collect();
    drop(inside);
    for _ in 0..3 {
        f = jacobi_pass(&f, &grid);
    }
    let h = grid.cell;
    let sample = |i: usize, j: usize, k: usize| f[grid.index(i, j, k)] * h;
    let cells = voxel::sign_change_cells(&grid, &sample);
    let mut out = voxel::surface_nets(&grid, &sample, &cells);
    if out.positions.is_empty() {
        bail!("point cloud reconstruction produced no surface");
    }
    voxel::smooth_net(&mut out, 2, 0.5, None);
    transfer_point_colors(points, &grid, &mut out);
    out.compute_smooth_normals();
    log::info!(
        "reconstruct: {} points, res {resolution}, grid {:?}, {} quads in {:.2?}",
        points.positions.len(),
        grid.dims,
        out.polygons.len(),
        t0.elapsed()
    );
    Ok(out)
}
