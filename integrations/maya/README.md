# Polysquish for Maya

`polysquish_maya.py` is a `maya.cmds` shelf tool: pick a model file, run `polysquish squish --target maya`
in the background, import the FBX (V2) or OBJ result and assign a **Standard Surface** shader wired to the
baked textures.

> Written against the Maya 2022–2025 Python 3 API; not yet executed inside Maya in this environment.
> See the manual test checklist in [`../README.md`](../README.md).

## Install

1. Download the `polysquish` executable from <https://github.com/10v32/Polysquish/releases>.
2. Copy `polysquish_maya.py` into a folder on Maya's Python path, e.g.
   - Windows: `%USERPROFILE%\Documents\maya\scripts\`
   - macOS: `~/Library/Preferences/Autodesk/maya/scripts/`
   - Linux: `~/maya/scripts/`
3. In the Script Editor (Python tab):

   ```python
   import polysquish_maya
   polysquish_maya.install_shelf_button()   # adds a "PSQ" button to the current shelf
   polysquish_maya.show()                   # or open the window directly
   ```

The executable is looked up in this order: `POLYSQUISH_BIN` environment variable → the path stored in the
`polysquishExecutable` optionVar (set from the window) → `polysquish` on `PATH` → default install folders.

## Use

- **Executable**: shows the resolved path, or a link to the releases page when nothing is found.
- **Input**: *Browse…* opens `cmds.fileDialog2` filtered to `.obj .ply .stl .glb .gltf`.
- **Squish**: preset (default `dcc` — 150k triangles, 4K textures, no LODs, Blender/Maya/C4D friendly),
  optional triangle budget override, texture size, bake toggles, and whether to import LODs / collision shapes
  into hidden groups.
- Press **Squish**. The process runs in a worker thread (`subprocess.Popen`) and the status line / progress bar
  are updated through `maya.utils.executeDeferred`, so Maya stays responsive.

On success the tool:

1. imports `<name>.fbx` when present (loads `fbxmaya`), otherwise `<name>.obj` (loads `objExport`), via
   `cmds.file(path, i=True, type="FBX"|"OBJ", namespace=<name>, returnNewNodes=True)`, and groups the meshes as
   `<name>_squished`;
2. creates `<name>_mat` (`standardSurface`) with
   `_albedo` → *Base Color* (sRGB), `_normal` → `bump2d` in tangent-space mode → *Normal Camera* (Raw colour space;
   Polysquish bakes OpenGL +Y normals, which is Maya's convention), `_orm` G → *Specular Roughness*,
   `_orm` B → *Metalness*, and multiplies `_ao` into the base colour when both exist;
3. optionally imports `<name>_LOD{n}.obj` into a hidden `<name>_LODs` group and `<name>_collision_*.obj`
   into a hidden, wireframe `<name>_Collision` group;
4. prints the before/after triangle counts and per-stage timings from `result.json`.

Textures are referenced from the output folder (`<temp>/polysquish_*/<name>_squished/`), which is kept; use
*File ▸ Archive Scene* or copy the PNGs into your project's `sourceimages` if you need them relocated.
Non-zero exit codes show a dialog with the last lines of the executable's output.

## Scripting

```python
import polysquish_maya
polysquish_maya.squish("/models/dragon.glb", preset="hero", target_tris=30000, texture="2048", bake=True, ao=True, import_lods=True)
```

Note that `--target maya` scales metre-sized sources to centimetres on export, matching Maya's default unit.
