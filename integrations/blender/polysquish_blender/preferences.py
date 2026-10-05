"""Add-on preferences: where the polysquish executable lives."""

import bpy
from bpy.props import BoolProperty, StringProperty

from . import binary


def get_prefs(context=None):
    context = context or bpy.context
    addon = context.preferences.addons.get(__package__)
    return addon.preferences if addon else None


def resolve_executable(context=None):
    prefs = get_prefs(context)
    return binary.find_executable(prefs.executable if prefs else "")


class POLYSQUISH_OT_detect_executable(bpy.types.Operator):
    """Look for the polysquish executable on PATH and in the usual install folders"""

    bl_idname = "polysquish.detect_executable"
    bl_label = "Auto-detect"
    bl_options = {"INTERNAL"}

    def execute(self, context):
        prefs = get_prefs(context)
        found = binary.find_executable(prefs.executable if prefs else "")
        if not found:
            self.report({"WARNING"}, binary.missing_message())
            return {"CANCELLED"}
        if prefs is not None:
            prefs.executable = found
        self.report({"INFO"}, "Using {} ({})".format(found, binary.version_of(found) or "version unknown"))
        return {"FINISHED"}


class POLYSQUISH_OT_open_releases(bpy.types.Operator):
    """Open the Polysquish releases page in a browser"""

    bl_idname = "polysquish.open_releases"
    bl_label = "Download Polysquish"
    bl_options = {"INTERNAL"}

    def execute(self, context):
        bpy.ops.wm.url_open(url=binary.RELEASES_URL)
        return {"FINISHED"}


class PolysquishPreferences(bpy.types.AddonPreferences):
    bl_idname = __package__

    executable: StringProperty(
        name="Executable",
        description="Path to the polysquish executable (leave empty to search PATH and default folders)",
        subtype="FILE_PATH",
        default="",
    )
    keep_temp_files: BoolProperty(
        name="Keep temporary files",
        description="Do not delete the exported GLB and the squish output folder after importing",
        default=False,
    )

    def draw(self, context):
        layout = self.layout
        row = layout.row(align=True)
        row.prop(self, "executable")
        row.operator(POLYSQUISH_OT_detect_executable.bl_idname, icon="VIEWZOOM", text="")

        found = binary.find_executable(self.executable)
        box = layout.box()
        if found:
            version = binary.version_of(found)
            box.label(text="Found: {}".format(found), icon="CHECKMARK")
            box.label(text=version or "Could not read the version (is the file executable?)")
        else:
            box.label(text="polysquish was not found.", icon="ERROR")
            box.label(text="Set the path above, put polysquish on PATH, or set {}.".format(binary.ENV_VAR))
            box.operator(POLYSQUISH_OT_open_releases.bl_idname, icon="URL")
        layout.prop(self, "keep_temp_files")


classes = (
    POLYSQUISH_OT_detect_executable,
    POLYSQUISH_OT_open_releases,
    PolysquishPreferences,
)
