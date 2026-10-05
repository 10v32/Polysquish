"""Operators: run the polysquish executable without blocking the UI and import the result."""

import json
import os
import queue
import re
import shutil
import subprocess
import tempfile
import threading
import webbrowser

import bpy
from bpy.props import StringProperty
from bpy_extras.io_utils import ImportHelper
from mathutils import Vector

from . import binary
from .preferences import get_prefs, resolve_executable

# Stage labels printed by the CLI on stderr ("▶ label" when a stage starts, "✓ label" when it ends).
STAGE_LABELS = (
    "Reading model",
    "Health check",
    "Cleaning",
    "Squishing polygons",
    "Unwrapping UVs",
    "Baking textures",
    "Building LODs",
    "Collision shapes",
    "Exporting",
)
TIMING_ORDER = ("import", "analyze", "clean", "decimate", "uv", "bake", "lods", "collision", "export")
SUPPORTED_INPUTS = (".obj", ".ply", ".stl", ".glb", ".gltf")


def sanitize_name(name):
    name = re.sub(r"[^\w\-]+", "_", name).strip("_")
    return name or "model"


def redraw_view3d():
    for window in bpy.context.window_manager.windows:
        for area in window.screen.areas:
            if area.type in {"VIEW_3D", "PROPERTIES"}:
                area.tag_redraw()


def set_status(wm, text=None, progress=None):
    if text is not None:
        wm.polysquish_status = text
    if progress is not None:
        wm.polysquish_progress = max(0.0, min(1.0, progress))
    redraw_view3d()


class SquishJob:
    """A running ``polysquish squish`` process with a background stderr reader."""

    def __init__(self, cmd, cwd):
        self.cmd = cmd
        self.lines = []
        self.stages_done = 0
        self.current = "Starting polysquish"
        self._queue = queue.Queue()
        self.process = subprocess.Popen(
            cmd,
            cwd=cwd,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            stdin=subprocess.DEVNULL,
            text=True,
            encoding="utf-8",
            errors="replace",
            bufsize=1,
            **binary.popen_kwargs()
        )
        self._thread = threading.Thread(target=self._reader, daemon=True)
        self._thread.start()

    def _reader(self):
        pipe = self.process.stdout
        try:
            for line in iter(pipe.readline, ""):
                self._queue.put(line.rstrip("\r\n"))
        except (OSError, ValueError):
            pass
        finally:
            try:
                pipe.close()
            except OSError:
                pass

    def pump(self):
        """Drain new output lines; returns True when something changed."""
        changed = False
        while True:
            try:
                line = self._queue.get_nowait()
            except queue.Empty:
                break
            changed = True
            self.lines.append(line)
            text = line.strip()
            if text.startswith("▶"):
                label = text[1:].strip()
                if label in STAGE_LABELS:
                    self.current = label
            elif text.startswith("✓"):
                if text[1:].strip() in STAGE_LABELS:
                    self.stages_done += 1
            elif text and not text.startswith("Done in"):
                # Progress log lines ("Loaded 1,204,330 triangles", ...)
                self.current = text[:80]
        return changed

    @property
    def progress(self):
        return self.stages_done / float(len(STAGE_LABELS))

    def finished(self):
        return self.process.poll() is not None

    @property
    def returncode(self):
        return self.process.returncode

    def terminate(self):
        if self.process.poll() is None:
            try:
                self.process.terminate()
                self.process.wait(timeout=5)
            except (OSError, subprocess.TimeoutExpired):
                try:
                    self.process.kill()
                except OSError:
                    pass

    def tail(self, n=6):
        interesting = [l for l in self.lines if l.strip() and not l.startswith(("▶", "✓"))]
        return " | ".join(interesting[-n:]) if interesting else "(no output)"


def build_command(exe, input_path, out_dir, name, settings):
    cmd = [exe, "squish", input_path, "--preset", settings.preset, "--target", "blender", "-o", out_dir, "--name", name]
    if settings.override_budget:
        cmd += ["--target-tris", str(settings.target_tris)]
    if settings.texture != "0":
        cmd += ["--texture", settings.texture]
    if not settings.bake:
        cmd.append("--no-bake")
    elif not settings.bake_ao:
        cmd.append("--no-ao")
    return cmd


def world_bounds(objects):
    points = []
    for obj in objects:
        for corner in obj.bound_box:
            points.append(obj.matrix_world @ Vector(corner))
    if not points:
        return None
    lo = Vector((min(p.x for p in points), min(p.y for p in points), min(p.z for p in points)))
    hi = Vector((max(p.x for p in points), max(p.y for p in points), max(p.z for p in points)))
    return lo, hi


def export_selection(context, objects, filepath, with_rig):
    """Export ``objects`` to a GLB. Returns an error string or None."""
    if context.mode != "OBJECT":
        try:
            bpy.ops.object.mode_set(mode="OBJECT")
        except RuntimeError as exc:
            return "Could not switch to Object mode: {}".format(exc)
    for obj in context.view_layer.objects:
        obj.select_set(False)
    for obj in objects:
        obj.select_set(True)
    context.view_layer.objects.active = objects[0]
    kwargs = dict(
        filepath=filepath,
        export_format="GLB",
        use_selection=True,
        export_apply=True,
        export_yup=True,
        export_animations=with_rig,
        export_skins=with_rig,
        export_morph=False,
        export_lights=False,
        export_cameras=False,
        export_materials="EXPORT",
        export_image_format="AUTO",
    )
    try:
        bpy.ops.export_scene.gltf(**kwargs)
    except TypeError:
        # Older/newer exporter with a different keyword set: fall back to the essentials.
        try:
            bpy.ops.export_scene.gltf(filepath=filepath, export_format="GLB", use_selection=True)
        except (TypeError, RuntimeError) as exc:
            return "glTF export failed: {}".format(exc)
    except RuntimeError as exc:
        return "glTF export failed: {}".format(exc)
    if not os.path.isfile(filepath):
        return "glTF export produced no file"
    return None


def move_to_collection(obj, collection):
    for coll in list(obj.users_collection):
        coll.objects.unlink(obj)
    collection.objects.link(obj)


def hide_collection(context, collection):
    def find_layer(layer):
        if layer.collection == collection:
            return layer
        for child in layer.children:
            found = find_layer(child)
            if found:
                return found
        return None

    layer = find_layer(context.view_layer.layer_collection)
    if layer is not None:
        layer.hide_viewport = True
    for obj in collection.objects:
        obj.hide_render = True


def import_objs(context, paths, collection):
    """Import a list of OBJ files into ``collection``. Returns the imported objects."""
    imported = []
    for path in paths:
        if not os.path.isfile(path):
            continue
        before = set(bpy.data.objects)
        try:
            bpy.ops.wm.obj_import(filepath=path)
        except (RuntimeError, AttributeError) as exc:
            print("[polysquish] OBJ import failed for {}: {}".format(path, exc))
            continue
        for obj in set(bpy.data.objects) - before:
            move_to_collection(obj, collection)
            imported.append(obj)
    return imported


def format_timings(timings):
    parts = []
    total = 0.0
    for key in TIMING_ORDER:
        value = timings.get(key)
        if isinstance(value, (int, float)):
            parts.append("{} {:.1f}s".format(key, value))
            total += float(value)
    for key, value in timings.items():
        if key not in TIMING_ORDER and isinstance(value, (int, float)):
            parts.append("{} {:.1f}s".format(key, value))
            total += float(value)
    if not parts:
        return ""
    return "total {:.1f}s  ({})".format(total, ", ".join(parts))


class POLYSQUISH_OT_squish(bpy.types.Operator):
    """Squish the selected objects (or a file) with polysquish and import the result"""

    bl_idname = "polysquish.squish"
    bl_label = "Squish Selected"
    bl_options = {"REGISTER", "UNDO"}

    filepath: StringProperty(
        name="Input file",
        description="Model file to squish; leave empty to squish the selected objects",
        default="",
        options={"HIDDEN", "SKIP_SAVE"},
    )

    _timer = None
    _job = None
    _workdir = None
    _out_dir = None
    _name = ""
    _source_names = ()
    _target_collection_name = ""
    _remove_workdir = False

    @classmethod
    def poll(cls, context):
        return not getattr(context.window_manager, "polysquish_running", False)

    # ------------------------------------------------------------------ start
    def invoke(self, context, event):
        return self.execute(context)

    def execute(self, context):
        wm = context.window_manager
        if wm.polysquish_running:
            self.report({"WARNING"}, "Polysquish is already running")
            return {"CANCELLED"}

        exe = resolve_executable(context)
        if not exe:
            self.report({"ERROR"}, binary.missing_message())
            return {"CANCELLED"}

        settings = context.scene.polysquish
        prefs = get_prefs(context)
        self._workdir = tempfile.mkdtemp(prefix="polysquish_")
        self._remove_workdir = not (prefs and prefs.keep_temp_files)

        if self.filepath:
            input_path = bpy.path.abspath(self.filepath)
            if not os.path.isfile(input_path):
                self.report({"ERROR"}, "File not found: {}".format(input_path))
                return self._abort()
            if os.path.splitext(input_path)[1].lower() not in SUPPORTED_INPUTS:
                self.report({"ERROR"}, "Unsupported file type; polysquish reads " + ", ".join(SUPPORTED_INPUTS))
                return self._abort()
            self._name = sanitize_name(os.path.splitext(os.path.basename(input_path))[0])
            self._source_names = ()
            self._target_collection_name = context.collection.name
        else:
            objects = [o for o in context.selected_objects if o.type in {"MESH", "ARMATURE", "EMPTY"}]
            if not any(o.type == "MESH" for o in objects):
                self.report({"ERROR"}, "Select at least one mesh object")
                return self._abort()
            active = context.active_object if context.active_object in objects else objects[0]
            self._name = sanitize_name(active.name)
            self._source_names = tuple(o.name for o in objects)
            self._target_collection_name = active.users_collection[0].name if active.users_collection else context.collection.name
            input_path = os.path.join(self._workdir, self._name + "_source.glb")
            set_status(wm, "Exporting selection…", 0.0)
            error = export_selection(context, objects, input_path, with_rig=settings.preset == "character")
            if error:
                self.report({"ERROR"}, error)
                return self._abort()

        if settings.output_dir:
            root = bpy.path.abspath(settings.output_dir)
            self._out_dir = os.path.join(root, self._name + "_squished")
        else:
            self._out_dir = os.path.join(self._workdir, self._name + "_squished")

        cmd = build_command(exe, input_path, self._out_dir, self._name, settings)
        print("[polysquish] " + " ".join('"{}"'.format(c) if " " in c else c for c in cmd))
        try:
            self._job = SquishJob(cmd, self._workdir)
        except OSError as exc:
            self.report({"ERROR"}, "Could not start polysquish: {}".format(exc))
            return self._abort()

        wm.polysquish_running = True
        wm.polysquish_cancel = False
        wm.polysquish_timings = ""
        set_status(wm, "Starting polysquish…", 0.0)
        self._timer = wm.event_timer_add(0.15, window=context.window)
        wm.modal_handler_add(self)
        return {"RUNNING_MODAL"}

    def _abort(self):
        self._cleanup_workdir(force=True)
        return {"CANCELLED"}

    # ------------------------------------------------------------------ modal
    def modal(self, context, event):
        if event.type != "TIMER":
            return {"PASS_THROUGH"}
        wm = context.window_manager
        job = self._job
        if wm.polysquish_cancel:
            job.terminate()
            self._end(context)
            set_status(wm, "Cancelled", 0.0)
            self.report({"INFO"}, "Polysquish cancelled")
            return {"CANCELLED"}
        if job.pump():
            set_status(wm, job.current, job.progress)
        if not job.finished():
            return {"PASS_THROUGH"}
        job.pump()
        self._end(context)
        if job.returncode != 0:
            set_status(wm, "Failed", 0.0)
            print("[polysquish] exit code {}\n{}".format(job.returncode, "\n".join(job.lines)))
            self.report({"ERROR"}, "polysquish failed (exit {}): {}".format(job.returncode, job.tail()))
            return {"CANCELLED"}
        try:
            return self._import_results(context)
        finally:
            self._cleanup_workdir()

    def _end(self, context):
        wm = context.window_manager
        if self._timer is not None:
            wm.event_timer_remove(self._timer)
            self._timer = None
        wm.polysquish_running = False
        wm.polysquish_cancel = False
        wm.polysquish_last_output = self._out_dir or ""

    def cancel(self, context):
        if self._job is not None:
            self._job.terminate()
        self._end(context)

    def _cleanup_workdir(self, force=False):
        if self._workdir and (self._remove_workdir or force) and os.path.isdir(self._workdir):
            shutil.rmtree(self._workdir, ignore_errors=True)

    # ------------------------------------------------------------------ import
    def _import_results(self, context):
        wm = context.window_manager
        name = self._name
        out = self._out_dir
        result = {}
        result_path = os.path.join(out, "result.json")
        if os.path.isfile(result_path):
            try:
                with open(result_path, "r", encoding="utf-8") as fh:
                    result = json.load(fh)
            except (OSError, ValueError) as exc:
                print("[polysquish] could not read result.json: {}".format(exc))

        glb = os.path.join(out, result.get("main_glb") or (name + ".glb"))
        if not os.path.isfile(glb):
            set_status(wm, "Failed", 0.0)
            self.report({"ERROR"}, "polysquish finished but {} is missing".format(glb))
            return {"CANCELLED"}

        set_status(wm, "Importing result…", 0.95)
        sources = [bpy.data.objects[n] for n in self._source_names if n in bpy.data.objects]
        bounds = world_bounds([o for o in sources if o.type == "MESH"]) if sources else None

        before = set(bpy.data.objects)
        try:
            bpy.ops.import_scene.gltf(filepath=glb)
        except (RuntimeError, AttributeError) as exc:
            set_status(wm, "Failed", 0.0)
            self.report({"ERROR"}, "glTF import failed (is the glTF add-on enabled?): {}".format(exc))
            return {"CANCELLED"}
        new_objects = [o for o in bpy.data.objects if o not in before]
        meshes = [o for o in new_objects if o.type == "MESH"]
        if not meshes:
            self.report({"ERROR"}, "The imported GLB contains no mesh")
            return {"CANCELLED"}
        main = max(meshes, key=lambda o: len(o.data.polygons))
        main.name = name if context.scene.polysquish.placement == "REPLACE" else name + "_squished"
        if main.data:
            main.data.name = main.name

        target_coll = bpy.data.collections.get(self._target_collection_name) or context.scene.collection
        for obj in new_objects:
            move_to_collection(obj, target_coll)

        settings = context.scene.polysquish
        if sources:
            if settings.placement == "BESIDE" and bounds:
                width = (bounds[1].x - bounds[0].x) or 1.0
                for obj in new_objects:
                    if obj.parent is None:
                        obj.location.x += width * 1.1
            elif settings.placement == "REPLACE":
                for obj in sources:
                    bpy.data.objects.remove(obj, do_unlink=True)
            else:  # HIDE
                for obj in sources:
                    obj.hide_set(True)
                    obj.hide_render = True

        if settings.import_lods:
            lod_paths = sorted(
                os.path.join(out, f) for f in os.listdir(out) if re.fullmatch(re.escape(name) + r"_LOD\d+\.obj", f)
            )
            if lod_paths:
                coll = bpy.data.collections.new(name + "_LODs")
                context.scene.collection.children.link(coll)
                import_objs(context, lod_paths, coll)
                hide_collection(context, coll)
        if settings.import_collision:
            coll_paths = sorted(
                os.path.join(out, f) for f in os.listdir(out) if f.startswith(name + "_collision_") and f.endswith(".obj")
            )
            if coll_paths:
                coll = bpy.data.collections.new(name + "_Collision")
                context.scene.collection.children.link(coll)
                for obj in import_objs(context, coll_paths, coll):
                    obj.display_type = "WIRE"
                hide_collection(context, coll)

        for obj in bpy.data.objects:
            obj.select_set(False)
        main.select_set(True)
        context.view_layer.objects.active = main

        timings = format_timings(result.get("timings") or {})
        wm.polysquish_timings = timings
        before_tris = (result.get("before") or {}).get("triangles")
        after_tris = (result.get("after") or {}).get("triangles", len(main.data.polygons))
        if before_tris:
            summary = "Squished {:,} → {:,} triangles".format(before_tris, after_tris)
        else:
            summary = "Squished to {:,} triangles".format(after_tris)
        set_status(wm, summary, 1.0)
        self.report({"INFO"}, "{}. {}".format(summary, timings))
        return {"FINISHED"}


class POLYSQUISH_OT_squish_file(bpy.types.Operator, ImportHelper):
    """Pick a model file, squish it with polysquish and import the result"""

    bl_idname = "polysquish.squish_file"
    bl_label = "Squish File…"
    bl_options = {"REGISTER"}

    filename_ext = ".glb"
    filter_glob: StringProperty(default="*.obj;*.ply;*.stl;*.glb;*.gltf", options={"HIDDEN"})

    @classmethod
    def poll(cls, context):
        return not getattr(context.window_manager, "polysquish_running", False)

    def execute(self, context):
        return bpy.ops.polysquish.squish("INVOKE_DEFAULT", filepath=self.filepath)


class POLYSQUISH_OT_cancel(bpy.types.Operator):
    """Stop the running polysquish process"""

    bl_idname = "polysquish.cancel"
    bl_label = "Cancel"
    bl_options = {"INTERNAL"}

    @classmethod
    def poll(cls, context):
        return getattr(context.window_manager, "polysquish_running", False)

    def execute(self, context):
        context.window_manager.polysquish_cancel = True
        return {"FINISHED"}


class POLYSQUISH_OT_open_output(bpy.types.Operator):
    """Open the last output folder (or its HTML report)"""

    bl_idname = "polysquish.open_output"
    bl_label = "Open Report"
    bl_options = {"INTERNAL"}

    report_only: bpy.props.BoolProperty(default=True, options={"HIDDEN"})

    @classmethod
    def poll(cls, context):
        path = getattr(context.window_manager, "polysquish_last_output", "")
        return bool(path) and os.path.isdir(path)

    def execute(self, context):
        folder = context.window_manager.polysquish_last_output
        report = os.path.join(folder, "report.html")
        target = report if self.report_only and os.path.isfile(report) else folder
        try:
            webbrowser.open("file://" + os.path.abspath(target))
        except Exception as exc:  # pragma: no cover - platform specific
            self.report({"ERROR"}, "Could not open {}: {}".format(target, exc))
            return {"CANCELLED"}
        return {"FINISHED"}


classes = (
    POLYSQUISH_OT_squish,
    POLYSQUISH_OT_squish_file,
    POLYSQUISH_OT_cancel,
    POLYSQUISH_OT_open_output,
)
