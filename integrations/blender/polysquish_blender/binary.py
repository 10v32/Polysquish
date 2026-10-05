"""Locating and describing the ``polysquish`` executable.

Every Polysquish integration resolves the executable the same way:

1. the ``POLYSQUISH_BIN`` environment variable,
2. the path stored in the host application's preferences,
3. ``polysquish`` on ``PATH``,
4. a handful of default install folders per platform.
"""

import os
import platform
import shutil
import subprocess
import sys

RELEASES_URL = "https://github.com/10v32/Polysquish/releases"
ENV_VAR = "POLYSQUISH_BIN"

_version_cache = {}


def executable_name():
    return "polysquish.exe" if sys.platform.startswith("win") else "polysquish"


def default_locations():
    """Default install folders, most likely first."""
    home = os.path.expanduser("~")
    exe = executable_name()
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
    """Return the path of the polysquish executable or ``None``.

    ``preferred`` is the path from the add-on preferences (may be empty).
    """
    env = os.environ.get(ENV_VAR, "").strip()
    if _usable(env):
        return env
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
        "Polysquish executable not found. Download it from {} and set the path in "
        "Edit > Preferences > Add-ons > Polysquish (or set the {} environment variable)."
    ).format(RELEASES_URL, ENV_VAR)


def popen_kwargs():
    """Extra ``subprocess.Popen`` keywords that keep the console window hidden on Windows."""
    kwargs = {}
    if sys.platform.startswith("win"):
        kwargs["creationflags"] = getattr(subprocess, "CREATE_NO_WINDOW", 0x08000000)
    return kwargs


def version_of(path):
    """``polysquish --version`` output, cached per path. Returns '' on failure."""
    if not path:
        return ""
    try:
        key = (path, os.path.getmtime(path))
    except OSError:
        return ""
    if key in _version_cache:
        return _version_cache[key]
    text = ""
    try:
        out = subprocess.run(
            [path, "--version"],
            capture_output=True,
            text=True,
            timeout=10,
            **popen_kwargs(),
        )
        text = (out.stdout or out.stderr or "").strip()
    except (OSError, subprocess.SubprocessError):
        text = ""
    _version_cache[key] = text
    return text


def platform_label():
    return "{} {}".format(platform.system(), platform.machine())
