use glam::Vec3;
use polysquish::bvh::{Bvh, RayTracer};
use polysquish::gpu::{self, GpuTracer};
use polysquish::mesh::Mesh;
use std::time::Instant;

fn make_sphere(seg: u32) -> Mesh {
    // UV sphere, outward winding (same construction as tests/pipeline.rs).
    let mut m = Mesh::default();
    for i in 0..=seg {
        let v = i as f32 / seg as f32;
        let phi = v * std::f32::consts::PI;
        for j in 0..=seg * 2 {
            let u = j as f32 / (seg * 2) as f32;
            let theta = u * std::f32::consts::TAU;
            m.positions.push(Vec3::new(phi.sin() * theta.cos(), phi.cos(), phi.sin() * theta.sin()));
        }
    }
    let w = seg * 2 + 1;
    for i in 0..seg {
        for j in 0..seg * 2 {
            let a = i * w + j;
            let (b, c, d) = (a + 1, a + w, a + w + 1);
            m.indices.extend_from_slice(&[a, b, c, b, d, c]);
        }
    }
    m
}

/// The kernels must at least be valid WGSL, GPU or not: parse *and* validate with naga, the same
/// front-end wgpu uses, so a shader typo fails the test on every machine.
#[test]
fn trace_wgsl_is_valid() {
    let module = naga::front::wgsl::parse_str(gpu::TRACE_WGSL)
        .unwrap_or_else(|e| panic!("trace.wgsl does not parse:\n{}", e.emit_to_string(gpu::TRACE_WGSL)));
    let info = naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::empty())
        .validate(&module)
        .unwrap_or_else(|e| panic!("trace.wgsl does not validate: {e:?}"));
    let entries: Vec<&str> = module.entry_points.iter().map(|ep| ep.name.as_str()).collect();
    assert!(entries.contains(&"closest_hits"), "entry points: {entries:?}");
    assert!(entries.contains(&"any_hits"), "entry points: {entries:?}");
    for ep in &module.entry_points {
        assert_eq!(ep.stage, naga::ShaderStage::Compute);
        assert_eq!(ep.workgroup_size, [64, 1, 1], "{} workgroup size", ep.name);
    }
    drop(info);
}

#[test]
fn flatten_matches_tree() {
    let m = make_sphere(16);
    let bvh = Bvh::build(&m);
    let flat = bvh.flatten();
    assert_eq!(flat.node_count(), bvh.node_count());
    assert_eq!(flat.triangle_count(), m.triangle_count());
    assert_eq!(flat.tri_order.len(), m.triangle_count());
    // Every triangle appears exactly once in the leaves, every node is well-formed.
    let mut seen = vec![false; m.triangle_count()];
    for n in 0..flat.node_count() {
        let (first, count) = (flat.node_info[2 * n] as usize, flat.node_info[2 * n + 1] as usize);
        let (lo, hi) = (flat.node_bounds[2 * n], flat.node_bounds[2 * n + 1]);
        assert!(lo[0] <= hi[0] && lo[1] <= hi[1] && lo[2] <= hi[2], "node {n} bounds {lo:?} {hi:?}");
        if count > 0 {
            for &t in &flat.tri_order[first..first + count] {
                assert!(!seen[t as usize], "triangle {t} in two leaves");
                seen[t as usize] = true;
            }
        } else {
            assert!(first + 1 < flat.node_count(), "node {n} child {first} out of range");
        }
    }
    assert!(seen.iter().all(|&s| s));
    // Root bounds are the sphere's bounds.
    let (lo, hi) = (flat.node_bounds[0], flat.node_bounds[1]);
    for k in 0..3 {
        assert!((lo[k] + 1.0).abs() < 1e-5 && (hi[k] - 1.0).abs() < 1e-5, "root bounds {lo:?} {hi:?}");
    }
}

/// On a machine without a GPU `GpuTracer::new` must fail fast and quietly; with one, it must
/// agree with the CPU BVH on 1000 random rays.
#[test]
fn gpu_tracer_falls_back_or_matches_cpu() {
    let m = make_sphere(48);
    let bvh = Bvh::build(&m);

    // The very first wgpu instance may pay a one-off cost for loading driver libraries from a cold
    // disk, so timing is measured on a warm second call.
    let cold = Instant::now();
    let probed = gpu::probe();
    let cold_probe_time = cold.elapsed();
    let t0 = Instant::now();
    assert_eq!(gpu::probe(), probed, "probe() must be deterministic");
    let probe_time = t0.elapsed();
    let t1 = Instant::now();
    let tracer = GpuTracer::new(&m);
    let new_time = t1.elapsed();
    eprintln!(
        "probe = {probed:?} (cold {cold_probe_time:?}, warm {probe_time:?}); new = {} ({new_time:?})",
        if tracer.is_ok() { "Ok" } else { "Err" }
    );

    match tracer {
        Err(e) => {
            // (a) no usable GPU: the error is descriptive, quick, and probe agrees.
            let msg = e.to_string();
            assert!(msg.starts_with("gpu:"), "unexpected error text: {msg}");
            assert!(new_time.as_secs_f32() < 2.0, "GpuTracer::new took {new_time:?} to fail");
            assert!(probe_time.as_secs_f32() < 2.0, "warm probe took {probe_time:?}");
            if probed.is_some() {
                // An adapter exists but the device / self-test rejected it; that is a legitimate
                // fallback too, but say why.
                eprintln!("adapter {probed:?} was rejected: {msg}");
            }
        }
        Ok(tracer) => {
            // (b) a GPU is present: results must match the CPU tracer.
            assert_eq!(tracer.name(), "gpu");
            assert!(!tracer.adapter_name().is_empty());
            assert!(probed.is_some(), "probe() said no GPU but new() succeeded");
            let rays = GpuTracer::self_test_rays(&m, 1000, 42);
            let tol = 1e-4 * m.bounds().diagonal;
            tracer.compare_with_cpu(&bvh, &rays, tol).expect("GPU and CPU tracers disagree");

            let cpu_hits = bvh.closest_hits(&rays);
            let gpu_hits = tracer.closest_hits(&rays);
            let cpu_any = bvh.any_hits(&rays);
            let gpu_any = tracer.any_hits(&rays);
            let mut hits = 0;
            for i in 0..rays.len() {
                assert_eq!(cpu_hits[i].is_some(), gpu_hits[i].is_some(), "ray {i} hit/miss");
                assert_eq!(cpu_any[i], gpu_any[i], "ray {i} occlusion");
                if let (Some(c), Some(g)) = (cpu_hits[i], gpu_hits[i]) {
                    hits += 1;
                    assert!((c.t - g.t).abs() <= tol, "ray {i}: cpu t {} vs gpu t {}", c.t, g.t);
                    assert!(g.u >= 0.0 && g.v >= 0.0 && g.u + g.v <= 1.0 + 1e-4, "ray {i}: barycentrics {} {}", g.u, g.v);
                }
            }
            assert!(hits > 100 && hits < rays.len(), "test rays should mix hits and misses, got {hits} hits");
        }
    }
}
