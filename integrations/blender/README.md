# Polysquish for Blender

A Blender 4.x add-on that runs the `polysquish` executable on the selected objects (or on a file)
and imports the result: a clean, decimated mesh with baked normal / albedo / AO / ORM textures, and
optionally the LOD chain and collision shapes.

> Written against the Blender 4.x Python API; not yet executed inside Blender in this environment.
> See the manual test checklist in [`../README.md`](../README.md).

## Requirements

- Blender 4.0 or newer (4.2+ can install it as an Extension).
- The `polysquish` executable: download it from <https://github.com/10v32/Polysquish/releases>
  (or `cargo build --release` in this repository).
- The bundled glTF 2.0 add-on must be enabled (it is by default); OBJ import is built in.

## Install

1. Build the zip: `./package.sh` (produces `dist/polysquish_blender-<version>.zip`), or zip the
   `polysquish_blender/` folder yourself so that the zip contains `polysquish_blender/__init__.py`.
2. In Blender: *Edit ▸ Preferences ▸ Add-ons ▸ Install…* (4.0/4.1) or
   *Edit ▸ Preferences ▸ Get Extensions ▸ ⌄ ▸ Install from Disk…* (4.2+), pick the zip and enable **Polysquish**.
3. In the add-on preferences, set **Executable** or press the magnifier button to auto-detect it.
   The executable is looked up in this order: `POLYSQUISH_BIN` environment variable → preferences →
   `polysquish` on `PATH` → default install folders (`/usr/local/bin`, `~/.local/bin`, `~/.cargo/bin`,
   `/Applications/Polysquish`, `%LOCALAPPDATA%\Programs\Polysquish`, `~/Downloads`, …).

## Use

Open the sidebar in the 3D viewport (`N`) and pick the **Polysquish** tab.

| Control | Effect (CLI flag) |
|---|---|
| Preset | `--preset hero \| prop \| mobile \| character \| dcc` |
| Override triangle budget | `--target-tris N` |
| Texture size | `--texture 512..8192` (preset default when unset) |
| Bake textures / Ambient occlusion | `--no-bake` / `--no-ao` |
| Result | replace the originals, hide them, or place the result beside them (+X) |
| Import LODs / collision | loads `<name>_LOD{n}.obj` and `<name>_collision_*.obj` into hidden collections |
| Output folder | `-o <folder>/<name>_squished`; empty = temporary folder, deleted after import |

- **Squish Selected** exports the selection to a temporary GLB (`bpy.ops.export_scene.gltf`, modifiers
  applied, world transforms kept), runs `polysquish squish … --target blender`, then imports the GLB.
  The result lands exactly where the originals were because transforms are baked into the export.
- **Squish File…** opens a file browser (`.obj .ply .stl .glb .gltf`) and imports the result at the origin.
- Blender stays responsive while the executable runs: the operator is modal and polls the process on a
  timer. The panel shows the current stage, a progress bar and, when finished, the per-stage timings read
  from `result.json`. **Cancel** terminates the process.
- *Object ▸ Polysquish: Squish Selected / Squish File…* are also available in the Object menu.

The full command line is printed to the system console; on failure the last lines of the executable's
output are shown in the error message.

## Notes

- The GLB contains an `MSFT_lod` chain, but Blender's importer only loads the scene's root node (LOD0).
  Tick **Import LODs** to load the per-LOD OBJ files instead.
- Textures are embedded in the GLB and unpacked by the importer into the material's image nodes.
  The baked normal map uses the OpenGL (+Y) convention, which is what Blender expects.
- With the `character` preset the selection is exported with skins and animations so the rig is carried over.
