/* Polysquish UI — single-page state machine + three.js viewer.
   Plain ES module, no build step. API contract: docs/API.md.
   Open with ?mock=1 to run against an in-page fake backend. */

import * as THREE from 'three';
import { OrbitControls } from 'three/addons/controls/OrbitControls.js';
import { GLTFLoader } from 'three/addons/loaders/GLTFLoader.js';
import { RoomEnvironment } from 'three/addons/environments/RoomEnvironment.js';

const params = new URLSearchParams(location.search);
const MOCK = params.has('mock') && params.get('mock') !== '0';
const MOCK_FAIL = params.get('fail') === '1';
const POLL_MS = 400;
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
function trim(v, d) { return Number(v.toFixed(d)).toString(); }
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const clamp = (v, a, b) => Math.min(b, Math.max(a, v));
const deepClone = (o) => JSON.parse(JSON.stringify(o));
const fileStem = (name = '') => name.replace(/^.*[\\/]/, '').replace(/\.[^.]+$/, '') || 'model';
const extOf = (name = '') => (name.match(/\.[^.]+$/) || [''])[0].toLowerCase();

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
  download: '<path d="M12 4v11m0 0l-4-4m4 4l4-4M4 19h16"/>',
  copy: '<rect x="9" y="9" width="11" height="11" rx="2"/><path d="M5 15V6a2 2 0 0 1 2-2h9"/>',
  x: '<path d="M6 6l12 12M18 6L6 18"/>',
  shield: '<path d="M12 3l8 3v6c0 4.5-3.5 7.8-8 9-4.5-1.2-8-4.5-8-9V6z"/><path d="M9 12l2 2 4-4"/>',
};
const presetIcon = (name = '') => {
  const k = name.toLowerCase();
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
  job: (id) => request(`/api/jobs/${encodeURIComponent(id)}`),
  cancel: (id) => request(`/api/jobs/${encodeURIComponent(id)}/cancel`, { method: 'POST' }),
  fileUrl: (jobId, name) => `/api/jobs/${encodeURIComponent(jobId)}/files/${encodeURIComponent(name)}`,
  zipUrl: (result, jobId) => result.download_zip || `/api/jobs/${encodeURIComponent(jobId)}/zip`,
  previewObject: null, // real backend serves GLBs; the viewer loads them via GLTFLoader
};

/* =====================================================================
   Mock backend (?mock=1)
   ===================================================================== */
function createMockApi() {
  const baseRecipe = (over = {}) => deepMerge({
    preset: 'hero',
    cleanup: { weld: true, weld_tolerance: 0.00001, remove_degenerate: true, remove_floaters: true, floater_min_fraction: 0.001, fix_winding: true },
    decimate: { target_triangles: 30000, target_ratio: null, max_error: null, lock_border: true, preserve_uvs: true, preserve_colors: true, aggressive: false },
    uv: { enabled: true, resolution: 2048, padding: 4, keep_existing_if_good: true },
    bake: { enabled: true, resolution: 2048, normal_map: true, albedo: true, ao: true, ao_samples: 32, metallic_roughness: true, normal_convention: 'opengl', ray_distance: null, dilation_px: 8, supersample: 2 },
    lods: { count: 3, ratios: [0.5, 0.25, 0.1] },
    collision: { convex_hull: true, box: true, simplified_mesh: true, simplified_triangles: 300 },
    export: { glb: true, obj: true, report: true, scale: 1.0, target: 'generic' },
    seed: 1337,
  }, over);
  const presets = [
    { id: 'hero', name: 'Hero asset', tagline: 'PC / console close-up', description: '30k triangles, 2K textures, 3 LODs.', icon: 'sparkles', target_triangles: 30000, texture_size: 2048, recipe: baseRecipe({ preset: 'hero' }) },
    { id: 'prop', name: 'Prop', tagline: 'Mid-distance set dressing', description: '8k triangles, 1K textures, 2 LODs.', icon: 'cube', target_triangles: 8000, texture_size: 1024,
      recipe: baseRecipe({ preset: 'prop', decimate: { target_triangles: 8000 }, uv: { resolution: 1024 }, bake: { resolution: 1024, ao_samples: 16 }, lods: { count: 2, ratios: [0.5, 0.25] } }) },
    { id: 'mobile', name: 'Mobile / web', tagline: 'Tiny and fast', description: '3k triangles, 512px textures, 2 LODs, no AO.', icon: 'phone', target_triangles: 3000, texture_size: 512,
      recipe: baseRecipe({ preset: 'mobile', decimate: { target_triangles: 3000, aggressive: true }, uv: { resolution: 512 }, bake: { resolution: 512, ao: false, metallic_roughness: false }, lods: { count: 2, ratios: [0.5, 0.25] }, collision: { simplified_mesh: false, simplified_triangles: 120 }, export: { obj: false } }) },
    { id: 'dcc', name: 'DCC clean-up', tagline: 'Blender · Maya · C4D', description: 'Light decimation to 200k, keep detail, no baking.', icon: 'wrench', target_triangles: 200000, texture_size: 4096,
      recipe: baseRecipe({ preset: 'dcc', decimate: { target_triangles: 200000, lock_border: false }, uv: { enabled: false, resolution: 4096 }, bake: { enabled: false, resolution: 4096 }, lods: { count: 0, ratios: [] }, collision: { convex_hull: false, box: false, simplified_mesh: false }, export: { target: 'blender' } }) },
    { id: 'custom', name: 'Custom', tagline: 'Your own recipe', description: 'Start from Hero and tweak anything.', icon: 'sliders', target_triangles: 30000, texture_size: 2048, recipe: baseRecipe({ preset: 'custom' }) },
  ];

  const uploads = new Map();
  const jobs = new Map();
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
    ['clean', 'Cleaning', 0.5, 'Welded 1,202 duplicate vertices', 'Removed 14 floating fragments (0.8% of surface)', 'Fixed 3 degenerate triangles'],
    ['decimate', 'Squishing polygons', 1.3, 'Quadric decimation 1,204,330 → {tris} triangles', 'Max deviation 0.021% of diagonal'],
    ['uv', 'Unwrapping UVs', 0.9, 'Generated 23 UV charts, 91.4% atlas coverage'],
    ['bake', 'Baking textures', 1.2, 'Baked albedo {res}×{res}', 'Baked normal map (OpenGL convention)', 'Baked ambient occlusion, 32 samples'],
    ['lods', 'Building LODs', 0.5, 'LOD1 {lod1} · LOD2 {lod2} · LOD3 {lod3} triangles'],
    ['collision', 'Collision shapes', 0.3, 'Convex hull 64 verts · box · simplified mesh 300 tris'],
    ['export', 'Exporting', 0.35, 'Wrote {name}.glb, {name}.obj, textures and report.html'],
  ];
  const INSPECT_STAGES = [
    ['import', 'Reading model', 0.9, 'Loaded 1,204,330 triangles'],
    ['analyze', 'Health check', 0.6, 'Found 15 components, 1,202 duplicate vertices'],
    ['preview', 'Building preview', 0.4, 'Source preview: 196,800 triangles'],
  ];

  function makeJob(kind, stageDefs, finish, vars = {}) {
    const id = nid('j');
    const total = stageDefs.reduce((a, s) => a + s[2], 0);
    const job = {
      id, kind, status: 'queued', progress: 0, stage: null, stage_label: null, stage_progress: 0,
      stages: stageDefs.map(([sid, label]) => ({ id: sid, label, status: 'pending', seconds: null })),
      log: [], error: null, result: null,
      _t0: performance.now() + 250, _defs: stageDefs, _total: total, _finish: finish, _vars: vars, _cancelled: false, _logged: new Set(),
    };
    jobs.set(id, job);
    return job;
  }
  const fill = (s, vars) => s.replace(/\{(\w+)\}/g, (_, k) => vars[k] ?? `{${k}}`);
  function tick(job) {
    if (['done', 'error', 'cancelled'].includes(job.status)) return;
    const t = (performance.now() - job._t0) / 1000;
    if (t < 0) { job.status = 'queued'; return; }
    job.status = 'running';
    let acc = 0;
    for (let i = 0; i < job._defs.length; i++) {
      const [sid, label, dur, ...lines] = job._defs[i];
      const st = job.stages[i];
      if (t >= acc + dur) {
        st.status = 'done'; st.seconds = Number((dur * (0.9 + 0.2 * Math.random())).toFixed(2));
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
  const publicJob = (job) => { tick(job); const { _t0, _defs, _total, _finish, _vars, _cancelled, _logged, ...pub } = job; return deepClone(pub); };

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
    }
    texCache[kind] = c;
    return c;
  }
  const dataUrlFor = (name) => {
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
    const files = [];
    if (recipe.export.glb) files.push({ name: `${name}.glb`, size_bytes: Math.round(tris * 51 + (bakeOn ? res * res * 0.9 : 0)), kind: 'glb' });
    if (recipe.export.obj) files.push({ name: `${name}.obj`, size_bytes: Math.round(tris * 27), kind: 'obj' });
    if (bakeOn && recipe.bake.albedo) files.push({ name: `${name}_albedo.png`, size_bytes: Math.round(res * res * 0.52), kind: 'texture' });
    if (bakeOn && recipe.bake.normal_map) files.push({ name: `${name}_normal.png`, size_bytes: Math.round(res * res * 0.57), kind: 'texture' });
    if (bakeOn && recipe.bake.ao) files.push({ name: `${name}_ao.png`, size_bytes: Math.round(res * res * 0.21), kind: 'texture' });
    if (bakeOn && recipe.bake.metallic_roughness) files.push({ name: `${name}_mr.png`, size_bytes: Math.round(res * res * 0.18), kind: 'texture' });
    if (recipe.export.report) files.push({ name: 'report.html', size_bytes: 20480, kind: 'report' });
    const lods = [{ level: 0, triangles: tris, screen_coverage: 1.0 }];
    (recipe.lods.ratios || []).slice(0, recipe.lods.count).forEach((r, i) => lods.push({ level: i + 1, triangles: Math.round(tris * r), screen_coverage: Number((r).toFixed(3)) }));
    const after_size = files.reduce((a, f) => a + f.size_bytes, 0);
    const fixed = ['Welded 1,202 duplicate vertices', 'Removed 14 floating fragments', 'Fixed 3 degenerate triangles', 'Repaired 12 non-manifold edges', 'Generated UV layout (91.4% coverage)'];
    return {
      output_dir: `/home/me/Polysquish/${name}`,
      files,
      before: { triangles: 1204330, vertices: 602167, size_bytes: upload.size_bytes },
      after: { triangles: tris, vertices: verts, texture_size: res, size_bytes: after_size, lods },
      problems_fixed: fixed,
      report: resultReport(tris, verts),
      preview: { source: `/api/jobs/${job.id}/preview/source.glb`, result: `/api/jobs/${job.id}/preview/result.glb` },
      download_zip: `/api/jobs/${job.id}/zip`,
      timings: Object.fromEntries(job.stages.map((s) => [s.id, s.seconds])),
    };
  }

  return {
    async health() { await latency(); return { version: '0.1.0-mock', threads: 8, output_root: '/home/me/Polysquish' }; },
    async presets() { await latency(); return deepClone(presets); },
    async upload(files) {
      await sleep(500 + Math.random() * 400);
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
      const up = { upload_id: nid('u'), main_file: path.replace(/^.*[\\/]/, ''), files: [path.replace(/^.*[\\/]/, '')], size_bytes: 98000000 };
      uploads.set(up.upload_id, up);
      return deepClone(up);
    },
    async inspect(upload_id) {
      await latency();
      const up = uploads.get(upload_id);
      if (!up) throw new ApiError('Unknown upload id', 404);
      const job = makeJob('inspect', INSPECT_STAGES, (j) => ({ report: sourceReport(up.size_bytes), preview_url: `/api/jobs/${j.id}/preview/source.glb` }));
      return { job_id: job.id };
    },
    async squish({ upload_id, recipe, name }) {
      await latency();
      const up = uploads.get(upload_id);
      if (!up) throw new ApiError('Unknown upload id', 404);
      if (!name) throw new ApiError('Output name is required', 400);
      const r = recipe.decimate.resolution;
      const vars = { recipe, name, upload: up, tris: fmtInt(recipe.decimate.target_triangles), res: recipe.bake.resolution,
        lod1: fmtInt(recipe.decimate.target_triangles * (recipe.lods.ratios[0] ?? 0.5)), lod2: fmtInt(recipe.decimate.target_triangles * (recipe.lods.ratios[1] ?? 0.25)), lod3: fmtInt(recipe.decimate.target_triangles * (recipe.lods.ratios[2] ?? 0.1)) };
      void r;
      const job = makeJob('squish', SQUISH_STAGES, squishResult, vars);
      return { job_id: job.id };
    },
    async job(id) {
      await sleep(40 + Math.random() * 60);
      const job = jobs.get(id);
      if (!job) throw new ApiError('Unknown job id', 404);
      return publicJob(job);
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
    fileUrl: (jobId, name) => dataUrlFor(name),
    zipUrl: () => 'data:application/zip;base64,UEsFBgAAAAAAAAAAAAAAAAAAAAAAAA==',
    /* Builds a stand-in for the preview GLB the real backend would serve. */
    previewObject(url) {
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
        // noisy displacement to mimic generation bumps
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
function deepMerge(base, over) {
  const out = Array.isArray(base) ? [...base] : { ...base };
  for (const [k, v] of Object.entries(over || {})) {
    if (v && typeof v === 'object' && !Array.isArray(v) && base && typeof base[k] === 'object' && !Array.isArray(base[k])) out[k] = deepMerge(base[k], v);
    else out[k] = v;
  }
  return out;
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
    this.controls.addEventListener('start', () => { this.controls.autoRotate = false; });

    // soft 3-point rig + hemisphere
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
    this.controls.update();
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
    this.el.dataset.active = name;
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
  clear() { for (const k of [...this.slots.keys()]) this.removeSlot(k); this.fitted = false; this._loading = 0; if (this.loadingEl) this.loadingEl.hidden = true; delete this.el.dataset.loaded; delete this.el.dataset.active; if (this.hud) this.hud.firstElementChild.textContent = '—'; }
  dispose() {
    this.disposed = true; cancelAnimationFrame(this._raf); this._ro.disconnect(); this.clear();
    this.controls.dispose(); this.wireMat.dispose(); this.renderer.dispose(); this.renderer.domElement.remove();
  }
}

/* =====================================================================
   App state
   ===================================================================== */
const state = {
  view: 'drop',
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
  retry: null,          // () => void for the error view
  pollAbort: null,
};
const viewers = {};
window.__polysquish = { state, viewers, MOCK, fmtInt, fmtShort, fmtBytes, fmtSeconds };

function showView(name) {
  state.view = name;
  for (const v of document.querySelectorAll('.view')) v.hidden = v.dataset.view !== name;
  document.body.dataset.view = name;
  window.scrollTo({ top: 0, behavior: 'instant' in window ? 'instant' : 'auto' });
  const heading = $(`#view-${name} h1`);
  if (heading) { heading.setAttribute('tabindex', '-1'); heading.focus({ preventScroll: true }); }
}
function showError(title, err, retry) {
  $('#error-title').textContent = title;
  $('#error-msg').textContent = err?.message || String(err);
  state.retry = retry;
  $('#error-retry').hidden = !retry;
  showView('error');
}

/* ---------- Job polling ---------- */
async function pollJob(id, onUpdate) {
  const token = { cancelled: false };
  state.pollAbort = () => { token.cancelled = true; };
  for (;;) {
    const job = await api.job(id);
    if (token.cancelled) return null;
    onUpdate?.(job);
    if (['done', 'error', 'cancelled'].includes(job.status)) return job;
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

async function startWithFiles(files) {
  const main = files.find((f) => ACCEPTED.includes(extOf(f.name)));
  if (!main) {
    toast({ kind: 'error', title: 'No supported model in that drop', msg: `Accepted main files: ${ACCEPTED.join('  ')}. Drop the model together with its .mtl / textures.` });
    return;
  }
  const dz = $('#dropzone');
  dz.classList.add('is-busy'); dz.setAttribute('aria-busy', 'true');
  const t = toast({ title: `Uploading ${main.name}…`, msg: files.length > 1 ? `${files.length} files, ${fmtBytes(files.reduce((a, f) => a + f.size, 0))}` : fmtBytes(main.size), timeout: 0 });
  try {
    const up = await api.upload(files);
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
  state.inspect = null; state.result = null; state.job = null;
  state.name = fileStem(up.main_file);
  $('#output-name').value = state.name;
  $('#health-file-chip').textContent = `${up.main_file}${up.files.length > 1 ? ` +${up.files.length - 1}` : ''}`;
  $('#health-file-chip').title = up.files.join(', ');
  $('#inspect-title').textContent = 'Health check';
  renderHealthSkeleton();
  viewers.source.clear();
  viewers.source.setLoading(true);
  if (!state.presetId) selectPreset(state.presets[0]?.id);
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
async function loadPreview(viewer, slot, url, opts) {
  if (api.previewObject) return viewer.setSlot(slot, api.previewObject(url), opts);
  return viewer.loadGLB(slot, url, opts);
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
      p.recipe?.lods?.count ? h('span', { text: `${p.recipe.lods.count} LOD${p.recipe.lods.count === 1 ? '' : 's'}` }) : null))));
  updatePresetChip();
}
function selectPreset(id) {
  const p = state.presets.find((x) => x.id === id);
  if (!p) return;
  state.presetId = id;
  state.recipe = deepClone(p.recipe);
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
function updatePresetChip() {
  const chip = $('#preset-active-chip');
  const p = state.presets.find((x) => x.id === state.presetId);
  const custom = state.presetId === 'custom';
  chip.textContent = custom ? 'Custom recipe' : p ? p.name : '—';
  chip.className = `chip ${custom ? 'chip-violet' : 'chip-pink'}`;
  const r = state.recipe;
  if (r) $('#adv-summary').textContent = `${fmtShort(r.decimate.target_triangles)} triangles · ${r.bake.enabled ? `${r.bake.resolution}px textures` : 'no baking'} · ${r.lods.count} LOD${r.lods.count === 1 ? '' : 's'} · ${r.export.target}`;
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

  const toggleCtl = (label, path, { onchange } = {}) => {
    const input = h('input', { type: 'checkbox', checked: !!getPath(r, path) });
    input.addEventListener('change', () => { setPath(r, path, input.checked); onchange?.(input.checked); onEdit(); });
    return h('label', { class: 'toggle' }, h('span', { class: 'toggle-text', text: label }), input, h('span', { class: 'switch', 'aria-hidden': 'true' }));
  };
  const selectCtl = (label, path, options, { onchange, parse = (v) => v } = {}) => {
    const sel = h('select', { class: 'select', 'aria-label': label });
    for (const [val, text] of options) sel.append(h('option', { value: String(val), text, selected: String(getPath(r, path)) === String(val) }));
    sel.addEventListener('change', () => { setPath(r, path, parse(sel.value)); onchange?.(parse(sel.value)); onEdit(); });
    return h('div', { class: 'ctl' }, h('span', { class: 'ctl-label', text: label }), sel);
  };

  // target triangles: log slider + number
  const triVal = h('span', { class: 'val', text: fmtInt(r.decimate.target_triangles) });
  const range = h('input', { type: 'range', class: 'range', min: '0', max: '1000', step: '1', value: String(Math.round(sliderFromTri(r.decimate.target_triangles) * 1000)), 'aria-label': 'Target triangles (log scale)' });
  const num = h('input', { type: 'number', class: 'input', min: String(TRI_MIN), max: String(TRI_MAX), step: '1', value: String(r.decimate.target_triangles), 'aria-label': 'Target triangles' });
  const paintRange = () => range.style.setProperty('--pct', `${(range.value / 10).toFixed(1)}%`);
  paintRange();
  range.addEventListener('input', () => { const t = triFromSlider(range.value / 1000); r.decimate.target_triangles = t; num.value = t; triVal.textContent = fmtInt(t); paintRange(); onEdit(); });
  num.addEventListener('change', () => { const t = clamp(Math.round(Number(num.value) || TRI_MIN), TRI_MIN, TRI_MAX); r.decimate.target_triangles = t; num.value = t; range.value = Math.round(sliderFromTri(t) * 1000); triVal.textContent = fmtInt(t); paintRange(); onEdit(); });

  const texSelect = selectCtl('Texture size', 'bake.resolution', [[512, '512 px'], [1024, '1024 px'], [2048, '2048 px'], [4096, '4096 px']], { parse: Number, onchange: (v) => { r.uv.resolution = v; } });
  const lodSelect = selectCtl('LOD count', 'lods.count', [[0, 'None'], [1, '1 LOD'], [2, '2 LODs'], [3, '3 LODs'], [4, '4 LODs']], { parse: Number, onchange: (v) => { r.lods.ratios = LOD_RATIOS.slice(0, v); } });
  const engineSelect = selectCtl('Target engine', 'export.target', [['generic', 'Generic (glTF standard)'], ['unity', 'Unity'], ['unreal', 'Unreal Engine'], ['godot', 'Godot'], ['blender', 'Blender'], ['maya', 'Maya'], ['c4d', 'Cinema 4D']]);

  const section = (title, ...kids) => h('div', { class: 'adv-section' }, h('h3', { class: 'adv-section-title', text: title }), ...kids);
  panel.replaceChildren(
    section('Polygons',
      h('div', { class: 'ctl' },
        h('span', { class: 'ctl-label' }, 'Target triangles', triVal),
        h('div', { class: 'range-row' }, range, num),
        h('div', { class: 'range-ticks', 'aria-hidden': 'true' }, h('span', { text: '500' }), h('span', { text: '5k' }), h('span', { text: '50k' }), h('span', { text: '500k' }))),
      h('div', { class: 'ctl-grid', style: 'margin-top:12px' }, toggleCtl('Lock open borders', 'decimate.lock_border'), toggleCtl('Aggressive (flatter result)', 'decimate.aggressive'), toggleCtl('Preserve vertex colors', 'decimate.preserve_colors'), toggleCtl('Preserve existing UVs', 'decimate.preserve_uvs'))),
    section('Cleanup',
      h('div', { class: 'ctl-grid' }, toggleCtl('Remove floating fragments', 'cleanup.remove_floaters'), toggleCtl('Weld duplicate vertices', 'cleanup.weld'), toggleCtl('Remove degenerate triangles', 'cleanup.remove_degenerate'), toggleCtl('Fix inverted faces', 'cleanup.fix_winding'))),
    section('Textures',
      h('div', { class: 'ctl-grid' }, texSelect, h('div', { class: 'ctl' }, h('span', { class: 'ctl-label', text: 'Unwrap & bake' }), toggleCtl('Generate UVs and bake maps', 'bake.enabled', { onchange: (v) => { r.uv.enabled = v; } }))),
      h('div', { class: 'ctl-grid', style: 'margin-top:12px' }, toggleCtl('Normal map', 'bake.normal_map'), toggleCtl('Albedo (color)', 'bake.albedo'), toggleCtl('Ambient occlusion', 'bake.ao'), toggleCtl('Metallic / roughness', 'bake.metallic_roughness'))),
    section('LODs & collision',
      h('div', { class: 'ctl-grid' }, lodSelect, h('div', { class: 'ctl' }, h('span', { class: 'ctl-label', text: 'Collision shapes' }), h('div', { class: 'ctl-grid one', style: 'gap:8px' }, toggleCtl('Convex hull', 'collision.convex_hull'), toggleCtl('Bounding box', 'collision.box'), toggleCtl('Simplified mesh', 'collision.simplified_mesh'))))),
    section('Export',
      h('div', { class: 'ctl-grid' }, engineSelect, h('div', { class: 'ctl' }, h('span', { class: 'ctl-label', text: 'Files' }), h('div', { class: 'ctl-grid one', style: 'gap:8px' }, toggleCtl('GLB (binary glTF)', 'export.glb'), toggleCtl('OBJ + MTL', 'export.obj'), toggleCtl('HTML report', 'export.report'))))),
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
    renderResults(job);
    showView('results');
  } catch (err) {
    clearInterval(elapsedTimer);
    showError("Couldn't start squishing", err, () => { showView('inspect'); });
  } finally { btn.disabled = false; }
}
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
  stages.forEach((s, i) => {
    const li = pipe.children[i]; if (!li) return;
    li.className = `stage is-${s.status || 'pending'}`;
    const dot = li.querySelector('.stage-dot');
    if (s.status === 'done') dot.replaceChildren(svg(ICONS.check, 14, 'stroke-width="3"'));
    else if (s.status === 'running') dot.replaceChildren(h('span', { class: 'spinner', 'aria-hidden': 'true' }));
    else if (s.status === 'error') dot.replaceChildren(svg(ICONS.x, 14, 'stroke-width="3"'));
    else dot.replaceChildren(h('span', { class: 'stage-num', text: String(i + 1) }));
    li.querySelector('.stage-time').textContent = s.seconds != null ? fmtSeconds(s.seconds) : '';
    li.setAttribute('aria-current', s.status === 'running' ? 'step' : 'false');
  });
  const pct = Math.round((job?.progress || 0) * 100);
  $('#progress-fill').style.width = `${pct}%`;
  $('#progress-bar').setAttribute('aria-valuenow', String(pct));
  $('#progress-pct').textContent = `${pct}%`;
  const label = $('#progress-stage-label');
  const text = !job ? 'Starting…' : job.status === 'queued' ? 'Queued…' : job.status === 'done' ? 'Done!' : job.status === 'error' ? 'Failed' : job.status === 'cancelled' ? 'Cancelled' : `${job.stage_label || job.stage || 'Working'}…`;
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
function renderResults(job) {
  const r = job.result, id = job.id;
  $('#results-name').textContent = state.name;
  const before = r.before || {}, after = r.after || {};

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

  // LODs
  const lods = after.lods || [];
  $('#lods-body').replaceChildren(lods.length
    ? h('table', { class: 'table' },
        h('thead', {}, h('tr', {}, h('th', { text: 'Level' }), h('th', { text: 'Triangles', class: 'num' }), h('th', { text: 'Coverage', class: 'num' }), h('th', { text: '' }))),
        h('tbody', {}, lods.map((l) => h('tr', {}, h('td', { text: `LOD${l.level}` }), h('td', { class: 'num', text: fmtInt(l.triangles) }), h('td', { class: 'num', text: l.screen_coverage != null ? `${Math.round(l.screen_coverage * 100)}%` : '—' }),
          h('td', {}, h('div', { class: 'lod-bar' }, h('i', { style: `width:${Math.max(3, (l.triangles / (lods[0].triangles || 1)) * 100)}%` })))))))
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

  // files
  $('#output-dir').value = r.output_dir || '';
  $('#file-list').replaceChildren(...(r.files || []).map((f) => h('li', { class: 'file' },
    h('span', { class: `file-kind k-${f.kind || 'file'}`, text: (f.kind || extOf(f.name).slice(1) || 'file').slice(0, 4) }),
    h('div', { class: 'file-info' }, h('span', { class: 'file-name', text: f.name, title: f.name }), h('span', { class: 'file-size', text: fmtBytes(f.size_bytes) })),
    h('a', { class: 'btn btn-sm btn-secondary', href: api.fileUrl(id, f.name), download: f.name, 'aria-label': `Download ${f.name}` }, svg(ICONS.download, 15), 'Download'))));
  const zip = $('#zip-btn');
  zip.href = api.zipUrl(r, id);
  zip.setAttribute('download', `${state.name}.zip`);

  // before/after viewer (same camera)
  const v = viewers.result;
  v.clear();
  v.setWireframe(false); $('#res-wire').setAttribute('aria-pressed', 'false');
  setResultSide('after');
  const prev = r.preview || {};
  (async () => {
    try {
      if (prev.source) await loadPreview(v, 'before', prev.source, { fit: true, show: false });
      if (prev.result) await loadPreview(v, 'after', prev.result, { fit: !prev.source, show: false });
      v.show(v.slots.has('after') ? 'after' : 'before');
      setResultSide(v.active);
    } catch (e) { toast({ kind: 'warn', title: "Couldn't load the 3D preview", msg: e.message }); }
  })();
}
function setResultSide(side) {
  $('#res-before').setAttribute('aria-pressed', String(side === 'before'));
  $('#res-after').setAttribute('aria-pressed', String(side === 'after'));
  const v = viewers.result;
  if (v.slots.has(side)) v.show(side);
}
function resetAll() {
  state.pollAbort?.();
  clearInterval(elapsedTimer);
  state.upload = null; state.inspect = null; state.result = null; state.job = null; state.squishJobId = null; state.inspectJobId = null;
  viewers.source.clear(); viewers.result.clear();
  renderProgress(null);
  showView('drop');
  $('#dropzone').focus({ preventScroll: true });
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
  $('#res-wire').addEventListener('click', (e) => { const on = e.currentTarget.getAttribute('aria-pressed') !== 'true'; e.currentTarget.setAttribute('aria-pressed', String(on)); viewers.result.setWireframe(on); });
  $('#res-before').addEventListener('click', () => setResultSide('before'));
  $('#res-after').addEventListener('click', () => setResultSide('after'));
  $('#error-retry').addEventListener('click', () => state.retry?.());
  $('#error-reset').addEventListener('click', resetAll);
  $('#brand').addEventListener('click', (e) => { e.preventDefault(); if (state.view !== 'drop' && !confirm('Start over? The current model will be dropped.')) return; resetAll(); });
  $('#copy-dir').addEventListener('click', async () => {
    const v = $('#output-dir').value;
    try { await navigator.clipboard.writeText(v); toast({ kind: 'success', title: 'Copied output folder', msg: v, timeout: 3000 }); }
    catch { $('#output-dir').select(); toast({ kind: 'warn', title: 'Select and copy manually', msg: 'Clipboard access was blocked.' }); }
  });
  window.addEventListener('keydown', (e) => { if (e.key === 'Escape') for (const t of document.querySelectorAll('.toast')) t.querySelector('.toast-close')?.click(); });

  showView('drop');
  const [health, presets] = await Promise.allSettled([api.health(), api.presets()]);
  if (health.status === 'fulfilled') {
    state.health = health.value;
    $('#version-chip').textContent = `v${health.value.version}`;
    $('#foot-threads').textContent = `${health.value.threads} threads · output root ${health.value.output_root}`;
    $('#squish-note').textContent = `Output goes to a new folder under ${health.value.output_root}.`;
  } else {
    $('#version-chip').textContent = 'offline';
    toast({ kind: 'error', title: 'Backend not reachable', msg: health.reason?.message, timeout: 0 });
  }
  if (presets.status === 'fulfilled' && Array.isArray(presets.value) && presets.value.length) {
    state.presets = presets.value;
    renderPresets();
    selectPreset(state.presets.find((p) => p.id === 'hero')?.id || state.presets[0].id);
  } else if (presets.status === 'rejected') {
    toast({ kind: 'error', title: "Couldn't load presets", msg: presets.reason?.message });
  }
}
boot().catch((err) => { console.error(err); toast({ kind: 'error', title: 'The UI failed to start', msg: err.message, timeout: 0 }); });
