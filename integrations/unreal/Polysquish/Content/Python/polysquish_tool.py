"""Polysquish for Unreal Engine 5 (Editor Python, content-only plugin).

Adds *Tools ▸ Polysquish* and a toolbar button. The tool runs the ``polysquish`` executable with
``--target unreal`` on a model file (or on the selected Static Mesh, exported to OBJ first), then
imports the result as a Static Mesh with LODs, UCX collision and correctly configured textures.

Console usage (Output Log, Python mode)::

    import polysquish_tool
    polysquish_tool.squish(r"C:/models/dragon.glb")              # default preset from settings
    polysquish_tool.squish(r"C:/models/dragon.glb", preset="prop", target_tris=4000, texture=1024)
    polysquish_tool.set_executable(r"C:/Tools/polysquish.exe")

Written against the UE 5.1+ Python API; not executed inside the editor in this environment.
"""

import json
import os
import platform
import queue
import re
import shutil
import subprocess
import sys
import tempfile
import threading

import unreal

RELEASES_URL = "https://github.com/10v32/Polysquish/releases"
ENV_VAR = "POLYSQUISH_BIN"
PRESETS = ("hero", "prop", "mobile", "character", "dcc")
TEXTURE_SIZES = (0, 512, 1024, 2048, 4096, 8192)
DEST_ROOT = "/Game/Polysquish"
SUPPORTED_INPUTS = (".obj", ".ply", ".stl", ".glb", ".gltf")
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
# Screen sizes for LOD0..LOD3 when polysquish's own coverage values are not available.
DEFAULT_SCREEN_SIZES = (1.0, 0.5, 0.25, 0.1)

MENU_OWNER = "Polysquish"
TOOLS_MENU = "LevelEditor.MainMenu.Tools"
TOOLBAR_MENU = "LevelEditor.LevelEditorToolBar.User"

_active_job = None
_tick_handle = None
_last_output_dir = None


# --------------------------------------------------------------------------- settings


def _settings_path():
    return os.path.join(unreal.Paths.project_saved_dir(), "Polysquish", "settings.json")


def load_settings():
    defaults = {"executable": "", "preset": "hero", "target_tris": 0, "texture": 0, "bake": True, "ao": True}
    try:
        with open(_settings_path(), "r", encoding="utf-8") as fh:
            data = json.load(fh)
        if isinstance(data, dict):
            defaults.update(data)
    except (OSError, ValueError):
        pass
    return defaults


def save_settings(settings):
    path = _settings_path()
    try:
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w", encoding="utf-8") as fh:
            json.dump(settings, fh, indent=2)
    except OSError as exc:
        unreal.log_warning("[Polysquish] could not save settings: {}".format(exc))


def set_preset(preset):
    if preset not in PRESETS:
        unreal.log_error("[Polysquish] unknown preset {!r}; choose one of {}".format(preset, ", ".join(PRESETS)))
        return
    settings = load_settings()
    settings["preset"] = preset
    save_settings(settings)
    unreal.log("[Polysquish] preset set to {}".format(preset))


def set_texture(size):
    settings = load_settings()
    settings["texture"] = int(size)
    save_settings(settings)
    unreal.log("[Polysquish] texture size set to {}".format("preset default" if not size else size))


def toggle_setting(key):
    settings = load_settings()
    settings[key] = not settings.get(key, True)
    save_settings(settings)
    unreal.log("[Polysquish] {} = {}".format(key, settings[key]))


def set_executable(path):
    settings = load_settings()
    settings["executable"] = path or ""
    save_settings(settings)
    unreal.log("[Polysquish] executable set to {}".format(path or "(auto-detect)"))


def show_settings():
    settings = load_settings()
    exe = find_executable()
    text = (
        "Executable: {}\nPreset: {}\nTriangle budget: {}\nTexture size: {}\nBake textures: {}\nAmbient occlusion: {}\n\n"
        "Settings file: {}"
    ).format(
        exe or "NOT FOUND",
        settings["preset"],
        settings["target_tris"] or "preset default",
        settings["texture"] or "preset default",
        settings["bake"],
        settings["ao"],
        _settings_path(),
    )
    _message("Polysquish settings", text)


# --------------------------------------------------------------------------- executable discovery


def _executable_name():
    return "polysquish.exe" if sys.platform.startswith("win") else "polysquish"


def default_locations():
    home = os.path.expanduser("~")
    exe = _executable_name()
    if sys.platform.startswith("win"):
        roots = [
            os.environ.get("LOCALAPPDATA", ""),
            os.environ.get("ProgramFiles", r"C:\Program Files"),
            os.environ.get("ProgramFiles(x86)", r"C:\Program Files (x86)"),
            home,
        ]
        candidates = []
        for root in roots:
            if root:
                candidates.append(os.path.join(root, "Programs", "Polysquish", exe))
                candidates.append(os.path.join(root, "Polysquish", exe))
        candidates.append(os.path.join(home, "Downloads", exe))
        candidates.append(os.path.join(home, ".cargo", "bin", exe))
        return candidates
    if sys.platform == "darwin":
        return [
            "/Applications/Polysquish/" + exe,
            os.path.join(home, "Applications", "Polysquish", exe),
            "/usr/local/bin/" + exe,
            "/opt/homebrew/bin/" + exe,
            os.path.join(home, ".local", "bin", exe),
            os.path.join(home, ".cargo", "bin", exe),
            os.path.join(home, "Downloads", exe),
        ]
    return [
        "/usr/local/bin/" + exe,
        "/usr/bin/" + exe,
        os.path.join(home, ".local", "bin", exe),
        os.path.join(home, ".cargo", "bin", exe),
        "/opt/polysquish/" + exe,
        os.path.join(home, "Downloads", exe),
    ]


def _usable(path):
    return bool(path) and os.path.isfile(path) and os.access(path, os.X_OK)


def find_executable():
    """``POLYSQUISH_BIN`` → settings file → PATH → default install folders. ``None`` when not found."""
    env = os.environ.get(ENV_VAR, "").strip()
    if _usable(env):
        return env
    pref = load_settings().get("executable", "")
    if pref:
        pref = os.path.abspath(os.path.expandvars(os.path.expanduser(pref)))
        if _usable(pref):
            return pref
    on_path = shutil.which("polysquish")
    if on_path:
        return on_path
    for candidate in default_locations():
        if _usable(candidate):
            return candidate
    return None


def missing_message():
    return (
        "Polysquish executable not found.\n\nDownload it from {}\nthen use Tools > Polysquish > Set executable..., "
        "put it on PATH, or set the {} environment variable."
    ).format(RELEASES_URL, ENV_VAR)


def _popen_kwargs():
    kwargs = {}
    if sys.platform.startswith("win"):
        kwargs["creationflags"] = getattr(subprocess, "CREATE_NO_WINDOW", 0x08000000)
    return kwargs


# --------------------------------------------------------------------------- dialogs


def _message(title, text):
    try:
        unreal.EditorDialog.show_message(title, text, unreal.AppMsgType.OK)
    except Exception:  # headless / commandlet
        unreal.log(u"[Polysquish] {}: {}".format(title, text))


def _confirm(title, text):
    try:
        result = unreal.EditorDialog.show_message(title, text, unreal.AppMsgType.YES_NO)
        return result == unreal.AppReturnType.YES
    except Exception:
        return True


def pick_file(title="Choose a model", extensions=SUPPORTED_INPUTS):
    """Native open-file dialog without Slate or C++. Returns a path or ``None`` when unavailable/cancelled."""
    try:
        if sys.platform.startswith("win"):
            return _pick_file_windows(title, extensions)
        if sys.platform == "darwin":
            return _pick_file_macos(title, extensions)
        return _pick_file_linux(title, extensions)
    except Exception as exc:  # pragma: no cover - platform specific
        unreal.log_warning("[Polysquish] file dialog unavailable: {}".format(exc))
        return None


def _pick_file_windows(title, extensions):
    import ctypes
    from ctypes import wintypes

    class OPENFILENAMEW(ctypes.Structure):
        _fields_ = [
            ("lStructSize", wintypes.DWORD),
            ("hwndOwner", wintypes.HWND),
            ("hInstance", wintypes.HINSTANCE),
            ("lpstrFilter", wintypes.LPCWSTR),
            ("lpstrCustomFilter", wintypes.LPWSTR),
            ("nMaxCustFilter", wintypes.DWORD),
            ("nFilterIndex", wintypes.DWORD),
            ("lpstrFile", wintypes.LPWSTR),
            ("nMaxFile", wintypes.DWORD),
            ("lpstrFileTitle", wintypes.LPWSTR),
            ("nMaxFileTitle", wintypes.DWORD),
            ("lpstrInitialDir", wintypes.LPCWSTR),
            ("lpstrTitle", wintypes.LPCWSTR),
            ("Flags", wintypes.DWORD),
            ("nFileOffset", wintypes.WORD),
            ("nFileExtension", wintypes.WORD),
            ("lpstrDefExt", wintypes.LPCWSTR),
            ("lCustData", wintypes.LPARAM),
            ("lpfnHook", ctypes.c_void_p),
            ("lpTemplateName", wintypes.LPCWSTR),
            ("pvReserved", ctypes.c_void_p),
            ("dwReserved", wintypes.DWORD),
            ("FlagsEx", wintypes.DWORD),
        ]

    if extensions:
        pattern = ";".join("*" + e for e in extensions)
        filter_spec = "Supported files\0{}\0All files\0*.*\0\0".format(pattern)
    else:
        filter_spec = "All files\0*.*\0\0"
    buffer = ctypes.create_unicode_buffer(4096)
    ofn = OPENFILENAMEW()
    ofn.lStructSize = ctypes.sizeof(OPENFILENAMEW)
    ofn.lpstrFilter = filter_spec
    ofn.nFilterIndex = 1
    ofn.lpstrFile = ctypes.cast(buffer, wintypes.LPWSTR)
    ofn.nMaxFile = len(buffer)
    ofn.lpstrTitle = title
    ofn.Flags = 0x00080000 | 0x00001000 | 0x00000800 | 0x00000008  # EXPLORER | FILEMUSTEXIST | PATHMUSTEXIST | NOCHANGEDIR
    if ctypes.windll.comdlg32.GetOpenFileNameW(ctypes.byref(ofn)):
        return buffer.value or None
    return None


def _pick_file_macos(title, extensions):
    script = 'POSIX path of (choose file with prompt "{}")'.format(title.replace('"', ""))
    out = subprocess.run(["osascript", "-e", script], capture_output=True, text=True, timeout=600)
    path = out.stdout.strip()
    return path or None


def _pick_file_linux(title, extensions):
    if shutil.which("zenity"):
        cmd = ["zenity", "--file-selection", "--title", title]
        if extensions:
            cmd += ["--file-filter", "Models | " + " ".join("*" + e for e in extensions)]
    elif shutil.which("kdialog"):
        cmd = ["kdialog", "--getopenfilename", os.path.expanduser("~"), "--title", title]
    else:
        return None
    out = subprocess.run(cmd, capture_output=True, text=True, timeout=600)
    path = out.stdout.strip()
    return path or None


# --------------------------------------------------------------------------- running the CLI


class _Job:
    def __init__(self, cmd, cwd, out_dir, name, dest_path, source_asset):
        self.cmd = cmd
        self.out_dir = out_dir
        self.name = name
        self.dest_path = dest_path
        self.source_asset = source_asset
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
            **_popen_kwargs()
        )
        threading.Thread(target=self._reader, daemon=True).start()

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
        changed = False
        while True:
            try:
                line = self._queue.get_nowait()
            except queue.Empty:
                break
            changed = True
            self.lines.append(line)
            text = line.strip()
            if text.startswith(u"\u25b6"):
                label = text[1:].strip()
                if label in STAGE_LABELS:
                    self.current = label
                    unreal.log("[Polysquish] {}".format(label))
            elif text.startswith(u"\u2713"):
                if text[1:].strip() in STAGE_LABELS:
                    self.stages_done += 1
            elif text:
                unreal.log("[Polysquish]   {}".format(text))
        return changed

    def finished(self):
        return self.process.poll() is not None

    def terminate(self):
        if self.process.poll() is None:
            try:
                self.process.terminate()
                self.process.wait(timeout=5)
            except (OSError, subprocess.TimeoutExpired):
                self.process.kill()

    def tail(self, n=8):
        lines = [l for l in self.lines if l.strip() and not l.startswith((u"\u25b6", u"\u2713"))]
        return "\n".join(lines[-n:]) if lines else "(no output)"


def _sanitize(name):
    name = re.sub(r"[^\w\-]+", "_", name or "").strip("_")
    return name or "model"


def squish(input_path, preset=None, target_tris=None, texture=None, bake=None, ao=None, name=None, dest_root=DEST_ROOT, source_asset=None, confirm=True):
    """Run polysquish on ``input_path`` asynchronously and import the result under ``dest_root/<name>``."""
    global _active_job, _tick_handle
    if _active_job is not None and not _active_job.finished():
        _message("Polysquish", "A squish is already running; wait for it to finish (see the Output Log).")
        return False

    exe = find_executable()
    if not exe:
        _message("Polysquish", missing_message())
        return False
    input_path = os.path.abspath(os.path.expanduser(input_path))
    if not os.path.isfile(input_path):
        _message("Polysquish", "File not found:\n{}".format(input_path))
        return False
    if os.path.splitext(input_path)[1].lower() not in SUPPORTED_INPUTS:
        _message("Polysquish", "Unsupported file type. polysquish reads: {}".format(" ".join(SUPPORTED_INPUTS)))
        return False

    settings = load_settings()
    preset = preset or settings["preset"]
    target_tris = settings["target_tris"] if target_tris is None else target_tris
    texture = settings["texture"] if texture is None else texture
    bake = settings["bake"] if bake is None else bake
    ao = settings["ao"] if ao is None else ao
    name = _sanitize(name or os.path.splitext(os.path.basename(input_path))[0])
    dest_path = "{}/{}".format(dest_root.rstrip("/"), name)

    if confirm and not _confirm(
        "Polysquish",
        "Squish\n  {}\nwith preset '{}' into {}?\n\nBudget: {}   Texture: {}   Bake: {}   AO: {}".format(
            input_path, preset, dest_path, target_tris or "preset default", texture or "preset default", bake, ao
        ),
    ):
        return False

    workdir = tempfile.mkdtemp(prefix="polysquish_")
    out_dir = os.path.join(workdir, name + "_squished")
    cmd = [exe, "squish", input_path, "--preset", preset, "--target", "unreal", "-o", out_dir, "--name", name]
    if target_tris:
        cmd += ["--target-tris", str(int(target_tris))]
    if texture:
        cmd += ["--texture", str(int(texture))]
    if not bake:
        cmd.append("--no-bake")
    elif not ao:
        cmd.append("--no-ao")
    unreal.log("[Polysquish] " + " ".join('"{}"'.format(c) if " " in c else c for c in cmd))

    try:
        _active_job = _Job(cmd, workdir, out_dir, name, dest_path, source_asset)
    except OSError as exc:
        shutil.rmtree(workdir, ignore_errors=True)
        _message("Polysquish", "Could not start {}:\n{}".format(exe, exc))
        return False
    _tick_handle = unreal.register_slate_post_tick_callback(_on_tick)
    return True


def cancel():
    global _active_job
    if _active_job is not None and not _active_job.finished():
        _active_job.terminate()
        unreal.log_warning("[Polysquish] cancelled")


def _on_tick(delta_seconds):
    global _active_job, _tick_handle, _last_output_dir
    job = _active_job
    if job is None:
        return
    job.pump()
    if not job.finished():
        return
    job.pump()
    if _tick_handle is not None:
        unreal.unregister_slate_post_tick_callback(_tick_handle)
        _tick_handle = None
    _active_job = None
    workdir = os.path.dirname(job.out_dir)
    try:
        if job.process.returncode != 0:
            unreal.log_error("[Polysquish] exit code {}\n{}".format(job.process.returncode, "\n".join(job.lines)))
            _message("Polysquish failed", "polysquish exited with code {}.\n\n{}".format(job.process.returncode, job.tail()))
            return
        _last_output_dir = job.out_dir
        with unreal.ScopedSlowTask(1, "Polysquish: importing {}".format(job.name)) as task:
            task.make_dialog(False)
            summary = import_results(job.out_dir, job.name, job.dest_path)
        _message("Polysquish", summary)
    except Exception as exc:
        unreal.log_error("[Polysquish] import failed: {!r}".format(exc))
        _message("Polysquish", "Importing the result failed:\n{}".format(exc))
    finally:
        # Keep the output folder when the import failed so the files can be imported by hand.
        if _last_output_dir != job.out_dir or not os.path.isdir(job.out_dir):
            shutil.rmtree(workdir, ignore_errors=True)


# --------------------------------------------------------------------------- import


def _editor_asset_lib():
    return unreal.EditorAssetLibrary


def _static_mesh_subsystem():
    try:
        return unreal.get_editor_subsystem(unreal.StaticMeshEditorSubsystem)
    except Exception:
        return None


def _import_task(filename, dest_path, name, options=None):
    task = unreal.AssetImportTask()
    task.set_editor_property("filename", filename)
    task.set_editor_property("destination_path", dest_path)
    task.set_editor_property("destination_name", name)
    task.set_editor_property("automated", True)
    task.set_editor_property("replace_existing", True)
    task.set_editor_property("save", True)
    if options is not None:
        task.set_editor_property("options", options)
    return task


def _fbx_options():
    """Options for the legacy FBX importer (used for .fbx and .obj): static mesh, UCX collision, no auto collision."""
    options = unreal.FbxImportUI()
    options.set_editor_property("import_mesh", True)
    options.set_editor_property("import_as_skeletal", False)
    options.set_editor_property("import_materials", True)
    options.set_editor_property("import_textures", True)
    options.set_editor_property("import_animations", False)
    options.set_editor_property("mesh_type_to_import", unreal.FBXImportType.FBXIT_STATIC_MESH)
    mesh_data = options.static_mesh_import_data
    mesh_data.set_editor_property("combine_meshes", True)
    mesh_data.set_editor_property("auto_generate_collision", False)
    mesh_data.set_editor_property("one_convex_hull_per_ucx", True)
    mesh_data.set_editor_property("generate_lightmap_u_vs", True)
    mesh_data.set_editor_property("normal_import_method", unreal.FBXNormalImportMethod.FBXNIM_IMPORT_NORMALS_AND_TANGENTS)
    return options


def _find_static_mesh(paths, dest_path):
    for path in paths:
        asset = _editor_asset_lib().load_asset(str(path).split(".")[0]) if path else None
        if isinstance(asset, unreal.StaticMesh):
            return asset
    for path in _editor_asset_lib().list_assets(dest_path, recursive=False, include_folder=False):
        asset = _editor_asset_lib().load_asset(path)
        if isinstance(asset, unreal.StaticMesh):
            return asset
    return None


def import_results(out_dir, name, dest_path):
    """Import the files polysquish wrote to ``out_dir`` under ``dest_path``. Returns a summary string."""
    result = {}
    result_path = os.path.join(out_dir, "result.json")
    if os.path.isfile(result_path):
        try:
            with open(result_path, "r", encoding="utf-8") as fh:
                result = json.load(fh)
        except (OSError, ValueError) as exc:
            unreal.log_warning("[Polysquish] could not read result.json: {}".format(exc))

    fbx = os.path.join(out_dir, name + ".fbx")
    glb = os.path.join(out_dir, result.get("main_glb") or (name + ".glb"))
    obj = os.path.join(out_dir, result.get("main_obj") or (name + ".obj"))
    tools = unreal.AssetToolsHelpers.get_asset_tools()

    if os.path.isfile(fbx):
        main_file, task = fbx, _import_task(fbx, dest_path, name, _fbx_options())
    elif os.path.isfile(glb):
        main_file, task = glb, _import_task(glb, dest_path, name)  # Interchange glTF pipeline
    elif os.path.isfile(obj):
        main_file, task = obj, _import_task(obj, dest_path, name, _fbx_options())
    else:
        raise RuntimeError("no .fbx/.glb/.obj found in {}".format(out_dir))
    unreal.log("[Polysquish] importing {} -> {}".format(main_file, dest_path))
    tools.import_asset_tasks([task])
    imported = list(task.get_editor_property("imported_object_paths") or [])
    mesh = _find_static_mesh(imported, dest_path)
    if mesh is None:
        raise RuntimeError("the importer created no Static Mesh for {} (imported: {})".format(main_file, imported))

    lods_added = _ensure_lods(mesh, out_dir, name, result)
    collision_note = _ensure_collision(mesh, out_dir, name)
    textures = _import_textures(out_dir, name, dest_path, imported)
    material_note = _ensure_material(mesh, name, dest_path, textures)

    try:
        _editor_asset_lib().save_directory(dest_path, only_if_is_dirty=True, recursive=True)
    except Exception as exc:
        unreal.log_warning("[Polysquish] save failed: {}".format(exc))

    before = (result.get("before") or {}).get("triangles")
    after = (result.get("after") or {}).get("triangles")
    timings = result.get("timings") or {}
    lines = ["Imported {} ({} LODs)".format(mesh.get_path_name(), mesh.get_num_lods())]
    if before and after:
        lines.append("{:,} -> {:,} triangles".format(before, after))
    if timings:
        total = sum(v for v in timings.values() if isinstance(v, (int, float)))
        lines.append("Timings: total {:.1f}s ({})".format(total, ", ".join("{} {:.1f}s".format(k, v) for k, v in timings.items() if isinstance(v, (int, float)))))
    if lods_added:
        lines.append(lods_added)
    if collision_note:
        lines.append(collision_note)
    if material_note:
        lines.append(material_note)
    lines.append("Report: {}".format(os.path.join(out_dir, "report.html")))
    summary = "\n".join(lines)
    unreal.log("[Polysquish] " + summary.replace("\n", " | "))
    try:
        _editor_asset_lib().sync_browser_to_objects([mesh.get_path_name()])
    except Exception:
        pass
    return summary


def _lod_files(out_dir, name):
    files = []
    for i in range(1, 16):
        path = os.path.join(out_dir, "{}_LOD{}.obj".format(name, i))
        if not os.path.isfile(path):
            break
        files.append(path)
    return files


def _ensure_lods(mesh, out_dir, name, result):
    """Add LODs from the per-LOD OBJ files when the importer did not pick up MSFT_lod."""
    lod_files = _lod_files(out_dir, name)
    if not lod_files:
        return ""
    if mesh.get_num_lods() > 1:
        note = "LODs came from the imported file"
    else:
        subsystem = _static_mesh_subsystem()
        added = 0
        for index, path in enumerate(lod_files, start=1):
            try:
                if subsystem is not None:
                    ok = subsystem.import_lod(mesh, index, path)
                else:
                    ok = unreal.EditorStaticMeshLibrary.import_lod(mesh, index, path)
                # StaticMeshEditorSubsystem.import_lod returns the LOD index (-1 on failure);
                # the deprecated EditorStaticMeshLibrary returned a bool.
                succeeded = bool(ok) if isinstance(ok, bool) else (isinstance(ok, int) and ok >= 0)
                if succeeded:
                    added += 1
            except Exception as exc:
                unreal.log_warning("[Polysquish] could not import LOD{} from {}: {}".format(index, path, exc))
        note = "Added {} LOD(s) from the per-LOD OBJ files".format(added)
    # Screen sizes: use polysquish's coverage values when present, else the defaults.
    coverage = [l.get("screen_coverage") for l in (result.get("after") or {}).get("lods", [])]
    coverage = [c for c in coverage if isinstance(c, (int, float))]
    sizes = coverage if len(coverage) == mesh.get_num_lods() else list(DEFAULT_SCREEN_SIZES[: mesh.get_num_lods()])
    if len(sizes) == mesh.get_num_lods() and len(sizes) > 1:
        try:
            subsystem = _static_mesh_subsystem()
            if subsystem is not None:
                subsystem.set_lod_screen_sizes(mesh, sizes)
            else:
                unreal.EditorStaticMeshLibrary.set_lod_screen_sizes(mesh, sizes)
        except Exception as exc:
            unreal.log_warning("[Polysquish] could not set LOD screen sizes: {}".format(exc))
    return note


def _ensure_collision(mesh, out_dir, name):
    """UCX_ nodes are recognised by the FBX/OBJ importer. If nothing came through, generate a hull."""
    subsystem = _static_mesh_subsystem()
    try:
        count = subsystem.get_simple_collision_count(mesh) if subsystem else unreal.EditorStaticMeshLibrary.get_simple_collision_count(mesh)
    except Exception:
        count = -1
    if count > 0:
        return "Collision: {} simple shape(s) from UCX_ nodes".format(count)
    if count == 0:
        try:
            shape = unreal.ScriptCollisionShapeType.NDOP18
            if subsystem is not None:
                subsystem.add_simple_collisions(mesh, shape)
            else:
                unreal.EditorStaticMeshLibrary.add_simple_collisions(mesh, shape)
            hull = os.path.join(out_dir, name + "_collision_hull.obj")
            hint = " ({} is available for a hand import)".format(hull) if os.path.isfile(hull) else ""
            return "Collision: UCX_ nodes were not recognised, generated an 18-DOP hull instead" + hint
        except Exception as exc:
            unreal.log_warning("[Polysquish] could not add collision: {}".format(exc))
    return ""


def _import_textures(out_dir, name, dest_path, already_imported):
    """Import the baked PNGs (if the mesh importer did not) and fix their settings. Returns {kind: Texture2D}."""
    kinds = ("albedo", "normal", "ao", "orm")
    textures = {}
    tools = unreal.AssetToolsHelpers.get_asset_tools()
    for kind in kinds:
        png = os.path.join(out_dir, "{}_{}.png".format(name, kind))
        if not os.path.isfile(png):
            continue
        asset_name = "T_{}_{}".format(name, kind)
        asset_path = "{}/{}".format(dest_path, asset_name)
        texture = None
        if _editor_asset_lib().does_asset_exist(asset_path):
            texture = _editor_asset_lib().load_asset(asset_path)
        if texture is None:
            task = _import_task(png, dest_path, asset_name)
            tools.import_asset_tasks([task])
            for path in list(task.get_editor_property("imported_object_paths") or []):
                asset = _editor_asset_lib().load_asset(str(path).split(".")[0])
                if isinstance(asset, unreal.Texture2D):
                    texture = asset
                    break
        if texture is None:
            unreal.log_warning("[Polysquish] could not import {}".format(png))
            continue
        _apply_texture_settings(texture, kind)
        textures[kind] = texture
    # Also fix textures the mesh importer created from the MTL/GLB.
    for path in _editor_asset_lib().list_assets(dest_path, recursive=False, include_folder=False):
        asset = _editor_asset_lib().load_asset(path)
        if isinstance(asset, unreal.Texture2D):
            lowered = asset.get_name().lower()
            for kind in kinds:
                if lowered.endswith("_" + kind):
                    _apply_texture_settings(asset, kind)
                    textures.setdefault(kind, asset)
    return textures


def _apply_texture_settings(texture, kind):
    try:
        if kind == "normal":
            texture.set_editor_property("compression_settings", unreal.TextureCompressionSettings.TC_NORMALMAP)
            texture.set_editor_property("srgb", False)
            texture.set_editor_property("lod_group", unreal.TextureGroup.TEXTUREGROUP_WORLD_NORMAL_MAP)
            # Polysquish bakes OpenGL (+Y) normals; Unreal expects DirectX (-Y).
            texture.set_editor_property("flip_green_channel", True)
        elif kind in ("orm", "ao"):
            texture.set_editor_property("srgb", False)
            texture.set_editor_property("compression_settings", unreal.TextureCompressionSettings.TC_MASKS)
        elif kind == "albedo":
            texture.set_editor_property("srgb", True)
            texture.set_editor_property("compression_settings", unreal.TextureCompressionSettings.TC_DEFAULT)
        texture.modify()
        texture.post_edit_change()
        _editor_asset_lib().save_loaded_asset(texture, only_if_is_dirty=True)
    except Exception as exc:
        unreal.log_warning("[Polysquish] texture settings for {}: {}".format(texture.get_name(), exc))


def _ensure_material(mesh, name, dest_path, textures):
    """Build M_<name> from the baked textures and assign it to every material slot."""
    if not textures:
        return ""
    mel = unreal.MaterialEditingLibrary
    mat_name = "M_" + name
    mat_path = "{}/{}".format(dest_path, mat_name)
    material = _editor_asset_lib().load_asset(mat_path) if _editor_asset_lib().does_asset_exist(mat_path) else None
    if material is None:
        material = unreal.AssetToolsHelpers.get_asset_tools().create_asset(mat_name, dest_path, unreal.Material, unreal.MaterialFactoryNew())
    if material is None:
        return "Material: could not create {}".format(mat_path)

    def sample(texture, sampler, y):
        node = mel.create_material_expression(material, unreal.MaterialExpressionTextureSample, -400, y)
        node.set_editor_property("texture", texture)
        node.set_editor_property("sampler_type", sampler)
        return node

    if "albedo" in textures:
        node = sample(textures["albedo"], unreal.MaterialSamplerType.SAMPLERTYPE_COLOR, -300)
        mel.connect_material_property(node, "RGB", unreal.MaterialProperty.MP_BASE_COLOR)
    if "normal" in textures:
        node = sample(textures["normal"], unreal.MaterialSamplerType.SAMPLERTYPE_NORMAL, 0)
        mel.connect_material_property(node, "RGB", unreal.MaterialProperty.MP_NORMAL)
    if "orm" in textures:
        node = sample(textures["orm"], unreal.MaterialSamplerType.SAMPLERTYPE_LINEAR_COLOR, 300)
        mel.connect_material_property(node, "R", unreal.MaterialProperty.MP_AMBIENT_OCCLUSION)
        mel.connect_material_property(node, "G", unreal.MaterialProperty.MP_ROUGHNESS)
        mel.connect_material_property(node, "B", unreal.MaterialProperty.MP_METALLIC)
    elif "ao" in textures:
        node = sample(textures["ao"], unreal.MaterialSamplerType.SAMPLERTYPE_LINEAR_COLOR, 300)
        mel.connect_material_property(node, "R", unreal.MaterialProperty.MP_AMBIENT_OCCLUSION)
    mel.recompile_material(material)
    _editor_asset_lib().save_loaded_asset(material, only_if_is_dirty=False)

    slots = len(mesh.get_editor_property("static_materials") or []) or 1
    for index in range(slots):
        try:
            mesh.set_material(index, material)
        except Exception as exc:
            unreal.log_warning("[Polysquish] could not assign material slot {}: {}".format(index, exc))
    return "Material: {} ({})".format(mat_path, ", ".join(sorted(textures)))


# --------------------------------------------------------------------------- entry points used by the menu


def squish_file_dialog():
    exe = find_executable()
    if not exe:
        _message("Polysquish", missing_message())
        return
    path = pick_file()
    if not path:
        _message(
            "Polysquish",
            "No file chosen (or no native file dialog is available on this system).\n\n"
            "Run this in the Output Log (Python):\n  import polysquish_tool; polysquish_tool.squish(r\"/path/to/model.glb\")",
        )
        return
    squish(path)


def squish_selected_static_mesh():
    """Export the selected Static Mesh to OBJ, squish it and import the result as <name>_squished."""
    selected = [a for a in unreal.EditorUtilityLibrary.get_selected_assets() if isinstance(a, unreal.StaticMesh)]
    if not selected:
        _message("Polysquish", "Select a Static Mesh in the Content Browser first.")
        return
    mesh = selected[0]
    name = _sanitize(mesh.get_name())
    workdir = tempfile.mkdtemp(prefix="polysquish_src_")
    obj_path = os.path.join(workdir, name + ".obj")
    task = unreal.AssetExportTask()
    task.set_editor_property("object", mesh)
    task.set_editor_property("filename", obj_path)
    task.set_editor_property("automated", True)
    task.set_editor_property("prompt", False)
    task.set_editor_property("replace_identical", True)
    try:
        task.set_editor_property("exporter", unreal.StaticMeshExporterOBJ())
    except Exception:
        pass  # let the engine pick an exporter for the .obj extension
    if not unreal.Exporter.run_asset_export_task(task) or not os.path.isfile(obj_path):
        shutil.rmtree(workdir, ignore_errors=True)
        _message("Polysquish", "Could not export {} to OBJ.".format(mesh.get_name()))
        return
    squish(obj_path, name=name + "_squished", source_asset=mesh)


def set_executable_dialog():
    path = pick_file("Locate the polysquish executable", extensions=())
    if path:
        set_executable(path)
        _message("Polysquish", "Executable set to\n{}".format(path))
    else:
        _message("Polysquish", "No file chosen. You can also run:\n  import polysquish_tool; polysquish_tool.set_executable(r\"/path/to/polysquish\")")


def open_releases():
    unreal.SystemLibrary.launch_url(RELEASES_URL)


def open_last_output():
    if _last_output_dir and os.path.isdir(_last_output_dir):
        unreal.SystemLibrary.launch_url("file:///" + _last_output_dir.replace("\\", "/"))
    else:
        _message("Polysquish", "Nothing has been squished in this session yet.")


# --------------------------------------------------------------------------- menu registration


def _entry(name, label, tooltip, command, entry_type=None):
    entry = unreal.ToolMenuEntry(name=name, type=entry_type or unreal.MultiBlockType.MENU_ENTRY)
    entry.set_label(label)
    entry.set_tool_tip(tooltip)
    entry.set_string_command(unreal.ToolMenuStringCommandType.PYTHON, "", string="import polysquish_tool; polysquish_tool.{}".format(command))
    return entry


def register():
    menus = unreal.ToolMenus.get()
    tools_menu = menus.find_menu(TOOLS_MENU)
    if tools_menu is None:
        unreal.log_warning("[Polysquish] menu {} not found; use the Python console instead".format(TOOLS_MENU))
    else:
        sub = tools_menu.add_sub_menu(tools_menu.menu_name, "Polysquish", "PolysquishMenu", "Polysquish", "Squish AI meshes into game-ready Static Meshes")
        sub.add_section("Actions", "Actions")
        sub.add_menu_entry("Actions", _entry("Polysquish.SquishFile", "Squish file...", "Pick a model file (.obj .ply .stl .glb .gltf) and squish it", "squish_file_dialog()"))
        sub.add_menu_entry("Actions", _entry("Polysquish.SquishSelected", "Squish selected Static Mesh", "Export the selected Static Mesh to OBJ, squish it and import the result", "squish_selected_static_mesh()"))
        sub.add_menu_entry("Actions", _entry("Polysquish.Cancel", "Cancel running squish", "Terminate the running polysquish process", "cancel()"))
        sub.add_section("Settings", "Settings")
        preset_menu = sub.add_sub_menu(sub.menu_name, "Settings", "PolysquishPreset", "Preset", "Preset used for the next squish")
        preset_menu.add_section("Presets", "Presets")
        for preset in PRESETS:
            preset_menu.add_menu_entry("Presets", _entry("Polysquish.Preset." + preset, preset, "Use the '{}' preset".format(preset), "set_preset('{}')".format(preset)))
        tex_menu = sub.add_sub_menu(sub.menu_name, "Settings", "PolysquishTexture", "Texture size", "Baked texture resolution")
        tex_menu.add_section("Sizes", "Sizes")
        for size in TEXTURE_SIZES:
            label = "Preset default" if size == 0 else str(size)
            tex_menu.add_menu_entry("Sizes", _entry("Polysquish.Texture.{}".format(size), label, "", "set_texture({})".format(size)))
        sub.add_menu_entry("Settings", _entry("Polysquish.ToggleBake", "Toggle texture baking", "Skip baking entirely for a quick decimate", "toggle_setting('bake')"))
        sub.add_menu_entry("Settings", _entry("Polysquish.ToggleAO", "Toggle ambient occlusion", "AO is the slowest part of baking", "toggle_setting('ao')"))
        sub.add_menu_entry("Settings", _entry("Polysquish.ShowSettings", "Show current settings", "", "show_settings()"))
        sub.add_menu_entry("Settings", _entry("Polysquish.SetExecutable", "Set executable...", "Locate the polysquish executable", "set_executable_dialog()"))
        sub.add_section("Help", "Help")
        sub.add_menu_entry("Help", _entry("Polysquish.OpenOutput", "Open last output folder", "", "open_last_output()"))
        sub.add_menu_entry("Help", _entry("Polysquish.Releases", "Download Polysquish...", RELEASES_URL, "open_releases()"))

    toolbar = menus.find_menu(TOOLBAR_MENU)
    if toolbar is not None:
        button = _entry("Polysquish.ToolbarSquish", "Polysquish", "Squish a model file with Polysquish", "squish_file_dialog()", unreal.MultiBlockType.TOOL_BAR_BUTTON)
        try:
            button.set_icon("EditorStyle", "ClassIcon.StaticMesh")
        except Exception:
            pass
        toolbar.add_section("Polysquish", "Polysquish")
        toolbar.add_menu_entry("Polysquish", button)
    menus.refresh_all_widgets()
    unreal.log("[Polysquish] menu registered (executable: {})".format(find_executable() or "not found - see Tools > Polysquish > Set executable..."))
