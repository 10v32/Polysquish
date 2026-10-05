# Polysquish for Unreal Engine 5

A content-only editor plugin (no C++ to compile) that adds **Tools ▸ Polysquish** and a toolbar button.
It runs the `polysquish` executable with `--target unreal` and imports the result as a Static Mesh with
LODs, UCX collision, correctly configured textures and a material built from the baked maps.

> Written against the UE 5.1+ Python API (`unreal.ToolMenus`, `unreal.AssetImportTask`,
> `unreal.StaticMeshEditorSubsystem`, `unreal.MaterialEditingLibrary`); not yet executed inside the editor in
> this environment. See the manual test checklist in [`../../README.md`](../../README.md).

## Requirements

- Unreal Engine 5.1 or newer.
- **Python Editor Script Plugin** enabled (*Edit ▸ Plugins ▸ Scripting*). The plugin descriptor lists it and
  *Editor Scripting Utilities* as dependencies, so the editor offers to enable them.
- The `polysquish` executable: <https://github.com/10v32/Polysquish/releases>.

## Install

1. Copy the `Polysquish/` folder (this folder, containing `Polysquish.uplugin`) into your project's `Plugins/`
   directory (`<Project>/Plugins/Polysquish/`), or into `Engine/Plugins/Marketplace/` to share it across projects.
2. Start the editor and enable **Polysquish** under *Edit ▸ Plugins ▸ Editor* if it is not enabled already.
3. `Content/Python/init_unreal.py` runs on start-up and registers the menu. If the executable is not found,
   use **Tools ▸ Polysquish ▸ Set executable…**. Lookup order: `POLYSQUISH_BIN` environment variable →
   `<Project>/Saved/Polysquish/settings.json` → `PATH` → default install folders.

## Use

**Tools ▸ Polysquish**

| Entry | What it does |
|---|---|
| Squish file… | Native open-file dialog (Win32 `GetOpenFileName`, macOS `choose file`, Linux `zenity`/`kdialog`), then runs the CLI |
| Squish selected Static Mesh | Exports the selected Static Mesh to OBJ (`unreal.AssetExportTask`), squishes it and imports `<name>_squished` |
| Cancel running squish | Terminates the process |
| Preset / Texture size / Toggle baking / Toggle AO | Stored in `Saved/Polysquish/settings.json` and used for the next squish |
| Show current settings, Set executable…, Open last output folder, Download Polysquish… | |

The toolbar button is the same as **Squish file…**. Every action is also available from the Python console:

```python
import polysquish_tool
polysquish_tool.squish(r"C:/models/dragon.glb", preset="prop", target_tris=4000, texture=1024)
```

The process runs in the background; stage changes and log lines go to the Output Log (`LogPython`). The editor
polls it from a Slate post-tick callback, so nothing freezes. On exit code 0 the tool imports into
`/Game/Polysquish/<name>/`:

1. **Mesh**: `<name>.fbx` when present (V2), otherwise `<name>.glb` through Interchange, otherwise `<name>.obj`
   through the FBX importer. FBX/OBJ imports use `FbxImportUI` with *combine meshes*, *auto-generate collision
   off* and *one convex hull per UCX*, so the `UCX_<name>_hull` / `UBX_<name>_box` objects become simple collision.
2. **LODs**: if the imported mesh has a single LOD, `<name>_LOD{n}.obj` are added with
   `StaticMeshEditorSubsystem.import_lod` (falls back to `EditorStaticMeshLibrary`). Screen sizes come from
   `result.json` (`after.lods[].screen_coverage`), else 1.0 / 0.5 / 0.25 / 0.1.
3. **Collision**: if no simple collision came through (e.g. Interchange ignored the UCX nodes), an 18-DOP hull is
   generated and the message names `<name>_collision_hull.obj` for a hand import.
4. **Textures**: `T_<name>_albedo|normal|ao|orm` are imported if the mesh importer did not. `_normal` →
   `TC_Normalmap`, sRGB off, *flip green channel* on (Polysquish bakes OpenGL +Y normals; Unreal expects DirectX);
   `_orm` and `_ao` → sRGB off, `TC_Masks`; `_albedo` → sRGB on.
5. **Material** `M_<name>`: albedo → Base Color, normal → Normal, ORM R/G/B → AO / Roughness / Metallic (or AO alone),
   assigned to every material slot.

Non-zero exit codes show a dialog with the last lines of the executable's output; the full log is in the Output Log.
The temporary output folder is kept when the import fails so the files can be imported by hand
(**Open last output folder**).

## Notes

- Polysquish exports with `--target unreal`, which scales metre-sized sources to centimetres in the OBJ/FBX
  outputs and names collision objects `UCX_`/`UBX_`.
- A `Modules` list is intentionally empty: the plugin ships only content and Python, so no compiler is needed and
  it works with Launcher (binary) engine builds.
