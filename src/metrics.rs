//! Quality metrics for the squished mesh: geometric deviation from the source, texel density,
//! and vertex-coloured heat-map meshes for the viewer.

use crate::bvh::Bvh;
use crate::mesh::Mesh;
use glam::Vec3;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct DeviationStats {
    /// Fractions of the bounding diagonal.
    pub mean: f32,
    pub max: f32,
    pub p95: f32,
    pub unit: String,
    pub mean_abs: f32,
    pub max_abs: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct DensityStats {
    /// Texels per world unit.
    pub mean: f32,
    pub min: f32,
    pub max: f32,
    pub unit: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct Metrics {
    pub deviation: DeviationStats,
    pub texel_density: Option<DensityStats>,
    pub uv_charts: usize,
    pub quads: usize,
    pub polygons: usize,
    pub watertight: bool,
    pub hidden_faces_removed: usize,
    pub tracer: String,
}

/// Per-vertex distance from `low` to the surface held by `bvh` (absolute units), plus stats.
pub fn deviation(low: &Mesh, bvh: &Bvh, diag: f32) -> (DeviationStats, Vec<f32>) {
    let diag = diag.max(1e-9);
    let limit = diag * 0.5;
    let per_vertex: Vec<f32> = low
        .positions
        .par_iter()
        .map(|p| bvh.closest_point(*p, limit).map(|(h, _)| h.t).unwrap_or(limit))
        .collect();
    // Also sample triangle centroids so flat-shaded errors between vertices are counted.
    let centroids: Vec<f32> = (0..low.triangle_count())
        .into_par_iter()
        .map(|t| {
            let [a, b, c] = low.tri(t);
            let p = (low.positions[a as usize] + low.positions[b as usize] + low.positions[c as usize]) / 3.0;
            bvh.closest_point(p, limit).map(|(h, _)| h.t).unwrap_or(limit)
        })
        .collect();
    let mut all: Vec<f32> = per_vertex.iter().chain(centroids.iter()).copied().collect();
    if all.is_empty() {
        return (DeviationStats::default(), per_vertex);
    }
    all.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = all.len();
    let mean = all.iter().sum::<f32>() / n as f32;
    let max = all[n - 1];
    let p95 = all[((n as f32 * 0.95) as usize).min(n - 1)];
    (
        DeviationStats {
            mean: mean / diag,
            max: max / diag,
            p95: p95 / diag,
            unit: "fraction_of_size".into(),
            mean_abs: mean,
            max_abs: max,
        },
        per_vertex,
    )
}

/// Texels per world unit for each triangle, given the atlas resolution.
pub fn texel_density(low: &Mesh, resolution: u32) -> Option<(DensityStats, Vec<f32>)> {
    if !low.has_uvs() || low.triangle_count() == 0 {
        return None;
    }
    let res = resolution as f32;
    let per_tri: Vec<f32> = (0..low.triangle_count())
        .map(|t| {
            let [a, b, c] = low.tri(t);
            let (pa, pb, pc) = (low.positions[a as usize], low.positions[b as usize], low.positions[c as usize]);
            let (ua, ub, uc) = (low.uvs[a as usize], low.uvs[b as usize], low.uvs[c as usize]);
            let world = (pb - pa).cross(pc - pa).length() * 0.5;
            let uv = ((ub - ua).perp_dot(uc - ua)).abs() * 0.5 * res * res;
            if world > 1e-12 {
                (uv / world).sqrt()
            } else {
                0.0
            }
        })
        .collect();
    let valid: Vec<f32> = per_tri.iter().copied().filter(|v| *v > 0.0 && v.is_finite()).collect();
    if valid.is_empty() {
        return None;
    }
    // Area-weighted mean.
    let mut wsum = 0.0f64;
    let mut sum = 0.0f64;
    for t in 0..low.triangle_count() {
        let [a, b, c] = low.tri(t);
        let w = (low.positions[b as usize] - low.positions[a as usize]).cross(low.positions[c as usize] - low.positions[a as usize]).length() as f64;
        if per_tri[t] > 0.0 && per_tri[t].is_finite() {
            sum += per_tri[t] as f64 * w;
            wsum += w;
        }
    }
    let mean = if wsum > 0.0 { (sum / wsum) as f32 } else { 0.0 };
    let mut sorted = valid.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let min = sorted[(sorted.len() as f32 * 0.02) as usize];
    let max = sorted[((sorted.len() as f32 * 0.98) as usize).min(sorted.len() - 1)];
    Some((DensityStats { mean, min, max, unit: "texels_per_unit".into() }, per_tri))
}

/// Fixed ramp: blue (0) → mint → amber → coral (1).
pub fn ramp(t: f32) -> [f32; 4] {
    let stops: [(f32, Vec3); 4] = [
        (0.0, Vec3::new(0.35, 0.45, 1.0)),
        (0.33, Vec3::new(0.37, 0.95, 0.76)),
        (0.66, Vec3::new(1.0, 0.77, 0.40)),
        (1.0, Vec3::new(1.0, 0.42, 0.54)),
    ];
    let t = t.clamp(0.0, 1.0);
    let mut c = stops[3].1;
    for w in stops.windows(2) {
        let (t0, c0) = w[0];
        let (t1, c1) = w[1];
        if t <= t1 {
            let k = ((t - t0) / (t1 - t0)).clamp(0.0, 1.0);
            c = c0 + (c1 - c0) * k;
            break;
        }
    }
    [c.x, c.y, c.z, 1.0]
}

/// A copy of `low` with vertex colours from per-vertex values in `[0, max]`.
pub fn heatmap_vertices(low: &Mesh, values: &[f32], max: f32) -> Mesh {
    let mut m = low.clone();
    m.uvs.clear();
    let max = max.max(1e-12);
    m.colors = values.iter().map(|v| ramp(v / max)).collect();
    m
}

/// A copy of `low` coloured by per-triangle values (vertices get the mean of their triangles),
/// mapped from `min..max`.
pub fn heatmap_triangles(low: &Mesh, values: &[f32], min: f32, max: f32) -> Mesh {
    let mut m = low.clone();
    m.uvs.clear();
    let mut acc = vec![0.0f32; m.vertex_count()];
    let mut cnt = vec![0u32; m.vertex_count()];
    for t in 0..m.triangle_count() {
        for i in m.tri(t) {
            acc[i as usize] += values[t];
            cnt[i as usize] += 1;
        }
    }
    let span = (max - min).max(1e-12);
    m.colors = (0..m.vertex_count())
        .map(|i| {
            let v = if cnt[i] > 0 { acc[i] / cnt[i] as f32 } else { min };
            ramp((v - min) / span)
        })
        .collect();
    m
}
