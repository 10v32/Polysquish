/* Polysquish UI — single-page state machine + three.js viewer.
   Plain ES module, no build step. API contract: docs/API.md (v1 + "V2 additions").
   Open with ?mock=1 to run against an in-page fake backend (&cpu=1 → no GPU, &fail=1 → bake fails). */

import * as THREE from 'three';
import { OrbitControls } from 'three/addons/controls/OrbitControls.js';
import { GLTFLoader } from 'three/addons/loaders/GLTFLoader.js';
import { RoomEnvironment } from 'three/addons/environments/RoomEnvironment.js';

const params = new URLSearchParams(location.search);
const MOCK = params.has('mock') && params.get('mock') !== '0';
const MOCK_FAIL = params.get('fail') === '1';
const MOCK_CPU = params.get('cpu') === '1';
const POLL_MS = 400;
const QUEUE_POLL_MS = 1000;
const WATCH_POLL_MS = 3000;
const ACCEPTED = ['.obj', '.ply', '.stl', '.glb', '.gltf'];

/* =====================================================================
   Formatting helpers
   ===================================================================== */
export function fmtInt(n) {
  if (n == null || !isFinite(n)) return '—';
  return Math.round(n).toLocaleString('en-US');
}
export function fmtShort(n) {
  if (n == null || !isFinite(n)) return '—';
  const abs = Math.abs(n);
  if (abs >= 1e9) return trim(n / 1e9, 1) + 'B';
  if (abs >= 1e6) return trim(n / 1e6, abs >= 1e8 ? 0 : 1) + 'M';
  if (abs >= 1e4) return trim(n / 1e3, 0) + 'k';
  if (abs >= 1e3) return trim(n / 1e3, 1) + 'k';
  return String(Math.round(n));
}
export function fmtBytes(b) {
  if (b == null || !isFinite(b)) return '—';
  if (b < 1024) return `${b} B`;
  const units = ['KB', 'MB', 'GB', 'TB'];
  let v = b / 1024, i = 0;
  while (v >= 1024 && i < units.length - 1) { v /= 1024; i++; }
  return `${v.toFixed(1)} ${units[i]}`;
}
export function fmtSeconds(s) {
  if (s == null || !isFinite(s)) return '—';
  if (s < 10) return `${s.toFixed(1)}s`;
  if (s < 60) return `${Math.round(s)}s`;
  const m = Math.floor(s / 60), sec = Math.round(s % 60);
  if (m < 60) return `${m}m ${String(sec).padStart(2, '0')}s`;
  const hh = Math.floor(m / 60), mm = m % 60;
  return `${hh}h ${String(mm).padStart(2, '0')}m`;
}
export function fmtDelta(before, after) {
  if (!before) return null;
  const d = (after - before) / before * 100;
  const sign = d < 0 ? '–' : d > 0 ? '+' : '';
  const abs = Math.abs(d);
  return `${sign}${abs >= 99.95 ? abs.toFixed(2) : abs >= 10 ? abs.toFixed(1) : abs.toFixed(1)}%`;
}
/* fraction (0..1) of the model size → percentage string */
export function fmtPct(frac) {
  if (frac == null || !isFinite(frac)) return '—';
  const v = frac * 100;
  if (v === 0) return '0%';
  if (v < 0.01) return '<0.01%';
  if (v < 1) return `${v.toFixed(2)}%`;
  if (v < 10) return `${v.toFixed(1)}%`;
  return `${Math.round(v)}%`;
}
function trim(v, d) { return Number(v.toFixed(d)).toString(); }
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const clamp = (v, a, b) => Math.min(b, Math.max(a, v));
const deepClone = (o) => JSON.parse(JSON.stringify(o));
const fileStem = (name = '') => name.replace(/^.*[\\/]/, '').replace(/\.[^.]+$/, '') || 'model';
const extOf = (name = '') => (name.match(/\.[^.]+$/) || [''])[0].toLowerCase();
const baseName = (p = '') => p.replace(/[\\/]+$/, '').replace(/^.*[\\/]/, '') || p;
const isTerminal = (status) => ['done', 'error', 'cancelled'].includes(status);

/* =====================================================================
   DOM helpers
   ===================================================================== */
const $ = (sel, root = document) => root.querySelector(sel);
function h(tag, props = {}, ...children) {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(props || {})) {
    if (v == null || v === false) continue;
    if (k === 'class') el.className = v;
    else if (k === 'text') el.textContent = v;
    else if (k === 'html') el.innerHTML = v;
    else if (k === 'dataset') Object.assign(el.dataset, v);
    else if (k.startsWith('on') && typeof v === 'function') el.addEventListener(k.slice(2).toLowerCase(), v);
    else if (k in el && typeof v !== 'string' && k !== 'style') el[k] = v;
    else el.setAttribute(k, v === true ? '' : v);
  }
  for (const c of children.flat()) {
    if (c == null || c === false) continue;
    el.append(c.nodeType ? c : document.createTextNode(String(c)));
  }
  return el;
}
function svg(path, size = 16, extra = '') {
  const w = document.createElement('span');
  w.innerHTML = `<svg viewBox="0 0 24 24" width="${size}" height="${size}" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" ${extra}>${path}</svg>`;
  return w.firstElementChild;
}
const ICONS = {
  check: '<path d="M5 12.5l4.5 4.5L19 7"/>',
  sparkles: '<path d="M12 3l1.9 5.1L19 10l-5.1 1.9L12 17l-1.9-5.1L5 10l5.1-1.9z"/><path d="M19 16l.8 2.2L22 19l-2.2.8L19 22l-.8-2.2L16 19l2.2-.8z"/><path d="M4 3l.6 1.4L6 5l-1.4.6L4 7l-.6-1.4L2 5l1.4-.6z"/>',
  cube: '<path d="M12 3l8 4.5v9L12 21l-8-4.5v-9z"/><path d="M4 7.5l8 4.5 8-4.5M12 12v9"/>',
  phone: '<rect x="7" y="2.5" width="10" height="19" rx="2.5"/><path d="M11 18.5h2"/>',
  wrench: '<path d="M14.5 6.5a4 4 0 0 0 5 5L9 22l-3-3L16.5 8.5"/><path d="M14.5 6.5L18 3l3 3-3.5 3.5"/>',
  sliders: '<path d="M4 7h10M18 7h2M4 17h4M12 17h8"/><circle cx="16" cy="7" r="2"/><circle cx="10" cy="17" r="2"/>',
  box: '<path d="M3 8l9-5 9 5v8l-9 5-9-5z"/><path d="M3 8l9 5 9-5M12 13v8"/>',
  person: '<circle cx="12" cy="7.5" r="3.5"/><path d="M5 21c0-4 3.1-6.5 7-6.5s7 2.5 7 6.5"/>',
  folder: '<path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/>',
  file: '<path d="M7 3h7l5 5v13H7z"/><path d="M14 3v5h5"/>',
  download: '<path d="M12 4v11m0 0l-4-4m4 4l4-4M4 19h16"/>',
  copy: '<rect x="9" y="9" width="11" height="11" rx="2"/><path d="M5 15V6a2 2 0 0 1 2-2h9"/>',
  x: '<path d="M6 6l12 12M18 6L6 18"/>',
  shield: '<path d="M12 3l8 3v6c0 4.5-3.5 7.8-8 9-4.5-1.2-8-4.5-8-9V6z"/><path d="M9 12l2 2 4-4"/>',
  eye: '<path d="M2 12s3.5-6 10-6 10 6 10 6-3.5 6-10 6S2 12 2 12z"/><circle cx="12" cy="12" r="3"/>',
  stop: '<rect x="6" y="6" width="12" height="12" rx="2"/>',
  bone: '<path d="M7 4a2.5 2.5 0 0 1 2.4 3.2l5.4 5.4A2.5 2.5 0 1 1 13 15.4L7.6 10A2.5 2.5 0 1 1 7 4z"/>',
  bolt: '<path d="M13 2L4 14h7l-1 8 9-12h-7z"/>',
  cpu: '<rect x="6" y="6" width="12" height="12" rx="2"/><path d="M9 2v4M15 2v4M9 18v4M15 18v4M2 9h4M2 15h4M18 9h4M18 15h4"/>',
};
const presetIcon = (name = '') => {
  const k = name.toLowerCase();
  if (k.includes('person') || k.includes('character') || k.includes('rig')) return ICONS.person;
  if (k.includes('spark') || k.includes('hero') || k.includes('star')) return ICONS.sparkles;
  if (k.includes('phone') || k.includes('mobile')) return ICONS.phone;
  if (k.includes('wrench') || k.includes('tool') || k.includes('dcc') || k.includes('brush')) return ICONS.wrench;
  if (k.includes('slider') || k.includes('custom')) return ICONS.sliders;
  if (k.includes('cube') || k.includes('box') || k.includes('prop')) return ICONS.cube;
  return ICONS.box;
};

/* =====================================================================
   Toasts
   ===================================================================== */
function toast({ title, msg = '', kind = 'info', timeout = 6000 }) {
  const host = $('#toasts');
  const el = h('div', { class: `toast toast-${kind}`, role: kind === 'error' ? 'alert' : 'status' },
    h('span', { class: 'toast-ico', 'aria-hidden': 'true' }),
    h('div', { class: 'toast-body' }, h('div', { class: 'toast-title', text: title }), msg ? h('div', { class: 'toast-msg', text: msg }) : null),
    h('button', { class: 'toast-close', type: 'button', 'aria-label': 'Dismiss', text: '×', onclick: close }));
  function close() {
    if (el.classList.contains('is-leaving')) return;
    el.classList.add('is-leaving');
    setTimeout(() => el.remove(), 260);
  }
  host.append(el);
  if (timeout) setTimeout(close, timeout);
  return close;
}

/* =====================================================================
   Recipe defaults (V2 fields are optional in the API; the UI fills them in)
   ===================================================================== */
const V2_DEFAULTS = {
  cleanup: { remove_hidden: true, hidden_samples: 48 },
  decimate: { chunk_threshold: 1500000, keep_materials: false },
  retopo: { mode: 'triangles', voxel_resolution: 256, voxel_keep_fraction: 1.0 },
  bake: { hard_edge_angle: 60, ao_denoise: true, gpu: true },
  lods: { imposter: false, imposter_resolution: 1024 },
  export: { fbx: true, skin: true },
};
const normalizeRecipe = (r) => deepMerge(deepClone(V2_DEFAULTS), r || {});
function deepMerge(base, over) {
  const out = Array.isArray(base) ? [...base] : { ...base };
  for (const [k, v] of Object.entries(over || {})) {
    if (v && typeof v === 'object' && !Array.isArray(v) && base && typeof base[k] === 'object' && !Array.isArray(base[k])) out[k] = deepMerge(base[k], v);
    else out[k] = v;
  }
  return out;
}

/* =====================================================================
   API layer (real)
   ===================================================================== */
class ApiError extends Error {
  constructor(message, status = 0) { super(message); this.name = 'ApiError'; this.status = status; }
}
async function request(path, { method = 'GET', body, json = true } = {}) {
  const init = { method, headers: {} };
  if (body instanceof FormData) init.body = body;
  else if (body !== undefined) { init.headers['Content-Type'] = 'application/json'; init.body = JSON.stringify(body); }
  let res;
  try { res = await fetch(path, init); }
  catch (e) { throw new ApiError(`Can't reach the Polysquish backend (${e.message}). Is the polysquish executable still running?`); }
  if (!res.ok) {
    let msg = `${res.status} ${res.statusText}`;
    try { const j = await res.json(); if (j && j.error) msg = j.error; } catch { /* non-JSON error body */ }
    throw new ApiError(msg, res.status);
  }
  return json ? res.json() : res;
}
const enc = encodeURIComponent;
const realApi = {
  health: () => request('/api/health'),
  presets: () => request('/api/presets'),
  upload(files) {
    const fd = new FormData();
    for (const f of files) fd.append('file', f, f.name);
    return request('/api/upload', { method: 'POST', body: fd });
  },
  uploadPath: (path) => request('/api/upload/path', { method: 'POST', body: { path } }),
  inspect: (upload_id) => request('/api/inspect', { method: 'POST', body: { upload_id } }),
  squish: (payload) => request('/api/squish', { method: 'POST', body: payload }),
  job: (id) => request(`/api/jobs/${enc(id)}`),
  jobs: () => request('/api/jobs'),
  cancel: (id) => request(`/api/jobs/${enc(id)}/cancel`, { method: 'POST' }),
  batch: (payload) => request('/api/batch', { method: 'POST', body: payload }),
  batchStatus: (id) => request(`/api/batch/${enc(id)}`),
  watchList: () => request('/api/watch'),
  watchStart: (payload) => request('/api/watch', { method: 'POST', body: payload }),
  watchStop: (id) => request(`/api/watch/${enc(id)}`, { method: 'DELETE' }),
  fs: (path) => request(`/api/fs${path ? `?path=${enc(path)}` : ''}`),
  openFolder: (path) => request('/api/open-folder', { method: 'POST', body: { path } }),
  fileUrl: (jobId, name) => `/api/jobs/${enc(jobId)}/files/${enc(name)}`,
  zipUrl: (result, jobId) => result?.download_zip || `/api/jobs/${enc(jobId)}/zip`,
  previewObject: null, // real backend serves GLBs; the viewer loads them via GLTFLoader
};

/* =====================================================================
   Mock backend (?mock=1)
   ===================================================================== */
function createMockApi() {
  const gpu = MOCK_CPU ? { available: false, name: null } : { available: true, name: 'Mock GPU' };
  const baseRecipe = (over = {}) => deepMerge({
    preset: 'hero',
    cleanup: { weld: true, weld_tolerance: 0.00001, remove_degenerate: true, remove_floaters: true, floater_min_fraction: 0.001, fix_winding: true, remove_hidden: true, hidden_samples: 48 },
    decimate: { target_triangles: 30000, target_ratio: null, max_error: null, lock_border: true, preserve_uvs: true, preserve_colors: true, aggressive: false, chunk_threshold: 1500000, keep_materials: false },
    retopo: { mode: 'triangles', voxel_resolution: 256, voxel_keep_fraction: 1.0 },
    uv: { enabled: true, resolution: 2048, padding: 4, keep_existing_if_good: true },
    bake: { enabled: true, resolution: 2048, normal_map: true, albedo: true, ao: true, ao_samples: 32, metallic_roughness: true, normal_convention: 'opengl', ray_distance: null, dilation_px: 8, supersample: 2, hard_edge_angle: 60, ao_denoise: true, gpu: true },
    lods: { count: 3, ratios: [0.5, 0.25, 0.1], imposter: false, imposter_resolution: 1024 },
    collision: { convex_hull: true, box: true, simplified_mesh: true, simplified_triangles: 300 },
    export: { glb: true, obj: true, report: true, scale: 1.0, target: 'generic', fbx: true, skin: true },
    seed: 1337,
  }, over);
  const presets = [
    { id: 'character', name: 'Character', tagline: 'Rigged, animated, game-ready', description: '40k triangles, 2K textures, 3 LODs, keeps skin and animations, materials stay separate.', icon: 'person', target_triangles: 40000, texture_size: 2048,
      recipe: baseRecipe({ preset: 'character', decimate: { target_triangles: 40000, keep_materials: true }, bake: { hard_edge_angle: 70 }, export: { fbx: true, skin: true } }) },
    { id: 'hero', name: 'Hero asset', tagline: 'PC / console close-up', description: '30k triangles, 2K textures, 3 LODs.', icon: 'sparkles', target_triangles: 30000, texture_size: 2048, recipe: baseRecipe({ preset: 'hero' }) },
    { id: 'prop', name: 'Prop', tagline: 'Mid-distance set dressing', description: '8k triangles, 1K textures, 2 LODs.', icon: 'cube', target_triangles: 8000, texture_size: 1024,
      recipe: baseRecipe({ preset: 'prop', decimate: { target_triangles: 8000 }, uv: { resolution: 1024 }, bake: { resolution: 1024, ao_samples: 16 }, lods: { count: 2, ratios: [0.5, 0.25], imposter: true, imposter_resolution: 512 }, export: { skin: false } }) },
    { id: 'mobile', name: 'Mobile / web', tagline: 'Tiny and fast', description: '3k triangles, 512px textures, 2 LODs, no AO.', icon: 'phone', target_triangles: 3000, texture_size: 512,
      recipe: baseRecipe({ preset: 'mobile', decimate: { target_triangles: 3000, aggressive: true }, uv: { resolution: 512 }, bake: { resolution: 512, ao: false, metallic_roughness: false }, lods: { count: 2, ratios: [0.5, 0.25], imposter: true, imposter_resolution: 512 }, collision: { simplified_mesh: false, simplified_triangles: 120 }, export: { obj: false, fbx: false, skin: false } }) },
    { id: 'dcc', name: 'DCC clean-up', tagline: 'Blender · Maya · C4D', description: 'Quad-dominant retopo to 200k, keep detail, no baking.', icon: 'wrench', target_triangles: 200000, texture_size: 4096,
      recipe: baseRecipe({ preset: 'dcc', decimate: { target_triangles: 200000, lock_border: false, keep_materials: true }, retopo: { mode: 'quad_dominant' }, uv: { enabled: false, resolution: 4096 }, bake: { enabled: false, resolution: 4096 }, lods: { count: 0, ratios: [] }, collision: { convex_hull: false, box: false, simplified_mesh: false }, export: { target: 'blender', fbx: true } }) },
    { id: 'custom', name: 'Custom', tagline: 'Your own recipe', description: 'Start from Hero and tweak anything.', icon: 'sliders', target_triangles: 30000, texture_size: 2048, recipe: baseRecipe({ preset: 'custom' }) },
  ];

  const uploads = new Map();
  const jobs = new Map();
  const jobOrder = [];          // squish jobs, creation order → run one at a time
  const batches = new Map();
  const watches = new Map();
  const lastSquish = new Map(); // upload_id → recipe of the last squish (stage cache)
  let seq = 0;
  const nid = (p) => `${p}_${(++seq).toString(16).padStart(2, '0')}${Math.random().toString(16).slice(2, 5)}`;
  const latency = () => sleep(120 + Math.random() * 180);

  const sourceReport = (sizeBytes) => ({
    triangles: 1204330, vertices: 602167,
    bounds: { min: [-0.81, -0.02, -0.63], max: [0.79, 1.42, 0.61], size: [1.6, 1.44, 1.24], diagonal: 2.48 },
    components: 15, largest_component_fraction: 0.992,
    non_manifold_edges: 12, boundary_edges: 340, degenerate_triangles: 3, duplicate_vertices: 1202,
    watertight: false, has_normals: true, has_uvs: false, has_vertex_colors: true,
    materials: 1, textures: [{ kind: 'base_color', width: 4096, height: 4096 }],
    units_guess: 'meters', up_axis_guess: 'Y',
    problems: [
      { id: 'too_many_triangles', severity: 'error', title: '1.2M triangles is far too dense for real-time', detail: 'Game engines want 3k–50k triangles for an asset this size. The extra detail is mostly noise from the generator.', fix: 'Decimation will squish it to your target while keeping the silhouette.' },
      { id: 'floaters', severity: 'warn', title: '14 floating fragments', detail: 'Small disconnected pieces that are usually generation noise.', fix: 'Removed automatically during cleanup.' },
      { id: 'duplicate_vertices', severity: 'warn', title: '1,202 duplicate vertices', detail: 'Overlapping vertices split the surface and make shading seams.', fix: 'Welded during cleanup.' },
      { id: 'hidden_faces', severity: 'warn', title: '12,034 hidden interior faces', detail: 'Faces that can never be seen from outside (inner shells, teeth, eyeballs behind lids).', fix: 'Removed by the hidden-face pass so no triangles are wasted on them.' },
      { id: 'no_uvs', severity: 'info', title: 'No UV coordinates', detail: 'Textures need a UV layout to stick to the surface.', fix: 'A fresh UV layout is generated and the vertex colors are baked into an albedo map.' },
      { id: 'non_manifold', severity: 'info', title: '12 non-manifold edges', detail: 'Edges shared by more than two triangles; harmless for rendering, awkward for collision.', fix: 'Repaired where possible; the rest is reported.' },
    ],
    size_bytes: sizeBytes,
  });
  const resultReport = (tris, verts) => ({
    triangles: tris, vertices: verts,
    bounds: { min: [-0.81, -0.02, -0.63], max: [0.79, 1.42, 0.61], size: [1.6, 1.44, 1.24], diagonal: 2.48 },
    components: 1, largest_component_fraction: 1, non_manifold_edges: 0, boundary_edges: 0, degenerate_triangles: 0, duplicate_vertices: 0,
    watertight: true, has_normals: true, has_uvs: true, has_vertex_colors: false, materials: 1,
    textures: [], units_guess: 'meters', up_axis_guess: 'Y', problems: [],
  });

  const SQUISH_STAGES = [
    ['import', 'Reading model', 0.6, 'Loaded 1,204,330 triangles / 602,167 vertices'],
    ['analyze', 'Health check', 0.35, 'Found 15 components, 1,202 duplicate vertices, 12 non-manifold edges'],
    ['clean', 'Cleaning', 0.5, 'Welded 1,202 duplicate vertices', 'Removed 14 floating fragments (0.8% of surface)', 'Removed 12,034 hidden interior faces', 'Fixed 3 degenerate triangles'],
    ['decimate', 'Squishing polygons', 1.3, '{topo} 1,204,330 → {tris} triangles', 'Max deviation 0.61% of size (p95 0.21%)'],
    ['uv', 'Unwrapping UVs', 0.9, 'Generated 61 UV charts, 91.4% atlas coverage'],
    ['bake', 'Baking textures', 1.2, 'Baked albedo {res}×{res} on the {tracer}', 'Baked normal map (OpenGL convention, hard edges > {hard}°)', 'Baked ambient occlusion, 32 samples{denoise}'],
    ['lods', 'Building LODs', 0.5, 'LOD1 {lod1} · LOD2 {lod2} · LOD3 {lod3} triangles{imposter}'],
    ['collision', 'Collision shapes', 0.3, 'Convex hull 64 verts · box · simplified mesh 300 tris'],
    ['export', 'Exporting', 0.35, 'Wrote {name}.glb{fbx}, {name}.obj, textures and report.html'],
  ];
  const CACHEABLE = ['import', 'analyze', 'clean', 'decimate', 'uv'];
  const INSPECT_STAGES = [
    ['import', 'Reading model', 0.9, 'Loaded 1,204,330 triangles'],
    ['analyze', 'Health check', 0.6, 'Found 15 components, 1,202 duplicate vertices'],
    ['preview', 'Building preview', 0.4, 'Source preview: 196,800 triangles'],
  ];

  function makeJob(kind, stageDefs, finish, vars = {}, { cached = new Set(), name = null } = {}) {
    const id = nid('j');
    const defs = stageDefs.map((d) => cached.has(d[0]) ? [d[0], d[1], 0, `Cache hit: reused "${d[1]}" from the previous squish`] : d);
    const total = defs.reduce((a, s) => a + s[2], 0);
    const job = {
      id, kind, name, status: 'queued', progress: 0, stage: null, stage_label: null, stage_progress: 0,
      stages: defs.map(([sid, label]) => ({ id: sid, label, status: 'pending', seconds: null })),
      log: [], error: null, result: null,
      _t0: kind === 'squish' ? null : performance.now() + 250, _defs: defs, _total: total, _finish: finish, _vars: vars, _logged: new Set(),
    };
    jobs.set(id, job);
    if (kind === 'squish') jobOrder.push(id);
    return job;
  }
  const fill = (s, vars) => s.replace(/\{(\w+)\}/g, (_, k) => vars[k] ?? `{${k}}`);
  /* squish jobs run strictly one at a time, in creation order */
  function schedule() {
    const now = performance.now();
    let cursor = now + 150;
    for (const id of jobOrder) {
      const j = jobs.get(id);
      if (isTerminal(j.status)) continue;
      if (j._t0 == null) j._t0 = cursor;
      cursor = Math.max(cursor, j._t0 + j._total * 1000 + 120);
    }
  }
  function tick(job) {
    if (isTerminal(job.status)) return;
    schedule();
    if (job._t0 == null) { job.status = 'queued'; return; }
    const t = (performance.now() - job._t0) / 1000;
    if (t < 0) { job.status = 'queued'; return; }
    job.status = 'running';
    let acc = 0;
    for (let i = 0; i < job._defs.length; i++) {
      const [sid, label, dur, ...lines] = job._defs[i];
      const st = job.stages[i];
      if (t >= acc + dur) {
        st.status = 'done'; st.seconds = dur === 0 ? 0 : Number((dur * (0.9 + 0.2 * Math.random())).toFixed(2));
        if (!job._logged.has(sid)) { job._logged.add(sid); for (const l of lines) job.log.push(fill(l, job._vars)); }
      } else {
        st.status = 'running';
        job.stage = sid; job.stage_label = label; job.stage_progress = (t - acc) / dur;
        job.progress = clamp(t / job._total, 0, 0.999);
        if (MOCK_FAIL && job.kind === 'squish' && sid === 'bake' && job.stage_progress > 0.4) {
          st.status = 'error'; job.status = 'error';
          job.error = 'Baking failed: the UV atlas has overlapping charts (mock failure for testing — remove &fail=1 from the URL).';
          job.log.push('ERROR: overlapping UV charts at (0.42, 0.77)');
        }
        return;
      }
      acc += dur;
    }
    job.status = 'done'; job.progress = 1; job.stage = null; job.stage_label = 'Done'; job.stage_progress = 1;
    job.result = job._finish(job);
    job.log.push('Done.');
  }
  const publicJob = (job, { log = true } = {}) => {
    tick(job);
    const { _t0, _defs, _total, _finish, _vars, _logged, ...pub } = job;
    if (!log) delete pub.log;
    return deepClone(pub);
  };

  // procedural textures (also used by the viewer for the "after" mesh)
  const texCache = {};
  function canvasTexture(kind, size = 512) {
    if (texCache[kind]) return texCache[kind];
    const c = document.createElement('canvas'); c.width = c.height = size;
    const g = c.getContext('2d');
    if (kind === 'albedo') {
      const lg = g.createLinearGradient(0, 0, size, size);
      lg.addColorStop(0, '#ff6ad5'); lg.addColorStop(0.5, '#8b5cf6'); lg.addColorStop(1, '#5ff2c3');
      g.fillStyle = lg; g.fillRect(0, 0, size, size);
      g.globalAlpha = 0.18; g.strokeStyle = '#0b0716'; g.lineWidth = 2;
      for (let i = 0; i <= 16; i++) { const p = i * size / 16; g.beginPath(); g.moveTo(p, 0); g.lineTo(p, size); g.moveTo(0, p); g.lineTo(size, p); g.stroke(); }
      g.globalAlpha = 0.25;
      for (let i = 0; i < 1200; i++) { g.fillStyle = Math.random() > .5 ? '#fff' : '#0b0716'; g.fillRect(Math.random() * size, Math.random() * size, 2 + Math.random() * 5, 2 + Math.random() * 5); }
    } else if (kind === 'normal') {
      g.fillStyle = '#8080ff'; g.fillRect(0, 0, size, size);
      for (let i = 0; i < 2500; i++) { const r = 2 + Math.random() * 10; g.fillStyle = `rgb(${Math.round(110 + Math.random() * 40)},${Math.round(110 + Math.random() * 40)},${Math.round(225 + Math.random() * 30)})`; g.beginPath(); g.arc(Math.random() * size, Math.random() * size, r, 0, Math.PI * 2); g.fill(); }
    } else if (kind === 'ao') {
      const rg = g.createRadialGradient(size / 2, size / 2, size * .1, size / 2, size / 2, size * .7);
      rg.addColorStop(0, '#f4f4f4'); rg.addColorStop(1, '#6e6e6e');
      g.fillStyle = rg; g.fillRect(0, 0, size, size);
      g.globalAlpha = .35;
      for (let i = 0; i < 300; i++) { g.fillStyle = '#222'; g.beginPath(); g.arc(Math.random() * size, Math.random() * size, 4 + Math.random() * 26, 0, Math.PI * 2); g.fill(); }
    } else if (kind === 'mr') {
      g.fillStyle = '#20a040'; g.fillRect(0, 0, size, size);
      g.globalAlpha = .4;
      for (let i = 0; i < 400; i++) { g.fillStyle = Math.random() > .5 ? '#0a6020' : '#40c060'; g.fillRect(Math.random() * size, Math.random() * size, 10 + Math.random() * 40, 10 + Math.random() * 40); }
    } else if (kind === 'imposter') {
      g.fillStyle = '#120a24'; g.fillRect(0, 0, size, size);
      const cell = size / 4;
      for (let y = 0; y < 4; y++) for (let x = 0; x < 4; x++) {
        const cx = x * cell + cell / 2, cy = y * cell + cell / 2;
        const rg = g.createRadialGradient(cx - cell * .15, cy - cell * .15, cell * .05, cx, cy, cell * .45);
        rg.addColorStop(0, '#ff9ae3'); rg.addColorStop(0.6, '#8b5cf6'); rg.addColorStop(1, 'rgba(139,92,246,0)');
        g.fillStyle = rg; g.beginPath(); g.ellipse(cx, cy, cell * .42, cell * .32 + (x % 2) * cell * .08, (x + y) * .4, 0, Math.PI * 2); g.fill();
      }
    }
    texCache[kind] = c;
    return c;
  }

  /* ---- heat-map GLBs: real binary glTF with COLOR_0, so the viewer exercises the GLTFLoader path ---- */
  const HEAT_STOPS = ['#4f7cff', '#5ff2c3', '#ffc466', '#ff6b8a'].map((c) => new THREE.Color(c).convertSRGBToLinear());
  function rampColor(t, out) {
    const seg = HEAT_STOPS.length - 1, f = clamp(t, 0, 1) * seg, i = Math.min(Math.floor(f), seg - 1);
    return out.copy(HEAT_STOPS[i]).lerp(HEAT_STOPS[i + 1], f - i);
  }
  const heatCache = new Map();
  function heatGLB(kind) {
    if (heatCache.has(kind)) return heatCache.get(kind);
    const geo = new THREE.TorusKnotGeometry(1, 0.34, 250, 60, 2, 3);
    const pos = geo.attributes.position, nor = geo.attributes.normal, n = pos.count;
    const col = new Float32Array(n * 3), tmp = new THREE.Color();
    for (let i = 0; i < n; i++) {
      const x = pos.getX(i), y = pos.getY(i), z = pos.getZ(i);
      let t;
      if (kind === 'deviation') { const s = Math.sin(x * 6.1) * Math.cos(y * 4.3) * Math.sin(z * 7.7); t = Math.pow(clamp(0.5 + 0.5 * s, 0, 1), 2.2); }
      else { t = clamp(0.1 + 0.9 * (0.5 + 0.5 * nor.getY(i)) * (0.7 + 0.3 * Math.sin(x * 3 + z * 2)), 0, 1); }
      rampColor(t, tmp);
      col[i * 3] = tmp.r; col[i * 3 + 1] = tmp.g; col[i * 3 + 2] = tmp.b;
    }
    geo.setAttribute('color', new THREE.BufferAttribute(col, 3));
    const url = encodeGLB(geo);
    heatCache.set(kind, url);
    return url;
  }
  function encodeGLB(geo) {
    const pos = geo.attributes.position.array, nor = geo.attributes.normal.array, col = geo.attributes.color.array;
    const idx = geo.index.array instanceof Uint32Array ? geo.index.array : Uint32Array.from(geo.index.array);
    const parts = [pos, nor, col, idx].map((a) => new Uint8Array(a.buffer, a.byteOffset, a.byteLength));
    const pad4 = (v) => (v + 3) & ~3;
    const views = []; let off = 0;
    parts.forEach((p, i) => { views.push({ buffer: 0, byteOffset: off, byteLength: p.byteLength, target: i === 3 ? 34963 : 34962 }); off = pad4(off + p.byteLength); });
    const bin = new Uint8Array(off); parts.forEach((p, i) => bin.set(p, views[i].byteOffset));
    const count = pos.length / 3;
    const min = [Infinity, Infinity, Infinity], max = [-Infinity, -Infinity, -Infinity];
    for (let i = 0; i < pos.length; i += 3) for (let k = 0; k < 3; k++) { min[k] = Math.min(min[k], pos[i + k]); max[k] = Math.max(max[k], pos[i + k]); }
    const json = {
      asset: { version: '2.0', generator: 'polysquish-mock' }, scene: 0, scenes: [{ nodes: [0] }], nodes: [{ mesh: 0, name: 'heatmap' }],
      meshes: [{ primitives: [{ attributes: { POSITION: 0, NORMAL: 1, COLOR_0: 2 }, indices: 3, material: 0 }] }],
      materials: [{ name: 'heat', pbrMetallicRoughness: { baseColorFactor: [1, 1, 1, 1], metallicFactor: 0, roughnessFactor: 1 } }],
      accessors: [
        { bufferView: 0, componentType: 5126, count, type: 'VEC3', min, max },
        { bufferView: 1, componentType: 5126, count, type: 'VEC3' },
        { bufferView: 2, componentType: 5126, count, type: 'VEC3' },
        { bufferView: 3, componentType: 5125, count: idx.length, type: 'SCALAR' }],
      bufferViews: views, buffers: [{ byteLength: bin.byteLength }],
    };
    let jsonStr = JSON.stringify(json); while (jsonStr.length % 4) jsonStr += ' ';
    const jsonBytes = new TextEncoder().encode(jsonStr);
    const total = 12 + 8 + jsonBytes.byteLength + 8 + bin.byteLength;
    const out = new ArrayBuffer(total), dv = new DataView(out), u8 = new Uint8Array(out);
    dv.setUint32(0, 0x46546C67, true); dv.setUint32(4, 2, true); dv.setUint32(8, total, true);
    dv.setUint32(12, jsonBytes.byteLength, true); dv.setUint32(16, 0x4E4F534A, true); u8.set(jsonBytes, 20);
    const o = 20 + jsonBytes.byteLength;
    dv.setUint32(o, bin.byteLength, true); dv.setUint32(o + 4, 0x004E4942, true); u8.set(bin, o + 8);
    geo.dispose();
    return URL.createObjectURL(new Blob([out], { type: 'model/gltf-binary' }));
  }

  const dataUrlFor = (name) => {
    if (/heatmap_deviation/i.test(name)) return heatGLB('deviation');
    if (/heatmap_density/i.test(name)) return heatGLB('density');
    if (/imposter.*albedo|imposter.*png/i.test(name)) return canvasTexture('imposter').toDataURL('image/png');
    if (/albedo/i.test(name)) return canvasTexture('albedo').toDataURL('image/png');
    if (/normal/i.test(name)) return canvasTexture('normal').toDataURL('image/png');
    if (/_ao\b/i.test(name)) return canvasTexture('ao').toDataURL('image/png');
    if (/metal|rough|_mr\b/i.test(name)) return canvasTexture('mr').toDataURL('image/png');
    return 'data:text/plain;charset=utf-8,' + encodeURIComponent(`Polysquish mock output: ${name}\n`);
  };

  function squishResult(job) {
    const { recipe, name, upload } = job._vars;
    const tris = recipe.decimate.target_triangles;
    const verts = Math.round(tris * 0.54);
    const res = recipe.bake.resolution;
    const bakeOn = recipe.bake.enabled;
    const uvOn = recipe.uv.enabled;
    const files = [];
    if (recipe.export.glb) files.push({ name: `${name}.glb`, size_bytes: Math.round(tris * 51 + (bakeOn ? res * res * 0.9 : 0)), kind: 'glb' });
    if (recipe.export.fbx) files.push({ name: `${name}.fbx`, size_bytes: Math.round(tris * 64 + (recipe.export.skin ? 180000 : 0)), kind: 'fbx' });
    if (recipe.export.obj) files.push({ name: `${name}.obj`, size_bytes: Math.round(tris * 27), kind: 'obj' });
    if (bakeOn && recipe.bake.albedo) files.push({ name: `${name}_albedo.png`, size_bytes: Math.round(res * res * 0.52), kind: 'texture' });
    if (bakeOn && recipe.bake.normal_map) files.push({ name: `${name}_normal.png`, size_bytes: Math.round(res * res * 0.57), kind: 'texture' });
    if (bakeOn && recipe.bake.ao) files.push({ name: `${name}_ao.png`, size_bytes: Math.round(res * res * 0.21), kind: 'texture' });
    if (bakeOn && recipe.bake.metallic_roughness) files.push({ name: `${name}_mr.png`, size_bytes: Math.round(res * res * 0.18), kind: 'texture' });
    if (recipe.lods.imposter) {
      const ir = recipe.lods.imposter_resolution || 1024;
      files.push({ name: `${name}_imposter_albedo.png`, size_bytes: Math.round(ir * ir * 0.6), kind: 'imposter' });
      files.push({ name: `${name}_imposter_normal.png`, size_bytes: Math.round(ir * ir * 0.62), kind: 'imposter' });
      files.push({ name: `${name}_imposter.glb`, size_bytes: 4120, kind: 'imposter' });
    }
    files.push({ name: `${name}_heatmap_deviation.glb`, size_bytes: Math.round(tris * 18), kind: 'heatmap' });
    if (uvOn) files.push({ name: `${name}_heatmap_density.glb`, size_bytes: Math.round(tris * 18), kind: 'heatmap' });
    if (recipe.export.report) files.push({ name: 'report.html', size_bytes: 20480, kind: 'report' });
    const lods = [{ level: 0, triangles: tris, screen_coverage: 1.0 }];
    (recipe.lods.ratios || []).slice(0, recipe.lods.count).forEach((r, i) => lods.push({ level: i + 1, triangles: Math.round(tris * r), screen_coverage: Number((r).toFixed(3)) }));
    const after_size = files.filter((f) => f.kind !== 'heatmap').reduce((a, f) => a + f.size_bytes, 0);
    const fixed = ['Welded 1,202 duplicate vertices', 'Removed 14 floating fragments', 'Fixed 3 degenerate triangles', 'Repaired 12 non-manifold edges'];
    if (recipe.cleanup.remove_hidden) fixed.push('Removed 12,034 hidden interior faces');
    if (uvOn) fixed.push('Generated UV layout (91.4% coverage)');
    const quadMode = recipe.retopo.mode === 'quad_dominant';
    const polygons = quadMode ? Math.round(tris * 0.58) : tris;
    const quads = quadMode ? Math.round(polygons * 0.84) : 0;
    const rig = recipe.export.skin && (/\.(glb|gltf|fbx)$/i.test(upload.main_file) || recipe.preset === 'character')
      ? { joints: 42, animations: ['Idle', 'Walk', 'Run'] } : null;
    return {
      output_dir: `/home/me/Polysquish/${name}`,
      files,
      before: { triangles: 1204330, vertices: 602167, size_bytes: upload.size_bytes },
      after: { triangles: tris, vertices: verts, texture_size: res, size_bytes: after_size, lods },
      problems_fixed: fixed,
      report: resultReport(tris, verts),
      metrics: {
        deviation: { mean: 0.0008, max: 0.0061, p95: 0.0021, unit: 'fraction_of_size', mean_abs: 0.0014, max_abs: 0.011 },
        texel_density: uvOn ? { mean: 1024.5 * (res / 2048), min: 310.2 * (res / 2048), max: 2210.0 * (res / 2048), unit: 'texels_per_unit' } : null,
        uv_charts: uvOn ? 61 : 0, quads, polygons, watertight: true,
        hidden_faces_removed: recipe.cleanup.remove_hidden ? 12034 : 0,
        tracer: recipe.bake.gpu && gpu.available ? 'gpu' : 'cpu',
      },
      rig,
      preview: {
        source: `/api/jobs/${job.id}/preview/source.glb`, result: `/api/jobs/${job.id}/preview/result.glb`,
        heatmap_deviation: `/api/jobs/${job.id}/preview/heatmap_deviation.glb`,
        heatmap_density: uvOn ? `/api/jobs/${job.id}/preview/heatmap_density.glb` : null,
      },
      download_zip: `/api/jobs/${job.id}/zip`,
      timings: Object.fromEntries(job.stages.map((s) => [s.id, s.seconds])),
    };
  }
  const TOPO_WORD = { triangles: 'Quadric decimation', quad_dominant: 'Quad-dominant retopo', voxel: 'Voxel rebuild + decimation' };
  function createSquish(up, recipe, name) {
    const prev = lastSquish.get(up.upload_id);
    const key = (r) => JSON.stringify({ c: r.cleanup, d: r.decimate, r: r.retopo, u: r.uv });
    const cached = prev && key(prev) === key(recipe) ? new Set(CACHEABLE) : new Set();
    lastSquish.set(up.upload_id, deepClone(recipe));
    const tt = recipe.decimate.target_triangles;
    const vars = {
      recipe, name, upload: up, tris: fmtInt(tt), res: recipe.bake.resolution, topo: TOPO_WORD[recipe.retopo.mode] || TOPO_WORD.triangles,
      tracer: recipe.bake.gpu && gpu.available ? `GPU (${gpu.name})` : 'CPU', hard: recipe.bake.hard_edge_angle, denoise: recipe.bake.ao_denoise ? ', denoised' : '',
      imposter: recipe.lods.imposter ? ` · imposter atlas ${recipe.lods.imposter_resolution}px` : '', fbx: recipe.export.fbx ? `, ${name}.fbx` : '',
      lod1: fmtInt(tt * (recipe.lods.ratios[0] ?? 0.5)), lod2: fmtInt(tt * (recipe.lods.ratios[1] ?? 0.25)), lod3: fmtInt(tt * (recipe.lods.ratios[2] ?? 0.1)),
    };
    return makeJob('squish', SQUISH_STAGES, squishResult, vars, { cached, name });
  }

  /* ---- fake file system for /api/fs ---- */
  const F = (name, size_bytes, supported = ACCEPTED.includes(extOf(name))) => ({ name, size_bytes, supported });
  const HOME = '/home/me';
  const FS = {
    '/': { dirs: ['home', 'tmp', 'usr'], files: [] },
    '/home': { dirs: ['me'], files: [] },
    '/tmp': { dirs: [], files: [F('scratch.txt', 120)] },
    '/usr': { dirs: ['local', 'share'], files: [] },
    '/usr/local': { dirs: [], files: [] }, '/usr/share': { dirs: [], files: [] },
    [HOME]: { dirs: ['Downloads', 'Documents', 'models', 'Polysquish'], files: [F('.zshrc', 812)] },
    [`${HOME}/Downloads`]: { dirs: ['meshy', 'tripo', 'scans'], files: [F('robot.glb', 48211000), F('installer.dmg', 221000000), F('notes.txt', 2200)] },
    [`${HOME}/Downloads/meshy`]: { dirs: [], files: [F('dragon.obj', 98000000), F('dragon.mtl', 410), F('dragon_albedo.png', 12400000), F('castle.glb', 61000000)] },
    [`${HOME}/Downloads/tripo`]: { dirs: ['batch-04'], files: [F('chair.glb', 9100000), F('lamp.glb', 7200000)] },
    [`${HOME}/Downloads/tripo/batch-04`]: { dirs: [], files: [F('mug.glb', 3100000), F('plant.glb', 15200000), F('rug.glb', 2000000)] },
    [`${HOME}/Downloads/scans`]: { dirs: [], files: [F('statue_raw.ply', 410000000), F('statue_raw.jpg', 8800000)] },
    [`${HOME}/Documents`]: { dirs: ['Blender', 'Unity Projects'], files: [F('todo.md', 900)] },
    [`${HOME}/Documents/Blender`]: { dirs: [], files: [F('scene.blend', 73000000)] },
    [`${HOME}/Documents/Unity Projects`]: { dirs: ['Dungeon'], files: [] },
    [`${HOME}/Documents/Unity Projects/Dungeon`]: { dirs: ['Assets'], files: [] },
    [`${HOME}/Documents/Unity Projects/Dungeon/Assets`]: { dirs: ['Models'], files: [] },
    [`${HOME}/Documents/Unity Projects/Dungeon/Assets/Models`]: { dirs: [], files: [F('door.fbx', 2000000), F('torch.glb', 1200000)] },
    [`${HOME}/models`]: { dirs: ['kitbash'], files: [F('bunny.ply', 3000000), F('helmet.glb', 24000000), F('teapot.stl', 1600000)] },
    [`${HOME}/models/kitbash`]: { dirs: [], files: Array.from({ length: 9 }, (_, i) => F(`part_${String(i + 1).padStart(2, '0')}.obj`, 400000 + i * 90000)) },
    [`${HOME}/Polysquish`]: { dirs: ['dragon', 'robot'], files: [] },
    [`${HOME}/Polysquish/dragon`]: { dirs: [], files: [F('dragon.glb', 1532211), F('dragon_albedo.png', 2200000), F('report.html', 20000)] },
    [`${HOME}/Polysquish/robot`]: { dirs: [], files: [F('robot.glb', 980000), F('report.html', 19000)] },
  };
  const WATCH_FILES = ['gargoyle.glb', 'sword_lowres.obj', 'barrel.ply', 'knight_helmet.glb', 'tree_stump.obj'];
  const watchJobsOf = (w) => w.job_ids.map((id) => jobs.get(id)).filter(Boolean);
  const publicWatch = (w) => {
    const js = watchJobsOf(w); js.forEach(tick);
    return { id: w.id, folder: w.folder, output_dir: w.output_dir, preset: w.preset, processed: js.filter((j) => j.status === 'done').length, queued: js.filter((j) => !isTerminal(j.status)).length, job_ids: [...w.job_ids], active: w.active };
  };

  return {
    async health() { await latency(); return { version: '0.2.0-mock', threads: 8, output_root: `${HOME}/Polysquish`, gpu: { ...gpu } }; },
    async presets() { await latency(); return deepClone(presets); },
    async upload(files) {
      await sleep(400 + Math.random() * 400);
      const list = [...files];
      const main = list.find((f) => ACCEPTED.includes(extOf(f.name)));
      if (!main) throw new ApiError(`No supported model in the drop. Accepted: ${ACCEPTED.join(' ')}`, 400);
      const up = { upload_id: nid('u'), main_file: main.name, files: list.map((f) => f.name), size_bytes: Math.max(list.reduce((a, f) => a + (f.size || 0), 0), 98000000) };
      uploads.set(up.upload_id, up);
      return deepClone(up);
    },
    async uploadPath(path) {
      await latency();
      if (!path || !path.startsWith('/') && !/^[A-Za-z]:[\\/]/.test(path)) throw new ApiError('Please give an absolute path, e.g. /home/me/models/dragon.obj', 400);
      if (!ACCEPTED.includes(extOf(path))) throw new ApiError(`Unsupported file type "${extOf(path) || '(none)'}". Accepted: ${ACCEPTED.join(' ')}`, 400);
      const up = { upload_id: nid('u'), main_file: baseName(path), files: [baseName(path)], size_bytes: 98000000 };
      uploads.set(up.upload_id, up);
      return deepClone(up);
    },
    async inspect(upload_id) {
      await latency();
      const up = uploads.get(upload_id);
      if (!up) throw new ApiError('Unknown upload id', 404);
      const job = makeJob('inspect', INSPECT_STAGES, (j) => ({ report: sourceReport(up.size_bytes), preview_url: `/api/jobs/${j.id}/preview/source.glb` }), {}, { name: fileStem(up.main_file) });
      return { job_id: job.id };
    },
    async squish({ upload_id, recipe, name }) {
      await latency();
      const up = uploads.get(upload_id);
      if (!up) throw new ApiError('Unknown upload id', 404);
      if (!name) throw new ApiError('Output name is required', 400);
      return { job_id: createSquish(up, normalizeRecipe(recipe), name).id };
    },
    async batch({ upload_ids, recipe }) {
      await latency();
      if (!Array.isArray(upload_ids) || !upload_ids.length) throw new ApiError('upload_ids must be a non-empty array', 400);
      const ups = upload_ids.map((id) => { const u = uploads.get(id); if (!u) throw new ApiError(`Unknown upload id ${id}`, 404); return u; });
      const r = normalizeRecipe(recipe);
      const job_ids = ups.map((u) => createSquish(u, r, fileStem(u.main_file)).id);
      const b = { id: nid('b'), job_ids };
      batches.set(b.id, b);
      return { batch_id: b.id, job_ids: [...job_ids] };
    },
    async batchStatus(id) {
      await sleep(40 + Math.random() * 60);
      const b = batches.get(id);
      if (!b) throw new ApiError('Unknown batch id', 404);
      const js = b.job_ids.map((j) => jobs.get(j)); js.forEach(tick);
      const done = js.filter((j) => isTerminal(j.status)).length;
      return { id: b.id, job_ids: [...b.job_ids], done, total: js.length, status: done === js.length ? (js.some((j) => j.status === 'error') ? 'error' : 'done') : js.some((j) => j.status === 'running') ? 'running' : 'queued' };
    },
    async job(id) {
      await sleep(40 + Math.random() * 60);
      const job = jobs.get(id);
      if (!job) throw new ApiError('Unknown job id', 404);
      return publicJob(job);
    },
    async jobs() {
      await sleep(40 + Math.random() * 60);
      return [...jobs.values()].map((j) => publicJob(j, { log: false }));
    },
    async cancel(id) {
      await latency();
      const job = jobs.get(id);
      if (!job) throw new ApiError('Unknown job id', 404);
      tick(job);
      if (job.status === 'running' || job.status === 'queued') {
        job.status = 'cancelled';
        for (const s of job.stages) if (s.status === 'running') s.status = 'cancelled';
        job.log.push('Cancelled by user.');
      }
      return { ok: true };
    },
    async watchList() { await latency(); return [...watches.values()].filter((w) => w.active).map(publicWatch); },
    async watchStart({ folder, output_dir = null, recipe, preset }) {
      await latency();
      if (!folder || !folder.startsWith('/') && !/^[A-Za-z]:[\\/]/.test(folder)) throw new ApiError('Please give an absolute folder path.', 400);
      if (!FS[folder]) throw new ApiError(`Folder not found: ${folder}`, 404);
      const w = { id: nid('w'), folder, output_dir, preset: preset || recipe?.preset || 'custom', recipe: normalizeRecipe(recipe), job_ids: [], active: true, _n: 0 };
      const discover = () => {
        if (!w.active || w._n >= WATCH_FILES.length) { clearInterval(w._timer); return; }
        const fname = WATCH_FILES[w._n++];
        const up = { upload_id: nid('u'), main_file: fname, files: [fname], size_bytes: 20000000 + Math.round(Math.random() * 60000000) };
        uploads.set(up.upload_id, up);
        const job = createSquish(up, w.recipe, fileStem(fname));
        job.log.unshift(`Discovered ${folder}/${fname} (watch ${w.id})`);
        w.job_ids.push(job.id);
      };
      watches.set(w.id, w);
      setTimeout(discover, 800);                   // a file that was already sitting in the folder
      w._timer = setInterval(discover, 5000);      // …then a new one every ~5 s
      return { watch_id: w.id };
    },
    async watchStop(id) {
      await latency();
      const w = watches.get(id);
      if (!w) throw new ApiError('Unknown watch id', 404);
      w.active = false; clearInterval(w._timer);
      return { ok: true };
    },
    async fs(path) {
      await latency();
      let p = (path || HOME).replace(/\/+$/, '') || '/';
      const node = FS[p];
      if (!node) throw new ApiError(`Folder not found: ${p}`, 404);
      const parent = p === '/' ? null : (p.replace(/\/[^/]*$/, '') || '/');
      return { path: p, parent, dirs: [...node.dirs], files: deepClone(node.files) };
    },
    async openFolder(path) { await latency(); if (!path) throw new ApiError('path is required', 400); return { ok: true }; },
    fileUrl: (jobId, name) => dataUrlFor(name),
    zipUrl: () => 'data:application/zip;base64,UEsFBgAAAAAAAAAAAAAAAAAAAAAAAA==',
    /* Stand-ins for the preview GLBs the real backend serves. Returns a THREE object (source/result)
       or a blob: URL of a real GLB (heat-maps) so the GLTFLoader path is exercised too. */
    previewObject(url) {
      if (/heatmap_deviation\.glb/.test(url)) return heatGLB('deviation');
      if (/heatmap_density\.glb/.test(url)) return heatGLB('density');
      const isResult = /result\.glb/.test(url);
      const group = new THREE.Group();
      if (!isResult) {
        // dense "AI generated" source: ~196.8k triangles with vertex colors + a few floaters
        const geo = new THREE.TorusKnotGeometry(1, 0.34, 820, 120, 2, 3);
        const pos = geo.attributes.position; const col = new Float32Array(pos.count * 3);
        const a = new THREE.Color('#ff6ad5'), b = new THREE.Color('#9b7bff'), c = new THREE.Color('#5ff2c3'), tmp = new THREE.Color();
        for (let i = 0; i < pos.count; i++) {
          const t = (pos.getY(i) + 1.4) / 2.8;
          tmp.copy(a).lerp(b, clamp(t * 1.4, 0, 1)).lerp(c, clamp((pos.getX(i) + 1.4) / 2.8 * 0.5, 0, 0.5));
          col[i * 3] = tmp.r; col[i * 3 + 1] = tmp.g; col[i * 3 + 2] = tmp.b;
        }
        geo.setAttribute('color', new THREE.BufferAttribute(col, 3));
        const n = geo.attributes.normal;
        for (let i = 0; i < pos.count; i++) {
          const d = (Math.sin(pos.getX(i) * 23) * Math.cos(pos.getY(i) * 19) * Math.sin(pos.getZ(i) * 29)) * 0.012;
          pos.setXYZ(i, pos.getX(i) + n.getX(i) * d, pos.getY(i) + n.getY(i) * d, pos.getZ(i) + n.getZ(i) * d);
        }
        geo.computeVertexNormals();
        group.add(new THREE.Mesh(geo, new THREE.MeshStandardMaterial({ vertexColors: true, roughness: 0.55, metalness: 0.05 })));
        const fl = new THREE.MeshStandardMaterial({ color: '#c9b8ff', roughness: 0.6 });
        for (let i = 0; i < 14; i++) {
          const m = new THREE.Mesh(new THREE.IcosahedronGeometry(0.02 + Math.random() * 0.03, 0), fl);
          const ang = i / 14 * Math.PI * 2;
          m.position.set(Math.cos(ang) * (1.25 + Math.random() * .2), (Math.random() - .5) * 1.2, Math.sin(ang) * (1.25 + Math.random() * .2));
          group.add(m);
        }
      } else {
        // squished result: exactly 30,000 triangles (250 × 60 × 2) with a baked albedo
        const geo = new THREE.TorusKnotGeometry(1, 0.34, 250, 60, 2, 3);
        const tex = new THREE.CanvasTexture(canvasTexture('albedo'));
        tex.colorSpace = THREE.SRGBColorSpace; tex.wrapS = tex.wrapT = THREE.RepeatWrapping; tex.repeat.set(6, 1);
        group.add(new THREE.Mesh(geo, new THREE.MeshStandardMaterial({ map: tex, roughness: 0.5, metalness: 0.08 })));
      }
      return group;
    },
  };
}

const api = MOCK ? createMockApi() : realApi;

/* =====================================================================
   3D viewer
   ===================================================================== */
let sharedEnvironment = null;
class Viewer {
  constructor(el, { hud = true, autoRotate = true } = {}) {
    this.el = el;
    this.slots = new Map();
    this.active = null;
    this.wireframe = false;
    this.fitted = false;
    this.disposed = false;
    this._loading = 0;
    this.linked = null;
    this.pair = null;      // { leader } shared with the linked viewer

    this.renderer = new THREE.WebGLRenderer({ antialias: true, alpha: true, powerPreference: 'high-performance' });
    this.renderer.setClearColor(0x000000, 0);
    this.renderer.setPixelRatio(Math.min(window.devicePixelRatio || 1, 2));
    this.renderer.outputColorSpace = THREE.SRGBColorSpace;
    this.renderer.toneMapping = THREE.ACESFilmicToneMapping;
    this.renderer.toneMappingExposure = 1.05;
    el.append(this.renderer.domElement);

    this.scene = new THREE.Scene();
    if (!sharedEnvironment) {
      const pmrem = new THREE.PMREMGenerator(this.renderer);
      sharedEnvironment = pmrem.fromScene(new RoomEnvironment(), 0.04).texture;
      pmrem.dispose();
    }
    this.scene.environment = sharedEnvironment;
    this.scene.environmentIntensity = 0.4;

    this.camera = new THREE.PerspectiveCamera(38, 1, 0.01, 1000);
    this.camera.position.set(2.2, 1.4, 3.2);

    this.controls = new OrbitControls(this.camera, this.renderer.domElement);
    this.controls.enableDamping = true;
    this.controls.dampingFactor = 0.08;
    this.controls.autoRotate = autoRotate;
    this.controls.autoRotateSpeed = 0.7;
    this.controls.addEventListener('start', () => {
      this.controls.autoRotate = false;
      if (!this.linked) return;
      this.linked.controls.autoRotate = false;
      // the viewer being touched becomes the leader; drop the other's damping momentum so it mirrors cleanly
      const old = this.pair.leader;
      if (old !== this) { old.controls._sphericalDelta?.set(0, 0, 0); old.controls._panOffset?.set(0, 0, 0); old.controls._scale = 1; this.pair.leader = this; }
    });

    // identical soft 3-point rig + hemisphere in every viewer (side-by-side stays comparable)
    const hemi = new THREE.HemisphereLight(0xc9bbff, 0x2a1545, 0.55);
    const key = new THREE.DirectionalLight(0xffffff, 1.5); key.position.set(3, 4, 5);
    const fill = new THREE.DirectionalLight(0x9b7bff, 0.9); fill.position.set(-4, 1.5, 2);
    const rim = new THREE.DirectionalLight(0xff6ad5, 1.4); rim.position.set(-1, 3, -5);
    this.scene.add(hemi, key, fill, rim);

    this.wireMat = new THREE.MeshBasicMaterial({ color: 0xff6ad5, wireframe: true, transparent: true, opacity: 0.25, depthTest: true, polygonOffset: true, polygonOffsetFactor: -1, polygonOffsetUnits: -1 });

    if (hud) {
      this.hud = h('div', { class: 'viewer-hud' }, h('span', { class: 'hud-strong', text: '—' }), h('span', { text: 'preview' }));
      this.hint = h('div', { class: 'viewer-hint', text: 'drag to orbit · wheel to zoom' });
      this.loadingEl = h('div', { class: 'viewer-loading' }, h('span', { class: 'spinner' }), 'Loading preview…');
      this.loadingEl.hidden = true;
      el.append(this.hud, this.hint, this.loadingEl);
    }

    this._ro = new ResizeObserver(() => this.resize());
    this._ro.observe(el);
    this.resize();
    this._tick = this._tick.bind(this);
    this._raf = requestAnimationFrame(this._tick);
  }
  resize() {
    const w = Math.max(1, this.el.clientWidth), hgt = Math.max(1, this.el.clientHeight);
    this.renderer.setSize(w, hgt, false);
    this.camera.aspect = w / hgt;
    this.camera.updateProjectionMatrix();
  }
  _visible() { return this.el.isConnected && this.el.getClientRects().length > 0 && this.el.clientWidth > 0; }
  _tick() {
    if (this.disposed) return;
    this._raf = requestAnimationFrame(this._tick);
    if (!this._visible()) return;
    if (!this.pair || this.pair.leader === this) {
      this.controls.update();
      if (this.linked) this.syncTo(this.linked);   // followers never integrate their own controls
    }
    this.renderer.render(this.scene, this.camera);
  }
  setLoading(on) {
    this._loading = Math.max(0, this._loading + (on ? 1 : -1));
    if (this.loadingEl) this.loadingEl.hidden = this._loading === 0;
  }
  async loadGLB(name, url, opts = {}) {
    this.setLoading(true);
    try {
      const loader = new GLTFLoader();
      const gltf = await loader.loadAsync(url);
      const root = gltf.scene || gltf.scenes[0];
      root.traverse((o) => {
        if (!o.isMesh) return;
        if (opts.unlit) {
          // heat-maps: COLOR_0 shown as-is, no lighting or tone mapping so the legend matches
          const old = Array.isArray(o.material) ? o.material : [o.material];
          o.material = new THREE.MeshBasicMaterial({ vertexColors: !!o.geometry.attributes.color, toneMapped: false, side: THREE.FrontSide });
          for (const m of old) m?.dispose?.();
          return;
        }
        const mats = Array.isArray(o.material) ? o.material : [o.material];
        for (const m of mats) {
          if (!m) continue;
          if (o.geometry.attributes.color && !m.map) { m.vertexColors = true; m.needsUpdate = true; }
          if (m.map) m.map.colorSpace = THREE.SRGBColorSpace;
          m.side = m.side === THREE.DoubleSide ? m.side : THREE.FrontSide;
        }
      });
      return this.setSlot(name, root, opts);
    } finally { this.setLoading(false); }
  }
  setSlot(name, object, { fit = 'auto', show = true } = {}) {
    this.removeSlot(name);
    const root = new THREE.Group(); root.name = `slot:${name}`; root.add(object);
    const wire = new THREE.Group(); wire.name = 'wire'; wire.visible = this.wireframe;
    object.updateMatrixWorld(true);
    let tris = 0;
    object.traverse((o) => {
      if (!o.isMesh || o.userData.isWire) return;
      const g = o.geometry;
      tris += g.index ? g.index.count / 3 : (g.attributes.position ? g.attributes.position.count / 3 : 0);
      const w = new THREE.Mesh(g, this.wireMat); w.userData.isWire = true; w.renderOrder = 1;
      w.matrixAutoUpdate = false; w.matrix.copy(o.matrixWorld); w.matrixWorldNeedsUpdate = true;
      wire.add(w);
    });
    root.add(wire);
    root.visible = false;
    this.scene.add(root);
    this.slots.set(name, { root, wire, tris: Math.round(tris) });
    if (fit === true || (fit === 'auto' && !this.fitted)) this.fitTo(object);
    if (show) this.show(name);
    this.el.dataset.loaded = [...this.slots.keys()].join(' ');
    return this.slots.get(name);
  }
  removeSlot(name) {
    const s = this.slots.get(name);
    if (!s) return;
    this.scene.remove(s.root);
    s.root.traverse((o) => {
      if (o.userData.isWire) return;
      if (o.isMesh) {
        o.geometry?.dispose();
        const mats = Array.isArray(o.material) ? o.material : [o.material];
        for (const m of mats) {
          if (!m) continue;
          for (const k of ['map', 'normalMap', 'roughnessMap', 'metalnessMap', 'aoMap', 'emissiveMap', 'alphaMap']) m[k]?.dispose?.();
          m.dispose();
        }
      }
    });
    this.slots.delete(name);
    if (this.active === name) this.active = null;
  }
  show(name) {
    for (const [k, s] of this.slots) s.root.visible = k === name;
    this.active = name;
    const s = this.slots.get(name);
    if (this.hud) this.hud.firstElementChild.textContent = s ? `${fmtInt(s.tris)} tris` : '—';
    if (name) this.el.dataset.active = name; else delete this.el.dataset.active;
  }
  setWireframe(on) {
    this.wireframe = !!on;
    for (const s of this.slots.values()) s.wire.visible = this.wireframe;
  }
  fitTo(object) {
    const box = new THREE.Box3().setFromObject(object);
    if (box.isEmpty()) return;
    const size = box.getSize(new THREE.Vector3()), center = box.getCenter(new THREE.Vector3());
    const maxDim = Math.max(size.x, size.y, size.z) || 1;
    const fov = THREE.MathUtils.degToRad(this.camera.fov);
    const dist = (maxDim / 2) / Math.tan(fov / 2) * 1.25;
    const dir = new THREE.Vector3(1, 0.55, 1.35).normalize();
    this.camera.position.copy(center).addScaledVector(dir, dist);
    this.camera.near = Math.max(0.001, dist / 100); this.camera.far = dist * 100;
    this.camera.updateProjectionMatrix();
    this.controls.target.copy(center);
    this.controls.minDistance = maxDim * 0.2; this.controls.maxDistance = dist * 6;
    this.controls.update();
    this.fitted = true;
  }
  /* copy this camera pose to another viewer (side-by-side). OrbitControls re-derives its spherical
     state from the camera on its next update, so the follower picks up seamlessly when it takes the lead. */
  syncTo(other) {
    if (!other || other.disposed) return;
    other.camera.position.copy(this.camera.position);
    other.camera.quaternion.copy(this.camera.quaternion);
    other.camera.zoom = this.camera.zoom; other.camera.near = this.camera.near; other.camera.far = this.camera.far;
    other.camera.updateProjectionMatrix();
    other.controls.target.copy(this.controls.target);
    other.controls.minDistance = this.controls.minDistance; other.controls.maxDistance = this.controls.maxDistance;
  }
  link(other) {
    this.linked = other; other.linked = this;
    this.pair = other.pair = { leader: this };
    other.controls.autoRotate = false;       // the leader drives; the follower only mirrors
    this.syncTo(other);
  }
  unlink() { if (this.linked) { this.linked.linked = null; this.linked.pair = null; this.linked = null; } this.pair = null; }
  clear() { for (const k of [...this.slots.keys()]) this.removeSlot(k); this.fitted = false; this._loading = 0; if (this.loadingEl) this.loadingEl.hidden = true; delete this.el.dataset.loaded; delete this.el.dataset.active; if (this.hud) this.hud.firstElementChild.textContent = '—'; }
  dispose() {
    this.disposed = true; cancelAnimationFrame(this._raf); this._ro.disconnect(); this.clear(); this.unlink();
    this.controls.dispose(); this.wireMat.dispose(); this.renderer.dispose(); this.renderer.domElement.remove();
  }
}

/* =====================================================================
   App state
   ===================================================================== */
const MAIN_VIEWS = ['drop', 'inspect', 'batch', 'progress', 'results'];
const state = {
  view: 'drop',
  mainView: 'drop',
  health: null,
  presets: [],
  upload: null,
  inspectJobId: null,
  inspect: null,        // { report, preview_url }
  presetId: null,
  recipe: null,
  name: '',
  squishJobId: null,
  job: null,
  result: null,
  resultJobId: null,
  resultsFrom: 'single', // single | queue
  retry: null,          // () => void for the error view
  pollAbort: null,
  batch: null,          // { groups: [{ main, files, status, upload, error }] }
  queue: { batchId: null, batch: null, jobIds: [], names: new Map(), jobs: new Map(), all: [], watches: [] },
  watches: [],
  res: { mode: 'shaded', side: 'after', split: false, wire: false, token: 0, preview: null, metrics: null },
};
const viewers = {};
window.__polysquish = { state, viewers, MOCK, fmtInt, fmtShort, fmtBytes, fmtSeconds, fmtPct };

function showView(name) {
  state.view = name;
  if (MAIN_VIEWS.includes(name)) state.mainView = name;
  for (const v of document.querySelectorAll('.view')) v.hidden = v.dataset.view !== name;
  document.body.dataset.view = name;
  $('#nav-queue').setAttribute('aria-pressed', String(name === 'queue'));
  $('#nav-watch').setAttribute('aria-pressed', String(name === 'watch'));
  window.scrollTo({ top: 0, behavior: 'instant' in window ? 'instant' : 'auto' });
  const heading = $(`#view-${name} h1`);
  if (heading) { heading.setAttribute('tabindex', '-1'); heading.focus({ preventScroll: true }); }
  if (name === 'queue') startQueuePolling();
  if (name === 'watch') startWatchPolling();
}
function showError(title, err, retry) {
  $('#error-title').textContent = title;
  $('#error-msg').textContent = err?.message || String(err);
  state.retry = retry;
  $('#error-retry').hidden = !retry;
  showView('error');
}
function mountRecipe(where) {
  const block = $('#recipe-block');
  const slot = where === 'batch' ? $('#batch-recipe-slot') : $('#inspect-recipe-slot');
  if (block.parentElement !== slot) slot.prepend(block);
}

/* ---------- Job polling ---------- */
async function pollJob(id, onUpdate) {
  const token = { cancelled: false };
  state.pollAbort = () => { token.cancelled = true; };
  for (;;) {
    const job = await api.job(id);
    if (token.cancelled) return null;
    onUpdate?.(job);
    if (isTerminal(job.status)) return job;
    await sleep(POLL_MS);
    if (token.cancelled) return null;
  }
}

/* =====================================================================
   1. Drop hero
   ===================================================================== */
function initDrop() {
  const dz = $('#dropzone'), input = $('#file-input');
  const openBrowse = () => input.click();
  $('#browse-btn').addEventListener('click', (e) => { e.stopPropagation(); openBrowse(); });
  dz.addEventListener('click', (e) => { if (e.target.closest('button, input, form, a')) return; openBrowse(); });
  dz.addEventListener('keydown', (e) => { if ((e.key === 'Enter' || e.key === ' ') && e.target === dz) { e.preventDefault(); openBrowse(); } });
  input.addEventListener('change', () => { if (input.files.length) startWithFiles([...input.files]); input.value = ''; });

  let depth = 0;
  dz.addEventListener('dragenter', (e) => { e.preventDefault(); depth++; dz.classList.add('is-over'); });
  dz.addEventListener('dragover', (e) => { e.preventDefault(); e.dataTransfer.dropEffect = 'copy'; });
  dz.addEventListener('dragleave', () => { if (--depth <= 0) { depth = 0; dz.classList.remove('is-over'); } });
  dz.addEventListener('drop', (e) => {
    e.preventDefault(); depth = 0; dz.classList.remove('is-over');
    const files = [...(e.dataTransfer?.files || [])];
    if (files.length) startWithFiles(files);
  });
  for (const ev of ['dragover', 'drop']) document.addEventListener(ev, (e) => { if (!dz.contains(e.target)) e.preventDefault(); });

  const toggle = $('#path-toggle'), row = $('#path-row');
  toggle.addEventListener('click', () => {
    const open = row.hidden;
    row.hidden = !open; toggle.setAttribute('aria-expanded', String(open));
    if (open) $('#path-input').focus();
  });
  row.addEventListener('submit', (e) => { e.preventDefault(); const p = $('#path-input').value.trim(); if (p) startWithPath(p); });
}

/* Group a drop into models + their sidecars. Sidecars go with the model whose stem they share;
   leftovers (e.g. textures named differently) travel with the first model. */
function groupDrop(files) {
  const models = files.filter((f) => ACCEPTED.includes(extOf(f.name)));
  const groups = models.map((m) => ({ main: m, files: [m], stem: fileStem(m.name).toLowerCase(), status: 'ready', upload: null, error: null }));
  const leftovers = [];
  for (const f of files) {
    if (models.includes(f)) continue;
    const st = fileStem(f.name).toLowerCase();
    const g = groups.find((x) => st === x.stem) || groups.find((x) => st.startsWith(x.stem + '_') || st.startsWith(x.stem + '.') || st.startsWith(x.stem + '-'));
    if (g) g.files.push(f); else leftovers.push(f);
  }
  if (groups[0]) groups[0].files.push(...leftovers);
  return groups;
}
async function startWithFiles(files) {
  const groups = groupDrop(files);
  if (!groups.length) {
    toast({ kind: 'error', title: 'No supported model in that drop', msg: `Accepted main files: ${ACCEPTED.join('  ')}. Drop the model together with its .mtl / textures.` });
    return;
  }
  if (groups.length > 1) { openBatch(groups); return; }
  const main = groups[0].main;
  const dz = $('#dropzone');
  dz.classList.add('is-busy'); dz.setAttribute('aria-busy', 'true');
  const t = toast({ title: `Uploading ${main.name}…`, msg: files.length > 1 ? `${files.length} files, ${fmtBytes(files.reduce((a, f) => a + f.size, 0))}` : fmtBytes(main.size), timeout: 0 });
  try {
    const up = await api.upload(groups[0].files);
    t();
    await handleUpload(up);
  } catch (err) {
    t();
    toast({ kind: 'error', title: 'Upload failed', msg: err.message });
  } finally {
    dz.classList.remove('is-busy'); dz.removeAttribute('aria-busy');
  }
}
async function startWithPath(path) {
  const btn = $('#path-submit'); btn.disabled = true;
  try {
    const up = await api.uploadPath(path);
    await handleUpload(up);
  } catch (err) {
    toast({ kind: 'error', title: "Couldn't load that path", msg: err.message });
  } finally { btn.disabled = false; }
}

/* =====================================================================
   2. Inspect: health + presets + recipe
   ===================================================================== */
async function handleUpload(up) {
  state.upload = up;
  state.inspect = null; state.result = null; state.job = null; state.squishJobId = null;
  state.name = fileStem(up.main_file);
  $('#output-name').value = state.name;
  $('#health-file-chip').textContent = `${up.main_file}${up.files.length > 1 ? ` +${up.files.length - 1}` : ''}`;
  $('#health-file-chip').title = up.files.join(', ');
  $('#inspect-title').textContent = 'Health check';
  renderHealthSkeleton();
  viewers.source.clear();
  viewers.source.setLoading(true);
  if (!state.presetId) selectPreset(state.presets[0]?.id);
  mountRecipe('inspect');
  showView('inspect');
  await runInspect();
}
async function runInspect() {
  const up = state.upload;
  try {
    const { job_id } = await api.inspect(up.upload_id);
    state.inspectJobId = job_id;
    const job = await pollJob(job_id, (j) => {
      const note = $('#health-skel-note');
      if (note && j.stage_label) note.textContent = `${j.stage_label}… ${Math.round((j.progress || 0) * 100)}%`;
    });
    if (!job) return;
    if (job.status === 'error') throw new ApiError(job.error || 'Inspection failed');
    if (job.status === 'cancelled') throw new ApiError('Inspection was cancelled');
    state.inspect = job.result;
    renderHealth(job.result.report, up);
    viewers.source.setLoading(false);
    loadPreview(viewers.source, 'source', job.result.preview_url, { fit: true }).catch((e) => toast({ kind: 'warn', title: "Couldn't load the source preview", msg: e.message }));
  } catch (err) {
    viewers.source.setLoading(false);
    showError("Couldn't inspect that model", err, () => { renderHealthSkeleton(); viewers.source.setLoading(true); showView('inspect'); runInspect(); });
  }
}
function renderHealthSkeleton() {
  $('#health-card').setAttribute('aria-busy', 'true');
  $('#health-body').replaceChildren(
    h('div', { class: 'sk-row' }, h('div', { class: 'skeleton sk-stat' }), h('div', { class: 'skeleton sk-stat' }), h('div', { class: 'skeleton sk-stat' })),
    h('div', { class: 'skeleton sk-line w80' }), h('div', { class: 'skeleton sk-line w60' }), h('div', { class: 'skeleton sk-line w40' }),
    h('p', { class: 'skeleton-note' }, h('span', { class: 'spinner', 'aria-hidden': 'true' }), h('span', { id: 'health-skel-note', text: 'Reading your model…' })));
  $('#problems-chip').textContent = '…';
  $('#problems-chip').className = 'chip chip-ghost';
  $('#problems-body').replaceChildren(h('div', { class: 'skeleton sk-line w80' }), h('div', { class: 'skeleton sk-line w60' }), h('div', { class: 'skeleton sk-line w40' }));
}
function renderHealth(r, up) {
  $('#health-card').setAttribute('aria-busy', 'false');
  const stat = (label, value, sub) => h('div', { class: 'stat' }, h('span', { class: 'stat-label', text: label }), h('span', { class: 'stat-value', text: value, title: sub || '' }), sub ? h('span', { class: 'stat-sub', text: sub }) : null);
  const yn = (b) => h('span', { class: b ? 'ok' : 'no', text: b ? 'yes' : 'no' });
  const fact = (label, val) => h('span', { class: 'fact' }, label, h('b', {}, val));
  const sizeBytes = up?.size_bytes ?? r.size_bytes;
  $('#health-body').replaceChildren(
    h('div', { class: 'stat-row' },
      stat('Triangles', fmtShort(r.triangles), fmtInt(r.triangles)),
      stat('Vertices', fmtShort(r.vertices), fmtInt(r.vertices)),
      stat('File size', fmtBytes(sizeBytes), up?.files?.length > 1 ? `${up.files.length} files` : null)),
    h('div', { class: 'fact-grid' },
      fact('Pieces ', `${fmtInt(r.components)}`), fact('Watertight ', yn(r.watertight)), fact('Normals ', yn(r.has_normals)), fact('UVs ', yn(r.has_uvs)),
      fact('Vertex colors ', yn(r.has_vertex_colors)), fact('Materials ', `${r.materials ?? 0}`),
      r.textures?.length ? fact('Textures ', `${r.textures.length} (${r.textures[0].width}×${r.textures[0].height})`) : null,
      r.bounds ? fact('Size ', `${r.bounds.size.map((v) => Number(v).toFixed(2)).join(' × ')} ${r.units_guess || ''}`.trim()) : null,
      r.up_axis_guess ? fact('Up axis ', r.up_axis_guess) : null),
    h('div', { class: 'verdict' }, svg(ICONS.sparkles, 20), h('p', { text: verdictFor(r) })));

  const problems = r.problems || [];
  const counts = { error: 0, warn: 0, info: 0 };
  for (const p of problems) counts[p.severity] = (counts[p.severity] || 0) + 1;
  const chip = $('#problems-chip');
  if (counts.error) { chip.textContent = `${counts.error} serious`; chip.className = 'chip chip-coral'; }
  else if (counts.warn) { chip.textContent = `${counts.warn} to clean up`; chip.className = 'chip chip-amber'; }
  else { chip.textContent = 'Looks healthy'; chip.className = 'chip chip-mint'; }
  $('#problems-heading').textContent = problems.length ? `${problems.length} thing${problems.length === 1 ? '' : 's'} we noticed` : 'Nothing to fix';
  $('#problems-body').replaceChildren(problems.length
    ? h('ul', { class: 'problems' }, problems.map((p) => h('li', { class: 'problem' },
        h('span', { class: `sev sev-${p.severity || 'info'}`, text: p.severity || 'info' }),
        h('div', {}, h('div', { class: 'problem-title', text: p.title }), p.detail ? h('div', { class: 'problem-detail', text: p.detail }) : null, p.fix ? h('div', { class: 'problem-fix', text: p.fix }) : null))))
    : h('div', { class: 'all-clear' }, svg(ICONS.shield, 20), 'This mesh is already clean. Squishing will just make it smaller.'));
}
function verdictFor(r) {
  const parts = [];
  const t = r.triangles || 0;
  if (t > 150000) parts.push(`At ${fmtShort(t)} triangles this is about ${fmtInt(Math.round(t / 30000))}× denser than a hero game asset needs, so there is a lot of room to squish.`);
  else if (t > 20000) parts.push(`${fmtShort(t)} triangles is already in the ballpark for a hero asset; squishing will mostly tidy it up and build LODs.`);
  else parts.push(`${fmtShort(t)} triangles is light already; squishing will focus on cleanup, textures and LODs.`);
  if ((r.components || 1) > 1) parts.push(`It comes in ${fmtInt(r.components)} pieces${r.largest_component_fraction ? `, the biggest being ${(r.largest_component_fraction * 100).toFixed(1)}% of the surface` : ''}, so the small ones are probably noise.`);
  if (!r.has_uvs) parts.push(r.has_vertex_colors ? 'There are no UVs yet, so a layout will be generated and the vertex colors baked into textures.' : 'There are no UVs yet, so a fresh layout will be generated for baking.');
  else if (!r.watertight) parts.push('The surface has open edges; collision shapes will be built from a closed hull instead.');
  return parts.join(' ');
}
async function loadPreview(viewer, slot, url, opts = {}) {
  const o = api.previewObject ? api.previewObject(url) : null;
  if (o && typeof o !== 'string') return viewer.setSlot(slot, o, opts);
  return viewer.loadGLB(slot, typeof o === 'string' ? o : url, opts);
}

/* ---------- Presets ---------- */
function renderPresets() {
  const grid = $('#preset-grid');
  const list = state.presets.filter((p) => p.id !== 'custom');
  const shown = list.length ? list : state.presets;
  grid.replaceChildren(...shown.map((p) => h('button', {
    class: 'preset', type: 'button', role: 'radio', 'aria-checked': String(p.id === state.presetId), dataset: { preset: p.id },
    onclick: () => selectPreset(p.id),
  },
    h('div', { class: 'preset-top' }, h('span', { class: 'preset-ico' }, svg(presetIcon(p.icon || p.id), 22)), h('span', { class: 'preset-check' }, svg(ICONS.check, 12, 'stroke-width="3"'))),
    h('div', { class: 'preset-name', text: p.name }),
    h('div', { class: 'preset-tagline', text: p.tagline || p.description || '' }),
    h('div', { class: 'preset-meta' }, h('span', { text: `${fmtShort(p.target_triangles ?? p.recipe?.decimate?.target_triangles)} tris` }), h('span', { text: `${p.texture_size ?? p.recipe?.bake?.resolution}px` }),
      p.recipe?.lods?.count ? h('span', { text: `${p.recipe.lods.count} LOD${p.recipe.lods.count === 1 ? '' : 's'}` }) : null,
      p.recipe?.export?.skin && p.id === 'character' ? h('span', { text: 'rig' }) : null))));
  updatePresetChip();
}
function selectPreset(id) {
  const p = state.presets.find((x) => x.id === id);
  if (!p) return;
  state.presetId = id;
  state.recipe = normalizeRecipe(deepClone(p.recipe));
  state.recipe.preset = id;
  for (const b of document.querySelectorAll('.preset')) b.setAttribute('aria-checked', String(b.dataset.preset === id));
  updatePresetChip();
  renderAdvanced();
}
function markCustom() {
  if (state.presetId === 'custom') return;
  state.presetId = 'custom';
  state.recipe.preset = 'custom';
  for (const b of document.querySelectorAll('.preset')) b.setAttribute('aria-checked', 'false');
  updatePresetChip();
}
const TOPO = [
  ['triangles', 'Triangles', 'Classic decimation. Best fidelity per triangle — what game engines render anyway.'],
  ['quad_dominant', 'Quad-dominant', 'Cleaner edge flow for sculpting, subdivision and DCC hand-offs. A little heavier for the same detail.'],
  ['voxel', 'Voxel rebuild', 'Remeshes from a solid voxel shell first: fixes self-intersections, shells inside shells and broken scans.'],
];
function updatePresetChip() {
  const chip = $('#preset-active-chip');
  const p = state.presets.find((x) => x.id === state.presetId);
  const custom = state.presetId === 'custom';
  chip.textContent = custom ? 'Custom recipe' : p ? p.name : '—';
  chip.className = `chip ${custom ? 'chip-violet' : 'chip-pink'}`;
  const r = state.recipe;
  if (r) {
    const topo = TOPO.find((t) => t[0] === r.retopo?.mode);
    $('#adv-summary').textContent = [`${fmtShort(r.decimate.target_triangles)} triangles`, topo && topo[0] !== 'triangles' ? topo[1].toLowerCase() : null,
      r.bake.enabled ? `${r.bake.resolution}px textures` : 'no baking', `${r.lods.count} LOD${r.lods.count === 1 ? '' : 's'}${r.lods.imposter ? ' + imposter' : ''}`,
      [r.export.glb && 'GLB', r.export.fbx && 'FBX', r.export.obj && 'OBJ'].filter(Boolean).join('/'), r.export.target].filter(Boolean).join(' · ');
  }
}

/* ---------- Advanced recipe panel ---------- */
const TRI_MIN = 500, TRI_MAX = 500000;
const triFromSlider = (v) => { const x = Math.exp(Math.log(TRI_MIN) + v * (Math.log(TRI_MAX) - Math.log(TRI_MIN))); const mag = Math.pow(10, Math.floor(Math.log10(x)) - 1); return clamp(Math.round(x / mag) * mag, TRI_MIN, TRI_MAX); };
const sliderFromTri = (t) => clamp((Math.log(clamp(t, TRI_MIN, TRI_MAX)) - Math.log(TRI_MIN)) / (Math.log(TRI_MAX) - Math.log(TRI_MIN)), 0, 1);
const getPath = (o, p) => p.split('.').reduce((a, k) => a?.[k], o);
const setPath = (o, p, v) => { const ks = p.split('.'); const last = ks.pop(); const t = ks.reduce((a, k) => (a[k] ??= {}), o); t[last] = v; };
const LOD_RATIOS = [0.5, 0.25, 0.1, 0.05];

function renderAdvanced() {
  const panel = $('#adv-panel');
  const r = state.recipe;
  if (!r) return;
  const onEdit = () => { markCustom(); updatePresetChip(); };
  const gpuOk = state.health?.gpu?.available !== false;

  const toggleCtl = (label, path, { onchange, disabled = false, title = null, hint = null } = {}) => {
    const input = h('input', { type: 'checkbox', checked: !!getPath(r, path), disabled });
    input.addEventListener('change', () => { setPath(r, path, input.checked); onchange?.(input.checked); onEdit(); });
    return h('label', { class: `toggle${disabled ? ' is-disabled' : ''}`, title },
      h('span', { class: 'toggle-text' }, h('span', { text: label }), hint ? h('span', { class: 'toggle-hint', text: hint }) : null), input, h('span', { class: 'switch', 'aria-hidden': 'true' }));
  };
  const selectCtl = (label, path, options, { onchange, parse = (v) => v } = {}) => {
    const sel = h('select', { class: 'select', 'aria-label': label });
    for (const [val, text] of options) sel.append(h('option', { value: String(val), text, selected: String(getPath(r, path)) === String(val) }));
    sel.addEventListener('change', () => { setPath(r, path, parse(sel.value)); onchange?.(parse(sel.value)); onEdit(); });
    return h('div', { class: 'ctl' }, h('span', { class: 'ctl-label', text: label }), sel);
  };
  const rangeCtl = (label, path, { min, max, step = 1, unit = '', ticks = [] } = {}) => {
    const val = h('span', { class: 'val', text: `${getPath(r, path)}${unit}` });
    const range = h('input', { type: 'range', class: 'range', min: String(min), max: String(max), step: String(step), value: String(getPath(r, path)), 'aria-label': label });
    const paint = () => range.style.setProperty('--pct', `${((range.value - min) / (max - min) * 100).toFixed(1)}%`);
    paint();
    range.addEventListener('input', () => { const v = Number(range.value); setPath(r, path, v); val.textContent = `${v}${unit}`; paint(); onEdit(); });
    return h('div', { class: 'ctl' }, h('span', { class: 'ctl-label' }, label, val), range,
      ticks.length ? h('div', { class: 'range-ticks', 'aria-hidden': 'true' }, ticks.map((t) => h('span', { text: t }))) : null);
  };

  // target triangles: log slider + number
  const triVal = h('span', { class: 'val', text: fmtInt(r.decimate.target_triangles) });
  const range = h('input', { type: 'range', class: 'range', min: '0', max: '1000', step: '1', value: String(Math.round(sliderFromTri(r.decimate.target_triangles) * 1000)), 'aria-label': 'Target triangles (log scale)' });
  const num = h('input', { type: 'number', class: 'input', min: String(TRI_MIN), max: String(TRI_MAX), step: '1', value: String(r.decimate.target_triangles), 'aria-label': 'Target triangles' });
  const paintRange = () => range.style.setProperty('--pct', `${(range.value / 10).toFixed(1)}%`);
  paintRange();
  range.addEventListener('input', () => { const t = triFromSlider(range.value / 1000); r.decimate.target_triangles = t; num.value = t; triVal.textContent = fmtInt(t); paintRange(); onEdit(); });
  num.addEventListener('change', () => { const t = clamp(Math.round(Number(num.value) || TRI_MIN), TRI_MIN, TRI_MAX); r.decimate.target_triangles = t; num.value = t; range.value = Math.round(sliderFromTri(t) * 1000); triVal.textContent = fmtInt(t); paintRange(); onEdit(); });

  // topology: segmented radio group + explanation + voxel resolution
  const topoNote = h('p', { class: 'seg-note', id: 'topo-note' });
  const voxelCtl = selectCtl('Voxel resolution', 'retopo.voxel_resolution', [[128, '128³ · fast, chunky'], [256, '256³ · balanced'], [384, '384³ · detailed'], [512, '512³ · slow, finest']], { parse: Number });
  voxelCtl.id = 'voxel-ctl';
  const topoSeg = h('div', { class: 'seg seg-wide', role: 'radiogroup', 'aria-label': 'Topology mode', id: 'topo-seg' });
  const paintTopo = () => {
    const mode = r.retopo.mode;
    for (const b of topoSeg.children) { const on = b.dataset.mode === mode; b.setAttribute('aria-checked', String(on)); b.tabIndex = on ? 0 : -1; }
    topoNote.textContent = (TOPO.find((t) => t[0] === mode) || TOPO[0])[2];
    voxelCtl.hidden = mode !== 'voxel';
  };
  const setTopo = (mode, focus = false) => { r.retopo.mode = mode; paintTopo(); onEdit(); if (focus) topoSeg.querySelector('[aria-checked="true"]')?.focus(); };
  for (const [id, label] of TOPO) topoSeg.append(h('button', { type: 'button', class: 'seg-btn', role: 'radio', dataset: { mode: id }, text: label, onclick: () => setTopo(id) }));
  topoSeg.addEventListener('keydown', (e) => {
    const i = TOPO.findIndex((t) => t[0] === r.retopo.mode);
    if (e.key === 'ArrowRight' || e.key === 'ArrowDown') { e.preventDefault(); setTopo(TOPO[(i + 1) % TOPO.length][0], true); }
    if (e.key === 'ArrowLeft' || e.key === 'ArrowUp') { e.preventDefault(); setTopo(TOPO[(i + TOPO.length - 1) % TOPO.length][0], true); }
  });
  paintTopo();

  const texSelect = selectCtl('Texture size', 'bake.resolution', [[512, '512 px'], [1024, '1024 px'], [2048, '2048 px'], [4096, '4096 px']], { parse: Number, onchange: (v) => { r.uv.resolution = v; } });
  const lodSelect = selectCtl('LOD count', 'lods.count', [[0, 'None'], [1, '1 LOD'], [2, '2 LODs'], [3, '3 LODs'], [4, '4 LODs']], { parse: Number, onchange: (v) => { r.lods.ratios = LOD_RATIOS.slice(0, v); } });
  const imposterRes = selectCtl('Imposter resolution', 'lods.imposter_resolution', [[256, '256 px'], [512, '512 px'], [1024, '1024 px'], [2048, '2048 px']], { parse: Number });
  imposterRes.hidden = !r.lods.imposter;
  const imposterToggle = toggleCtl('Imposter as last LOD', 'lods.imposter', { hint: 'Billboard atlas for far distances', onchange: (v) => { imposterRes.hidden = !v; } });
  const engineSelect = selectCtl('Target engine', 'export.target', [['generic', 'Generic (glTF standard)'], ['unity', 'Unity'], ['unreal', 'Unreal Engine'], ['godot', 'Godot'], ['blender', 'Blender'], ['maya', 'Maya'], ['c4d', 'Cinema 4D']]);

  const section = (title, ...kids) => h('div', { class: 'adv-section' }, h('h3', { class: 'adv-section-title', text: title }), ...kids);
  panel.replaceChildren(
    section('Topology',
      h('div', { class: 'ctl' }, h('span', { class: 'ctl-label', text: 'Rebuild the mesh as' }), topoSeg, topoNote),
      h('div', { class: 'ctl-grid', style: 'margin-top:12px' }, voxelCtl)),
    section('Polygons',
      h('div', { class: 'ctl' },
        h('span', { class: 'ctl-label' }, 'Target triangles', triVal),
        h('div', { class: 'range-row' }, range, num),
        h('div', { class: 'range-ticks', 'aria-hidden': 'true' }, h('span', { text: '500' }), h('span', { text: '5k' }), h('span', { text: '50k' }), h('span', { text: '500k' }))),
      h('div', { class: 'ctl-grid', style: 'margin-top:12px' }, toggleCtl('Lock open borders', 'decimate.lock_border'), toggleCtl('Aggressive (flatter result)', 'decimate.aggressive'), toggleCtl('Preserve vertex colors', 'decimate.preserve_colors'), toggleCtl('Preserve existing UVs', 'decimate.preserve_uvs'),
        toggleCtl('Keep materials separate (UDIM tiles)', 'decimate.keep_materials', { hint: 'One UV tile per material instead of one atlas' }))),
    section('Cleanup',
      h('div', { class: 'ctl-grid' }, toggleCtl('Remove floating fragments', 'cleanup.remove_floaters'), toggleCtl('Weld duplicate vertices', 'cleanup.weld'), toggleCtl('Remove degenerate triangles', 'cleanup.remove_degenerate'), toggleCtl('Fix inverted faces', 'cleanup.fix_winding'),
        toggleCtl('Remove hidden interior faces', 'cleanup.remove_hidden', { hint: 'Faces no camera can ever see' }))),
    section('Textures',
      h('div', { class: 'ctl-grid' }, texSelect, h('div', { class: 'ctl' }, h('span', { class: 'ctl-label', text: 'Unwrap & bake' }), toggleCtl('Generate UVs and bake maps', 'bake.enabled', { onchange: (v) => { r.uv.enabled = v; } }))),
      h('div', { class: 'ctl-grid', style: 'margin-top:12px' }, toggleCtl('Normal map', 'bake.normal_map'), toggleCtl('Albedo (color)', 'bake.albedo'), toggleCtl('Ambient occlusion', 'bake.ao'), toggleCtl('Metallic / roughness', 'bake.metallic_roughness'),
        toggleCtl('AO denoise', 'bake.ao_denoise', { hint: 'Smooths the occlusion map' }),
        toggleCtl('Use GPU when available', 'bake.gpu', gpuOk ? { hint: state.health?.gpu?.name ? `Ray tracing on ${state.health.gpu.name}` : 'Ray tracing on the GPU' } : { disabled: true, title: 'No GPU detected on this machine — baking runs on the CPU.', hint: 'No GPU detected · CPU only' })),
      h('div', { class: 'ctl-grid one', style: 'margin-top:12px' }, rangeCtl('Hard-edge angle', 'bake.hard_edge_angle', { min: 20, max: 90, unit: '°', ticks: ['20° soft', '55°', '90° crisp'] }))),
    section('LODs & collision',
      h('div', { class: 'ctl-grid' }, lodSelect, h('div', { class: 'ctl' }, h('span', { class: 'ctl-label', text: 'Collision shapes' }), h('div', { class: 'ctl-grid one', style: 'gap:8px' }, toggleCtl('Convex hull', 'collision.convex_hull'), toggleCtl('Bounding box', 'collision.box'), toggleCtl('Simplified mesh', 'collision.simplified_mesh')))),
      h('div', { class: 'ctl-grid', style: 'margin-top:12px' }, imposterToggle, imposterRes)),
    section('Export',
      h('div', { class: 'ctl-grid' }, engineSelect, h('div', { class: 'ctl' }, h('span', { class: 'ctl-label', text: 'Files' }), h('div', { class: 'ctl-grid one', style: 'gap:8px' }, toggleCtl('GLB (binary glTF)', 'export.glb'), toggleCtl('FBX', 'export.fbx'), toggleCtl('OBJ + MTL', 'export.obj'), toggleCtl('HTML report', 'export.report')))),
      h('div', { class: 'ctl-grid', style: 'margin-top:12px' }, toggleCtl('Keep rig and animations', 'export.skin', { hint: 'Skin weights, joints and clips survive the squish' }))),
  );
}
function initAdvanced() {
  const t = $('#adv-toggle'), p = $('#adv-panel');
  t.addEventListener('click', () => { const open = p.hidden; p.hidden = !open; t.setAttribute('aria-expanded', String(open)); });
}

/* =====================================================================
   3. Squish + progress
   ===================================================================== */
let elapsedTimer = null, jobStarted = 0;
async function startSquish() {
  const name = $('#output-name').value.trim() || state.name;
  if (!name) { toast({ kind: 'warn', title: 'Give the output a name' }); $('#output-name').focus(); return; }
  if (!state.recipe || !state.upload) return;
  state.name = name;
  const btn = $('#squish-btn'); btn.disabled = true;
  try {
    const payload = { upload_id: state.upload.upload_id, recipe: state.recipe, name, output_dir: null };
    const { job_id } = await api.squish(payload);
    state.squishJobId = job_id; state.job = null; state.result = null;
    state.queue.names.set(job_id, name);
    $('#progress-name').textContent = name;
    renderProgress(null);
    showView('progress');
    jobStarted = performance.now();
    clearInterval(elapsedTimer);
    elapsedTimer = setInterval(() => { $('#progress-elapsed').textContent = fmtSeconds((performance.now() - jobStarted) / 1000); }, 250);
    const job = await pollJob(job_id, renderProgress);
    clearInterval(elapsedTimer);
    if (!job) return;
    if (job.status === 'error') {
      showError('Squish failed', new ApiError(job.error || 'The job ended with an error.'), () => startSquish());
      return;
    }
    if (job.status === 'cancelled') {
      toast({ kind: 'warn', title: 'Squish cancelled', msg: 'Your recipe is still here — tweak it and try again.' });
      showView('inspect');
      return;
    }
    state.result = job.result;
    openResults(job, 'single');
  } catch (err) {
    clearInterval(elapsedTimer);
    showError("Couldn't start squishing", err, () => { showView('inspect'); });
  } finally { btn.disabled = false; }
}
const isCached = (s) => s.status === 'done' && s.seconds === 0;
function renderProgress(job) {
  state.job = job;
  const pipe = $('#pipeline');
  const stages = job?.stages || [];
  if (!pipe.children.length || pipe.children.length !== stages.length) {
    pipe.replaceChildren(...stages.map((s, i) => h('li', { class: 'stage', dataset: { stage: s.id } },
      h('span', { class: 'stage-dot' }, h('span', { class: 'stage-num', text: String(i + 1) })),
      h('span', { class: 'stage-label', text: s.label || s.id }),
      h('span', { class: 'stage-time' }))));
  }
  let cachedCount = 0;
  stages.forEach((s, i) => {
    const li = pipe.children[i]; if (!li) return;
    const cached = isCached(s);
    if (cached) cachedCount++;
    li.className = `stage is-${s.status || 'pending'}${cached ? ' is-cached' : ''}`;
    const dot = li.querySelector('.stage-dot');
    if (s.status === 'done') dot.replaceChildren(svg(ICONS.check, 14, 'stroke-width="3"'));
    else if (s.status === 'running') dot.replaceChildren(h('span', { class: 'spinner', 'aria-hidden': 'true' }));
    else if (s.status === 'error') dot.replaceChildren(svg(ICONS.x, 14, 'stroke-width="3"'));
    else dot.replaceChildren(h('span', { class: 'stage-num', text: String(i + 1) }));
    const time = li.querySelector('.stage-time');
    if (cached) time.replaceChildren(h('span', { class: 'cached-chip', title: 'Unchanged since the last squish of this upload — result reused.', text: 'cached' }));
    else time.textContent = s.seconds != null ? fmtSeconds(s.seconds) : '';
    li.setAttribute('aria-current', s.status === 'running' ? 'step' : 'false');
  });
  const pct = Math.round((job?.progress || 0) * 100);
  $('#progress-fill').style.width = `${pct}%`;
  $('#progress-bar').setAttribute('aria-valuenow', String(pct));
  $('#progress-pct').textContent = `${pct}%`;
  const label = $('#progress-stage-label');
  const text = !job ? 'Starting…' : job.status === 'queued' ? 'Queued — waiting for the pipeline…' : job.status === 'done' ? 'Done!' : job.status === 'error' ? 'Failed' : job.status === 'cancelled' ? 'Cancelled' : `${job.stage_label || job.stage || 'Working'}…${cachedCount ? ` (${cachedCount} stage${cachedCount === 1 ? '' : 's'} cached)` : ''}`;
  if (label.textContent !== text) label.textContent = text;
  $('#progress-spinner').hidden = !!job && job.status !== 'running' && job.status !== 'queued';
  const log = $('#log');
  const lines = job?.log || [];
  if (log.childElementCount !== lines.length) {
    const atBottom = log.scrollTop + log.clientHeight >= log.scrollHeight - 8;
    for (let i = log.childElementCount; i < lines.length; i++) log.append(h('span', { class: 'log-line' }, h('span', { class: 'log-idx', text: String(i + 1).padStart(2, '0') }), lines[i]));
    $('#log-count').textContent = `${lines.length} line${lines.length === 1 ? '' : 's'}`;
    if (atBottom) log.scrollTop = log.scrollHeight;
  }
  if (!lines.length && !job) { log.replaceChildren(); $('#log-count').textContent = '0 lines'; }
}
async function cancelSquish() {
  if (!state.squishJobId) return;
  const btn = $('#cancel-btn'); btn.disabled = true;
  try { await api.cancel(state.squishJobId); }
  catch (err) { toast({ kind: 'error', title: "Couldn't cancel", msg: err.message }); }
  finally { btn.disabled = false; }
}

/* =====================================================================
   4. Results
   ===================================================================== */
const KIND_LABEL = { glb: 'GLB', obj: 'OBJ', fbx: 'FBX', texture: 'TEX', report: 'HTML', imposter: 'IMP', heatmap: 'HEAT' };
const KIND_TITLE = { imposter: 'Imposter: billboard atlas + card mesh for the farthest LOD', heatmap: 'Vertex-coloured heat-map mesh (open in any glTF viewer)', fbx: 'Autodesk FBX with rig and animations when kept' };
function openResults(job, from = 'single') {
  state.resultsFrom = from;
  state.resultJobId = job.id;
  if (from === 'single') state.name = state.name || nameOfJob(job);
  else state.name = nameOfJob(job);
  renderResults(job);
  showView('results');
}
function renderResults(job) {
  const r = job.result, id = job.id;
  $('#results-name').textContent = state.name;
  const before = r.before || {}, after = r.after || {};
  $('#resquish-btn').hidden = !(state.resultsFrom === 'single' && state.upload && state.squishJobId === id);
  $('#results-queue-btn').hidden = state.resultsFrom !== 'queue';

  // delta badge + comparison table
  $('#delta-badge').textContent = fmtDelta(before.triangles, after.triangles) || '—';
  const deltaCell = (b, a) => { const d = fmtDelta(b, a); const cls = !d ? 'delta-flat' : d.startsWith('–') ? 'delta-down' : d.startsWith('+') ? 'delta-up' : 'delta-flat'; return h('span', { class: `delta ${cls}`, text: d || '—' }); };
  const row = (label, b, a, fmt, full) => h('tr', {}, h('td', { text: label }), h('td', { class: 'before', text: fmt(b), title: full ? full(b) : '' }), h('td', { class: 'after', text: fmt(a), title: full ? full(a) : '' }), h('td', {}, deltaCell(b, a)));
  $('#compare-body').replaceChildren(
    h('table', { class: 'compare-table' },
      h('thead', {}, h('tr', {}, h('th', { text: '' }), h('th', { text: 'Before' }), h('th', { text: 'After' }), h('th', { text: 'Change' }))),
      h('tbody', {},
        row('Triangles', before.triangles, after.triangles, fmtShort, fmtInt),
        row('Vertices', before.vertices, after.vertices, fmtShort, fmtInt),
        row('File size', before.size_bytes, after.size_bytes, fmtBytes))),
    h('p', { class: 'compare-foot' },
      after.texture_size ? h('span', { class: 'chip chip-ghost', text: `${after.texture_size}px textures` }) : null,
      h('span', { class: 'chip chip-ghost', text: `${(after.lods || []).length ? after.lods.length - 1 : 0} extra LOD${(after.lods || []).length === 2 ? '' : 's'}` }),
      r.report ? h('span', { class: `chip ${r.report.watertight ? 'chip-mint' : 'chip-ghost'}`, text: r.report.watertight ? 'Watertight' : `${fmtInt(r.report.boundary_edges)} open edges` }) : null,
      r.timings ? h('span', { class: 'chip chip-ghost', text: `${fmtSeconds(Object.values(r.timings).reduce((a, v) => a + (v || 0), 0))} total` }) : null));

  renderMetrics(r.metrics, r.rig);

  // LODs
  const lods = after.lods || [];
  $('#lods-body').replaceChildren(lods.length
    ? h('table', { class: 'table' },
        h('thead', {}, h('tr', {}, h('th', { text: 'Level' }), h('th', { text: 'Triangles', class: 'num' }), h('th', { text: 'Coverage', class: 'num' }), h('th', { text: '' }))),
        h('tbody', {}, lods.map((l) => h('tr', {}, h('td', { text: `LOD${l.level}` }), h('td', { class: 'num', text: fmtInt(l.triangles) }), h('td', { class: 'num', text: l.screen_coverage != null ? `${Math.round(l.screen_coverage * 100)}%` : '—' }),
          h('td', {}, h('div', { class: 'lod-bar' }, h('i', { style: `width:${Math.max(3, (l.triangles / (lods[0].triangles || 1)) * 100)}%` }))))),
          (r.files || []).some((f) => f.kind === 'imposter') ? h('tr', { class: 'lod-imposter' }, h('td', { text: 'Imposter' }), h('td', { class: 'num', text: '2' }), h('td', { class: 'num', text: 'far' }), h('td', {}, h('span', { class: 'chip chip-ghost', text: 'billboard atlas' }))) : null))
    : h('p', { class: 'empty-note', text: 'No LODs were generated for this recipe.' }));

  // problems fixed
  const fixed = r.problems_fixed || [];
  $('#fixed-body').replaceChildren(fixed.length
    ? h('ul', { class: 'fixed-list' }, fixed.map((t) => h('li', {}, svg(ICONS.check, 18, 'class="tick" stroke-width="2.5"'), h('span', { text: t }))))
    : h('p', { class: 'empty-note', text: 'Nothing needed fixing. Clean input!' }));

  // textures
  const texs = (r.files || []).filter((f) => f.kind === 'texture');
  $('#textures-card').hidden = !texs.length;
  $('#textures-body').replaceChildren(h('div', { class: 'tex-grid' }, texs.map((f) => {
    const url = api.fileUrl(id, f.name);
    return h('a', { class: 'tex', href: url, target: '_blank', rel: 'noopener', title: `Open ${f.name}` },
      h('div', { class: 'tex-img' }, h('img', { src: url, alt: f.name, loading: 'lazy' })),
      h('span', { class: 'tex-name', text: f.name }), h('span', { class: 'tex-size', text: fmtBytes(f.size_bytes) }));
  })));

  // files + quick downloads
  $('#output-dir').value = r.output_dir || '';
  const files = r.files || [];
  const quick = ['glb', 'fbx', 'obj'].map((k) => files.find((f) => f.kind === k)).filter(Boolean);
  $('#quick-dl').replaceChildren(...quick.map((f) => h('a', { class: `btn btn-sm ${f.kind === 'fbx' ? 'btn-primary' : 'btn-secondary'} quick-btn`, href: api.fileUrl(id, f.name), download: f.name, title: `Download ${f.name} (${fmtBytes(f.size_bytes)})` },
    svg(ICONS.download, 14), h('span', { text: KIND_LABEL[f.kind] }))));
  $('#file-list').replaceChildren(...files.map((f) => h('li', { class: 'file', title: KIND_TITLE[f.kind] || null },
    h('span', { class: `file-kind k-${f.kind || 'file'}`, text: KIND_LABEL[f.kind] || (extOf(f.name).slice(1) || 'file').slice(0, 4).toUpperCase() }),
    h('div', { class: 'file-info' }, h('span', { class: 'file-name', text: f.name, title: f.name }), h('span', { class: 'file-size', text: fmtBytes(f.size_bytes) })),
    h('a', { class: 'btn btn-sm btn-secondary', href: api.fileUrl(id, f.name), download: f.name, 'aria-label': `Download ${f.name}` }, svg(ICONS.download, 15), 'Download'))));
  const zip = $('#zip-btn');
  zip.href = api.zipUrl(r, id);
  zip.setAttribute('download', `${state.name}.zip`);

  // viewers
  const res = state.res;
  res.preview = r.preview || {};
  res.metrics = r.metrics || null;
  res.mode = 'shaded'; res.side = 'after'; res.split = false; res.wire = false; res.token++;
  const v = viewers.result;
  v.clear(); v.setWireframe(false); v.unlink();
  if (viewers.resultBefore) { viewers.resultBefore.clear(); viewers.resultBefore.setWireframe(false); }
  for (const b of $('#res-mode').children) {
    const m = b.dataset.mode;
    const avail = m === 'shaded' || (m === 'deviation' ? !!res.preview.heatmap_deviation : !!res.preview.heatmap_density);
    b.disabled = !avail;
    b.title = avail ? '' : m === 'density' ? 'No texel-density map: this recipe generated no UVs.' : 'No deviation map was produced for this job.';
  }
  applyViewerState();
  const token = res.token;
  (async () => {
    try {
      if (res.preview.source) await loadPreview(v, 'before', res.preview.source, { fit: true, show: false });
      if (token !== res.token) return;
      if (res.preview.result) await loadPreview(v, 'after', res.preview.result, { fit: !res.preview.source, show: false });
      if (token !== res.token) return;
      applyViewerState();
    } catch (e) { toast({ kind: 'warn', title: "Couldn't load the 3D preview", msg: e.message }); }
  })();
}
function renderMetrics(m, rig) {
  const card = $('#metrics-card');
  card.hidden = !m && !rig;
  if (card.hidden) return;
  const tile = (label, value, sub, extra) => h('div', { class: 'metric' }, h('span', { class: 'metric-label', text: label }), h('span', { class: 'metric-value', text: value }), sub ? h('span', { class: 'metric-sub', text: sub }) : null, extra || null);
  const tiles = [];
  if (m?.deviation) {
    const d = m.deviation, pctUnit = d.unit === 'fraction_of_size' || !d.unit;
    const f = (x) => pctUnit ? fmtPct(x) : `${x} ${d.unit}`;
    tiles.push(tile('Max deviation', f(d.max), `mean ${f(d.mean)}${d.p95 != null ? ` · p95 ${f(d.p95)}` : ''}`,
      h('span', { class: 'metric-note', text: pctUnit ? 'of the model size' : '' })));
  }
  if (m?.texel_density) {
    const t = m.texel_density;
    tiles.push(tile('Texel density', fmtInt(t.mean), `${fmtInt(t.min)} – ${fmtInt(t.max)} ${t.unit === 'texels_per_unit' || !t.unit ? 'px / unit' : t.unit}`));
  }
  if (m && m.uv_charts != null) tiles.push(tile('UV charts', fmtInt(m.uv_charts), m.uv_charts ? 'islands in the atlas' : 'no UVs generated'));
  if (m && m.quads > 0) {
    const tris = Math.max(0, (m.polygons || 0) - m.quads), share = m.polygons ? m.quads / m.polygons : 0;
    tiles.push(tile('Quads vs triangles', `${Math.round(share * 100)}%`, `${fmtInt(m.quads)} quads · ${fmtInt(tris)} triangles`,
      h('div', { class: 'split-bar', role: 'img', 'aria-label': `${Math.round(share * 100)} percent quads` }, h('i', { style: `width:${(share * 100).toFixed(1)}%` }))));
  }
  if (m && m.hidden_faces_removed != null) tiles.push(tile('Hidden faces removed', fmtInt(m.hidden_faces_removed), m.hidden_faces_removed ? 'interior faces nobody could see' : 'nothing was hidden'));
  $('#metrics-body').replaceChildren(...tiles);
  const badges = [];
  if (m?.tracer) badges.push(h('span', { class: `chip ${m.tracer === 'gpu' ? 'chip-violet' : 'chip-ghost'}`, title: 'Ray tracer used for baking' }, svg(m.tracer === 'gpu' ? ICONS.bolt : ICONS.cpu, 13), `${m.tracer.toUpperCase()} tracer`));
  if (m && m.watertight != null) badges.push(h('span', { class: `chip ${m.watertight ? 'chip-mint' : 'chip-amber'}`, text: m.watertight ? 'Watertight' : 'Open edges' }));
  if (rig) badges.push(h('span', { class: 'chip chip-pink rig-badge', title: `Rig kept: ${rig.joints} joints${rig.animations?.length ? `, animations: ${rig.animations.join(', ')}` : ''}` },
    svg(ICONS.bone, 13), `Rig kept · ${fmtInt(rig.joints)} joints`, rig.animations?.length ? h('span', { class: 'rig-anims', text: `· ${rig.animations.join(' · ')}` }) : null));
  $('#metrics-badges').replaceChildren(...badges);
}
const HEAT_SLOT = { deviation: 'heat-dev', density: 'heat-den' };
function ensureBeforeViewer() {
  if (!viewers.resultBefore) viewers.resultBefore = new Viewer($('#viewer-result-before'), { autoRotate: false });
  return viewers.resultBefore;
}
/* Single source of truth for the result viewer(s): mode × side × split × wireframe. */
async function applyViewerState() {
  const res = state.res, v = viewers.result;
  const heat = res.mode !== 'shaded';
  const heatSlot = heat ? HEAT_SLOT[res.mode] : null;
  for (const b of $('#res-mode').children) b.setAttribute('aria-checked', String(b.dataset.mode === res.mode));
  $('#res-before').setAttribute('aria-pressed', String(res.side === 'before'));
  $('#res-after').setAttribute('aria-pressed', String(res.side === 'after'));
  $('#res-split').setAttribute('aria-pressed', String(res.split));
  $('#res-wire').setAttribute('aria-pressed', String(res.wire));
  $('#res-side-seg').hidden = res.split || heat;
  $('#pane-before').hidden = !res.split;
  $('#pane-after-tag').hidden = !res.split;
  $('#pane-after-tag').textContent = heat ? (res.mode === 'deviation' ? 'After · deviation' : 'After · texel density') : 'After';
  $('#viewer-duo').classList.toggle('is-split', res.split);
  const note = $('#viewer-note');
  note.hidden = !heat && !res.split;
  note.textContent = heat
    ? (res.mode === 'deviation' ? 'How far each point of the squished surface moved from the original, as a share of the model size. Blue is untouched; coral is the worst spot.' : 'Texels per world unit across the surface. Even density means textures look equally sharp everywhere.')
    : 'Both views share one camera and identical lighting — orbit either side.';
  v.setWireframe(res.wire);

  // legend
  const legend = $('#heat-legend');
  legend.hidden = !heat;
  if (heat) {
    const m = res.metrics || {};
    if (res.mode === 'deviation') {
      const d = m.deviation || {}; const pctUnit = d.unit === 'fraction_of_size' || !d.unit;
      $('#heat-title').textContent = 'Deviation';
      $('#heat-min').textContent = pctUnit ? '0%' : '0';
      $('#heat-max').textContent = d.max != null ? (pctUnit ? fmtPct(d.max) : `${d.max} ${d.unit}`) : 'max';
    } else {
      const t = m.texel_density || {};
      $('#heat-title').textContent = 'Texel density';
      $('#heat-min').textContent = t.min != null ? `${fmtInt(t.min)}` : 'min';
      $('#heat-max').textContent = t.max != null ? `${fmtInt(t.max)} px/unit` : 'max';
    }
  }

  // side-by-side: the before viewer mirrors the main one
  if (res.split) {
    const vb = ensureBeforeViewer();
    vb.setWireframe(res.wire);
    if (!vb.slots.has('before') && !vb._loadingBefore && res.preview?.source) {
      vb._loadingBefore = true;
      loadPreview(vb, 'before', res.preview.source, { fit: true, show: true })
        .then(() => { if (state.res.split) v.syncTo(vb); })
        .catch((e) => toast({ kind: 'warn', title: "Couldn't load the before preview", msg: e.message }))
        .finally(() => { vb._loadingBefore = false; });
    } else vb.show('before');
    requestAnimationFrame(() => { v.resize(); vb.resize(); });
    if (v.linked !== vb) v.link(vb);
  } else if (v.linked) { v.unlink(); requestAnimationFrame(() => v.resize()); }

  // which slot does the main viewer show?
  const want = heat ? heatSlot : (res.split ? 'after' : res.side);
  if (!v.slots.has(want) && heat) {
    const url = res.preview?.[res.mode === 'deviation' ? 'heatmap_deviation' : 'heatmap_density'];
    if (!url) { v.show(null); return; }
    const token = res.token;
    v.show(null);
    try {
      await loadPreview(v, want, url, { fit: false, show: false, unlit: true });
    } catch (e) { toast({ kind: 'warn', title: "Couldn't load the heat-map", msg: e.message }); return; }
    if (token !== res.token || state.res.mode !== res.mode) return;
  }
  v.show(v.slots.has(want) ? want : (v.slots.has('after') ? 'after' : null));
}
function setResultMode(mode) { state.res.mode = mode; applyViewerState(); }
function setResultSide(side) { state.res.side = side; applyViewerState(); }
function resetAll() {
  state.pollAbort?.();
  clearInterval(elapsedTimer);
  state.upload = null; state.inspect = null; state.result = null; state.job = null; state.squishJobId = null; state.inspectJobId = null; state.batch = null;
  viewers.source.clear(); viewers.result.clear(); viewers.result.unlink(); viewers.resultBefore?.clear();
  renderProgress(null);
  mountRecipe('inspect');
  showView('drop');
  $('#dropzone').focus({ preventScroll: true });
}

/* =====================================================================
   5. Batch
   ===================================================================== */
function openBatch(groups) {
  state.batch = { groups, running: false };
  if (!state.presetId) selectPreset(state.presets[0]?.id);
  mountRecipe('batch');
  renderBatch();
  showView('batch');
}
const BATCH_STATUS = {
  ready: ['Ready', 'chip-ghost'], uploading: ['Uploading…', 'chip-violet'], uploaded: ['Uploaded', 'chip-mint'], failed: ['Failed', 'chip-coral'], queued: ['Queued', 'chip-pink'],
};
function renderBatch() {
  const b = state.batch; if (!b) return;
  const n = b.groups.length;
  $('#batch-count').textContent = `${n} models`;
  const total = b.groups.reduce((a, g) => a + g.files.reduce((x, f) => x + (f.size || 0), 0), 0);
  $('#batch-size-chip').textContent = `${b.groups.reduce((a, g) => a + g.files.length, 0)} files · ${fmtBytes(total)}`;
  $('#batch-list').replaceChildren(...b.groups.map((g, i) => {
    const [label, cls] = BATCH_STATUS[g.status] || BATCH_STATUS.ready;
    const side = g.files.filter((f) => f !== g.main);
    return h('li', { class: `batch-item is-${g.status}`, dataset: { status: g.status } },
      h('span', { class: 'batch-idx', text: String(i + 1).padStart(2, '0') }),
      h('span', { class: 'file-kind', text: extOf(g.main.name).slice(1).toUpperCase().slice(0, 4) }),
      h('div', { class: 'file-info' }, h('span', { class: 'file-name', text: g.main.name, title: g.main.name }),
        h('span', { class: 'file-size', text: `${fmtBytes(g.main.size)}${side.length ? ` · +${side.length} sidecar${side.length === 1 ? '' : 's'}: ${side.map((f) => f.name).join(', ')}` : ''}`, title: side.map((f) => f.name).join(', ') })),
      h('span', { class: `chip ${cls}`, title: g.error || '' }, g.status === 'uploading' ? h('span', { class: 'spinner', 'aria-hidden': 'true' }) : null, label),
      g.status === 'ready' && n > 1 && !b.running ? h('button', { class: 'icon-btn', type: 'button', 'aria-label': `Remove ${g.main.name} from the batch`, onclick: () => { b.groups.splice(i, 1); if (b.groups.length === 1) { toast({ title: 'One model left — opening the single-file flow' }); startWithFiles(b.groups[0].files); } else renderBatch(); } }, svg(ICONS.x, 14)) : null);
  }));
  $('#batch-squish-btn').disabled = b.running || !b.groups.some((g) => g.status === 'ready' || g.status === 'failed');
  if (state.health?.output_root) $('#batch-note').textContent = `Each model gets its own folder under ${state.health.output_root}.`;
}
async function startBatch() {
  const b = state.batch; if (!b || b.running || !state.recipe) return;
  b.running = true; renderBatch();
  const t = toast({ title: 'Uploading models…', msg: `${b.groups.length} uploads, one at a time`, timeout: 0 });
  for (const g of b.groups) {
    if (g.status === 'uploaded') continue;
    g.status = 'uploading'; g.error = null; renderBatch();
    try { g.upload = await api.upload(g.files); g.status = 'uploaded'; }
    catch (err) { g.status = 'failed'; g.error = err.message; }
    renderBatch();
  }
  t();
  const ok = b.groups.filter((g) => g.status === 'uploaded');
  const failed = b.groups.filter((g) => g.status === 'failed');
  if (failed.length) toast({ kind: failed.length === b.groups.length ? 'error' : 'warn', title: `${failed.length} upload${failed.length === 1 ? '' : 's'} failed`, msg: failed.map((g) => `${g.main.name}: ${g.error}`).join('\n') });
  if (!ok.length) { b.running = false; renderBatch(); return; }
  try {
    const { batch_id, job_ids } = await api.batch({ upload_ids: ok.map((g) => g.upload.upload_id), recipe: state.recipe, output_dir: null });
    ok.forEach((g, i) => { g.status = 'queued'; g.jobId = job_ids[i]; if (job_ids[i]) state.queue.names.set(job_ids[i], fileStem(g.upload.main_file || g.main.name)); });
    state.queue.batchId = batch_id; state.queue.batch = null; state.queue.jobIds = [...job_ids]; state.queue.jobs = new Map();
    renderBatch();
    toast({ kind: 'success', title: `Batch queued: ${job_ids.length} model${job_ids.length === 1 ? '' : 's'}`, msg: 'They squish one after another. You can keep using Polysquish meanwhile.', timeout: 5000 });
    showView('queue');
  } catch (err) {
    toast({ kind: 'error', title: "Couldn't start the batch", msg: err.message });
  } finally { b.running = false; renderBatch(); }
}

/* =====================================================================
   6. Queue view (batch + every job, incl. watch-created ones)
   ===================================================================== */
let queuePolling = false, watchPolling = false;
const nameOfJob = (j) => j?.name || state.queue.names.get(j?.id) || (j?.result?.output_dir ? baseName(j.result.output_dir) : null) || j?.id || 'job';
async function startQueuePolling() {
  if (queuePolling) return;
  queuePolling = true;
  let failures = 0;
  try {
    while (state.view === 'queue') {
      const q = state.queue;
      try {
        const all = await api.jobs();
        q.all = Array.isArray(all) ? all : [];
        if (q.batchId) {
          q.batch = await api.batchStatus(q.batchId);
          q.jobIds = q.batch.job_ids || q.jobIds;
          for (const id of q.jobIds) q.jobs.set(id, await api.job(id));
        }
        try { q.watches = await api.watchList(); } catch { /* watch list is optional here */ }
        failures = 0;
        renderQueue();
      } catch (err) {
        if (++failures === 1) toast({ kind: 'error', title: "Couldn't refresh the queue", msg: err.message });
      }
      await sleep(QUEUE_POLL_MS);
    }
  } finally { queuePolling = false; }
}
const STATUS_CHIP = { queued: ['Queued', 'chip-ghost'], running: ['Running', 'chip-violet'], done: ['Done', 'chip-mint'], error: ['Failed', 'chip-coral'], cancelled: ['Cancelled', 'chip-amber'] };
function renderQueue() {
  const q = state.queue;
  const byId = new Map(q.all.filter((j) => j.kind !== 'inspect').map((j) => [j.id, j]));
  for (const [id, j] of q.jobs) if (j.kind !== 'inspect') byId.set(id, j);        // detailed polls win
  const jobs = [...byId.values()];
  const watchJobIds = new Set((q.watches || []).flatMap((w) => w.job_ids || []));
  const batchIds = new Set(q.jobIds);
  const active = jobs.filter((j) => !isTerminal(j.status));
  const done = jobs.filter((j) => j.status === 'done');
  updateNavCounts(active.length, (q.watches || []).length);

  // overall
  let headline, label, pct, counts;
  if (q.batch) {
    const b = q.batch, running = q.jobIds.map((id) => byId.get(id)).find((j) => j?.status === 'running');
    pct = b.total ? ((b.done + (running?.progress || 0)) / b.total) * 100 : 0;
    headline = `${b.done} of ${b.total} done`;
    counts = `${b.done} of ${b.total} done`;
    label = b.status === 'done' || b.done === b.total ? 'Batch finished' : running ? `Squishing ${nameOfJob(running)} — ${running.stage_label || running.stage || 'working'}…` : 'Waiting for the pipeline…';
    $('#queue-spinner').hidden = b.done === b.total;
  } else {
    const total = jobs.length, fin = jobs.filter((j) => isTerminal(j.status)).length;
    const running = jobs.find((j) => j.status === 'running');
    pct = total ? ((fin + (running?.progress || 0)) / total) * 100 : 0;
    headline = total ? `${fin} of ${total} done` : 'nothing queued';
    counts = `${fin} of ${total} done`;
    label = running ? `Squishing ${nameOfJob(running)} — ${running.stage_label || running.stage || 'working'}…` : active.length ? 'Waiting for the pipeline…' : total ? 'All jobs finished' : 'Drop models or start a watch folder to fill the queue.';
    $('#queue-spinner').hidden = !active.length;
  }
  $('#queue-headline').textContent = headline;
  $('#queue-status-label').textContent = label;
  $('#queue-pct').textContent = `${Math.round(pct)}%`;
  $('#queue-counts').textContent = counts;
  $('#queue-fill').style.width = `${pct}%`;
  $('#queue-bar').setAttribute('aria-valuenow', String(Math.round(pct)));
  $('#queue-jobs-chip').textContent = `${active.length} active · ${jobs.length} total`;

  // rows
  const rows = $('#queue-rows');
  rows.replaceChildren(...(jobs.length ? jobs.map((j, i) => {
    const [sLabel, sCls] = STATUS_CHIP[j.status] || ['…', 'chip-ghost'];
    const p = Math.round((j.progress || 0) * 100);
    const cached = (j.stages || []).filter(isCached).length;
    return h('li', { class: `job-row is-${j.status}`, dataset: { job: j.id } },
      h('span', { class: 'job-idx', text: String(i + 1).padStart(2, '0') }),
      h('div', { class: 'job-main' },
        h('div', { class: 'job-title' }, h('span', { class: 'job-name', text: nameOfJob(j) }),
          batchIds.has(j.id) ? h('span', { class: 'tag', text: 'batch' }) : null, watchJobIds.has(j.id) ? h('span', { class: 'tag tag-watch' }, svg(ICONS.eye, 11), 'watch') : null,
          cached ? h('span', { class: 'tag', text: `${cached} cached` }) : null),
        h('div', { class: 'job-stage', text: j.status === 'running' ? `${j.stage_label || j.stage || 'Working'}… ${p}%` : j.status === 'queued' ? 'Waiting in line' : j.status === 'error' ? (j.error || 'Failed') : j.status === 'done' ? `${fmtShort(j.result?.after?.triangles)} triangles · ${fmtBytes(j.result?.after?.size_bytes)}` : 'Cancelled' }),
        h('div', { class: 'bar bar-thin', role: 'progressbar', 'aria-valuemin': '0', 'aria-valuemax': '100', 'aria-valuenow': String(p), 'aria-label': `${nameOfJob(j)} progress` }, h('div', { class: `bar-fill${j.status === 'done' ? ' is-done' : ''}`, style: `width:${j.status === 'done' ? 100 : p}%` }))),
      h('span', { class: `chip ${sCls}` }, j.status === 'running' ? h('span', { class: 'spinner', 'aria-hidden': 'true' }) : null, sLabel),
      h('div', { class: 'job-actions' },
        j.status === 'done' ? h('button', { class: 'btn btn-sm btn-secondary', type: 'button', onclick: () => viewJobResults(j.id) }, svg(ICONS.eye, 14), 'Results') : null,
        j.status === 'done' ? h('a', { class: 'btn btn-sm btn-ghost', href: api.zipUrl(j.result, j.id), download: `${nameOfJob(j)}.zip`, 'aria-label': `Download zip for ${nameOfJob(j)}` }, svg(ICONS.download, 14), 'Zip') : null,
        !isTerminal(j.status) ? h('button', { class: 'btn btn-sm btn-danger-ghost', type: 'button', 'aria-label': `Cancel ${nameOfJob(j)}`, onclick: async (e) => { e.currentTarget.disabled = true; try { await api.cancel(j.id); } catch (err) { toast({ kind: 'error', title: "Couldn't cancel", msg: err.message }); } } }, 'Cancel') : null));
  }) : [h('li', { class: 'empty-note', text: 'No jobs yet.' })]));

  // finished list
  $('#queue-results-card').hidden = !done.length;
  $('#queue-results-chip').textContent = `${done.length} ready`;
  $('#queue-results').replaceChildren(...done.map((j) => h('li', { class: 'file' },
    h('span', { class: 'file-kind k-zip', text: 'ZIP' }),
    h('div', { class: 'file-info' }, h('span', { class: 'file-name', text: nameOfJob(j) }), h('span', { class: 'file-size', text: `${j.result?.files?.length || 0} files · ${fmtBytes(j.result?.after?.size_bytes)} · ${j.result?.output_dir || ''}`, title: j.result?.output_dir || '' })),
    h('button', { class: 'btn btn-sm btn-ghost', type: 'button', onclick: () => viewJobResults(j.id) }, 'View'),
    h('a', { class: 'btn btn-sm btn-secondary', href: api.zipUrl(j.result, j.id), download: `${nameOfJob(j)}.zip` }, svg(ICONS.download, 14), 'Download zip'))));
}
async function viewJobResults(id) {
  try {
    const job = await api.job(id);
    if (job.status !== 'done' || !job.result) { toast({ kind: 'warn', title: 'That job has no results yet' }); return; }
    openResults(job, 'queue');
  } catch (err) { toast({ kind: 'error', title: "Couldn't open the results", msg: err.message }); }
}
function updateNavCounts(activeJobs, watchCount) {
  const q = $('#nav-queue-count'), w = $('#nav-watch-count');
  q.textContent = String(activeJobs); q.hidden = !activeJobs;
  w.textContent = String(watchCount); w.hidden = !watchCount;
  $('#nav-queue').setAttribute('aria-label', `Queue${activeJobs ? `, ${activeJobs} active job${activeJobs === 1 ? '' : 's'}` : ''}`);
  $('#nav-watch').setAttribute('aria-label', `Watch a folder${watchCount ? `, ${watchCount} active watch${watchCount === 1 ? '' : 'es'}` : ''}`);
}

/* =====================================================================
   7. Watch folders
   ===================================================================== */
function renderWatchPresetSelect() {
  const sel = $('#watch-preset');
  const cur = sel.value;
  sel.replaceChildren(...state.presets.map((p) => h('option', { value: p.id, text: `${p.name} — ${p.tagline || ''}` })));
  sel.value = state.presets.some((p) => p.id === cur) ? cur : (state.presets.find((p) => p.id === 'prop') || state.presets[0])?.id || '';
  updateWatchHint();
}
function updateWatchHint() {
  const p = state.presets.find((x) => x.id === $('#watch-preset').value);
  $('#watch-preset-hint').textContent = p ? p.description || '' : '';
}
async function startWatchPolling() {
  if (watchPolling) return;
  watchPolling = true;
  let failures = 0;
  try {
    while (state.view === 'watch') {
      try { state.watches = await api.watchList(); failures = 0; renderWatches(); }
      catch (err) { if (++failures === 1) toast({ kind: 'error', title: "Couldn't load watch folders", msg: err.message }); }
      await sleep(WATCH_POLL_MS);
    }
  } finally { watchPolling = false; }
}
function renderWatches() {
  const list = state.watches || [];
  $('#watch-list-chip').textContent = list.length ? `${list.length} active` : 'none';
  updateNavCounts(parseInt($('#nav-queue-count').textContent, 10) || 0, list.length);
  $('#watch-list').replaceChildren(...(list.length ? list.map((w) => {
    const p = state.presets.find((x) => x.id === w.preset);
    return h('li', { class: 'watch-row', dataset: { watch: w.id } },
      h('span', { class: 'watch-ico' }, svg(ICONS.eye, 18)),
      h('div', { class: 'watch-main' },
        h('div', { class: 'watch-folder mono', text: w.folder, title: w.folder }),
        h('div', { class: 'watch-meta' },
          h('span', { class: 'chip chip-pink', text: p ? p.name : w.preset }),
          h('span', { class: 'chip chip-mint', text: `${fmtInt(w.processed)} processed` }),
          h('span', { class: `chip ${w.queued ? 'chip-violet' : 'chip-ghost'}` }, w.queued ? h('span', { class: 'spinner', 'aria-hidden': 'true' }) : null, `${fmtInt(w.queued)} queued`),
          w.output_dir ? h('span', { class: 'chip chip-ghost mono', text: `→ ${w.output_dir}`, title: w.output_dir }) : null)),
      h('div', { class: 'job-actions' },
        h('button', { class: 'btn btn-sm btn-ghost', type: 'button', onclick: () => showView('queue') }, 'Jobs'),
        h('button', { class: 'btn btn-sm btn-danger-ghost', type: 'button', 'aria-label': `Stop watching ${w.folder}`, onclick: async (e) => {
          const btn = e.currentTarget; btn.disabled = true;
          try { await api.watchStop(w.id); state.watches = state.watches.filter((x) => x.id !== w.id); renderWatches(); toast({ kind: 'success', title: 'Stopped watching', msg: w.folder, timeout: 3500 }); }
          catch (err) { btn.disabled = false; toast({ kind: 'error', title: "Couldn't stop that watch", msg: err.message }); }
        } }, svg(ICONS.stop, 13), 'Stop')));
  }) : [h('li', { class: 'empty-note', text: 'No folders are being watched. Start one on the left — drop your generator’s export folder in and forget about it.' })]));
}
async function submitWatch(e) {
  e.preventDefault();
  const folder = $('#watch-folder').value.trim(), output = $('#watch-output').value.trim();
  const presetId = $('#watch-preset').value;
  const p = state.presets.find((x) => x.id === presetId);
  if (!folder) { $('#watch-folder').focus(); return; }
  if (!p) { toast({ kind: 'warn', title: 'Pick a preset first' }); return; }
  const btn = $('#watch-start'); btn.disabled = true;
  try {
    const recipe = normalizeRecipe(deepClone(p.recipe)); recipe.preset = p.id;
    await api.watchStart({ folder, output_dir: output || null, recipe, preset: p.id });
    toast({ kind: 'success', title: 'Watching', msg: folder, timeout: 4000 });
    $('#watch-folder').value = ''; $('#watch-output').value = '';
    state.watches = await api.watchList();
    renderWatches();
  } catch (err) {
    toast({ kind: 'error', title: "Couldn't start watching", msg: err.message });
  } finally { btn.disabled = false; }
}

/* ---------- Folder picker on GET /api/fs ---------- */
function pickFolder({ title = 'Pick a folder', start = '' } = {}) {
  const dlg = $('#fs-picker');
  if (typeof dlg.showModal !== 'function') { return Promise.resolve(prompt(title, start) || null); }
  return new Promise((resolve) => {
    let current = null, done = false;
    const finish = (val) => { if (done) return; done = true; cleanup(); if (dlg.open) dlg.close(); resolve(val); };
    const sep = (p) => (p && p.includes('\\') && !p.includes('/')) ? '\\' : '/';
    const join = (a, b) => (a === '/' || a === '' ? `/${b}` : a.replace(/[\\/]+$/, '') + sep(a) + b);
    async function load(path) {
      $('#fs-body').setAttribute('aria-busy', 'true');
      $('#fs-list').replaceChildren(h('li', { class: 'fs-loading' }, h('span', { class: 'spinner', 'aria-hidden': 'true' }), 'Reading folder…'));
      try {
        const r = await api.fs(path);
        current = r;
        renderCrumbs(r.path);
        const supported = (r.files || []).filter((f) => f.supported).length;
        $('#fs-summary').textContent = `${r.dirs.length} folder${r.dirs.length === 1 ? '' : 's'} · ${supported} supported model${supported === 1 ? '' : 's'}`;
        $('#fs-up').disabled = !r.parent;
        $('#fs-list').replaceChildren(
          ...r.dirs.map((d) => h('li', {}, h('button', { class: 'fs-item fs-dir', type: 'button', onclick: () => load(join(r.path, d)) }, svg(ICONS.folder, 16), h('span', { class: 'fs-name', text: d }), h('span', { class: 'fs-go', 'aria-hidden': 'true', text: '›' })))),
          ...(r.files || []).map((f) => h('li', {}, h('div', { class: `fs-item fs-file${f.supported ? ' is-supported' : ''}` }, svg(ICONS.file, 16), h('span', { class: 'fs-name', text: f.name }), f.supported ? h('span', { class: 'tag tag-mint', text: 'model' }) : null, h('span', { class: 'fs-size', text: fmtBytes(f.size_bytes) })))),
          ...(!r.dirs.length && !(r.files || []).length ? [h('li', { class: 'fs-empty', text: 'Empty folder' })] : []));
      } catch (err) {
        $('#fs-list').replaceChildren(h('li', { class: 'fs-empty' }, `Couldn't read that folder: ${err.message}`));
        $('#fs-summary').textContent = '';
        if (!current) { renderCrumbs(path || ''); $('#fs-up').disabled = true; }
      } finally { $('#fs-body').setAttribute('aria-busy', 'false'); }
    }
    function renderCrumbs(path) {
      const s = sep(path);
      const parts = path.split(/[\\/]+/).filter(Boolean);
      const crumbs = [];
      let acc = s === '/' ? '' : '';
      crumbs.push(h('button', { class: 'crumb', type: 'button', text: s === '/' ? '/' : parts.shift() || '/', onclick: () => load(s === '/' ? '/' : `${parts[0]}${s}`) }));
      for (const p of parts) {
        acc = acc ? `${acc}${s}${p}` : (s === '/' ? `/${p}` : p);
        const target = acc;
        crumbs.push(h('span', { class: 'crumb-sep', 'aria-hidden': 'true', text: '›' }), h('button', { class: 'crumb', type: 'button', text: p, onclick: () => load(target) }));
      }
      if (crumbs.length) crumbs[crumbs.length - 1].setAttribute('aria-current', 'location');
      $('#fs-crumbs').replaceChildren(...crumbs);
    }
    const onUp = () => { if (current?.parent) load(current.parent); };
    const onUse = () => finish(current?.path || null);
    const onClose = () => finish(null);
    const onCancel = () => finish(null);
    const onBackdrop = (e) => { if (e.target === dlg) finish(null); };
    function cleanup() {
      $('#fs-up').removeEventListener('click', onUp); $('#fs-use').removeEventListener('click', onUse); $('#fs-close').removeEventListener('click', onClose);
      dlg.removeEventListener('close', onCancel); dlg.removeEventListener('cancel', onCancel); dlg.removeEventListener('click', onBackdrop);
    }
    $('#fs-title').textContent = title;
    $('#fs-up').addEventListener('click', onUp); $('#fs-use').addEventListener('click', onUse); $('#fs-close').addEventListener('click', onClose);
    dlg.addEventListener('close', onCancel); dlg.addEventListener('cancel', onCancel); dlg.addEventListener('click', onBackdrop);
    dlg.showModal();
    load(start || undefined);
  });
}

/* =====================================================================
   Boot
   ===================================================================== */
async function boot() {
  $('#mock-badge').hidden = !MOCK;
  viewers.source = new Viewer($('#viewer-source'));
  viewers.result = new Viewer($('#viewer-result'));
  initDrop();
  initAdvanced();

  $('#inspect-back').addEventListener('click', resetAll);
  $('#again-btn').addEventListener('click', resetAll);
  $('#squish-btn').addEventListener('click', startSquish);
  $('#cancel-btn').addEventListener('click', cancelSquish);
  $('#output-name').addEventListener('input', (e) => { state.name = e.target.value.trim(); });
  $('#src-wire').addEventListener('click', (e) => { const on = e.currentTarget.getAttribute('aria-pressed') !== 'true'; e.currentTarget.setAttribute('aria-pressed', String(on)); viewers.source.setWireframe(on); });
  $('#res-wire').addEventListener('click', () => { state.res.wire = !state.res.wire; applyViewerState(); });
  $('#res-split').addEventListener('click', () => { state.res.split = !state.res.split; applyViewerState(); });
  $('#res-before').addEventListener('click', () => setResultSide('before'));
  $('#res-after').addEventListener('click', () => setResultSide('after'));
  for (const b of $('#res-mode').children) b.addEventListener('click', () => setResultMode(b.dataset.mode));
  $('#res-mode').addEventListener('keydown', (e) => {
    const modes = [...$('#res-mode').children].filter((b) => !b.disabled).map((b) => b.dataset.mode);
    const i = modes.indexOf(state.res.mode);
    if (e.key === 'ArrowRight') { e.preventDefault(); setResultMode(modes[(i + 1) % modes.length]); $(`#res-mode [data-mode="${state.res.mode}"]`).focus(); }
    if (e.key === 'ArrowLeft') { e.preventDefault(); setResultMode(modes[(i + modes.length - 1) % modes.length]); $(`#res-mode [data-mode="${state.res.mode}"]`).focus(); }
  });
  $('#resquish-btn').addEventListener('click', () => { if (!state.upload) return; mountRecipe('inspect'); showView('inspect'); $('#adv-toggle').focus({ preventScroll: true }); });
  $('#results-queue-btn').addEventListener('click', () => showView('queue'));
  $('#error-retry').addEventListener('click', () => state.retry?.());
  $('#error-reset').addEventListener('click', resetAll);
  $('#brand').addEventListener('click', (e) => { e.preventDefault(); if (!['drop', 'queue', 'watch'].includes(state.view) && !confirm('Start over? The current model will be dropped.')) return; resetAll(); });
  $('#copy-dir').addEventListener('click', async () => {
    const v = $('#output-dir').value;
    try { await navigator.clipboard.writeText(v); toast({ kind: 'success', title: 'Copied output folder', msg: v, timeout: 3000 }); }
    catch { $('#output-dir').select(); toast({ kind: 'warn', title: 'Select and copy manually', msg: 'Clipboard access was blocked.' }); }
  });
  $('#open-dir').addEventListener('click', async (e) => {
    const v = $('#output-dir').value; if (!v) return;
    e.currentTarget.disabled = true;
    try { await api.openFolder(v); } catch (err) { toast({ kind: 'error', title: "Couldn't open the folder", msg: err.message }); }
    finally { e.currentTarget.disabled = false; }
  });
  // batch
  $('#batch-back').addEventListener('click', resetAll);
  $('#batch-squish-btn').addEventListener('click', startBatch);
  // queue + watch navigation
  $('#nav-queue').addEventListener('click', () => showView(state.view === 'queue' ? state.mainView : 'queue'));
  $('#nav-watch').addEventListener('click', () => showView(state.view === 'watch' ? state.mainView : 'watch'));
  $('#queue-back').addEventListener('click', () => showView(state.mainView));
  $('#watch-back').addEventListener('click', () => showView(state.mainView));
  $('#watch-form').addEventListener('submit', submitWatch);
  $('#watch-preset').addEventListener('change', updateWatchHint);
  $('#watch-folder-pick').addEventListener('click', async () => { const p = await pickFolder({ title: 'Folder to watch', start: $('#watch-folder').value.trim() }); if (p) { $('#watch-folder').value = p; $('#watch-folder').focus(); } });
  $('#watch-output-pick').addEventListener('click', async () => { const p = await pickFolder({ title: 'Output folder', start: $('#watch-output').value.trim() }); if (p) { $('#watch-output').value = p; $('#watch-output').focus(); } });
  window.addEventListener('keydown', (e) => { if (e.key === 'Escape') for (const t of document.querySelectorAll('.toast')) t.querySelector('.toast-close')?.click(); });
  updateNavCounts(0, 0);

  showView('drop');
  const [health, presets] = await Promise.allSettled([api.health(), api.presets()]);
  if (health.status === 'fulfilled') {
    const hv = health.value;
    state.health = hv;
    $('#version-chip').textContent = `v${hv.version}`;
    $('#foot-threads').textContent = `${hv.threads} threads · output root ${hv.output_root}`;
    const gpuEl = $('#foot-gpu');
    if (hv.gpu?.available) { gpuEl.replaceChildren(svg(ICONS.bolt, 12), ` ${hv.gpu.name || 'GPU'}`); gpuEl.className = 'foot-gpu is-gpu'; gpuEl.title = 'Textures are baked with the GPU ray tracer'; }
    else { gpuEl.replaceChildren(svg(ICONS.cpu, 12), ' CPU only'); gpuEl.className = 'foot-gpu'; gpuEl.title = 'No GPU detected — baking runs on the CPU'; }
    $('#squish-note').textContent = `Output goes to a new folder under ${hv.output_root}.`;
    $('#batch-note').textContent = `Each model gets its own folder under ${hv.output_root}.`;
    $('#watch-output').placeholder = `Default: ${hv.output_root}`;
  } else {
    $('#version-chip').textContent = 'offline';
    $('#foot-gpu').textContent = '';
    toast({ kind: 'error', title: 'Backend not reachable', msg: health.reason?.message, timeout: 0 });
  }
  if (presets.status === 'fulfilled' && Array.isArray(presets.value) && presets.value.length) {
    state.presets = presets.value;
    renderPresets();
    selectPreset(state.presets.find((p) => p.id === 'hero')?.id || state.presets[0].id);
    renderWatchPresetSelect();
  } else if (presets.status === 'rejected') {
    toast({ kind: 'error', title: "Couldn't load presets", msg: presets.reason?.message });
  }
  if (state.health && state.presetId) renderAdvanced(); // GPU availability is known now
}
boot().catch((err) => { console.error(err); toast({ kind: 'error', title: 'The UI failed to start', msg: err.message, timeout: 0 }); });
