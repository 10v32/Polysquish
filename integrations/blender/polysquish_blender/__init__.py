"""Polysquish for Blender: squish huge AI-generated meshes into game-ready assets.

Thin wrapper around the ``polysquish`` executable (https://github.com/10v32/Polysquish).
"""

bl_info = {
    "name": "Polysquish",
    "author": "Polysquish contributors",
    "version": (0, 1, 0),
    "blender": (4, 0, 0),
    "location": "View3D > Sidebar > Polysquish",
    "description": "Squish multi-million-polygon meshes into clean, baked, LOD'd assets using the polysquish executable",
    "doc_url": "https://github.com/10v32/Polysquish",
    "tracker_url": "https://github.com/10v32/Polysquish/issues",
    "category": "Object",
}

import importlib

import bpy

from . import binary, operators, panel, preferences, properties

_modules = (binary, preferences, properties, operators, panel)

if "_reloaded" in locals():  # support F8 / script.reload during development
    for _m in _modules:
        importlib.reload(_m)
_reloaded = True


def menu_func(self, context):
    self.layout.separator()
    self.layout.operator(operators.POLYSQUISH_OT_squish.bl_idname, text="Polysquish: Squish Selected")
    self.layout.operator(operators.POLYSQUISH_OT_squish_file.bl_idname, text="Polysquish: Squish File…")


def register():
    for module in (preferences, properties, operators, panel):
        for cls in module.classes:
            bpy.utils.register_class(cls)
    properties.register_runtime_props()
    bpy.types.VIEW3D_MT_object.append(menu_func)


def unregister():
    bpy.types.VIEW3D_MT_object.remove(menu_func)
    properties.unregister_runtime_props()
    for module in (panel, operators, properties, preferences):
        for cls in reversed(module.classes):
            bpy.utils.unregister_class(cls)


if __name__ == "__main__":
    register()
