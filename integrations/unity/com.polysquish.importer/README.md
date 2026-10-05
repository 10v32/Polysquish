# Polysquish Importer for Unity

A UPM package (`com.polysquish.importer`) that runs the `polysquish` executable from the Unity Editor and
turns the result into project assets: textures with correct import settings, a material built from the baked
maps, and a `LODGroup` prefab.

> Written against the Unity 2022.3 LTS / Unity 6 Editor API (no third-party dependencies, editor-only
> assembly); not yet executed inside Unity in this environment. See the manual test checklist in
> [`../../README.md`](../../README.md).

## Requirements

- Unity 2022.3 LTS or newer (Unity 6 included).
- The `polysquish` executable: <https://github.com/10v32/Polysquish/releases>.
- Optional: a glTF importer such as **Unity glTFast** (`com.unity.cloud.gltfast`). Without one the GLB is
  kept as a plain file and the prefab is built from the OBJ outputs, which Unity imports natively.

## Install

Pick one:

- **Package Manager ▸ + ▸ Add package from disk…** and select `com.polysquish.importer/package.json`.
- **Add package from git URL…**:
  `https://github.com/10v32/Polysquish.git?path=integrations/unity/com.polysquish.importer`
- Copy the folder into your project's `Packages/` directory (embedded package).

Then set the executable in **Edit ▸ Preferences ▸ Polysquish** if it is not found automatically. Lookup order:
`POLYSQUISH_BIN` environment variable → the preference → `PATH` → default install folders.

## Use

**Window ▸ Polysquish**

1. Pick a model file with **Browse…**, type a path, or select a `.obj/.ply/.stl/.glb/.gltf` asset in the Project
   window and press **Use selected asset**.
2. Choose a preset (`hero`, `prop`, `mobile`, `character`, `dcc`), optionally override the triangle budget
   and texture size, choose the engine target (defaults to `unity`) and whether to bake textures / AO.
3. Press **Squish**. The executable runs asynchronously (`System.Diagnostics.Process`, polled from
   `EditorApplication.update`); the window shows the current stage and a progress bar. **Cancel** kills it.

When the process exits with code 0 the window:

- copies every output file into `Assets/Polysquish/<name>/` and imports it (`AssetDatabase.ImportAsset`);
- applies import settings, also enforced by an `AssetPostprocessor` for anything under `Assets/Polysquish/`:
  `*_normal.png` → *Normal map*, sRGB off; `*_orm.png` and `*_ao.png` → sRGB off; `*_albedo.png` → sRGB on;
  OBJ/FBX → material import off (we build our own), `*_collision_*.obj` → mesh collider + readable;
- creates `<name>_material.mat` (URP Lit, HDRP Lit or Standard depending on the active render pipeline) with
  the albedo, normal and AO maps. The ORM map is imported linear but not wired up, because Unity's
  metallic/smoothness packing (metallic in R, smoothness in A) differs from the glTF ORM layout;
- builds `<name>.prefab` with a `LODGroup`. If the installed glTF importer exposes the `MSFT_lod` nodes
  (children named `*_LOD1`, `*_LOD2`, …) they are used; otherwise LOD0 comes from `<name>.obj` (or the GLB when
  no OBJ exists) and the other levels from `<name>_LOD{n}.obj`. Transition screen heights are
  0.5 / 0.25 / 0.1 / 0.01;
- optionally adds a convex `MeshCollider` from `<name>_collision_hull.obj`;
- reads `result.json` and shows the before/after triangle counts and per-stage timings; **Open report** opens
  `report.html`.

Non-zero exit codes are reported with the last lines of the executable's output; the full log goes to the Console.

## Files

| Path | Purpose |
|---|---|
| `Editor/PolysquishWindow.cs` | The EditorWindow, import logic, material and LOD prefab builder |
| `Editor/PolysquishCli.cs` | Asynchronous process runner with stage/progress parsing |
| `Editor/PolysquishSettings.cs` | Executable resolution, `EditorPrefs` storage, Preferences page |
| `Editor/PolysquishPostprocessor.cs` | Import settings for `Assets/Polysquish/**` |
| `Editor/Polysquish.Editor.asmdef` | Editor-only assembly definition |
