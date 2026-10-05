"""Polysquish for Cinema 4D (R23+ / 2023+, Python 3).

A Script Manager script: pick a model file, choose a preset in a small dialog, run
``polysquish squish --target c4d`` (status shown in the status bar) and merge the FBX (V2) or OBJ
result into the active document with ``c4d.documents.MergeDocument``. The baked normal map is wired
into the imported material's Normal channel.

Install: copy this file to the Script Manager folder (Script ▸ Script Manager ▸ ⌄ ▸ Open Folder, or
``<prefs>/library/scripts/``), then run it from Extensions ▸ Script Manager or assign it a shortcut.

Written against the Cinema 4D Python API (R23 - 2025); not executed inside Cinema 4D in this environment.
"""

import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time

import c4d
from c4d import documents, gui, storage

RELEASES_URL = "https://github.com/10v32/Polysquish/releases"
ENV_VAR = "POLYSQUISH_BIN"
PRESETS = ("dcc", "hero", "prop", "mobile", "character")
TEXTURE_SIZES = ("Preset default", "512", "1024", "2048", "4096", "8192")
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

# Dialog element ids
ID_EXE, ID_EXE_BROWSE, ID_PRESET, ID_BUDGET_ON, ID_BUDGET, ID_TEXTURE, ID_BAKE, ID_AO, ID_LODS, ID_OK, ID_CANCEL, ID_RELEASES = range(1000, 1012)


# --------------------------------------------------------------------------- settings / executable discovery


def _settings_path():
    return os.path.join(storage.GeGetC4DPath(c4d.C4D_PATH_PREFS), "polysquish_settings.json")


def load_settings():
    data = {"executable": "", "preset": "dcc", "target_tris": 0, "texture": 0, "bake": True, "ao": False, "import_lods": False}
    try:
        with open(_settings_path(), "r", encoding="utf-8") as fh:
            loaded = json.load(fh)
        if isinstance(loaded, dict):
            data.update(loaded)
    except (OSError, ValueError):
        pass
    return data


def save_settings(data):
    try:
        with open(_settings_path(), "w", encoding="utf-8") as fh:
            json.dump(data, fh, indent=2)
    except OSError as exc:
        print("[polysquish] could not save settings: {}".format(exc))


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


def find_executable(preferred=None):
    """``POLYSQUISH_BIN`` → settings file → PATH → default install folders. ``None`` when not found."""
    env = os.environ.get(ENV_VAR, "").strip()
    if _usable(env):
        return env
    preferred = preferred if preferred is not None else load_settings().get("executable", "")
    if preferred:
        preferred = os.path.abspath(os.path.expandvars(os.path.expanduser(preferred)))
        if _usable(preferred):
            return preferred
    on_path = shutil.which("polysquish")
    if on_path:
        return on_path
    for candidate in default_locations():
        if _usable(candidate):
            return candidate
    return None


def missing_message():
    return (
        "Polysquish executable not found.\n\nDownload it from {}\nthen set the path in the Polysquish dialog, "
        "put it on PATH, or set the {} environment variable."
    ).format(RELEASES_URL, ENV_VAR)


def _popen_kwargs():
    kwargs = {}
    if sys.platform.startswith("win"):
        kwargs["creationflags"] = getattr(subprocess, "CREATE_NO_WINDOW", 0x08000000)
    return kwargs


# --------------------------------------------------------------------------- dialog


class SquishDialog(gui.GeDialog):
    def __init__(self, settings):
        super(SquishDialog, self).__init__()
        self.settings = settings
        self.accepted = False

    def CreateLayout(self):
        self.SetTitle("Polysquish")
        self.GroupBegin(0, c4d.BFH_SCALEFIT, cols=1, rows=0)
        self.GroupBorderSpace(10, 10, 10, 10)

        self.GroupBegin(0, c4d.BFH_SCALEFIT, cols=3, rows=1, title="Executable")
        self.GroupBorder(c4d.BORDER_GROUP_IN)
        self.GroupBorderSpace(6, 6, 6, 6)
        self.AddEditText(ID_EXE, c4d.BFH_SCALEFIT, initw=360)
        self.AddButton(ID_EXE_BROWSE, c4d.BFH_RIGHT, name="Browse...")
        self.AddButton(ID_RELEASES, c4d.BFH_RIGHT, name="Releases")
        self.GroupEnd()

        self.GroupBegin(0, c4d.BFH_SCALEFIT, cols=2, rows=0, title="Squish")
        self.GroupBorder(c4d.BORDER_GROUP_IN)
        self.GroupBorderSpace(6, 6, 6, 6)
        self.AddStaticText(0, c4d.BFH_LEFT, name="Preset")
        self.AddComboBox(ID_PRESET, c4d.BFH_SCALEFIT)
        for i, preset in enumerate(PRESETS):
            self.AddChild(ID_PRESET, i, preset)
        self.AddCheckbox(ID_BUDGET_ON, c4d.BFH_LEFT, initw=0, inith=0, name="Triangle budget")
        self.AddEditNumberArrows(ID_BUDGET, c4d.BFH_SCALEFIT)
        self.AddStaticText(0, c4d.BFH_LEFT, name="Texture size")
        self.AddComboBox(ID_TEXTURE, c4d.BFH_SCALEFIT)
        for i, size in enumerate(TEXTURE_SIZES):
            self.AddChild(ID_TEXTURE, i, size)
        self.AddCheckbox(ID_BAKE, c4d.BFH_LEFT, initw=0, inith=0, name="Bake textures")
        self.AddCheckbox(ID_AO, c4d.BFH_LEFT, initw=0, inith=0, name="Ambient occlusion")
        self.AddCheckbox(ID_LODS, c4d.BFH_LEFT, initw=0, inith=0, name="Import LODs (hidden null)")
        self.AddStaticText(0, c4d.BFH_LEFT, name="")
        self.GroupEnd()

        self.GroupBegin(0, c4d.BFH_RIGHT, cols=2, rows=1)
        self.AddButton(ID_CANCEL, c4d.BFH_RIGHT, name="Cancel")
        self.AddButton(ID_OK, c4d.BFH_RIGHT, name="Squish...")
        self.GroupEnd()
        self.GroupEnd()
        return True

    def InitValues(self):
        s = self.settings
        self.SetString(ID_EXE, s.get("executable", ""))
        self.SetInt32(ID_PRESET, PRESETS.index(s["preset"]) if s.get("preset") in PRESETS else 0)
        self.SetBool(ID_BUDGET_ON, bool(s.get("target_tris")))
        self.SetInt32(ID_BUDGET, int(s.get("target_tris") or 150000), min=50, max=5000000, step=1000)
        texture = str(s.get("texture") or 0)
        self.SetInt32(ID_TEXTURE, TEXTURE_SIZES.index(texture) if texture in TEXTURE_SIZES else 0)
        self.SetBool(ID_BAKE, bool(s.get("bake", True)))
        self.SetBool(ID_AO, bool(s.get("ao", False)))
        self.SetBool(ID_LODS, bool(s.get("import_lods", False)))
        return True

    def Command(self, cid, msg):
        if cid == ID_EXE_BROWSE:
            picked = storage.LoadDialog(type=c4d.FILESELECTTYPE_ANYTHING, title="Locate the polysquish executable", flags=c4d.FILESELECT_LOAD)
            if picked:
                self.SetString(ID_EXE, picked)
        elif cid == ID_RELEASES:
            c4d.storage.GeExecuteFile(RELEASES_URL) if hasattr(c4d.storage, "GeExecuteFile") else gui.MessageDialog(RELEASES_URL)
        elif cid == ID_OK:
            self._collect()
            self.accepted = True
            self.Close()
        elif cid == ID_CANCEL:
            self.Close()
        return True

    def _collect(self):
        s = self.settings
        s["executable"] = self.GetString(ID_EXE).strip()
        s["preset"] = PRESETS[self.GetInt32(ID_PRESET)]
        s["target_tris"] = self.GetInt32(ID_BUDGET) if self.GetBool(ID_BUDGET_ON) else 0
        texture = TEXTURE_SIZES[self.GetInt32(ID_TEXTURE)]
        s["texture"] = int(texture) if texture.isdigit() else 0
        s["bake"] = self.GetBool(ID_BAKE)
        s["ao"] = self.GetBool(ID_AO)
        s["import_lods"] = self.GetBool(ID_LODS)


# --------------------------------------------------------------------------- run + import


def run_polysquish(exe, input_path, settings):
    """Run the CLI, showing stages in the status bar. Returns (returncode, lines, out_dir, name)."""
    name = re.sub(r"[^\w\-]+", "_", os.path.splitext(os.path.basename(input_path))[0]).strip("_") or "model"
    workdir = tempfile.mkdtemp(prefix="polysquish_")
    out_dir = os.path.join(workdir, name + "_squished")
    cmd = [exe, "squish", input_path, "--preset", settings["preset"], "--target", "c4d", "-o", out_dir, "--name", name]
    if settings.get("target_tris"):
        cmd += ["--target-tris", str(int(settings["target_tris"]))]
    if settings.get("texture"):
        cmd += ["--texture", str(int(settings["texture"]))]
    if not settings.get("bake", True):
        cmd.append("--no-bake")
    elif not settings.get("ao", False):
        cmd.append("--no-ao")
    print("[polysquish] " + " ".join('"{}"'.format(c) if " " in c else c for c in cmd))

    lines = []
    c4d.StatusSetText("Polysquish: starting...")
    c4d.StatusSetSpin()
    try:
        proc = subprocess.Popen(cmd, cwd=workdir, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL, text=True, encoding="utf-8", errors="replace", bufsize=1, **_popen_kwargs())
        done = 0
        for line in iter(proc.stdout.readline, ""):
            line = line.rstrip("\r\n")
            lines.append(line)
            text = line.strip()
            if text.startswith(u"\u25b6") and text[1:].strip() in STAGE_LABELS:
                c4d.StatusSetText("Polysquish: " + text[1:].strip())
                c4d.StatusSetBar(int(100 * done / len(STAGE_LABELS)))
            elif text.startswith(u"\u2713") and text[1:].strip() in STAGE_LABELS:
                done += 1
                c4d.StatusSetBar(int(100 * done / len(STAGE_LABELS)))
        proc.stdout.close()
        code = proc.wait()
    except OSError as exc:
        code = -1
        lines.append(str(exc))
    finally:
        c4d.StatusClear()
    return code, lines, out_dir, name


def _collect_objects(root, out):
    while root:
        out.append(root)
        _collect_objects(root.GetDown(), out)
        root = root.GetNext()


def merge_file(doc, path):
    """Merge ``path`` into ``doc``; returns the objects and materials that were added."""
    objs_before = []
    _collect_objects(doc.GetFirstObject(), objs_before)
    mats_before = set(id(m) for m in doc.GetMaterials())
    flags = c4d.SCENEFILTER_OBJECTS | c4d.SCENEFILTER_MATERIALS | c4d.SCENEFILTER_MERGESCENE
    if not documents.MergeDocument(doc, path, flags):
        raise RuntimeError("Cinema 4D could not import {}".format(path))
    objs_after = []
    _collect_objects(doc.GetFirstObject(), objs_after)
    before_ids = set(id(o) for o in objs_before)
    new_objs = [o for o in objs_after if id(o) not in before_ids]
    new_mats = [m for m in doc.GetMaterials() if id(m) not in mats_before]
    return new_objs, new_mats


def _bitmap_shader(path):
    shader = c4d.BaseShader(c4d.Xbitmap)
    shader[c4d.BITMAPSHADER_FILENAME] = path
    return shader


def fix_materials(materials, name, out_dir):
    """Wire the baked textures into the imported material(s). Returns a note for the summary."""
    albedo = os.path.join(out_dir, name + "_albedo.png")
    normal = os.path.join(out_dir, name + "_normal.png")
    notes = []
    for mat in materials:
        if not mat.IsInstanceOf(c4d.Mmaterial):
            continue  # node materials (Redshift etc.) are left alone
        if os.path.isfile(albedo) and not mat[c4d.MATERIAL_COLOR_SHADER]:
            shader = _bitmap_shader(albedo)
            mat.InsertShader(shader)
            mat[c4d.MATERIAL_USE_COLOR] = True
            mat[c4d.MATERIAL_COLOR_SHADER] = shader
            notes.append("albedo")
        if os.path.isfile(normal):
            shader = _bitmap_shader(normal)
            mat.InsertShader(shader)
            mat[c4d.MATERIAL_USE_NORMAL] = True
            mat[c4d.MATERIAL_NORMAL_SHADER] = shader
            mat[c4d.MATERIAL_NORMAL_SPACE] = c4d.MATERIAL_NORMAL_SPACE_TANGENT
            mat[c4d.MATERIAL_NORMAL_REVERSEY] = False  # OpenGL (+Y) normals as baked by Polysquish
            notes.append("normal")
        mat.Message(c4d.MSG_UPDATE)
        mat.Update(True, True)
    return ("Material: wired " + ", ".join(sorted(set(notes)))) if notes else ""


def import_results(doc, out_dir, name, import_lods):
    result = {}
    result_path = os.path.join(out_dir, "result.json")
    if os.path.isfile(result_path):
        try:
            with open(result_path, "r", encoding="utf-8") as fh:
                result = json.load(fh)
        except (OSError, ValueError):
            pass
    fbx = os.path.join(out_dir, name + ".fbx")
    obj = os.path.join(out_dir, result.get("main_obj") or (name + ".obj"))
    main_file = fbx if os.path.isfile(fbx) else obj
    if not os.path.isfile(main_file):
        raise RuntimeError("no .fbx or .obj found in {}".format(out_dir))

    doc.StartUndo()
    try:
        new_objs, new_mats = merge_file(doc, main_file)
        for o in new_objs:
            doc.AddUndo(c4d.UNDOTYPE_NEW, o)
        note = fix_materials(new_mats, name, out_dir)
        extra = []
        if import_lods:
            lod_null = None
            for i in range(1, 16):
                path = os.path.join(out_dir, "{}_LOD{}.obj".format(name, i))
                if not os.path.isfile(path):
                    break
                lod_objs, _ = merge_file(doc, path)
                if lod_objs:
                    if lod_null is None:
                        lod_null = c4d.BaseObject(c4d.Onull)
                        lod_null.SetName(name + "_LODs")
                        doc.InsertObject(lod_null)
                        doc.AddUndo(c4d.UNDOTYPE_NEW, lod_null)
                    for o in lod_objs:
                        if o.GetUp() is None:
                            o.Remove()
                            o.InsertUnder(lod_null)
            if lod_null is not None:
                lod_null[c4d.ID_BASEOBJECT_VISIBILITY_EDITOR] = c4d.OBJECT_OFF
                lod_null[c4d.ID_BASEOBJECT_VISIBILITY_RENDER] = c4d.OBJECT_OFF
                extra.append("LODs under " + lod_null.GetName())
    finally:
        doc.EndUndo()
    c4d.EventAdd()

    before = (result.get("before") or {}).get("triangles")
    after = (result.get("after") or {}).get("triangles")
    timings = result.get("timings") or {}
    lines = ["Squished {:,} -> {:,} triangles".format(before, after) if before and after else "Imported " + os.path.basename(main_file)]
    if timings:
        total = sum(v for v in timings.values() if isinstance(v, (int, float)))
        lines.append("Timings: total {:.1f}s ({})".format(total, ", ".join("{} {:.1f}s".format(k, v) for k, v in timings.items() if isinstance(v, (int, float)))))
    if note:
        lines.append(note)
    lines.extend(extra)
    lines.append("Output folder (kept, textures are referenced from it):\n" + out_dir)
    lines.append("Report: " + os.path.join(out_dir, "report.html"))
    return "\n".join(lines)


# --------------------------------------------------------------------------- entry point


def main():
    doc = documents.GetActiveDocument()
    settings = load_settings()

    input_path = storage.LoadDialog(type=c4d.FILESELECTTYPE_ANYTHING, title="Polysquish: choose a model (.obj .ply .stl .glb .gltf)", flags=c4d.FILESELECT_LOAD)
    if not input_path:
        return
    if os.path.splitext(input_path)[1].lower() not in SUPPORTED_INPUTS:
        gui.MessageDialog("Unsupported file type. polysquish reads: " + " ".join(SUPPORTED_INPUTS))
        return

    dlg = SquishDialog(settings)
    dlg.Open(c4d.DLG_TYPE_MODAL, defaultw=520, defaulth=0)
    if not dlg.accepted:
        return
    save_settings(settings)

    exe = find_executable(settings.get("executable", ""))
    if not exe:
        gui.MessageDialog(missing_message())
        return

    code, lines, out_dir, name = run_polysquish(exe, input_path, settings)
    if code != 0:
        print("\n".join(lines))
        tail = "\n".join([l for l in lines if l.strip() and not l.startswith((u"\u25b6", u"\u2713"))][-8:]) or "(no output)"
        gui.MessageDialog("polysquish exited with code {}.\n\n{}".format(code, tail))
        shutil.rmtree(os.path.dirname(out_dir), ignore_errors=True)
        return
    try:
        summary = import_results(doc, out_dir, name, settings.get("import_lods", False))
    except Exception as exc:
        gui.MessageDialog("Importing the result failed:\n{}\n\nFiles are in {}".format(exc, out_dir))
        return
    print("[polysquish] " + summary.replace("\n", " | "))
    gui.MessageDialog(summary)


if __name__ == "__main__":
    main()
