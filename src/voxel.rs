//! Voxel remeshing: rebuild a watertight, outward-oriented surface from an arbitrary triangle
//! soup (self-intersecting, multi-shell, inconsistently wound, open), plus the reusable naive
//! surface-nets extractor behind it (also used by `io::pointcloud::reconstruct`).
//!
//! `voxel_remesh` pipeline:
//! 1. Optional hole closing on a working copy (`fill_holes`): weld coincident vertices (so
//!    unwelded seams are not mistaken for holes), drop degenerate triangles and fan-fill every
//!    closed boundary loop from its centroid. The fan may self-intersect for non-planar loops;
//!    the sign classification below does not care.
//! 2. A `Bvh` of that copy and a node grid with `resolution` cells along the longest axis,
//!    padded by [`PAD_CELLS`] on every side.
//! 3. Narrow band: every node within [`BAND_RADIUS`] cells of the surface. Each triangle's
//!    AABB dilated by the band radius is rasterised into a bitset (conservative), then the exact
//!    distance of every marked node is taken with `Bvh::closest_point` (parallel, rayon) and
//!    nodes farther than the band radius are dropped again. Only band nodes store a distance
//!    (sorted index vector + parallel f32 vector); everything else is one bit per node.
//! 4. Sign. Non-band nodes: flood fill from the grid border through non-band nodes; reached
//!    nodes are outside, the rest inside. Two adjacent non-band nodes are both farther than
//!    the band radius from the surface so the fill can never cross it, and holes narrower than
//!    the band (~3 cells) are bridged by the band itself. Band nodes: along each of the six axis
//!    directions, walk to the first non-band node (at most [`ANCHOR_STEPS`] nodes away) and, if
//!    the straight segment to it is not blocked by the surface (`Bvh::occluded`), adopt that
//!    anchor's class; the nearest unblocked anchor wins, ties are broken by majority. Nodes
//!    without any unblocked anchor (deep inside merged bands, e.g. thin plates) fall back to
//!    crossing parity along the +x/+y/+z rays (majority of the three).
//!
//!    Why not parity everywhere: for overlapping shells (the union case) a point inside two
//!    shells sees an even number of crossings and would be classified outside. Why not the
//!    closest-point normal: it depends on consistent winding and also fails on interior sheets
//!    of overlapping shells. The flood fill + occlusion anchors are winding-independent and
//!    ignore interior sheets, because they only ask "can this node reach the outside without
//!    crossing the surface?".
//! 5. Naive surface nets ([`surface_nets`]): one vertex per sign-changing cell at the mean of
//!    its distance-interpolated edge crossings, one quad per sign-changing lattice edge joining
//!    the four cells around it, wound so normals point from inside (negative) to outside.
//! 6. `smooth_iterations` passes of light Laplacian smoothing, each followed by re-projection
//!    onto the input surface (within one cell) so detail is kept and volume preserved, then
//!    vertex colour transfer from the closest input point (barycentric).
//!
//! Known limitations: a lattice face whose four corners alternate in sign (checkerboard, only
//! seen with features thinner than a cell) yields a non-manifold edge; cells containing two
//! disjoint surface pieces get a single vertex (non-manifold vertex); holes wider than the band
//! are only closed when `close_holes` fan-fills them; near the crease of two overlapping shells
//! the re-projection may snap onto the interior sheet by up to one cell.

use crate::bvh::Bvh;
use crate::clean;
use crate::mesh::{Mesh, NO_VERTEX};
use anyhow::{bail, Result};
use glam::Vec3;
use rayon::prelude::*;
use std::collections::{HashMap, HashSet, VecDeque};

/// Options for [`voxel_remesh`].
#[derive(Clone, Copy, Debug)]
pub struct VoxelOptions {
    /// Cells along the longest axis of the bounding box, clamped to `MIN_RESOLUTION..=MAX_RESOLUTION`.
    pub resolution: u32,
    /// Laplacian smoothing passes on the extracted net (each re-projected onto the input). Default 3.
    pub smooth_iterations: u32,
    /// Weld, drop degenerates and fan-fill boundary loops before voxelising so open meshes
    /// become closed volumes. Default true. Without it, open parts leak and may vanish.
    pub close_holes: bool,
}

impl Default for VoxelOptions {
    fn default() -> Self {
        Self { resolution: 128, smooth_iterations: 3, close_holes: true }
    }
}

pub const MIN_RESOLUTION: u32 = 32;
pub const MAX_RESOLUTION: u32 = 512;
/// Half-width of the narrow band, in cells.
pub const BAND_RADIUS: f32 = 1.5;
/// Empty cells added around the bounding box on every side.
pub const PAD_CELLS: usize = 3;
/// How many nodes a band node looks along each axis direction for a classified anchor.
const ANCHOR_STEPS: usize = 4;

// ---------------------------------------------------------------------------------------------
// Grid and bitset helpers
// ---------------------------------------------------------------------------------------------

/// A regular lattice of sample nodes. Cell `(i,j,k)` is the cube whose minimum corner is node
/// `(i,j,k)`; there are `dims[a] - 1` cells along axis `a`.
#[derive(Clone, Copy, Debug)]
pub struct Grid {
    /// Nodes per axis.
    pub dims: [usize; 3],
    /// World position of node `(0,0,0)`.
    pub origin: Vec3,
    /// Edge length of a cell.
    pub cell: f32,
}

impl Grid {
    /// Grid covering `[min, max]` with `resolution` cells along the longest axis and `pad`
    /// extra cells on every side.
    pub fn fit(min: Vec3, max: Vec3, resolution: u32, pad: usize) -> Grid {
        let extent = (max - min).max(Vec3::ZERO);
        let longest = extent.max_element().max(1e-9);
        let cell = longest / resolution.max(1) as f32;
        let mut dims = [0usize; 3];
        for a in 0..3 {
            let cells = (extent[a] / cell).ceil().max(0.0) as usize;
            dims[a] = cells + 2 * pad + 1;
        }
        Grid { dims, origin: min - Vec3::splat(pad as f32 * cell), cell }
    }

    #[inline]
    pub fn index(&self, i: usize, j: usize, k: usize) -> usize {
        (k * self.dims[1] + j) * self.dims[0] + i
    }

    #[inline]
    pub fn coords(&self, idx: usize) -> [usize; 3] {
        let i = idx % self.dims[0];
        let r = idx / self.dims[0];
        [i, r % self.dims[1], r / self.dims[1]]
    }

    #[inline]
    pub fn position(&self, i: usize, j: usize, k: usize) -> Vec3 {
        self.origin + Vec3::new(i as f32, j as f32, k as f32) * self.cell
    }

    pub fn node_count(&self) -> usize {
        self.dims[0] * self.dims[1] * self.dims[2]
    }

    /// Node nearest to `p`, clamped into the grid.
    #[inline]
    pub fn nearest_node(&self, p: Vec3) -> [usize; 3] {
        let q = ((p - self.origin) / self.cell).round();
        let mut out = [0usize; 3];
        for a in 0..3 {
            out[a] = (q[a].max(0.0) as usize).min(self.dims[a] - 1);
        }
        out
    }
}

/// A dense bitset over grid nodes.
#[derive(Clone)]
pub struct Bits {
    words: Vec<u64>,
    len: usize,
}

impl Bits {
    pub fn new(len: usize) -> Bits {
        Bits { words: vec![0; (len + 63) / 64], len }
    }
    #[inline]
    pub fn get(&self, i: usize) -> bool {
        (self.words[i >> 6] >> (i & 63)) & 1 == 1
    }
    #[inline]
    pub fn set(&mut self, i: usize) {
        self.words[i >> 6] |= 1u64 << (i & 63);
    }
    #[inline]
    pub fn clear(&mut self, i: usize) {
        self.words[i >> 6] &= !(1u64 << (i & 63));
    }
    /// Set every bit in `start..=end`.
    pub fn set_range(&mut self, start: usize, end: usize) {
        if start > end {
            return;
        }
        let (w0, w1) = (start >> 6, end >> 6);
        let lo_mask = u64::MAX << (start & 63);
        let hi_mask = u64::MAX >> (63 - (end & 63));
        if w0 == w1 {
            self.words[w0] |= lo_mask & hi_mask;
        } else {
            self.words[w0] |= lo_mask;
            for w in w0 + 1..w1 {
                self.words[w] = u64::MAX;
            }
            self.words[w1] |= hi_mask;
        }
    }
    pub fn count_ones(&self) -> usize {
        self.words.iter().map(|w| w.count_ones() as usize).sum()
    }
    /// Indices of all set bits, ascending.
    pub fn ones(&self) -> Vec<u32> {
        let mut out = Vec::with_capacity(self.count_ones());
        for (wi, &w) in self.words.iter().enumerate() {
            let mut w = w;
            while w != 0 {
                let b = w.trailing_zeros() as usize;
                let i = wi * 64 + b;
                if i < self.len {
                    out.push(i as u32);
                }
                w &= w - 1;
            }
        }
        out
    }
}

// ---------------------------------------------------------------------------------------------
// Surface nets
// ---------------------------------------------------------------------------------------------

#[inline]
fn corner_offset(c: usize) -> Vec3 {
    Vec3::new((c & 1) as f32, ((c >> 1) & 1) as f32, ((c >> 2) & 1) as f32)
}

/// Mean of the distance-interpolated edge crossings of cell `c`, in world space.
fn cell_vertex<F>(grid: &Grid, sample: &F, c: usize) -> Vec3
where
    F: Fn(usize, usize, usize) -> f32,
{
    let [i, j, k] = grid.coords(c);
    let mut v = [0f32; 8];
    for (corner, val) in v.iter_mut().enumerate() {
        *val = sample(i + (corner & 1), j + ((corner >> 1) & 1), k + ((corner >> 2) & 1));
    }
    let mut sum = Vec3::ZERO;
    let mut n = 0;
    for c0 in 0..8 {
        for a in 0..3 {
            if c0 & (1 << a) != 0 {
                continue;
            }
            let c1 = c0 | (1 << a);
            let (d0, d1) = (v[c0], v[c1]);
            if (d0 < 0.0) == (d1 < 0.0) {
                continue;
            }
            let mut t = d0 / (d0 - d1);
            if !t.is_finite() {
                t = 0.5;
            }
            let mut p = corner_offset(c0);
            p[a] += t.clamp(0.0, 1.0);
            sum += p;
            n += 1;
        }
    }
    let local = if n > 0 { sum / n as f32 } else { Vec3::splat(0.5) };
    grid.origin + (Vec3::new(i as f32, j as f32, k as f32) + local) * grid.cell
}

/// Naive surface nets over a signed field sampled at grid nodes (`sample(i, j, k)`; negative =
/// inside). `cells` lists the indices (minimum-corner node index) of every cell that may contain
/// a sign change; it must be complete (see [`sign_change_cells`]) or quads will be missing.
///
/// Output: one vertex per cell, quads in `polygons` (and their triangulation in `indices`)
/// wound so that normals point towards the positive side. No normals or colours.
pub fn surface_nets<F>(grid: &Grid, sample: &F, cells: &[u32]) -> Mesh
where
    F: Fn(usize, usize, usize) -> f32 + Sync,
{
    let mut cells: Vec<u32> = cells.to_vec();
    cells.sort_unstable();
    cells.dedup();
    let positions: Vec<Vec3> = cells
        .par_iter()
        .with_min_len(1024)
        .map(|&c| cell_vertex(grid, sample, c as usize))
        .collect();
    let find = |idx: usize| -> Option<u32> { cells.binary_search(&(idx as u32)).ok().map(|p| p as u32) };
    let mut polygons: Vec<[u32; 4]> = Vec::with_capacity(cells.len() * 2);
    let mut missing = 0usize;
    for &c in &cells {
        let m = grid.coords(c as usize);
        let v0 = sample(m[0], m[1], m[2]);
        for a in 0..3 {
            let mut n = m;
            n[a] += 1;
            let v1 = sample(n[0], n[1], n[2]);
            if (v0 < 0.0) == (v1 < 0.0) {
                continue;
            }
            let b = (a + 1) % 3;
            let cc = (a + 2) % 3;
            if m[b] == 0 || m[cc] == 0 {
                continue;
            }
            let cell_at = |db: usize, dc: usize| {
                let mut q = m;
                q[b] -= db;
                q[cc] -= dc;
                grid.index(q[0], q[1], q[2])
            };
            // Counter-clockwise when viewed from +a: normal points towards +a.
            let q = [find(cell_at(1, 1)), find(cell_at(0, 1)), find(cell_at(0, 0)), find(cell_at(1, 0))];
            let (Some(q0), Some(q1), Some(q2), Some(q3)) = (q[0], q[1], q[2], q[3]) else {
                missing += 1;
                continue;
            };
            if v0 < 0.0 {
                polygons.push([q0, q1, q2, q3]);
            } else {
                polygons.push([q3, q2, q1, q0]);
            }
        }
    }
    if missing > 0 {
        log::warn!("surface nets: {missing} quads skipped because a neighbouring cell was not listed");
    }
    let mut mesh = Mesh { positions, polygons, ..Default::default() };
    mesh.triangulate_polygons();
    mesh
}

/// Dense scan for every cell whose eight corners do not share a sign. Use for dense fields;
/// sparse fields can produce the list more cheaply themselves.
pub fn sign_change_cells<F>(grid: &Grid, sample: &F) -> Vec<u32>
where
    F: Fn(usize, usize, usize) -> f32 + Sync,
{
    let [nx, ny, nz] = grid.dims;
    if nx < 2 || ny < 2 || nz < 2 {
        return Vec::new();
    }
    let mut out: Vec<u32> = (0..nz - 1)
        .into_par_iter()
        .flat_map_iter(|k| {
            let mut v = Vec::new();
            for j in 0..ny - 1 {
                for i in 0..nx - 1 {
                    let s = sample(i, j, k) < 0.0;
                    let change = (1..8).any(|c| (sample(i + (c & 1), j + ((c >> 1) & 1), k + ((c >> 2) & 1)) < 0.0) != s);
                    if change {
                        v.push(grid.index(i, j, k) as u32);
                    }
                }
            }
            v
        })
        .collect();
    out.sort_unstable();
    out
}

/// Laplacian smoothing of a net: each vertex moves `lambda` of the way to the mean of its
/// polygon-edge neighbours; with `project = Some((bvh, radius))` it is then snapped back onto
/// the closest surface point within `radius` (volume preserving, keeps detail).
pub fn smooth_net(mesh: &mut Mesh, iterations: u32, lambda: f32, project: Option<(&Bvh, f32)>) {
    let n = mesh.positions.len();
    if iterations == 0 || n == 0 {
        return;
    }
    let mut edges: Vec<(u32, u32)> = Vec::with_capacity(mesh.polygons.len() * 8);
    if mesh.has_polygons() {
        for p in &mesh.polygons {
            let k = if p[3] == NO_VERTEX { 3 } else { 4 };
            for e in 0..k {
                let (a, b) = (p[e], p[(e + 1) % k]);
                edges.push((a, b));
                edges.push((b, a));
            }
        }
    } else {
        for t in 0..mesh.triangle_count() {
            let tri = mesh.tri(t);
            for e in 0..3 {
                let (a, b) = (tri[e], tri[(e + 1) % 3]);
                edges.push((a, b));
                edges.push((b, a));
            }
        }
    }
    edges.sort_unstable();
    edges.dedup();
    let mut offsets = vec![0u32; n + 1];
    for &(a, _) in &edges {
        offsets[a as usize + 1] += 1;
    }
    for i in 0..n {
        offsets[i + 1] += offsets[i];
    }
    let neighbours: Vec<u32> = edges.iter().map(|e| e.1).collect();
    for _ in 0..iterations {
        let pos = &mesh.positions;
        let new: Vec<Vec3> = (0..n)
            .into_par_iter()
            .with_min_len(2048)
            .map(|v| {
                let (s, e) = (offsets[v] as usize, offsets[v + 1] as usize);
                if e == s {
                    return pos[v];
                }
                let mut avg = Vec3::ZERO;
                for &nb in &neighbours[s..e] {
                    avg += pos[nb as usize];
                }
                avg /= (e - s) as f32;
                let mut q = pos[v] + (avg - pos[v]) * lambda;
                if let Some((bvh, radius)) = project {
                    if let Some((_, cp)) = bvh.closest_point(q, radius) {
                        q = cp;
                    }
                }
                q
            })
            .collect();
        mesh.positions = new;
    }
}

/// Copy vertex colours from `src` (which `bvh` indexes) onto every vertex of `out`, using the
/// barycentric colour of the closest surface point. No-op when `src` has no colours.
pub fn transfer_colors(src: &Mesh, bvh: &Bvh, out: &mut Mesh) {
    if !src.has_colors() {
        return;
    }
    out.colors = out
        .positions
        .par_iter()
        .with_min_len(1024)
        .map(|&p| match bvh.closest_point(p, f32::INFINITY) {
            Some((hit, _)) => {
                let [a, b, c] = src.tri(hit.tri as usize);
                let (ca, cb, cc) = (src.colors[a as usize], src.colors[b as usize], src.colors[c as usize]);
                let w0 = 1.0 - hit.u - hit.v;
                [0, 1, 2, 3].map(|k| ca[k] * w0 + cb[k] * hit.u + cc[k] * hit.v)
            }
            None => [1.0; 4],
        })
        .collect();
}

// ---------------------------------------------------------------------------------------------
// Hole filling
// ---------------------------------------------------------------------------------------------

/// Close a mesh for voxelisation: weld coincident vertices, drop degenerate triangles and
/// fan-fill every closed boundary loop from its centroid. Returns the number of loops filled.
/// Open chains (boundary edges that do not form a loop) are left alone. The polygon list is
/// dropped.
pub fn fill_holes(mesh: &mut Mesh) -> usize {
    if mesh.triangle_count() == 0 {
        return 0;
    }
    let diag = mesh.bounds().diagonal.max(1e-9);
    clean::weld(mesh, diag * 1e-6);
    clean::remove_degenerate(mesh);
    mesh.polygons.clear();

    let key = |a: u32, b: u32| if a < b { (a, b) } else { (b, a) };
    let mut count: HashMap<(u32, u32), u32> = HashMap::with_capacity(mesh.indices.len());
    for t in 0..mesh.triangle_count() {
        let tri = mesh.tri(t);
        for e in 0..3 {
            *count.entry(key(tri[e], tri[(e + 1) % 3])).or_insert(0) += 1;
        }
    }
    let mut boundary: Vec<(u32, u32)> = count.iter().filter(|(_, &c)| c == 1).map(|(&k, _)| k).collect();
    if boundary.is_empty() {
        return 0;
    }
    boundary.sort_unstable();
    let mut adj: HashMap<u32, Vec<u32>> = HashMap::with_capacity(boundary.len());
    for &(a, b) in &boundary {
        adj.entry(a).or_default().push(b);
        adj.entry(b).or_default().push(a);
    }
    let mut used: HashSet<(u32, u32)> = HashSet::with_capacity(boundary.len());
    let has_c = mesh.has_colors();
    let has_n = mesh.has_normals();
    let has_uv = mesh.has_uvs();
    let has_skin = mesh.has_skin();
    let has_mats = !mesh.material_ids.is_empty();
    let mut filled = 0;
    for &(a, b) in &boundary {
        if used.contains(&(a, b)) {
            continue;
        }
        used.insert((a, b));
        let mut loop_v = vec![a, b];
        let mut cur = b;
        let mut closed = false;
        while loop_v.len() <= boundary.len() {
            let next = adj
                .get(&cur)
                .and_then(|ns| ns.iter().copied().find(|&x| !used.contains(&key(cur, x))));
            let Some(x) = next else { break };
            used.insert(key(cur, x));
            if x == a {
                closed = true;
                break;
            }
            loop_v.push(x);
            cur = x;
        }
        if !closed || loop_v.len() < 3 {
            continue;
        }
        let inv = 1.0 / loop_v.len() as f32;
        let centroid = loop_v.iter().map(|&v| mesh.positions[v as usize]).sum::<Vec3>() * inv;
        let ci = mesh.positions.len() as u32;
        mesh.positions.push(centroid);
        if has_c {
            let mut c = [0.0f32; 4];
            for &v in &loop_v {
                for k in 0..4 {
                    c[k] += mesh.colors[v as usize][k] * inv;
                }
            }
            mesh.colors.push(c);
        }
        if has_n {
            mesh.normals.push(Vec3::Y);
        }
        if has_uv {
            mesh.uvs.push(glam::Vec2::ZERO);
        }
        if has_skin {
            mesh.joints.push([0; 4]);
            mesh.weights.push([1.0, 0.0, 0.0, 0.0]);
        }
        for e in 0..loop_v.len() {
            let p = loop_v[e];
            let q = loop_v[(e + 1) % loop_v.len()];
            mesh.indices.extend_from_slice(&[q, p, ci]);
            if has_mats {
                mesh.material_ids.push(0);
            }
        }
        filled += 1;
    }
    filled
}

// ---------------------------------------------------------------------------------------------
// Narrow-band signed distance field
// ---------------------------------------------------------------------------------------------

/// Sparse signed distance field: one sign bit per node, exact signed distances only for the
/// nodes inside the narrow band, `±far` elsewhere.
pub struct BandField {
    pub grid: Grid,
    outside: Bits,
    band: Bits,
    /// Sorted node indices of the band.
    band_idx: Vec<u32>,
    /// Signed distance per band node (same order as `band_idx`; strictly negative inside).
    band_dist: Vec<f32>,
    far: f32,
}

impl BandField {
    /// Build the field for `src` (indexed by `bvh`) on `grid`. See the module docs.
    pub fn build(src: &Mesh, bvh: &Bvh, grid: &Grid) -> BandField {
        let [nx, ny, nz] = grid.dims;
        let n = grid.node_count();
        let h = grid.cell;
        let radius = BAND_RADIUS * h;

        // 1. Conservative rasterisation of dilated triangle AABBs.
        let mut band = Bits::new(n);
        let to_node = |p: Vec3| (p - grid.origin) / h;
        for t in 0..src.triangle_count() {
            let [a, b, c] = src.tri(t);
            let (pa, pb, pc) = (src.positions[a as usize], src.positions[b as usize], src.positions[c as usize]);
            let lo = to_node(pa.min(pb).min(pc) - radius).floor();
            let hi = to_node(pa.max(pb).max(pc) + radius).ceil();
            if !(lo.is_finite() && hi.is_finite()) {
                continue;
            }
            let clamp = |v: f32, d: usize| (v.max(0.0) as usize).min(d - 1);
            let (i0, i1) = (clamp(lo.x, nx), clamp(hi.x, nx));
            let (j0, j1) = (clamp(lo.y, ny), clamp(hi.y, ny));
            let (k0, k1) = (clamp(lo.z, nz), clamp(hi.z, nz));
            for k in k0..=k1 {
                for j in j0..=j1 {
                    band.set_range(grid.index(i0, j, k), grid.index(i1, j, k));
                }
            }
        }

        // 2. Exact distances for the marked nodes (parallel), dropping those outside the band.
        let marked = band.ones();
        let dist: Vec<f32> = marked
            .par_iter()
            .with_min_len(512)
            .map(|&i| {
                let [x, y, z] = grid.coords(i as usize);
                bvh.closest_point(grid.position(x, y, z), radius).map(|(hit, _)| hit.t).unwrap_or(f32::INFINITY)
            })
            .collect();
        let mut band_idx = Vec::with_capacity(marked.len());
        let mut band_dist = Vec::with_capacity(marked.len());
        for (&i, &d) in marked.iter().zip(&dist) {
            if d.is_finite() {
                band_idx.push(i);
                band_dist.push(d);
            } else {
                band.clear(i as usize);
            }
        }
        drop(marked);
        drop(dist);

        // 3. Flood fill the outside through non-band nodes, seeded from the six grid faces.
        let mut outside = Bits::new(n);
        let mut queue: VecDeque<u32> = VecDeque::new();
        let seed = |i: usize, j: usize, k: usize, outside: &mut Bits, queue: &mut VecDeque<u32>| {
            let idx = grid.index(i, j, k);
            if !band.get(idx) && !outside.get(idx) {
                outside.set(idx);
                queue.push_back(idx as u32);
            }
        };
        for k in 0..nz {
            for j in 0..ny {
                seed(0, j, k, &mut outside, &mut queue);
                seed(nx - 1, j, k, &mut outside, &mut queue);
            }
            for i in 0..nx {
                seed(i, 0, k, &mut outside, &mut queue);
                seed(i, ny - 1, k, &mut outside, &mut queue);
            }
        }
        for j in 0..ny {
            for i in 0..nx {
                seed(i, j, 0, &mut outside, &mut queue);
                seed(i, j, nz - 1, &mut outside, &mut queue);
            }
        }
        while let Some(idx) = queue.pop_front() {
            let idx = idx as usize;
            let [i, j, k] = grid.coords(idx);
            let mut visit = |m: usize| {
                if !band.get(m) && !outside.get(m) {
                    outside.set(m);
                    queue.push_back(m as u32);
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

        // 4. Classify band nodes (parallel), then record the result.
        let flags: Vec<bool> = band_idx
            .par_iter()
            .with_min_len(256)
            .map(|&i| classify_band_node(grid, &band, &outside, bvh, i as usize))
            .collect();
        let tiny = h * 1e-6;
        for ((&i, &out), d) in band_idx.iter().zip(&flags).zip(band_dist.iter_mut()) {
            if out {
                outside.set(i as usize);
                *d = d.max(tiny);
            } else {
                *d = -d.max(tiny);
            }
        }
        BandField { grid: *grid, outside, band, band_idx, band_dist, far: radius }
    }

    /// Signed distance at node `(i,j,k)` (`±far` outside the band).
    #[inline]
    pub fn sample(&self, i: usize, j: usize, k: usize) -> f32 {
        let idx = self.grid.index(i, j, k);
        if self.band.get(idx) {
            match self.band_idx.binary_search(&(idx as u32)) {
                Ok(p) => self.band_dist[p],
                Err(_) => self.far,
            }
        } else if self.outside.get(idx) {
            self.far
        } else {
            -self.far
        }
    }

    #[inline]
    pub fn is_outside(&self, idx: usize) -> bool {
        self.outside.get(idx)
    }

    pub fn band_len(&self) -> usize {
        self.band_idx.len()
    }

    /// Every cell with a sign change. Such a cell always has a band corner, so the list is
    /// produced from the band: each cell is emitted by its lowest-index band corner.
    pub fn sign_change_cells(&self) -> Vec<u32> {
        let g = &self.grid;
        let [nx, ny, nz] = g.dims;
        let mut cells: Vec<u32> = self
            .band_idx
            .par_iter()
            .with_min_len(1024)
            .flat_map_iter(|&n| {
                let n = n as usize;
                let [i, j, k] = g.coords(n);
                let mut v: Vec<u32> = Vec::new();
                for dz in 0..2 {
                    for dy in 0..2 {
                        for dx in 0..2 {
                            if i < dx || j < dy || k < dz {
                                continue;
                            }
                            let (ci, cj, ck) = (i - dx, j - dy, k - dz);
                            if ci + 1 >= nx || cj + 1 >= ny || ck + 1 >= nz {
                                continue;
                            }
                            let s0 = self.outside.get(g.index(ci, cj, ck));
                            let mut lowest = true;
                            let mut change = false;
                            for c in 0..8 {
                                let m = g.index(ci + (c & 1), cj + ((c >> 1) & 1), ck + ((c >> 2) & 1));
                                if m < n && self.band.get(m) {
                                    lowest = false;
                                    break;
                                }
                                if self.outside.get(m) != s0 {
                                    change = true;
                                }
                            }
                            if lowest && change {
                                v.push(g.index(ci, cj, ck) as u32);
                            }
                        }
                    }
                }
                v
            })
            .collect();
        cells.sort_unstable();
        cells
    }
}

const AXIS_DIRS: [Vec3; 3] = [Vec3::X, Vec3::Y, Vec3::Z];

/// Is band node `idx` outside? See the module docs (anchors first, parity fallback).
fn classify_band_node(grid: &Grid, band: &Bits, outside: &Bits, bvh: &Bvh, idx: usize) -> bool {
    let h = grid.cell;
    let c = grid.coords(idx);
    let p = grid.position(c[0], c[1], c[2]);
    let mut best_step = usize::MAX;
    let mut best_vote = 0i32;
    let mut votes = 0i32;
    for axis in 0..3 {
        for sign in [1i64, -1i64] {
            let dir = AXIS_DIRS[axis] * sign as f32;
            for s in 1..=ANCHOR_STEPS {
                let q = c[axis] as i64 + sign * s as i64;
                if q < 0 || q >= grid.dims[axis] as i64 {
                    break;
                }
                let mut m = c;
                m[axis] = q as usize;
                let mi = grid.index(m[0], m[1], m[2]);
                if band.get(mi) {
                    continue;
                }
                if !bvh.occluded(p, dir, s as f32 * h) {
                    let v = if outside.get(mi) { 1 } else { -1 };
                    votes += v;
                    if s < best_step {
                        best_step = s;
                        best_vote = v;
                    } else if s == best_step {
                        best_vote += v;
                    }
                }
                break;
            }
        }
    }
    if best_step != usize::MAX {
        if best_vote != 0 {
            return best_vote > 0;
        }
        if votes != 0 {
            return votes > 0;
        }
    }
    // Fallback: crossing parity along +x, +y, +z to the grid border, majority vote.
    let mut inside_votes = 0;
    for axis in 0..3 {
        let tmax = (grid.dims[axis] - c[axis]) as f32 * h;
        if count_crossings(bvh, p, AXIS_DIRS[axis], tmax, h * 1e-4) % 2 == 1 {
            inside_votes += 1;
        }
    }
    inside_votes < 2
}

/// Number of surface crossings along a ray (repeated closest-hit queries).
fn count_crossings(bvh: &Bvh, origin: Vec3, dir: Vec3, tmax: f32, eps: f32) -> u32 {
    let mut o = origin;
    let mut remaining = tmax;
    let mut n = 0u32;
    while remaining > 0.0 {
        let Some(hit) = bvh.intersect(o, dir, remaining) else { break };
        n += 1;
        let adv = hit.t + eps;
        o += dir * adv;
        remaining -= adv;
        if n > 256 {
            break;
        }
    }
    n
}

// ---------------------------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------------------------

/// Rebuild a watertight surface from an arbitrary triangle mesh. See the module docs.
pub fn voxel_remesh(mesh: &Mesh, opts: &VoxelOptions) -> Result<Mesh> {
    if mesh.triangle_count() == 0 {
        bail!("voxel remesh needs a triangle mesh (for point clouds use io::pointcloud::reconstruct)");
    }
    let resolution = opts.resolution.clamp(MIN_RESOLUTION, MAX_RESOLUTION);
    let t0 = std::time::Instant::now();
    let mut src = Mesh {
        positions: mesh.positions.clone(),
        colors: mesh.colors.clone(),
        indices: mesh.indices.clone(),
        ..Default::default()
    };
    let mut filled = 0;
    if opts.close_holes {
        filled = fill_holes(&mut src);
    }
    if src.triangle_count() == 0 {
        bail!("voxel remesh: no triangles left after cleanup");
    }
    let b = src.bounds();
    if !(b.diagonal > 0.0) {
        bail!("voxel remesh: degenerate bounding box");
    }
    let grid = Grid::fit(b.min.into(), b.max.into(), resolution, PAD_CELLS);
    let bvh = Bvh::build(&src);
    let field = BandField::build(&src, &bvh, &grid);
    let t_field = t0.elapsed();
    let cells = field.sign_change_cells();
    let mut out = surface_nets(&grid, &|i, j, k| field.sample(i, j, k), &cells);
    if out.positions.is_empty() {
        bail!("voxel remesh found no enclosed volume (open mesh? enable close_holes or raise the resolution)");
    }
    let t_nets = t0.elapsed();
    smooth_net(&mut out, opts.smooth_iterations, 0.5, Some((&bvh, grid.cell)));
    transfer_colors(&src, &bvh, &mut out);
    out.compute_smooth_normals();
    log::info!(
        "voxel remesh: res {resolution}, grid {:?}, band {} nodes, {} holes filled, {} quads; field {:.2?}, nets {:.2?}, total {:.2?}",
        grid.dims,
        field.band_len(),
        filled,
        out.polygons.len(),
        t_field,
        t_nets - t_field,
        t0.elapsed()
    );
    Ok(out)
}
