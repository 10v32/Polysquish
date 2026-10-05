# Polysquish integrations

Thin wrappers that call the `polysquish` executable from inside engines and DCC tools and import the result.
They never link against the Rust crate; each one shells out to `polysquish squish … --target <engine>` and
then reads the output folder (`<name>.glb`, `<name>.obj/.mtl`, `<name>_LOD{n}.obj`,
`<name>_albedo/_normal/_ao/_orm.png`, `<name>_collision_*.obj`, `report.html`, `result.json`, and `<name>.fbx`
from V2 onwards).

> **Status: written against the public APIs of each host, not executed inside the engines in this environment.**
> Every Python file passes `python3 -m py_compile`; the C# files were checked for balanced braces and consistent
> `using` directives only (no Unity compiler was available). Use the checklists below for the first real run and
> please report anything that breaks at <https://github.com/10v32/Polysquish/issues>.

## Finding the executable

Every integration resolves the executable the same way, in this order:

1. the `POLYSQUISH_BIN` environment variable;
2. the path stored in the host's preferences (Blender add-on preferences, Unity `EditorPrefs`, Unreal
   `Saved/Polysquish/settings.json`, Maya `optionVar`, C4D `polysquish_settings.json`);
3. `polysquish` on `PATH`;
4. default install folders: `/usr/local/bin`, `/usr/bin`, `~/.local/bin`, `~/.cargo/bin`, `/opt/polysquish`,
   `/Applications/Polysquish`, `~/Applications/Polysquish`, `/opt/homebrew/bin`,
   `%LOCALAPPDATA%\Programs\Polysquish`, `%LOCALAPPDATA%\Polysquish`, `%ProgramFiles%\Polysquish`, `~/Downloads`.

When nothing is found, a clear message points to <https://github.com/10v32/Polysquish/releases>. A non-zero exit
code shows the last lines of the executable's stderr; the full log goes to the host's console.

## Matrix

| Host | Location | Entry point | Runs without freezing | Set up automatically | Tested |
|---|---|---|---|---|---|
| **Blender 4.x** | [`blender/`](blender/) | N-panel *Polysquish* tab, *Object* menu | Yes (modal operator polling `Popen`) | Selection → temp GLB → `--target blender`; GLB imported in place (replace / hide / beside); LOD OBJs and collision OBJs into hidden collections; timings from `result.json` | Not executed |
| **Unity 2022.3 / 6** | [`unity/com.polysquish.importer/`](unity/com.polysquish.importer/) | *Window ▸ Polysquish*, *Edit ▸ Preferences ▸ Polysquish* | Yes (`Process` + `EditorApplication.update`) | Files copied to `Assets/Polysquish/<name>/`; `_normal` → Normal map, `_orm`/`_ao` → sRGB off (also enforced by an `AssetPostprocessor`); material (URP/HDRP/Standard) from albedo/normal/AO; `LODGroup` prefab from `MSFT_lod` children if the glTF importer exposes them, else from per-LOD OBJs (0.5 / 0.25 / 0.1 / 0.01); convex `MeshCollider` from the hull | Not executed |
| **Unreal Engine 5.1+** | [`unreal/Polysquish/`](unreal/Polysquish/) | *Tools ▸ Polysquish*, toolbar button, Python console | Yes (Slate post-tick callback) | FBX/GLB (Interchange)/OBJ import with UCX collision; LODs via `StaticMeshEditorSubsystem.import_lod` from per-LOD OBJs with screen sizes from `result.json`; `_normal` → `TC_Normalmap` + flip green, `_orm`/`_ao` → sRGB off + `TC_Masks`; `M_<name>` material (BaseColor / Normal / AO-Roughness-Metallic) assigned to all slots | Not executed |
| **Maya 2022+** | [`maya/`](maya/) | Shelf button → window | Yes (worker thread + `executeDeferred`) | `cmds.file(i=True)` of FBX or OBJ into a namespace; Standard Surface with albedo, tangent-space normal (`bump2d`), ORM roughness/metalness, AO multiplied into base colour; optional hidden LOD / collision groups | Not executed |
| **Cinema 4D R23+** | [`c4d/`](c4d/) | Script Manager script | No (synchronous script; stages shown in the status bar) | `MergeDocument` of FBX or OBJ; albedo and normal channels on the imported material; optional hidden LOD null | Not executed |

CLI flags used by every integration: `--preset <id>` (`hero`, `prop`, `mobile`, `character`, `dcc`),
`--target <engine>`, `-o <dir>`, `--name <name>`, and optionally `--target-tris N`, `--texture S`, `--no-ao`,
`--no-bake`.

## Manual test checklists

Run each one once with a small model (`testdata/` after `scripts/fetch_testdata.sh`, or any `.glb`) and once
with a multi-million-triangle model. Before starting, make sure `polysquish --version` works in a terminal.

### Blender
1. `integrations/blender/package.sh`, install the zip, enable the add-on. Preferences show "Found: …" after
   *Auto-detect*; with the path cleared and `POLYSQUISH_BIN` unset the panel shows the "not found" box and a
   working *Download Polysquish* button.
2. Select a mesh, *Squish Selected* with preset `prop`. The progress bar advances through the stages, the viewport
   stays interactive, *Cancel* kills the process (check with the OS task manager).
3. Result: with *Hide original* the squished object sits exactly over the hidden source; *Replace original* deletes
   the source; *Place beside* offsets it on +X. The material has albedo, normal and ORM image nodes.
4. Tick *Import LODs* / *Import collision shapes*: hidden `<name>_LODs` and `<name>_Collision` collections appear
   with `<name>_LOD1…` and `<name>_collision_hull/box/simplified`.
5. *Squish File…* on an `.obj`/`.ply`/`.stl`; the panel shows "Squished N → M triangles" and the timings line;
   *Report* opens `report.html`.
6. Failure path: point the preferences at a non-executable file → clear error; squish a corrupt file → error with
   the CLI's last lines.

### Unity
1. Install the package (disk or git URL). No compile errors in the Console on 2022.3 LTS and Unity 6; the
   *Edit ▸ Preferences ▸ Polysquish* page shows the resolved executable and version.
2. *Window ▸ Polysquish*, *Browse…* to a model, *Squish* with `hero`. The progress bar updates, the editor stays
   responsive, *Cancel* works.
3. `Assets/Polysquish/<name>/` contains the GLB, OBJs, PNGs, `report.html`, `result.json`. Inspect
   `<name>_normal.png` (Texture Type = Normal map), `<name>_orm.png` / `<name>_ao.png` (sRGB Texture off),
   `<name>_albedo.png` (sRGB on).
4. `<name>.prefab` has a `LODGroup` with 4 levels (hero) at 50 % / 25 % / 10 % and a 1 % cull; drag it into a
   scene and scrub the LOD slider. The renderers use `<name>_material.mat` with the right shader for the active
   render pipeline. With *Add convex collider* a convex `MeshCollider` child named *Collision* exists.
5. With a glTF importer installed (e.g. `com.unity.cloud.gltfast`): the GLB becomes a model asset; if the importer
   exposes `_LOD1…` children the prefab is built from them, otherwise still from the OBJs.
6. Failure path: missing executable → dialog with the releases link; corrupt input → "polysquish failed (exit N)"
   dialog with the CLI's last lines.

### Unreal Engine
1. Copy `unreal/Polysquish/` to `<Project>/Plugins/`, enable the Python Editor Script Plugin, restart. The Output
   Log shows `[Polysquish] menu registered`, *Tools ▸ Polysquish* and the toolbar button exist.
2. *Set executable…* opens the native file dialog on your OS (Windows/macOS/Linux with zenity or kdialog) and the
   path is written to `Saved/Polysquish/settings.json`. *Show current settings* reflects preset/texture toggles.
3. *Squish file…* with preset `prop`: a confirmation dialog, then stage lines in the Output Log while the editor
   remains usable. On completion a summary dialog lists mesh, LOD count, timings and material.
4. `/Game/Polysquish/<name>/` contains `SM_…`/`<name>` static mesh with 4 LODs (screen sizes 1 / 0.5 / 0.25 / 0.1),
   simple collision from `UCX_`/`UBX_` (or an 18-DOP fallback with a note), `T_<name>_normal` (Normalmap,
   sRGB off, flip green), `T_<name>_orm` / `_ao` (Masks, sRGB off), and `M_<name>` assigned to the mesh.
5. *Squish selected Static Mesh* on an existing mesh produces `<name>_squished` next to it.
6. Failure path: non-zero exit → dialog with the CLI's last lines and the output folder kept (*Open last output
   folder*).

### Maya
1. Copy `polysquish_maya.py` to the scripts folder, `install_shelf_button()` adds the *PSQ* button; `show()` opens
   the window and the Executable line shows the resolved path (or the releases URL).
2. *Browse…*, preset `dcc`, *Squish*. The status line and progress bar update while Maya stays responsive.
3. The scene gets a `<name>_squished` group under namespace `<name>`, with `<name>_mat` (standardSurface): albedo →
   Base Color, `bump2d` (tangent space) → Normal Camera, ORM G/B → Roughness/Metalness, AO multiplied into the
   base colour. Scale is in centimetres.
4. With *Import LODs* / *collision shapes*: hidden `<name>_LODs` and wireframe `<name>_Collision` groups.
5. With an FBX in the output (V2): the FBX is preferred over the OBJ and `fbxmaya` is loaded automatically.
6. Failure path: missing executable → `confirmDialog` with the releases link; corrupt input → "Polysquish failed"
   dialog with the CLI's last lines.

### Cinema 4D
1. Copy `polysquish_c4d.py` to the Script Manager folder and run it. The file dialog, then the Polysquish dialog
   appear; *Browse…* sets the executable and the choice survives a restart (`polysquish_settings.json`).
2. *Squish…* with preset `dcc`: the status bar shows the stages and a progress bar.
3. The merged object(s) appear with the imported material showing the albedo in the Color channel and
   `<name>_normal.png` in the Normal channel (tangent space, Flip Y off). Ctrl+Z removes the import in one step.
4. With *Import LODs*: hidden `<name>_LODs` null with `<name>_LOD1…`.
5. Failure path: missing executable → message dialog with the releases link; corrupt input → dialog with the CLI's
   last lines.
