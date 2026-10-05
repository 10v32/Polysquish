//! GPU ray tracer: the CPU-built [`Bvh`] flattened into storage buffers and traversed by WGSL
//! compute kernels through `wgpu`.
//!
//! The GPU path is optional. [`GpuTracer::new`] returns `Err` when no usable adapter exists, when
//! the device refuses the pipeline, or when the built-in self-test disagrees with the CPU tracer;
//! callers then fall back to the CPU [`Bvh`]. On machines without a GPU this fails fast and
//! quietly (no panics, nothing printed; diagnostics go through `log`).

use crate::bvh::{Bvh, Hit, Ray, RayTracer};
use crate::mesh::Mesh;
use anyhow::{anyhow, bail, Context, Result};
use bytemuck::{Pod, Zeroable};
use glam::Vec3;
use std::sync::Arc;
use wgpu::util::DeviceExt;

/// WGSL source for both kernels (`closest_hits`, `any_hits`).
pub const TRACE_WGSL: &str = include_str!("shaders/trace.wgsl");

/// Rays are uploaded in chunks of at most this many to stay well under buffer-size limits
/// (32 MiB of rays, 16 MiB of hits per chunk).
pub const MAX_RAYS_PER_DISPATCH: usize = 1 << 20;

const WORKGROUP_SIZE: u32 = 64;
/// Matches `MISS` in trace.wgsl. Any `t` at or above this is a miss.
const MISS_T: f32 = 3.0e38;
/// Number of rays in the self-test run by `new()`.
const SELF_TEST_RAYS: usize = 64;

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct GpuRay {
    o: [f32; 3],
    tmax: f32,
    d: [f32; 3],
    pad: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct GpuHit {
    t: f32,
    tri: u32,
    u: f32,
    v: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct Params {
    ray_count: u32,
    pad: [u32; 3],
}

impl From<&Ray> for GpuRay {
    fn from(r: &Ray) -> Self {
        GpuRay { o: r.origin.to_array(), tmax: r.tmax, d: r.dir.to_array(), pad: 0.0 }
    }
}

/// Ray tracer that runs the BVH traversal on the GPU.
pub struct GpuTracer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    layout: wgpu::BindGroupLayout,
    closest: wgpu::ComputePipeline,
    any: wgpu::ComputePipeline,
    node_bounds: wgpu::Buffer,
    node_info: wgpu::Buffer,
    tris: wgpu::Buffer,
    tri_order: wgpu::Buffer,
    /// CPU tracer kept for the (rare) case where a dispatch fails at runtime, so a batch never
    /// silently comes back empty.
    cpu: Bvh,
    adapter_name: String,
    triangle_count: usize,
}

/// Adapter name if a GPU is usable for compute, without creating a device or any buffers.
pub fn probe() -> Option<String> {
    let instance = make_instance();
    let adapter = pick_adapter(&instance)?;
    Some(adapter.get_info().name)
}

fn make_instance() -> wgpu::Instance {
    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
    desc.backends = wgpu::Backends::PRIMARY | wgpu::Backends::SECONDARY;
    // No backend validation layers: wgpu-core's own validation (which we capture through error
    // scopes) is what we rely on, and missing layers would only add log noise.
    desc.flags = wgpu::InstanceFlags::empty();
    wgpu::Instance::new(desc)
}

/// Software adapters are normally skipped; `POLYSQUISH_GPU_ALLOW_SOFTWARE=1` lets them through so
/// the kernels can be exercised on machines without a GPU (tests, debugging).
fn allow_software() -> bool {
    std::env::var_os("POLYSQUISH_GPU_ALLOW_SOFTWARE").is_some_and(|v| v == "1" || v == "true")
}

/// A real (non-software) adapter with compute support, preferring the high-performance one.
fn pick_adapter(instance: &wgpu::Instance) -> Option<wgpu::Adapter> {
    let usable = |a: &wgpu::Adapter| -> bool {
        let info = a.get_info();
        if info.device_type == wgpu::DeviceType::Cpu && !allow_software() {
            // Software rasterisers (llvmpipe, lavapipe, WARP) are slower than the rayon BVH.
            return false;
        }
        a.get_downlevel_capabilities().flags.contains(wgpu::DownlevelFlags::COMPUTE_SHADERS)
    };
    let options = wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        force_fallback_adapter: false,
        compatible_surface: None,
        ..Default::default()
    };
    match pollster::block_on(instance.request_adapter(&options)) {
        Ok(a) if usable(&a) => return Some(a),
        Ok(a) => log::debug!("gpu: skipping adapter {:?} ({:?})", a.get_info().name, a.get_info().device_type),
        Err(e) => log::debug!("gpu: no adapter: {e}"),
    }
    // The preferred adapter was unusable; see whether any other one qualifies.
    pollster::block_on(instance.enumerate_adapters(wgpu::Backends::PRIMARY | wgpu::Backends::SECONDARY))
        .into_iter()
        .find(usable)
}

/// Run `f` with validation / OOM / internal error scopes pushed, turning any captured wgpu error
/// into an `Err` instead of the default "panic on uncaptured error" behaviour.
fn guarded<T>(device: &wgpu::Device, what: &str, f: impl FnOnce() -> T) -> Result<T> {
    let internal = device.push_error_scope(wgpu::ErrorFilter::Internal);
    let oom = device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
    let validation = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let value = f();
    let errors = [
        pollster::block_on(validation.pop()),
        pollster::block_on(oom.pop()),
        pollster::block_on(internal.pop()),
    ];
    if let Some(e) = errors.into_iter().flatten().next() {
        bail!("gpu: {what} failed: {e}");
    }
    Ok(value)
}

impl GpuTracer {
    /// Build the BVH on the CPU, upload it, compile the kernels and self-test them against the
    /// CPU tracer. Returns `Err` when no suitable adapter exists or the self-test fails, so callers
    /// can fall back to the CPU BVH.
    pub fn new(mesh: &Mesh) -> Result<GpuTracer> {
        if mesh.triangle_count() == 0 {
            bail!("gpu: mesh has no triangles");
        }
        let instance = make_instance();
        let adapter = pick_adapter(&instance).ok_or_else(|| anyhow!("gpu: no suitable GPU adapter found"))?;
        let info = adapter.get_info();
        let adapter_name = info.name.clone();
        log::debug!("gpu: using adapter {adapter_name:?} via {:?}", info.backend);

        let adapter_limits = adapter.limits();
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("polysquish-gpu-tracer"),
            required_features: wgpu::Features::empty(),
            // Ask for exactly what the adapter offers so large meshes get the biggest buffers.
            required_limits: adapter_limits,
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            memory_hints: wgpu::MemoryHints::Performance,
            trace: wgpu::Trace::Off,
        }))
        .with_context(|| format!("gpu: requesting a device from {adapter_name:?}"))?;
        // Never let a later, uncaptured error abort the process; log it and let the runtime
        // CPU fallback in `run` take over.
        device.on_uncaptured_error(Arc::new(|e: wgpu::Error| log::warn!("gpu: uncaptured wgpu error: {e}")));
        device.set_device_lost_callback(|reason, msg| log::warn!("gpu: device lost ({reason:?}): {msg}"));

        let cpu = Bvh::build(mesh);
        let flat = cpu.flatten();
        let limits = device.limits();
        let max_binding = limits.max_storage_buffer_binding_size.min(limits.max_buffer_size);
        let tri_bytes = (flat.tris.len() * 16) as u64;
        let node_bytes = (flat.node_bounds.len() * 16) as u64;
        if tri_bytes > max_binding || node_bytes > max_binding {
            bail!(
                "gpu: mesh too large for {adapter_name:?} (needs {} MiB per buffer, limit {} MiB)",
                tri_bytes.max(node_bytes) >> 20,
                max_binding >> 20
            );
        }

        let (layout, closest, any) = guarded(&device, "pipeline creation", || {
            let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("polysquish-trace"),
                source: wgpu::ShaderSource::Wgsl(TRACE_WGSL.into()),
            });
            let storage = |binding: u32, read_only: bool| wgpu::BindGroupLayoutEntry {
                binding,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            };
            let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("polysquish-trace-layout"),
                entries: &[
                    storage(0, true),
                    storage(1, true),
                    storage(2, true),
                    storage(3, true),
                    storage(4, true),
                    storage(5, false),
                    wgpu::BindGroupLayoutEntry {
                        binding: 6,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                ],
            });
            let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("polysquish-trace-pipeline-layout"),
                bind_group_layouts: &[Some(&layout)],
                immediate_size: 0,
            });
            let make = |entry: &str| {
                device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    label: Some(entry),
                    layout: Some(&pipeline_layout),
                    module: &module,
                    entry_point: Some(entry),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    cache: None,
                })
            };
            let closest = make("closest_hits");
            let any = make("any_hits");
            (layout, closest, any)
        })?;

        let (node_bounds, node_info, tris, tri_order) = guarded(&device, "BVH upload", || {
            let upload = |label: &str, bytes: &[u8]| {
                device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some(label),
                    contents: bytes,
                    usage: wgpu::BufferUsages::STORAGE,
                })
            };
            (
                upload("bvh-node-bounds", bytemuck::cast_slice(&flat.node_bounds)),
                upload("bvh-node-info", bytemuck::cast_slice(&flat.node_info)),
                upload("bvh-tris", bytemuck::cast_slice(&flat.tris)),
                upload("bvh-tri-order", bytemuck::cast_slice(&flat.tri_order)),
            )
        })?;
        // Make sure the uploads actually landed before trusting the self-test.
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|e| anyhow!("gpu: waiting for BVH upload: {e}"))?;

        let tracer = GpuTracer {
            device,
            queue,
            layout,
            closest,
            any,
            node_bounds,
            node_info,
            tris,
            tri_order,
            cpu,
            adapter_name,
            triangle_count: mesh.triangle_count(),
        };
        tracer.self_test(mesh)?;
        Ok(tracer)
    }

    pub fn adapter_name(&self) -> &str {
        &self.adapter_name
    }

    pub fn triangle_count(&self) -> usize {
        self.triangle_count
    }

    /// Deterministic rays from outside the mesh towards random points inside its bounds: a mix of
    /// hits and misses.
    pub fn self_test_rays(mesh: &Mesh, count: usize, seed: u64) -> Vec<Ray> {
        use rand::{Rng, SeedableRng};
        let b = mesh.bounds();
        let center = b.center();
        let radius = (b.diagonal * 0.5).max(1e-3);
        let mut rng = rand::rngs::SmallRng::seed_from_u64(seed);
        let unit = |rng: &mut rand::rngs::SmallRng| -> Vec3 {
            loop {
                let v = Vec3::new(
                    rng.random_range(-1.0f32..1.0),
                    rng.random_range(-1.0f32..1.0),
                    rng.random_range(-1.0f32..1.0),
                );
                let l = v.length();
                if l > 1e-3 && l <= 1.0 {
                    return v / l;
                }
            }
        };
        (0..count)
            .map(|i| {
                let origin = center + unit(&mut rng) * radius * 1.5;
                // Aim at a point inside (even rays) or near (odd rays) the bounding sphere.
                let spread = if i % 2 == 0 { 0.7 } else { 1.4 };
                let target = center + unit(&mut rng) * radius * spread * rng.random::<f32>().sqrt();
                let dir = (target - origin).normalize_or_zero();
                Ray { origin, dir, tmax: b.diagonal * 4.0 + 1.0 }
            })
            .collect()
    }

    /// Compare `rays` traced on the GPU against the CPU `Bvh`: hit/miss must agree exactly and
    /// `t` must agree within `tol`.
    pub fn compare_with_cpu(&self, cpu: &Bvh, rays: &[Ray], tol: f32) -> Result<()> {
        let gpu_hits = self.run(rays, Kernel::Closest).context("gpu: self-test dispatch")?;
        let gpu_any = self.run(rays, Kernel::Any).context("gpu: self-test dispatch")?;
        for (i, ray) in rays.iter().enumerate() {
            let expect = cpu.intersect(ray.origin, ray.dir, ray.tmax);
            let got = hit_from_gpu(gpu_hits[i]);
            match (expect, got) {
                (None, None) => {}
                (Some(e), Some(g)) => {
                    if (e.t - g.t).abs() > tol {
                        bail!(
                            "gpu: self-test mismatch on ray {i}: cpu t={} (tri {}) vs gpu t={} (tri {}), tolerance {tol}",
                            e.t, e.tri, g.t, g.tri
                        );
                    }
                    if g.tri as usize >= cpu.triangle_count() {
                        bail!("gpu: self-test produced out-of-range triangle {} on ray {i}", g.tri);
                    }
                }
                (e, g) => bail!(
                    "gpu: self-test hit/miss mismatch on ray {i}: cpu {} vs gpu {}",
                    if e.is_some() { "hit" } else { "miss" },
                    if g.is_some() { "hit" } else { "miss" }
                ),
            }
            let occluded = cpu.occluded(ray.origin, ray.dir, ray.tmax);
            if occluded != (gpu_any[i].tri != 0) {
                bail!("gpu: self-test occlusion mismatch on ray {i}: cpu {occluded} vs gpu {}", gpu_any[i].tri != 0);
            }
        }
        Ok(())
    }

    fn self_test(&self, mesh: &Mesh) -> Result<()> {
        let rays = Self::self_test_rays(mesh, SELF_TEST_RAYS, 0x5eed_0bad_cafe);
        let tol = 1e-4 * mesh.bounds().diagonal.max(1e-6);
        self.compare_with_cpu(&self.cpu, &rays, tol)
    }

    /// Dispatch one kernel over `rays` (chunked) and read the raw results back.
    fn run(&self, rays: &[Ray], kernel: Kernel) -> Result<Vec<GpuHit>> {
        let mut out = Vec::with_capacity(rays.len());
        for chunk in rays.chunks(MAX_RAYS_PER_DISPATCH) {
            out.extend(self.run_chunk(chunk, kernel)?);
        }
        Ok(out)
    }

    fn run_chunk(&self, rays: &[Ray], kernel: Kernel) -> Result<Vec<GpuHit>> {
        let n = rays.len();
        let gpu_rays: Vec<GpuRay> = rays.iter().map(GpuRay::from).collect();
        let hit_bytes = (n * std::mem::size_of::<GpuHit>()) as u64;
        let device = &self.device;

        let staging = guarded(device, "dispatch", || {
            let ray_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("rays"),
                contents: bytemuck::cast_slice(&gpu_rays),
                usage: wgpu::BufferUsages::STORAGE,
            });
            let hit_buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("hits"),
                size: hit_bytes,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            });
            let staging = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("hits-staging"),
                size: hit_bytes,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let params = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("params"),
                contents: bytemuck::bytes_of(&Params { ray_count: n as u32, pad: [0; 3] }),
                usage: wgpu::BufferUsages::UNIFORM,
            });
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("trace-bind-group"),
                layout: &self.layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: self.node_bounds.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: self.node_info.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 2, resource: self.tris.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 3, resource: self.tri_order.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 4, resource: ray_buf.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 5, resource: hit_buf.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 6, resource: params.as_entire_binding() },
                ],
            });
            let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("trace") });
            {
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("trace"),
                    timestamp_writes: None,
                });
                pass.set_pipeline(match kernel {
                    Kernel::Closest => &self.closest,
                    Kernel::Any => &self.any,
                });
                pass.set_bind_group(0, &bind_group, &[]);
                pass.dispatch_workgroups((n as u32).div_ceil(WORKGROUP_SIZE), 1, 1);
            }
            encoder.copy_buffer_to_buffer(&hit_buf, 0, &staging, 0, hit_bytes);
            self.queue.submit(Some(encoder.finish()));
            staging
        })?;

        let (tx, rx) = std::sync::mpsc::channel();
        staging.slice(..).map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        device.poll(wgpu::PollType::wait_indefinitely()).map_err(|e| anyhow!("gpu: waiting for results: {e}"))?;
        rx.recv()
            .map_err(|_| anyhow!("gpu: readback callback never ran"))?
            .map_err(|e| anyhow!("gpu: mapping results: {e}"))?;
        let hits: Vec<GpuHit> = {
            let view = staging.slice(..).get_mapped_range().map_err(|e| anyhow!("gpu: reading results: {e}"))?;
            bytemuck::cast_slice(&view[..]).to_vec()
        };
        staging.unmap();
        Ok(hits)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kernel {
    Closest,
    Any,
}

fn hit_from_gpu(h: GpuHit) -> Option<Hit> {
    if h.t < MISS_T && h.t.is_finite() {
        Some(Hit { t: h.t, tri: h.tri, u: h.u, v: h.v })
    } else {
        None
    }
}

impl RayTracer for GpuTracer {
    fn name(&self) -> &str {
        "gpu"
    }

    fn closest_hits(&self, rays: &[Ray]) -> Vec<Option<Hit>> {
        match self.run(rays, Kernel::Closest) {
            Ok(hits) => hits.into_iter().map(hit_from_gpu).collect(),
            Err(e) => {
                log::warn!("{e}; tracing this batch on the CPU instead");
                self.cpu.closest_hits(rays)
            }
        }
    }

    fn any_hits(&self, rays: &[Ray]) -> Vec<bool> {
        match self.run(rays, Kernel::Any) {
            Ok(hits) => hits.into_iter().map(|h| h.tri != 0).collect(),
            Err(e) => {
                log::warn!("{e}; tracing this batch on the CPU instead");
                self.cpu.any_hits(rays)
            }
        }
    }
}
