"""The N-panel ("Polysquish" tab in the 3D viewport sidebar)."""

import bpy

from . import binary
from .operators import POLYSQUISH_OT_cancel, POLYSQUISH_OT_open_output, POLYSQUISH_OT_squish, POLYSQUISH_OT_squish_file
from .preferences import POLYSQUISH_OT_open_releases, resolve_executable


class POLYSQUISH_PT_main(bpy.types.Panel):
    bl_label = "Polysquish"
    bl_idname = "POLYSQUISH_PT_main"
    bl_space_type = "VIEW_3D"
    bl_region_type = "UI"
    bl_category = "Polysquish"

    def draw(self, context):
        layout = self.layout
        wm = context.window_manager
        settings = context.scene.polysquish

        exe = resolve_executable(context)
        if not exe:
            box = layout.box()
            box.label(text="polysquish executable not found", icon="ERROR")
            box.label(text="Set it in Preferences > Add-ons > Polysquish")
            box.operator(POLYSQUISH_OT_open_releases.bl_idname, icon="URL")

        col = layout.column(align=True)
        col.prop(settings, "preset")
        row = col.row(align=True)
        row.prop(settings, "override_budget", text="", icon="MOD_DECIM")
        sub = row.row(align=True)
        sub.active = settings.override_budget
        sub.prop(settings, "target_tris")
        col.prop(settings, "texture")

        col = layout.column(align=True, heading="Bake")
        col.prop(settings, "bake")
        sub = col.row()
        sub.active = settings.bake
        sub.prop(settings, "bake_ao")

        col = layout.column(align=True, heading="Import")
        col.prop(settings, "placement", text="")
        col.prop(settings, "import_lods")
        col.prop(settings, "import_collision")
        layout.prop(settings, "output_dir")

        layout.separator()
        running = wm.polysquish_running
        if running:
            layout.operator(POLYSQUISH_OT_cancel.bl_idname, icon="CANCEL")
        else:
            col = layout.column(align=True)
            col.enabled = bool(exe)
            has_mesh = any(o.type == "MESH" for o in context.selected_objects)
            row = col.row(align=True)
            row.enabled = has_mesh
            row.operator(POLYSQUISH_OT_squish.bl_idname, icon="MOD_SMOOTH")
            col.operator(POLYSQUISH_OT_squish_file.bl_idname, icon="FILE_FOLDER")

        status = wm.polysquish_status
        if running or status:
            box = layout.box()
            if running and hasattr(box, "progress"):
                box.progress(factor=wm.polysquish_progress, text=status or "Working…", type="BAR")
            else:
                icon = "TIME" if running else ("CHECKMARK" if wm.polysquish_progress >= 1.0 else "INFO")
                box.label(text=status, icon=icon)
            if wm.polysquish_timings:
                for chunk in _wrap(wm.polysquish_timings, 38):
                    box.label(text=chunk)
            if not running and wm.polysquish_last_output:
                row = box.row(align=True)
                row.operator(POLYSQUISH_OT_open_output.bl_idname, icon="URL", text="Report").report_only = True
                row.operator(POLYSQUISH_OT_open_output.bl_idname, icon="FILE_FOLDER", text="Folder").report_only = False


def _wrap(text, width):
    words = text.split(" ")
    lines, current = [], ""
    for word in words:
        if current and len(current) + 1 + len(word) > width:
            lines.append(current)
            current = word
        else:
            current = (current + " " + word).strip()
    if current:
        lines.append(current)
    return lines


classes = (POLYSQUISH_PT_main,)
