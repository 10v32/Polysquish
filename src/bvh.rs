//! A compact binned-SAH bounding volume hierarchy for ray casting against the high-poly mesh.

use crate::mesh::Mesh;
use glam::Vec3;

#[derive(Clone, Copy, Debug)]
struct Node {
    bmin: Vec3,
    bmax: Vec3,
    /// Leaf: first index into `tri_order`. Inner: index of left child (right = left + 1).
    first: u32,
    /// Leaf when > 0: number of triangles.
    count: u32,
}

#[derive(Clone, Copy, Debug)]
struct Tri {
    p0: Vec3,
    e1: Vec3,
    e2: Vec3,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hit {
    pub t: f32,
    pub tri: u32,
    /// Barycentric weights for vertices 1 and 2 (vertex 0 weight is 1-u-v).
    pub u: f32,
    pub v: f32,
}

pub struct Bvh {
    nodes: Vec<Node>,
    tri_order: Vec<u32>,
    tris: Vec<Tri>,
}

const LEAF_SIZE: usize = 4;
const BINS: usize = 12;

impl Bvh {
    pub fn build(mesh: &Mesh) -> Bvh {
        let tc = mesh.triangle_count();
        let tris: Vec<Tri> = (0..tc)
            .map(|t| {
                let [a, b, c] = mesh.tri(t);
                let p0 = mesh.positions[a as usize];
                Tri {
                    p0,
                    e1: mesh.positions[b as usize] - p0,
                    e2: mesh.positions[c as usize] - p0,
                }
            })
            .collect();
        let centroids: Vec<Vec3> = tris
            .iter()
            .map(|t| t.p0 + (t.e1 + t.e2) / 3.0)
            .collect();
        let bounds: Vec<(Vec3, Vec3)> = tris
            .iter()
            .map(|t| {
                let p1 = t.p0 + t.e1;
                let p2 = t.p0 + t.e2;
                (t.p0.min(p1).min(p2), t.p0.max(p1).max(p2))
            })
            .collect();
        let mut tri_order: Vec<u32> = (0..tc as u32).collect();
        let mut nodes: Vec<Node> = Vec::with_capacity(tc / 2 + 1);
        nodes.push(Node { bmin: Vec3::ZERO, bmax: Vec3::ZERO, first: 0, count: tc as u32 });
        // Iterative build with an explicit stack of node indices.
        let mut stack = vec![0usize];
        while let Some(ni) = stack.pop() {
            let (first, count) = (nodes[ni].first as usize, nodes[ni].count as usize);
            let slice = &tri_order[first..first + count];
            let mut bmin = Vec3::splat(f32::INFINITY);
            let mut bmax = Vec3::splat(f32::NEG_INFINITY);
            let mut cmin = Vec3::splat(f32::INFINITY);
            let mut cmax = Vec3::splat(f32::NEG_INFINITY);
            for &t in slice {
                let (lo, hi) = bounds[t as usize];
                bmin = bmin.min(lo);
                bmax = bmax.max(hi);
                let c = centroids[t as usize];
                cmin = cmin.min(c);
                cmax = cmax.max(c);
            }
            nodes[ni].bmin = bmin;
            nodes[ni].bmax = bmax;
            if count <= LEAF_SIZE {
                continue;
            }
            // Binned SAH along the widest centroid axis.
            let ext = cmax - cmin;
            let axis = if ext.x >= ext.y && ext.x >= ext.z {
                0
            } else if ext.y >= ext.z {
                1
            } else {
                2
            };
            if ext[axis] <= 1e-12 {
                // All centroids coincide: split in the middle by count.
                let mid = first + count / 2;
                let left = nodes.len();
                nodes.push(Node { bmin, bmax, first: first as u32, count: (mid - first) as u32 });
                nodes.push(Node { bmin, bmax, first: mid as u32, count: (first + count - mid) as u32 });
                nodes[ni].first = left as u32;
                nodes[ni].count = 0;
                stack.push(left);
                stack.push(left + 1);
                continue;
            }
            let scale = BINS as f32 / ext[axis];
            let mut bin_count = [0u32; BINS];
            let mut bin_min = [Vec3::splat(f32::INFINITY); BINS];
            let mut bin_max = [Vec3::splat(f32::NEG_INFINITY); BINS];
            for &t in slice {
                let b = (((centroids[t as usize][axis] - cmin[axis]) * scale) as usize).min(BINS - 1);
                bin_count[b] += 1;
                let (lo, hi) = bounds[t as usize];
                bin_min[b] = bin_min[b].min(lo);
                bin_max[b] = bin_max[b].max(hi);
            }
            // Sweep to evaluate the SAH for each of the BINS-1 split planes.
            let area = |lo: Vec3, hi: Vec3| -> f32 {
                let d = (hi - lo).max(Vec3::ZERO);
                2.0 * (d.x * d.y + d.y * d.z + d.z * d.x)
            };
            let mut left_area = [0f32; BINS - 1];
            let mut left_cnt = [0u32; BINS - 1];
            let mut lmin = Vec3::splat(f32::INFINITY);
            let mut lmax = Vec3::splat(f32::NEG_INFINITY);
            let mut lc = 0;
            for i in 0..BINS - 1 {
                lmin = lmin.min(bin_min[i]);
                lmax = lmax.max(bin_max[i]);
                lc += bin_count[i];
                left_area[i] = area(lmin, lmax);
                left_cnt[i] = lc;
            }
            let mut rmin = Vec3::splat(f32::INFINITY);
            let mut rmax = Vec3::splat(f32::NEG_INFINITY);
            let mut rc = 0;
            let mut best_cost = f32::INFINITY;
            let mut best_split = usize::MAX;
            for i in (0..BINS - 1).rev() {
                rmin = rmin.min(bin_min[i + 1]);
                rmax = rmax.max(bin_max[i + 1]);
                rc += bin_count[i + 1];
                if left_cnt[i] == 0 || rc == 0 {
                    continue;
                }
                let cost = left_area[i] * left_cnt[i] as f32 + area(rmin, rmax) * rc as f32;
                if cost < best_cost {
                    best_cost = cost;
                    best_split = i;
                }
            }
            let leaf_cost = area(bmin, bmax) * count as f32;
            if best_split == usize::MAX || (best_cost >= leaf_cost && count <= LEAF_SIZE * 4) {
                continue;
            }
            // Partition in place.
            let split_pos = cmin[axis] + (best_split as f32 + 1.0) / scale;
            let slice = &mut tri_order[first..first + count];
            let mut i = 0usize;
            let mut j = count;
            while i < j {
                if centroids[slice[i] as usize][axis] < split_pos {
                    i += 1;
                } else {
                    j -= 1;
                    slice.swap(i, j);
                }
            }
            if i == 0 || i == count {
                i = count / 2;
            }
            let mid = first + i;
            let left = nodes.len();
            nodes.push(Node { bmin, bmax, first: first as u32, count: (mid - first) as u32 });
            nodes.push(Node { bmin, bmax, first: mid as u32, count: (first + count - mid) as u32 });
            nodes[ni].first = left as u32;
            nodes[ni].count = 0;
            stack.push(left);
            stack.push(left + 1);
        }
        Bvh { nodes, tri_order, tris }
    }

    #[inline]
    fn intersect_box(bmin: Vec3, bmax: Vec3, o: Vec3, inv_d: Vec3, tmax: f32) -> f32 {
        let t0 = (bmin - o) * inv_d;
        let t1 = (bmax - o) * inv_d;
        let tn = t0.min(t1);
        let tf = t0.max(t1);
        let tmin = tn.max_element().max(0.0);
        let tmx = tf.min_element().min(tmax);
        if tmin <= tmx {
            tmin
        } else {
            f32::INFINITY
        }
    }

    #[inline]
    fn intersect_tri(tri: &Tri, o: Vec3, d: Vec3, tmax: f32) -> Option<(f32, f32, f32)> {
        let pvec = d.cross(tri.e2);
        let det = tri.e1.dot(pvec);
        if det.abs() < 1e-12 {
            return None;
        }
        let inv = 1.0 / det;
        let tvec = o - tri.p0;
        let u = tvec.dot(pvec) * inv;
        if u < -1e-5 || u > 1.0 + 1e-5 {
            return None;
        }
        let qvec = tvec.cross(tri.e1);
        let v = d.dot(qvec) * inv;
        if v < -1e-5 || u + v > 1.0 + 1e-5 {
            return None;
        }
        let t = tri.e2.dot(qvec) * inv;
        if t < 0.0 || t > tmax {
            return None;
        }
        Some((t, u.clamp(0.0, 1.0), v.clamp(0.0, 1.0)))
    }

    /// Closest hit along the ray within `tmax`.
    pub fn intersect(&self, o: Vec3, d: Vec3, tmax: f32) -> Option<Hit> {
        if self.nodes.is_empty() || self.tris.is_empty() {
            return None;
        }
        let inv_d = Vec3::new(
            if d.x != 0.0 { 1.0 / d.x } else { f32::INFINITY },
            if d.y != 0.0 { 1.0 / d.y } else { f32::INFINITY },
            if d.z != 0.0 { 1.0 / d.z } else { f32::INFINITY },
        );
        let mut best: Option<Hit> = None;
        let mut tmax = tmax;
        let mut stack: [u32; 64] = [0; 64];
        let mut sp = 0usize;
        stack[sp] = 0;
        sp += 1;
        while sp > 0 {
            sp -= 1;
            let node = &self.nodes[stack[sp] as usize];
            if Self::intersect_box(node.bmin, node.bmax, o, inv_d, tmax) == f32::INFINITY {
                continue;
            }
            if node.count > 0 {
                for k in node.first..node.first + node.count {
                    let ti = self.tri_order[k as usize];
                    if let Some((t, u, v)) = Self::intersect_tri(&self.tris[ti as usize], o, d, tmax) {
                        tmax = t;
                        best = Some(Hit { t, tri: ti, u, v });
                    }
                }
            } else {
                let l = node.first as usize;
                let r = l + 1;
                let tl = Self::intersect_box(self.nodes[l].bmin, self.nodes[l].bmax, o, inv_d, tmax);
                let tr = Self::intersect_box(self.nodes[r].bmin, self.nodes[r].bmax, o, inv_d, tmax);
                // Push far first so the near child is visited first.
                if tl <= tr {
                    if tr < f32::INFINITY {
                        stack[sp] = r as u32;
                        sp += 1;
                    }
                    if tl < f32::INFINITY {
                        stack[sp] = l as u32;
                        sp += 1;
                    }
                } else {
                    stack[sp] = l as u32;
                    sp += 1;
                    stack[sp] = r as u32;
                    sp += 1;
                }
                if sp >= 62 {
                    // Extremely deep tree; fall back to brute force on remaining nodes.
                    sp = 62;
                }
            }
        }
        best
    }

    /// True if anything blocks the ray within `tmax`.
    pub fn occluded(&self, o: Vec3, d: Vec3, tmax: f32) -> bool {
        if self.nodes.is_empty() {
            return false;
        }
        let inv_d = Vec3::new(
            if d.x != 0.0 { 1.0 / d.x } else { f32::INFINITY },
            if d.y != 0.0 { 1.0 / d.y } else { f32::INFINITY },
            if d.z != 0.0 { 1.0 / d.z } else { f32::INFINITY },
        );
        let mut stack: [u32; 64] = [0; 64];
        let mut sp = 1usize;
        while sp > 0 {
            sp -= 1;
            let node = &self.nodes[stack[sp] as usize];
            if Self::intersect_box(node.bmin, node.bmax, o, inv_d, tmax) == f32::INFINITY {
                continue;
            }
            if node.count > 0 {
                for k in node.first..node.first + node.count {
                    let ti = self.tri_order[k as usize];
                    if Self::intersect_tri(&self.tris[ti as usize], o, d, tmax).is_some() {
                        return true;
                    }
                }
            } else {
                stack[sp] = node.first;
                sp += 1;
                stack[sp] = node.first + 1;
                sp += 1;
                if sp >= 62 {
                    sp = 62;
                }
            }
        }
        false
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }
}
