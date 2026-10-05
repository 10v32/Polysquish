// Ray / BVH intersection kernels. The traversal mirrors `Bvh::intersect` in src/bvh.rs exactly
// (same box test, same Möller–Trumbore epsilons, same near-first child ordering) so the GPU and
// CPU paths agree on hit/miss and on `t` up to floating-point noise.
//
// Buffer layouts come from `FlatBvh` (two vec4 per node, two u32 per node, three vec4 per tri).

struct Ray {
    o: vec3<f32>,
    tmax: f32,
    d: vec3<f32>,
    pad: f32,
}

struct Hit {
    t: f32,
    tri: u32,
    u: f32,
    v: f32,
}

struct Params {
    ray_count: u32,
    pad0: u32,
    pad1: u32,
    pad2: u32,
}

@group(0) @binding(0) var<storage, read> node_bounds: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read> node_info: array<u32>;
@group(0) @binding(2) var<storage, read> tris: array<vec4<f32>>;
@group(0) @binding(3) var<storage, read> tri_order: array<u32>;
@group(0) @binding(4) var<storage, read> rays: array<Ray>;
@group(0) @binding(5) var<storage, read_write> hits: array<Hit>;
@group(0) @binding(6) var<uniform> params: Params;

// Sentinel "no hit" distance. Real hits are always <= ray.tmax, which is finite and far smaller.
const MISS: f32 = 3.0e38;
const BIG: f32 = 1.0e30;
const STACK_SIZE: u32 = 64u;

// Slab test; returns the entry distance or MISS. Same semantics as `Bvh::intersect_box`.
fn box_t(ni: u32, o: vec3<f32>, inv_d: vec3<f32>, tmax: f32) -> f32 {
    let bmin = node_bounds[2u * ni].xyz;
    let bmax = node_bounds[2u * ni + 1u].xyz;
    let t0 = (bmin - o) * inv_d;
    let t1 = (bmax - o) * inv_d;
    let tn = min(t0, t1);
    let tf = max(t0, t1);
    let tmin = max(max(tn.x, max(tn.y, tn.z)), 0.0);
    let tmx = min(min(tf.x, min(tf.y, tf.z)), tmax);
    return select(MISS, tmin, tmin <= tmx);
}

// Möller–Trumbore. Returns (t, u, v, 1) on a hit and w <= 0 on a miss.
fn tri_hit(ti: u32, o: vec3<f32>, d: vec3<f32>, tmax: f32) -> vec4<f32> {
    let miss = vec4<f32>(0.0, 0.0, 0.0, -1.0);
    let p0 = tris[3u * ti].xyz;
    let e1 = tris[3u * ti + 1u].xyz;
    let e2 = tris[3u * ti + 2u].xyz;
    let pvec = cross(d, e2);
    let det = dot(e1, pvec);
    if (abs(det) < 1e-12) {
        return miss;
    }
    let inv = 1.0 / det;
    let tvec = o - p0;
    let u = dot(tvec, pvec) * inv;
    if (u < -1e-5 || u > 1.00001) {
        return miss;
    }
    let qvec = cross(tvec, e1);
    let v = dot(d, qvec) * inv;
    if (v < -1e-5 || u + v > 1.00001) {
        return miss;
    }
    let t = dot(e2, qvec) * inv;
    if (t < 0.0 || t > tmax) {
        return miss;
    }
    return vec4<f32>(t, clamp(u, 0.0, 1.0), clamp(v, 0.0, 1.0), 1.0);
}

// Fixed-stack traversal (depth <= 64). With `any_hit` set, returns at the first intersection.
fn trace(o: vec3<f32>, d: vec3<f32>, tmax_in: f32, any_hit: bool) -> Hit {
    var best = Hit(MISS, 0u, 0.0, 0.0);
    var tmax = tmax_in;
    let inv_d = select(1.0 / d, vec3<f32>(BIG, BIG, BIG), d == vec3<f32>(0.0, 0.0, 0.0));
    var stack: array<u32, STACK_SIZE>;
    var sp: u32 = 1u;
    stack[0] = 0u;
    loop {
        if (sp == 0u) {
            break;
        }
        sp = sp - 1u;
        let ni = stack[sp];
        if (box_t(ni, o, inv_d, tmax) >= MISS) {
            continue;
        }
        let first = node_info[2u * ni];
        let count = node_info[2u * ni + 1u];
        if (count > 0u) {
            for (var k: u32 = first; k < first + count; k = k + 1u) {
                let ti = tri_order[k];
                let h = tri_hit(ti, o, d, tmax);
                if (h.w > 0.0) {
                    tmax = h.x;
                    best = Hit(h.x, ti, h.y, h.z);
                    if (any_hit) {
                        return best;
                    }
                }
            }
        } else {
            let l = first;
            let r = first + 1u;
            let tl = box_t(l, o, inv_d, tmax);
            let tr = box_t(r, o, inv_d, tmax);
            // Push the far child first so the near child is visited first.
            if (tl <= tr) {
                if (tr < MISS) {
                    stack[sp] = r;
                    sp = sp + 1u;
                }
                if (tl < MISS) {
                    stack[sp] = l;
                    sp = sp + 1u;
                }
            } else {
                stack[sp] = l;
                sp = sp + 1u;
                stack[sp] = r;
                sp = sp + 1u;
            }
            // Extremely deep tree: cap the stack (mirrors the CPU traversal).
            sp = min(sp, STACK_SIZE - 2u);
        }
    }
    return best;
}

@compute @workgroup_size(64)
fn closest_hits(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i >= params.ray_count) {
        return;
    }
    let r = rays[i];
    hits[i] = trace(r.o, r.d, r.tmax, false);
}

@compute @workgroup_size(64)
fn any_hits(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i >= params.ray_count) {
        return;
    }
    let r = rays[i];
    let h = trace(r.o, r.d, r.tmax, true);
    hits[i] = Hit(h.t, select(0u, 1u, h.t < MISS), 0.0, 0.0);
}
