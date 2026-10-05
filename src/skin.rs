//! Skinning transfer: carry joint weights from the source mesh onto the squished one.

use crate::bvh::Bvh;
use crate::mesh::Mesh;
use rayon::prelude::*;

/// Give every vertex of `low` the (up to four) strongest joint influences found at its closest
/// point on `high`, barycentrically blended across the hit triangle. Returns false when `high`
/// carries no skin.
pub fn transfer_skin(high: &Mesh, bvh: &Bvh, low: &mut Mesh) -> bool {
    if !high.has_skin() {
        return false;
    }
    let diag = high.bounds().diagonal.max(1e-9);
    let results: Vec<([u16; 4], [f32; 4])> = low
        .positions
        .par_iter()
        .map(|p| {
            let Some((hit, _)) = bvh.closest_point(*p, diag) else {
                return ([0, 0, 0, 0], [1.0, 0.0, 0.0, 0.0]);
            };
            let [a, b, c] = high.tri(hit.tri as usize);
            let w = [1.0 - hit.u - hit.v, hit.u, hit.v];
            // Accumulate influences.
            let mut acc: Vec<(u16, f32)> = Vec::with_capacity(12);
            for (k, vi) in [a, b, c].iter().enumerate() {
                let vi = *vi as usize;
                for s in 0..4 {
                    let wt = high.weights[vi][s] * w[k];
                    if wt <= 0.0 {
                        continue;
                    }
                    let j = high.joints[vi][s];
                    if let Some(e) = acc.iter_mut().find(|e| e.0 == j) {
                        e.1 += wt;
                    } else {
                        acc.push((j, wt));
                    }
                }
            }
            acc.sort_by(|x, y| y.1.partial_cmp(&x.1).unwrap_or(std::cmp::Ordering::Equal));
            acc.truncate(4);
            let sum: f32 = acc.iter().map(|e| e.1).sum();
            let mut joints = [0u16; 4];
            let mut weights = [0f32; 4];
            for (i, (j, wt)) in acc.iter().enumerate() {
                joints[i] = *j;
                weights[i] = if sum > 0.0 { wt / sum } else { 0.0 };
            }
            if sum <= 0.0 {
                weights[0] = 1.0;
            }
            (joints, weights)
        })
        .collect();
    low.joints = results.iter().map(|r| r.0).collect();
    low.weights = results.iter().map(|r| r.1).collect();
    true
}
