"""Polysquish for Maya (2022+, Python 3).

A small `maya.cmds` tool: pick a model file, run ``polysquish squish --target maya`` in a background
thread, import the FBX (V2) or OBJ result with ``cmds.file(..., i=True)`` and wire the baked textures
into a Standard Surface shader.

Install::

    import polysquish_maya
    polysquish_maya.install_shelf_button()   # adds a button to the current shelf
    polysquish_maya.show()                   # or open the window directly

Written against the Maya 2022-2025 Python API; not executed inside Maya in this environment.
"""

import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import threading

import maya.cmds as cmds
import maya.mel as mel
import maya.utils

RELEASES_URL = "https://github.com/10v32/Polysquish/releases"
ENV_VAR = "POLYSQUISH_BIN"
OPTION_VAR = "polysquishExecutable"
PRESETS = ("hero", "prop", "mobile", "character", "dcc")
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
WINDOW = "polysquishWindow"

_ui = {}
_job = {"running": False}


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


def preferred_executable():
    return cmds.optionVar(q=OPTION_VAR) if cmds.optionVar(exists=OPTION_VAR) else ""


def set_preferred_executable(path):
    cmds.optionVar(sv=(OPTION_VAR, path or ""))


def find_executable():
    """``POLYSQUISH_BIN`` → optionVar → PATH → default install folders. ``None`` when not found."""
    env = os.environ.get(ENV_VAR, "").strip()
    if _usable(env):
        return env
    pref = preferred_executable()
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
        "Polysquish executable not found.\n\nDownload it from {}\nthen set the path in the Polysquish window, "
        "put it on PATH, or set the {} environment variable."
    ).format(RELEASES_URL, ENV_VAR)


def _popen_kwargs():
    kwargs = {}
    if sys.platform.startswith("win"):
        kwargs["creationflags"] = getattr(subprocess, "CREATE_NO_WINDOW", 0x08000000)
    return kwargs


# --------------------------------------------------------------------------- UI


def install_shelf_button():
    """Add a Polysquish button to the currently visible shelf."""
    shelf_top = mel.eval("$tmp = $gShelfTopLevel")
    current = cmds.tabLayout(shelf_top, query=True, selectTab=True) if shelf_top else None
    if not current:
        cmds.warning("[polysquish] no shelf is visible; open a shelf and try again")
        return None
    button = cmds.shelfButton(
        parent=current,
        label="Polysquish",
        annotation="Polysquish: squish a huge mesh into a game-ready asset",
        image1="polyReduce.png",
        imageOverlayLabel="PSQ",
        command="import polysquish_maya; polysquish_maya.show()",
        sourceType="python",
    )
    print("[polysquish] shelf button added to {}".format(current))
    return button


def show():
    if cmds.window(WINDOW, exists=True):
        cmds.deleteUI(WINDOW)
    window = cmds.window(WINDOW, title="Polysquish", widthHeight=(460, 420), sizeable=True)
    cmds.columnLayout(adjustableColumn=True, rowSpacing=6, columnOffset=("both", 8))

    cmds.frameLayout(label="Executable", collapsable=True, marginWidth=4, marginHeight=4)
    cmds.rowLayout(numberOfColumns=3, adjustableColumn=1, columnAttach=[(1, "both", 0), (2, "both", 2), (3, "both", 2)])
    _ui["exe"] = cmds.textField(text=preferred_executable(), placeholderText="auto-detect (POLYSQUISH_BIN, PATH, default folders)", changeCommand=_on_exe_changed)
    cmds.button(label="Browse…", command=_browse_exe)
    cmds.button(label="Releases", command=lambda *_: cmds.launch(web=RELEASES_URL))
    cmds.setParent("..")
    _ui["exe_status"] = cmds.text(label="", align="left")
    cmds.setParent("..")

    cmds.frameLayout(label="Input", marginWidth=4, marginHeight=4)
    cmds.rowLayout(numberOfColumns=2, adjustableColumn=1, columnAttach=[(1, "both", 0), (2, "both", 2)])
    _ui["input"] = cmds.textField(placeholderText="model.obj / .ply / .stl / .glb / .gltf")
    cmds.button(label="Browse…", command=_browse_input)
    cmds.setParent("..")
    cmds.setParent("..")

    cmds.frameLayout(label="Squish", marginWidth=4, marginHeight=4)
    _ui["preset"] = cmds.optionMenuGrp(label="Preset", columnWidth=(1, 110))
    for preset in PRESETS:
        cmds.menuItem(label=preset)
    cmds.optionMenuGrp(_ui["preset"], edit=True, value="dcc")
    _ui["budget"] = cmds.checkBoxGrp(label="Triangle budget", numberOfCheckBoxes=1, label1="override", columnWidth=(1, 110))
    _ui["tris"] = cmds.intFieldGrp(label="", value1=150000, columnWidth=(1, 110))
    _ui["texture"] = cmds.optionMenuGrp(label="Texture size", columnWidth=(1, 110))
    for size in TEXTURE_SIZES:
        cmds.menuItem(label=size)
    _ui["bake"] = cmds.checkBoxGrp(label="Bake", numberOfCheckBoxes=2, labelArray2=["textures", "ambient occlusion"], valueArray2=[True, False], columnWidth=(1, 110))
    _ui["import_lods"] = cmds.checkBoxGrp(label="Import", numberOfCheckBoxes=2, labelArray2=["LODs (hidden group)", "collision shapes"], valueArray2=[False, False], columnWidth=(1, 110))
    cmds.setParent("..")

    _ui["status"] = cmds.text(label="Ready", align="left")
    _ui["progress"] = cmds.progressBar(maxValue=len(STAGE_LABELS), width=200)
    _ui["run"] = cmds.button(label="Squish", height=34, command=_on_run)
    cmds.setParent("..")
    cmds.showWindow(window)
    _refresh_exe_status()


def _refresh_exe_status():
    exe = find_executable()
    if "exe_status" in _ui and cmds.text(_ui["exe_status"], exists=True):
        cmds.text(_ui["exe_status"], edit=True, label=("Using " + exe) if exe else "Not found - download from " + RELEASES_URL)


def _on_exe_changed(*_):
    set_preferred_executable(cmds.textField(_ui["exe"], query=True, text=True))
    _refresh_exe_status()


def _browse_exe(*_):
    picked = cmds.fileDialog2(fileMode=1, caption="Locate the polysquish executable", okCaption="Use")
    if picked:
        cmds.textField(_ui["exe"], edit=True, text=picked[0])
        _on_exe_changed()


def _browse_input(*_):
    filters = "3D models (*.obj *.ply *.stl *.glb *.gltf);;All files (*.*)"
    picked = cmds.fileDialog2(fileMode=1, caption="Model to squish", fileFilter=filters, okCaption="Squish")
    if picked:
        cmds.textField(_ui["input"], edit=True, text=picked[0])


def _set_status(text, progress=None):
    def apply():
        if cmds.text(_ui.get("status", ""), exists=True):
            cmds.text(_ui["status"], edit=True, label=text)
        if progress is not None and cmds.progressBar(_ui.get("progress", ""), exists=True):
            cmds.progressBar(_ui["progress"], edit=True, progress=progress)

    maya.utils.executeDeferred(apply)


# --------------------------------------------------------------------------- run


def _on_run(*_):
    if _job["running"]:
        cmds.warning("[polysquish] already running")
        return
    exe = find_executable()
    if not exe:
        cmds.confirmDialog(title="Polysquish", message=missing_message(), button=["OK"])
        return
    input_path = cmds.textField(_ui["input"], query=True, text=True).strip()
    if not input_path or not os.path.isfile(input_path):
        cmds.confirmDialog(title="Polysquish", message="Pick an existing model file first.", button=["OK"])
        return
    if os.path.splitext(input_path)[1].lower() not in SUPPORTED_INPUTS:
        cmds.confirmDialog(title="Polysquish", message="Unsupported file type. polysquish reads: " + " ".join(SUPPORTED_INPUTS), button=["OK"])
        return

    options = {
        "preset": cmds.optionMenuGrp(_ui["preset"], query=True, value=True),
        "target_tris": cmds.intFieldGrp(_ui["tris"], query=True, value1=True) if cmds.checkBoxGrp(_ui["budget"], query=True, value1=True) else 0,
        "texture": cmds.optionMenuGrp(_ui["texture"], query=True, value=True),
        "bake": cmds.checkBoxGrp(_ui["bake"], query=True, value1=True),
        "ao": cmds.checkBoxGrp(_ui["bake"], query=True, value2=True),
        "import_lods": cmds.checkBoxGrp(_ui["import_lods"], query=True, value1=True),
        "import_collision": cmds.checkBoxGrp(_ui["import_lods"], query=True, value2=True),
    }
    squish(input_path, exe=exe, **options)


def squish(input_path, exe=None, preset="dcc", target_tris=0, texture="Preset default", bake=True, ao=False, import_lods=False, import_collision=False):
    """Run polysquish in a background thread and import the result when it finishes."""
    exe = exe or find_executable()
    if not exe:
        cmds.warning(missing_message())
        return False
    name = re.sub(r"[^\w\-]+", "_", os.path.splitext(os.path.basename(input_path))[0]).strip("_") or "model"
    workdir = tempfile.mkdtemp(prefix="polysquish_")
    out_dir = os.path.join(workdir, name + "_squished")
    cmd = [exe, "squish", input_path, "--preset", preset, "--target", "maya", "-o", out_dir, "--name", name]
    if target_tris:
        cmd += ["--target-tris", str(int(target_tris))]
    if texture and texture.isdigit():
        cmd += ["--texture", texture]
    if not bake:
        cmd.append("--no-bake")
    elif not ao:
        cmd.append("--no-ao")
    print("[polysquish] " + " ".join('"{}"'.format(c) if " " in c else c for c in cmd))

    _job["running"] = True
    if "run" in _ui and cmds.button(_ui["run"], exists=True):
        cmds.button(_ui["run"], edit=True, enable=False, label="Squishing…")
    _set_status("Starting polysquish…", 0)

    def worker():
        lines = []
        done = 0
        try:
            proc = subprocess.Popen(cmd, cwd=workdir, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL, text=True, encoding="utf-8", errors="replace", bufsize=1, **_popen_kwargs())
            for line in iter(proc.stdout.readline, ""):
                line = line.rstrip("\r\n")
                lines.append(line)
                text = line.strip()
                if text.startswith(u"\u25b6") and text[1:].strip() in STAGE_LABELS:
                    _set_status(text[1:].strip(), done)
                elif text.startswith(u"\u2713") and text[1:].strip() in STAGE_LABELS:
                    done += 1
                    _set_status(text[1:].strip() + " done", done)
            proc.stdout.close()
            code = proc.wait()
        except OSError as exc:
            code = -1
            lines.append(str(exc))
        maya.utils.executeDeferred(lambda: _on_finished(code, lines, out_dir, name, workdir, import_lods, import_collision))

    threading.Thread(target=worker, daemon=True).start()
    return True


def _on_finished(code, lines, out_dir, name, workdir, import_lods, import_collision):
    _job["running"] = False
    if "run" in _ui and cmds.button(_ui["run"], exists=True):
        cmds.button(_ui["run"], edit=True, enable=True, label="Squish")
    if code != 0:
        print("\n".join(lines))
        tail = "\n".join([l for l in lines if l.strip() and not l.startswith((u"\u25b6", u"\u2713"))][-8:]) or "(no output)"
        _set_status("Failed (exit {})".format(code), 0)
        cmds.confirmDialog(title="Polysquish failed", message="polysquish exited with code {}.\n\n{}".format(code, tail), button=["OK"])
        shutil.rmtree(workdir, ignore_errors=True)
        return
    try:
        summary = import_results(out_dir, name, import_lods=import_lods, import_collision=import_collision)
        _set_status(summary.split("\n")[0], len(STAGE_LABELS))
        cmds.inViewMessage(assistMessage="Polysquish: " + summary.split("\n")[0], position="topCenter", fade=True)
        print("[polysquish] " + summary.replace("\n", " | "))
    except Exception as exc:  # keep the files for a manual import
        _set_status("Import failed", 0)
        cmds.confirmDialog(title="Polysquish", message="Importing the result failed:\n{}\n\nFiles are in {}".format(exc, out_dir), button=["OK"])
        return
    # Textures are referenced by absolute path from the output folder, so keep it.
    print("[polysquish] output folder kept at {} (textures are referenced from there)".format(out_dir))


# --------------------------------------------------------------------------- import


def _import_file(path, namespace):
    ext = os.path.splitext(path)[1].lower()
    if ext == ".fbx":
        cmds.loadPlugin("fbxmaya", quiet=True)
        kind, options = "FBX", "fbx"
    else:
        cmds.loadPlugin("objExport", quiet=True)
        kind, options = "OBJ", "mo=1"
    return cmds.file(path, i=True, type=kind, ignoreVersion=True, mergeNamespacesOnClash=True, namespace=namespace, options=options, preserveReferences=True, returnNewNodes=True) or []


def _meshes_of(nodes):
    shapes = cmds.ls(nodes, type="mesh", long=True) or []
    transforms = set()
    for shape in shapes:
        parents = cmds.listRelatives(shape, parent=True, fullPath=True) or []
        transforms.update(parents)
    return sorted(transforms)


def import_results(out_dir, name, import_lods=False, import_collision=False):
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

    new_nodes = _import_file(main_file, name)
    meshes = _meshes_of(new_nodes)
    if not meshes:
        raise RuntimeError("{} contained no mesh".format(main_file))
    group = cmds.group(meshes, name=name + "_squished")

    textures = {}
    for kind in ("albedo", "normal", "ao", "orm"):
        png = os.path.join(out_dir, "{}_{}.png".format(name, kind))
        if os.path.isfile(png):
            textures[kind] = png
    if textures:
        shader = build_standard_surface(name, textures)
        cmds.sets(meshes, edit=True, forceElement=shader + "SG")

    extra = []
    if import_lods:
        lod_group = None
        for i in range(1, 16):
            path = os.path.join(out_dir, "{}_LOD{}.obj".format(name, i))
            if not os.path.isfile(path):
                break
            lod_meshes = _meshes_of(_import_file(path, "{}_LOD{}".format(name, i)))
            if lod_meshes:
                if lod_group is None:
                    lod_group = cmds.group(empty=True, name=name + "_LODs")
                cmds.parent(lod_meshes, lod_group)
                if textures:
                    cmds.sets(lod_meshes, edit=True, forceElement=shader + "SG")
        if lod_group:
            cmds.setAttr(lod_group + ".visibility", 0)
            extra.append("LODs in " + lod_group)
    if import_collision:
        coll_group = None
        for kind in ("hull", "box", "simplified"):
            path = os.path.join(out_dir, "{}_collision_{}.obj".format(name, kind))
            if not os.path.isfile(path):
                continue
            coll_meshes = _meshes_of(_import_file(path, "{}_collision_{}".format(name, kind)))
            if coll_meshes:
                if coll_group is None:
                    coll_group = cmds.group(empty=True, name=name + "_Collision")
                cmds.parent(coll_meshes, coll_group)
                for m in coll_meshes:
                    cmds.setAttr(m + ".overrideEnabled", 1)
                    cmds.setAttr(m + ".overrideShading", 0)
        if coll_group:
            cmds.setAttr(coll_group + ".visibility", 0)
            extra.append("collision in " + coll_group)

    cmds.select(group, replace=True)
    before = (result.get("before") or {}).get("triangles")
    after = (result.get("after") or {}).get("triangles")
    timings = result.get("timings") or {}
    head = "Squished {:,} -> {:,} triangles".format(before, after) if before and after else "Imported " + group
    lines = [head]
    if timings:
        total = sum(v for v in timings.values() if isinstance(v, (int, float)))
        lines.append("Timings: total {:.1f}s ({})".format(total, ", ".join("{} {:.1f}s".format(k, v) for k, v in timings.items() if isinstance(v, (int, float)))))
    lines.extend(extra)
    lines.append("Report: " + os.path.join(out_dir, "report.html"))
    return "\n".join(lines)


def build_standard_surface(name, textures):
    """Create ``<name>_mat`` (standardSurface) wired to the baked textures. Returns the shader name."""
    shader = cmds.shadingNode("standardSurface", asShader=True, name=name + "_mat")
    sg = cmds.sets(renderable=True, noSurfaceShader=True, empty=True, name=shader + "SG")
    cmds.connectAttr(shader + ".outColor", sg + ".surfaceShader", force=True)

    def file_node(path, label, raw):
        node = cmds.shadingNode("file", asTexture=True, isColorManaged=True, name="{}_{}".format(name, label))
        place = cmds.shadingNode("place2dTexture", asUtility=True, name="{}_{}_place2d".format(name, label))
        for attr in ("coverage", "translateFrame", "rotateFrame", "mirrorU", "mirrorV", "stagger", "wrapU", "wrapV", "repeatUV", "offset", "rotateUV", "noiseUV", "vertexUvOne", "vertexUvTwo", "vertexUvThree", "vertexCameraOne"):
            try:
                cmds.connectAttr("{}.{}".format(place, attr), "{}.{}".format(node, attr), force=True)
            except RuntimeError:
                pass
        cmds.connectAttr(place + ".outUV", node + ".uvCoord", force=True)
        cmds.connectAttr(place + ".outUvFilterSize", node + ".uvFilterSize", force=True)
        cmds.setAttr(node + ".fileTextureName", path, type="string")
        if raw:
            try:
                cmds.setAttr(node + ".ignoreColorSpaceFileRules", 1)
                cmds.setAttr(node + ".colorSpace", "Raw", type="string")
            except RuntimeError:
                pass
        return node

    if "albedo" in textures:
        albedo = file_node(textures["albedo"], "albedo", raw=False)
        cmds.connectAttr(albedo + ".outColor", shader + ".baseColor", force=True)
    if "normal" in textures:
        normal = file_node(textures["normal"], "normal", raw=True)
        cmds.setAttr(normal + ".alphaIsLuminance", 0)
        bump = cmds.shadingNode("bump2d", asUtility=True, name=name + "_bump2d")
        cmds.setAttr(bump + ".bumpInterp", 1)  # tangent space normals (OpenGL +Y, as Maya expects)
        cmds.connectAttr(normal + ".outAlpha", bump + ".bumpValue", force=True)
        cmds.connectAttr(bump + ".outNormal", shader + ".normalCamera", force=True)
    if "orm" in textures:
        orm = file_node(textures["orm"], "orm", raw=True)
        cmds.connectAttr(orm + ".outColorG", shader + ".specularRoughness", force=True)
        cmds.connectAttr(orm + ".outColorB", shader + ".metalness", force=True)
    if "ao" in textures and "albedo" in textures:
        # Multiply AO into the base colour (Standard Surface has no dedicated AO input).
        ao = file_node(textures["ao"], "ao", raw=True)
        mult = cmds.shadingNode("multiplyDivide", asUtility=True, name=name + "_ao_mult")
        cmds.connectAttr(albedo + ".outColor", mult + ".input1", force=True)
        cmds.connectAttr(ao + ".outColor", mult + ".input2", force=True)
        cmds.connectAttr(mult + ".output", shader + ".baseColor", force=True)
    return shader
