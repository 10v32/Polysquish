#!/usr/bin/env bash
# Downloads public test models into testdata/ (not committed; ~27 MB).
set -euo pipefail
cd "$(dirname "$0")/../testdata"
base=https://raw.githubusercontent.com/alecjacobson/common-3d-test-models/master/data
for f in stanford-bunny.obj armadillo.obj xyzrgb_dragon.obj happy.obj; do
  [ -f "$f" ] || curl -sS -L -o "$f" "$base/$f"
done
[ -f DamagedHelmet.glb ] || curl -sS -L -o DamagedHelmet.glb \
  https://raw.githubusercontent.com/KhronosGroup/glTF-Sample-Assets/main/Models/DamagedHelmet/glTF-Binary/DamagedHelmet.glb
for m in Fox RiggedSimple CesiumMan; do
  [ -f "$m.glb" ] || curl -sS -L -o "$m.glb" "https://raw.githubusercontent.com/KhronosGroup/glTF-Sample-Assets/main/Models/$m/glTF-Binary/$m.glb"
done
echo "Test models ready. Make a 4M-triangle stress input with:"
echo "  cargo run --release -- synth testdata/xyzrgb_dragon.obj -o testdata/dragon_4m.ply --levels 2"
