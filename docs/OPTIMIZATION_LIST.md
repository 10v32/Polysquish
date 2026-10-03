# Polysquish — Optimization & Request List

**Status:** planning only. Nothing is built yet.

**Goal:** a standalone executable that takes AI-generated 3D models (Meshy, Tripo,
Hunyuan3D, TRELLIS, Rodin, photogrammetry-style outputs, etc.) with millions of
triangles and turns them into small, clean, game-ready meshes: retopologised,
UV-unwrapped, textures baked, LODs and collision generated, exported to engine
formats.

The list below is split into **optimizations** (things the pipeline must do or do
well) and **requests** (decisions or inputs I need from you before building).
Items are numbered 1–100 so we can refer to them by number later.

---

## A. Project foundation & architecture (1–10)

1. **Pick the core language and geometry stack.** Recommendation: Rust or C++ core
   for the heavy mesh work, with a thin UI layer. Python-only will not survive
   multi-million-triangle inputs without heavy native dependencies.
2. **Headless-first design.** Every operation runs from a CLI / library call with
   no GUI; the desktop UI is a client on top. This enables batch jobs, CI, and
   engine plugins later.
3. **Pipeline-as-data.** A squish job is a serialisable recipe (JSON/TOML): import
   → clean → decimate → retopo → UV → bake → LOD → export. Users save and reuse
   presets ("Mobile hero prop", "PC environment clutter").
4. **Deterministic output.** Same input + same recipe = byte-identical mesh. Seeds
   for every stochastic step (remeshing, UV packing) are stored in the recipe.
5. **Half-edge or indexed mesh core with stable IDs** so attributes (UVs, colors,
   normals, material IDs) survive every stage without re-association hacks.
6. **Non-destructive stage cache.** Each stage's result is hashed and cached so
   tweaking the bake resolution does not re-run decimation.
7. **Cancellation and progress reporting** plumbed through every long operation
   from day one (cooperative cancellation tokens, progress callbacks).
8. **Structured logging + crash reporting** with the input mesh stats attached, so
   failures on odd AI outputs are reproducible.
9. **Plugin boundary** for optional backends (e.g. Instant Meshes, Quadriflow, xatlas,
   Blender-as-a-service) so we can swap algorithms without touching the UI.
10. **Test corpus.** A versioned folder of real AI-generated meshes covering the
    failure modes below, with golden outputs for regression testing.

## B. Import & analysis (11–19)

11. **Streaming importers** for OBJ, PLY, STL, glTF/GLB, FBX, USD/USDZ that never
    hold the text file and the parsed mesh in memory at the same time.
12. **Memory-mapped parsing** for multi-GB OBJ/PLY so a 10M-triangle file opens
    in seconds instead of minutes.
13. **Auto-detect AI-generator quirks**: marching-cubes staircase surfaces, voxel
    grid artifacts, vertex-color-only textures, flipped handedness, Y-up/Z-up,
    centimetre vs metre scale.
14. **Mesh health report** before any processing: triangle count, non-manifold
    edges, degenerate faces, duplicate vertices, disconnected shells, holes, self-
    intersections, bounding box, genus estimate.
15. **Shell/component analysis** with size ranking, so floating debris (a very
    common AI artifact) is identified and offered for removal.
16. **Scale normalisation** to a user-chosen real-world size with unit metadata
    preserved for export.
17. **Texture ingestion**: detect vertex colors vs. baked albedo, read embedded
    glTF textures, and flag missing/low-res maps.
18. **Symmetry detection** (planar) to enable mirrored retopo and UV layouts for
    characters and props.
19. **Point-cloud / Gaussian-splat fallback**: if the input is a splat or dense
    point set, run a Poisson reconstruction stage first so the rest of the
    pipeline still works.

## C. Cleanup & repair (20–30)

20. **Weld vertices by tolerance** (spatial hash, not O(n²)).
21. **Remove degenerate and zero-area triangles**, T-junction detection.
22. **Remove isolated floaters** below a volume/triangle threshold, with a preview.
23. **Hole filling** with curvature-aware patches, bounded by a max hole size.
24. **Non-manifold repair**: split bowtie vertices, duplicate shared edges, fix
    inconsistent winding by flood-fill orientation.
25. **Self-intersection resolution** via voxel remesh or boolean union when
    the mesh is a pile of intersecting shells (typical of parts-based AI output).
26. **Interior geometry removal**: ambient-occlusion / visibility-based culling of
    faces that can never be seen from outside (huge poly savings on AI meshes).
27. **Voxel remesh as a "nuke and rebuild" option** (SDF → dual contouring) with
    adaptive resolution so the result is watertight and uniform.
28. **Smoothing of marching-cubes staircases** (Taubin / HC Laplacian) that does
    not shrink volume.
29. **Thin-feature preservation** so antennae, straps, hair cards survive repair.
30. **Hard-edge detection by dihedral angle** feeding later steps (decimation
    constraints, UV seams, normal splitting).

## D. Decimation (31–41)

31. **Quadric Error Metric (QEM) edge collapse** as the baseline simplifier, with
    attribute-aware quadrics (normals, UVs, vertex colors).
32. **Out-of-core / streaming decimation** for inputs that do not fit in RAM:
    spatially partition, decimate chunks, stitch boundaries.
33. **Multithreaded collapse** using independent-set edge scheduling or
    partition-parallel decimation.
34. **Boundary and seam preservation** flags (keep open borders, UV seams,
    material borders fixed or weighted).
35. **Feature-weighted decimation**: user paints or auto-detects "important"
    regions (face, hands, logos) that keep more density.
36. **Silhouette-preserving error metric** (view-dependent / normal deviation
    bound) rather than pure geometric distance.
37. **Target modes**: triangle count, percentage, max error (in scene units), or
    screen-space error at a chosen distance.
38. **Normal-map-aware budget**: estimate how much detail the bake will recover,
    and decimate more aggressively where the normal map will carry it.
39. **Topology-preserving option** (no genus change) vs. aggressive mode that
    allows collapsing tunnels and merging shells.
40. **Fast preview decimation** (vertex clustering) for interactive slider
    feedback, swapped for QEM on commit.
41. **Progressive mesh / LOD chain in one pass** so LOD generation reuses the
    collapse history instead of re-running from scratch.

## E. Retopology (42–51)

42. **Quad-dominant auto retopo** (field-aligned, Instant-Meshes/Quadriflow style)
    for assets that need to deform (characters, cloth).
43. **Triangle remeshing** (isotropic, curvature-adaptive) for static props where
    quads are not required.
44. **Edge-flow guidance** from curvature directions and user-drawn guide strokes
    or symmetry planes.
45. **Density control** by painted or automatic density map (more loops around
    joints and faces).
46. **Hard-edge and crease snapping** so retopo follows sharp mechanical edges.
47. **Pole / singularity minimisation** reporting (count and location of 3- and
    5-valence poles).
48. **Shrink-wrap projection** of the new mesh back onto the high-poly surface
    with offset to avoid intersection.
49. **Character-aware templates**: optional base-mesh fitting (humanoid, quadruped)
    for animation-ready topology.
50. **Keep-as-triangles toggle** for engines that will triangulate anyway, with
    consistent diagonal direction.
51. **Retopo quality score**: aspect ratio, valence histogram, Hausdorff distance
    to source.

## F. UV unwrapping (52–60)

52. **Automatic seam placement** from hard edges, curvature, and hidden-area
    analysis (seams go where the player will not look).
53. **Angle-based / LSCM / ABF++ flattening** with low stretch, plus an xatlas-
    style fallback.
54. **Chart packing** with configurable padding, power-of-two atlases, and
    rotation to maximise texel density.
55. **Texel density target** in texels/metre with a heat-map overlay.
56. **UDIM / multi-material split** for large assets.
57. **Mirrored UV overlap option** for symmetrical assets to double effective
    resolution.
58. **Second UV channel for lightmaps** (no overlaps, uniform, padded) for Unity
    and Unreal.
59. **Preserve existing UVs when sane**: detect whether the AI output's UVs are
    usable and only re-unwrap if stretch or overlap exceeds thresholds.
60. **Seam visibility prediction** to warn when a seam crosses a focal region.

## G. Baking (61–70)

61. **GPU ray-traced bake** (Vulkan / Metal / DX12 compute, or Embree on CPU as
    fallback) from high-poly to low-poly.
62. **Cage generation** with automatic ray distance, plus per-region overrides.
63. **Tangent-space normal maps** matching the target engine's tangent basis
    (MikkTSpace) and Y-flip convention.
64. **Albedo / vertex-color transfer** to texture, including from multi-shell
    sources.
65. **Ambient occlusion, curvature, thickness, position, world-normal, material
    ID maps** for use in Substance / engine shaders.
66. **Anti-aliasing & dilation** (edge padding) so mip-maps do not bleed seams.
67. **Match-by-name / explode bake** for multi-part assets to avoid cross-part
   projection artefacts.
68. **Skew / ray-direction painting** for correcting hard-surface bake skewing.
69. **Texture set presets** (PBR metal/rough, spec/gloss, Unity HDRP mask map,
    Unreal ORM packing) with channel packing.
70. **Texture compression on export** (BC7/BC5, ASTC, ETC2, KTX2/Basis) chosen
    per target platform.

## H. LODs, collision & engine readiness (71–79)

71. **Automatic LOD chain** (e.g. 100 / 50 / 25 / 12 / 5 %) with per-LOD screen-
    size thresholds written into the export.
72. **LOD transition analysis**: visual popping detection and a flipbook preview.
73. **Imposter / billboard generation** for the final LOD.
74. **Collision meshes**: convex hull, V-HACD convex decomposition, simplified
    tri-mesh, and primitive fitting (box/sphere/capsule).
75. **Pivot and origin placement** (base-centre, bounds-centre, custom) with
    forward-axis convention per engine.
76. **Smoothing-group / split-normal export** that matches the baked normal map.
77. **Vertex-count budgeting** that accounts for hard edges and UV seams splitting
    verts at the GPU level (the number that actually matters).
78. **Nanite / mesh-shader mode**: optionally skip aggressive decimation and
    instead clean, UV, and bake for virtualised-geometry engines.
79. **Skinning-weight transfer** when the source has a skeleton, or export-ready
    weight-painting friendly topology when it does not.

## I. Export (80–85)

80. **glTF 2.0 / GLB** with KHR_texture_basisu, KHR_materials extensions, and
    LODs via MSFT_lod.
81. **FBX** (via the Autodesk SDK or ufbx/ufbx-writer alternative — see request
    98) with correct units, axis and smoothing groups.
82. **USD/USDZ** for Omniverse and Apple pipelines.
83. **Engine presets**: Unity (metres, Y-up, left-handed), Unreal (centimetres,
    Z-up), Godot (metres, Y-up), Roblox.
84. **Direct engine drop-in**: write into a project folder with correct import
    settings (.meta / .uasset-compatible metadata where feasible).
85. **Side-by-side export report** (HTML/Markdown) with before/after stats, images
    and the recipe used.

## J. Performance & scale (86–92)

86. **Multi-core everywhere**: work-stealing task scheduler; target ≥ 80 % core
    utilisation on 16-core machines.
87. **SIMD-friendly SoA layouts** for vertex data and quadrics.
88. **GPU acceleration** for baking, AO, visibility culling, and voxelisation,
    with a mandatory CPU fallback.
89. **Memory budget mode**: user sets a RAM cap; the pipeline chooses in-core vs
    out-of-core paths automatically.
90. **Incremental viewport**: level-of-detail display of the source so a 20M-
    triangle input is navigable at 60 fps while processing.
91. **Benchmarks in CI** that fail if a stage regresses by > 10 % on the corpus.
92. **Startup under 2 s and binary under ~100 MB** so it feels like a tool, not a
    suite.

## K. UI / UX (93–96)

93. **Drag-and-drop single-window app**: drop a file, see health report, pick a
    preset, hit Squish, get an export folder.
94. **Before/after split view and wireframe toggle**, Hausdorff error heat-map,
    texel-density heat-map, UV island view.
95. **Guided "problems found" panel** that explains each defect in plain language
    with a one-click fix.
96. **Batch queue** with watch-folder mode (drop 50 generations overnight, wake up
    to game-ready assets).

## L. Requests — decisions I need from you (97–100)

97. **Target platforms and GPU floor.** Windows only, or Windows + macOS + Linux?
    Can we require a Vulkan/Metal-capable GPU, or must everything run on CPU?
98. **Licensing constraints.** Is GPL acceptable (unlocks Blender/Quadriflow
    components) or must the stack be permissive (MIT/Apache/BSD only)? Can we
    use the Autodesk FBX SDK (free but proprietary), or should FBX be via an
    open-source writer?
99. **Primary use case and budgets.** Which engine first (Unity, Unreal, Godot,
    other)? Typical poly budgets per asset class (hero 20–50k, prop 2–5k, mobile
    500–2k)? Characters that must animate, or static props only? This decides
    whether quad retopo (hard) is in v1 or v2.
100. **Reference inputs and success criteria.** Please provide 5–10 sample AI
    models you actually want squished, plus what "good enough" looks like for
    each (poly count, texture size, visual tolerance). Also: UI framework
    preference (native, Qt, Tauri/web, Dear ImGui), and whether a Python
    scripting API is required for your pipeline.

---

### Suggested v1 scope (if you want my recommendation)

Items 1–8, 10–17, 20–27, 30–34, 37, 40–41, 43, 52–55, 61–66, 71, 74–77, 80, 83,
85–86, 93–94. That is: robust import, cleanup, QEM decimation, triangle
remeshing, auto UVs, GPU bake of normals + albedo + AO, LODs, collision,
glTF export, and a minimal UI. Quad retopo, FBX, USD, imposters, skinning,
and the batch watch-folder come in v2.
