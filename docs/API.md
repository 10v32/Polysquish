# Polysquish local API (v1)

The `polysquish` executable serves the UI and this JSON API on `http://127.0.0.1:7777`
(`polysquish ui --port N` to change). Everything is local; no network is used.

## Conventions
- JSON everywhere except uploads (multipart) and file downloads.
- Long operations are **jobs**. Create one, then poll `GET /api/jobs/{id}` every ~400 ms.
- Errors: `{ "error": "human readable message" }` with a 4xx/5xx status.

## Endpoints

### `GET /api/health`
```json
{ "version": "0.1.0", "threads": 8, "output_root": "/home/me/Polysquish" }
```

### `GET /api/presets`
Array of presets. `recipe` is a complete Recipe (see below) the UI can edit before squishing.
```json
[{ "id": "hero", "name": "Hero asset", "tagline": "PC / console close-up",
   "description": "30k triangles, 2K textures, 3 LODs.", "icon": "sparkles",
   "target_triangles": 30000, "texture_size": 2048, "recipe": { ... } }]
```
Preset ids in v1: `hero`, `prop`, `mobile`, `dcc` (Blender/Maya/C4D clean-up, light decimation), `custom`.

### `POST /api/upload`  (multipart/form-data, field `file`, repeatable)
Upload a model and any sidecar files (`.mtl`, textures). Response:
```json
{ "upload_id": "u_8f3a", "main_file": "dragon.obj", "files": ["dragon.obj", "dragon.mtl"], "size_bytes": 123456 }
```
Accepted main-file extensions: `.obj .ply .stl .glb .gltf`.

### `POST /api/upload/path`  `{ "path": "/abs/path/model.obj" }`
Register a file already on disk (power users). Same response as above.

### `POST /api/inspect`  `{ "upload_id": "u_8f3a" }` → `{ "job_id": "j_01" }`
Parses the model, runs the health analysis and builds a lightweight source preview.
Job `result` when done:
```json
{ "report": HealthReport, "preview_url": "/api/jobs/j_01/preview/source.glb" }
```

### `POST /api/squish`
```json
{ "upload_id": "u_8f3a", "recipe": Recipe, "name": "dragon", "output_dir": null }
```
→ `{ "job_id": "j_02" }`. `output_dir` null means `<output_root>/<name>/`.

### `GET /api/jobs/{id}`
```json
{
  "id": "j_02", "kind": "squish", "status": "running",      // queued | running | done | error | cancelled
  "progress": 0.42,                                          // 0..1 overall
  "stage": "bake", "stage_label": "Baking textures", "stage_progress": 0.3,
  "stages": [ { "id": "import", "label": "Reading model", "status": "done", "seconds": 1.2 },
              { "id": "analyze", "label": "Health check", "status": "done", "seconds": 0.4 },
              { "id": "clean",   "label": "Cleaning",     "status": "done", "seconds": 0.9 },
              { "id": "decimate","label": "Squishing polygons", "status": "done", "seconds": 2.1 },
              { "id": "uv",      "label": "Unwrapping UVs", "status": "done", "seconds": 3.0 },
              { "id": "bake",    "label": "Baking textures", "status": "running", "seconds": null },
              { "id": "lods",    "label": "Building LODs", "status": "pending", "seconds": null },
              { "id": "collision","label": "Collision shapes", "status": "pending", "seconds": null },
              { "id": "export",  "label": "Exporting", "status": "pending", "seconds": null } ],
  "log": ["Loaded 1,204,330 triangles", "Removed 14 floating fragments"],
  "error": null,
  "result": null
}
```
Squish `result` when done:
```json
{
  "output_dir": "/home/me/Polysquish/dragon",
  "files": [ { "name": "dragon.glb", "size_bytes": 1532211, "kind": "glb" },
             { "name": "dragon.obj", "size_bytes": 812000, "kind": "obj" },
             { "name": "dragon_albedo.png", "size_bytes": 2200000, "kind": "texture" },
             { "name": "dragon_normal.png", "size_bytes": 2400000, "kind": "texture" },
             { "name": "dragon_ao.png", "size_bytes": 900000, "kind": "texture" },
             { "name": "report.html", "size_bytes": 20000, "kind": "report" } ],
  "before": { "triangles": 1204330, "vertices": 602167, "size_bytes": 98000000 },
  "after":  { "triangles": 30000, "vertices": 16210, "texture_size": 2048, "size_bytes": 6500000,
              "lods": [ { "level": 0, "triangles": 30000, "screen_coverage": 1.0 },
                        { "level": 1, "triangles": 15000, "screen_coverage": 0.5 } ] },
  "problems_fixed": [ "Welded 1,202 duplicate vertices", "Removed 14 floating fragments" ],
  "report": HealthReport,                                   // of the final mesh
  "preview": { "source": "/api/jobs/j_02/preview/source.glb", "result": "/api/jobs/j_02/preview/result.glb" },
  "download_zip": "/api/jobs/j_02/zip",
  "timings": { "import": 1.2, "decimate": 2.1 }
}
```

### `POST /api/jobs/{id}/cancel` → `{ "ok": true }`
### `GET /api/jobs/{id}/preview/source.glb`, `GET /api/jobs/{id}/preview/result.glb`
### `GET /api/jobs/{id}/files/{name}` download one output; `GET /api/jobs/{id}/zip` all outputs.
### `GET /api/jobs` → array of job summaries (same shape as above, without `log`).

## HealthReport
```json
{
  "triangles": 1204330, "vertices": 602167,
  "bounds": { "min": [x,y,z], "max": [x,y,z], "size": [x,y,z], "diagonal": 1.73 },
  "components": 15, "largest_component_fraction": 0.992,
  "non_manifold_edges": 12, "boundary_edges": 340, "degenerate_triangles": 3, "duplicate_vertices": 1202,
  "watertight": false, "has_normals": true, "has_uvs": false, "has_vertex_colors": true,
  "materials": 1, "textures": [ { "kind": "base_color", "width": 4096, "height": 4096 } ],
  "units_guess": "meters", "up_axis_guess": "Y",
  "problems": [
    { "id": "floaters", "severity": "warn",            // info | warn | error
      "title": "14 floating fragments",
      "detail": "Small disconnected pieces that are usually generation noise.",
      "fix": "Removed automatically during cleanup." } ]
}
```

## Recipe
```json
{
  "preset": "hero",
  "cleanup":  { "weld": true, "weld_tolerance": 0.00001, "remove_degenerate": true,
                "remove_floaters": true, "floater_min_fraction": 0.001, "fix_winding": true },
  "decimate": { "target_triangles": 30000, "target_ratio": null, "max_error": null,
                "lock_border": true, "preserve_uvs": true, "preserve_colors": true, "aggressive": false },
  "uv":       { "enabled": true, "resolution": 2048, "padding": 4, "keep_existing_if_good": true },
  "bake":     { "enabled": true, "resolution": 2048, "normal_map": true, "albedo": true, "ao": true,
                "ao_samples": 32, "metallic_roughness": true, "normal_convention": "opengl",
                "ray_distance": null, "dilation_px": 8, "supersample": 2 },
  "lods":     { "count": 3, "ratios": [0.5, 0.25, 0.1] },
  "collision":{ "convex_hull": true, "box": true, "simplified_mesh": true, "simplified_triangles": 300 },
  "export":   { "glb": true, "obj": true, "report": true, "scale": 1.0,
                "target": "generic" },           // generic | unity | unreal | godot | blender | maya | c4d
  "seed": 1337
}
```
`weld_tolerance`, `max_error` and `floater_min_fraction` are fractions of the bounding-box diagonal.

---

# V2 additions

## `GET /api/health` (extended)
```json
{ "version": "0.2.0", "threads": 8, "output_root": "...",
  "gpu": { "available": true, "name": "Apple M2" } }        // name null when CPU only
```

## Presets
A new preset `character` (id `character`, icon `person`) appears before `hero`.

## Recipe (new fields, all optional; defaults shown)
```json
"cleanup":  { ..., "remove_hidden": true, "hidden_samples": 48 },
"decimate": { ..., "chunk_threshold": 1500000, "keep_materials": false },
"retopo":   { "mode": "triangles",          // triangles | quad_dominant | voxel
              "voxel_resolution": 256, "voxel_keep_fraction": 1.0 },
"bake":     { ..., "hard_edge_angle": 60, "ao_denoise": true, "gpu": true },
"lods":     { ..., "imposter": false, "imposter_resolution": 1024 },
"export":   { ..., "fbx": true, "skin": true }
```

## Squish result (new fields)
```json
"metrics": {
  "deviation": { "mean": 0.0008, "max": 0.0061, "p95": 0.0021, "unit": "fraction_of_size",
                 "mean_abs": 0.0014, "max_abs": 0.011 },
  "texel_density": { "mean": 1024.5, "min": 310.2, "max": 2210.0, "unit": "texels_per_unit" },
  "uv_charts": 61, "quads": 0, "polygons": 4999, "watertight": true,
  "hidden_faces_removed": 12034, "tracer": "gpu"
},
"rig": { "joints": 42, "animations": ["Idle", "Walk"] } | null,
"preview": {
  "source": "/api/jobs/j_02/preview/source.glb",
  "result": "/api/jobs/j_02/preview/result.glb",
  "heatmap_deviation": "/api/jobs/j_02/preview/heatmap_deviation.glb",   // vertex-coloured LOD0
  "heatmap_density":   "/api/jobs/j_02/preview/heatmap_density.glb"      // may be null
}
```
Heat-maps use a fixed ramp: blue (0) → mint → amber → coral (max). `files[].kind` gains
`fbx`, `imposter` (atlas PNGs + card mesh) and `heatmap`.

## Batch
### `POST /api/batch`
```json
{ "upload_ids": ["u_1", "u_2"], "recipe": Recipe, "output_dir": null }
```
→ `{ "batch_id": "b_01", "job_ids": ["j_10", "j_11"] }`. Jobs run **one at a time** in order.
### `GET /api/batch/{id}` → `{ "id", "job_ids", "done": 1, "total": 2, "status": "running" }`
### `GET /api/jobs` lists every job (queued ones have `status: "queued"`), newest last.

## Watch folders
### `POST /api/watch`
```json
{ "folder": "/Users/me/Downloads/meshy", "output_dir": null, "recipe": Recipe, "preset": "prop" }
```
→ `{ "watch_id": "w_01" }`. New supported files appearing in `folder` (and already present ones
that have no output yet) are queued automatically once their size stops changing.
Outputs go to `<output_dir or output_root>/<file stem>/`.
### `GET /api/watch` → `[{ "id", "folder", "output_dir", "preset", "processed": 3, "queued": 1, "job_ids": [...], "active": true }]`
### `DELETE /api/watch/{id}` → `{ "ok": true }`

## File browser (for picking folders/paths from the UI)
### `GET /api/fs?path=/some/dir`
```json
{ "path": "/some/dir", "parent": "/some",
  "dirs": ["assets", "exports"],
  "files": [ { "name": "robot.glb", "size_bytes": 1234, "supported": true } ] }
```
Omit `path` for the user's home directory.

## Open output folder
### `POST /api/open-folder` `{ "path": "/abs/output/dir" }` → `{ "ok": true }` (only folders Polysquish created).
