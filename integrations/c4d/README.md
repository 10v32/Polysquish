# Polysquish for Cinema 4D

`polysquish_c4d.py` is a Script Manager script: pick a model with `c4d.storage.LoadDialog`, choose a preset in a
small `GeDialog`, run `polysquish squish --target c4d` (stage progress in the status bar), then
`c4d.documents.MergeDocument` the FBX (V2) or OBJ result into the active document and wire the baked normal
map into the imported material.

> Written against the Cinema 4D Python API (R23 – 2025, Python 3); not yet executed inside Cinema 4D in this
> environment. See the manual test checklist in [`../README.md`](../README.md).

## Install

1. Download the `polysquish` executable from <https://github.com/10v32/Polysquish/releases>.
2. Copy `polysquish_c4d.py` into the user scripts folder: *Extensions ▸ Script Manager ▸ ⌄ ▸ Open Script Folder*
   (`<C4D prefs>/library/scripts/`). Restart Cinema 4D or press *Refresh* in the Script Manager.
3. Run it from the Script Manager, or drag it onto a palette / assign a shortcut via *Window ▸ Customization*.

The executable is looked up in this order: `POLYSQUISH_BIN` environment variable → the path stored in
`<C4D prefs>/polysquish_settings.json` (set from the dialog) → `polysquish` on `PATH` → default install folders.

## Use

1. The file dialog asks for the model (`.obj .ply .stl .glb .gltf`).
2. The dialog shows the executable path (with *Browse…*), preset (default `dcc` — 150k triangles, 4K textures,
   no LODs), an optional triangle budget, texture size, bake toggles and whether to import LODs. Settings are
   remembered between runs.
3. **Squish…** runs the executable. Script Manager scripts are synchronous, so Cinema 4D waits for the process,
   but the status bar shows the current stage and a progress bar.

On success the script:

- merges `<name>.fbx` when present, otherwise `<name>.obj` (`SCENEFILTER_OBJECTS | SCENEFILTER_MATERIALS |
  SCENEFILTER_MERGESCENE`), inside one undo step;
- for each imported standard material: sets the albedo in the Color channel when the importer left it empty, and
  adds `<name>_normal.png` to the Normal channel (tangent space, *Flip Y* off — Polysquish bakes OpenGL +Y normals);
  Reflectance roughness/metalness from the ORM map are not set up automatically (node and Redshift materials are
  left untouched);
- optionally merges `<name>_LOD{n}.obj` under a hidden `<name>_LODs` null;
- shows the before/after triangle counts, timings from `result.json` and the output folder, which is kept because
  the textures are referenced from it (use *File ▸ Save Project with Assets…* to collect them).

Non-zero exit codes show a dialog with the last lines of the executable's output; the full log is in the Python
console. `--target c4d` scales metre-sized sources to centimetres, matching Cinema 4D's default unit.
