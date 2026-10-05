"""Scene-level squish settings and window-manager runtime state."""

import bpy
from bpy.props import BoolProperty, EnumProperty, FloatProperty, IntProperty, PointerProperty, StringProperty

PRESETS = (
    ("hero", "Hero asset", "30k triangles, 2K textures, 3 LODs, collision"),
    ("prop", "Environment prop", "5k triangles, 1K textures, 3 LODs, collision"),
    ("mobile", "Mobile / VR", "1.5k triangles, 1K textures, 2 LODs"),
    ("character", "Character", "40k faces, quad-dominant, rig kept, 2 LODs"),
    ("dcc", "Clean for DCC", "150k triangles, 4K textures, no LODs; keeps detail for further work"),
)

TEXTURE_SIZES = (
    ("0", "Preset default", "Use the texture size of the preset"),
    ("512", "512", ""),
    ("1024", "1K", ""),
    ("2048", "2K", ""),
    ("4096", "4K", ""),
    ("8192", "8K", ""),
)

PLACEMENTS = (
    ("REPLACE", "Replace original", "Delete the source objects and put the squished mesh in their place"),
    ("HIDE", "Hide original", "Keep the source objects but hide them; the squished mesh takes their place"),
    ("BESIDE", "Place beside", "Keep the source objects and put the squished mesh next to them on X"),
)


class PolysquishSceneSettings(bpy.types.PropertyGroup):
    preset: EnumProperty(name="Preset", items=PRESETS, default="hero")
    override_budget: BoolProperty(
        name="Override triangle budget",
        description="Use the triangle budget below instead of the preset's",
        default=False,
    )
    target_tris: IntProperty(
        name="Triangles",
        description="Triangle budget for LOD0",
        default=30000,
        min=50,
        soft_max=500000,
    )
    texture: EnumProperty(name="Texture size", items=TEXTURE_SIZES, default="0")
    bake: BoolProperty(
        name="Bake textures",
        description="Bake normal, albedo, AO and ORM maps from the original (disable for a quick decimate)",
        default=True,
    )
    bake_ao: BoolProperty(
        name="Ambient occlusion",
        description="Bake an AO map (slowest part of baking)",
        default=True,
    )
    placement: EnumProperty(name="Result", items=PLACEMENTS, default="HIDE")
    import_lods: BoolProperty(
        name="Import LODs",
        description="Load the per-LOD OBJ files into a hidden '<name>_LODs' collection",
        default=False,
    )
    import_collision: BoolProperty(
        name="Import collision shapes",
        description="Load the collision OBJ files into a hidden '<name>_Collision' collection",
        default=False,
    )
    output_dir: StringProperty(
        name="Output folder",
        description="Where the squished files are written (empty = a temporary folder that is removed after import)",
        subtype="DIR_PATH",
        default="",
    )


def register_runtime_props():
    wm = bpy.types.WindowManager
    wm.polysquish_running = BoolProperty(default=False, options={"SKIP_SAVE"})
    wm.polysquish_cancel = BoolProperty(default=False, options={"SKIP_SAVE"})
    wm.polysquish_status = StringProperty(default="", options={"SKIP_SAVE"})
    wm.polysquish_progress = FloatProperty(default=0.0, min=0.0, max=1.0, options={"SKIP_SAVE"})
    wm.polysquish_timings = StringProperty(default="", options={"SKIP_SAVE"})
    wm.polysquish_last_output = StringProperty(default="", options={"SKIP_SAVE"})
    bpy.types.Scene.polysquish = PointerProperty(type=PolysquishSceneSettings)


def unregister_runtime_props():
    for name in (
        "polysquish_running",
        "polysquish_cancel",
        "polysquish_status",
        "polysquish_progress",
        "polysquish_timings",
        "polysquish_last_output",
    ):
        if hasattr(bpy.types.WindowManager, name):
            delattr(bpy.types.WindowManager, name)
    if hasattr(bpy.types.Scene, "polysquish"):
        del bpy.types.Scene.polysquish


classes = (PolysquishSceneSettings,)
