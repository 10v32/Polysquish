#!/usr/bin/env sh
# Package the Blender add-on as an installable zip: dist/polysquish_blender-<version>.zip
# The zip contains a single top-level folder `polysquish_blender/` so Blender can install it
# both as a legacy add-on (4.0/4.1) and as an Extension from disk (4.2+).
set -eu

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
src="$here/polysquish_blender"
dist="$here/dist"

version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$src/blender_manifest.toml" | head -n 1)
[ -n "$version" ] || version="0.0.0"
out="$dist/polysquish_blender-$version.zip"

# Syntax check every module before packaging.
if command -v python3 >/dev/null 2>&1; then
    python3 -m py_compile "$src"/*.py
    find "$src" -name __pycache__ -type d -prune -exec rm -rf {} +
fi

mkdir -p "$dist"
rm -f "$out"
(
    cd "$here"
    if command -v zip >/dev/null 2>&1; then
        zip -r -X "$out" polysquish_blender -x '*/__pycache__/*' '*.pyc' '*.DS_Store'
    else
        python3 - "$out" <<'PY'
import os, sys, zipfile
out = sys.argv[1]
with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED) as zf:
    for root, dirs, files in os.walk("polysquish_blender"):
        dirs[:] = [d for d in dirs if d != "__pycache__"]
        for f in files:
            if f.endswith(".pyc") or f == ".DS_Store":
                continue
            zf.write(os.path.join(root, f))
PY
    fi
)
echo "Wrote $out"
