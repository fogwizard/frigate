#!/usr/bin/env bash
# Build vigil-linux-x86_64.tar.gz on Ubuntu x86-64.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${ROOT}/dist/vigil-linux-x86_64"
TARBALL="${ROOT}/dist/vigil-linux-x86_64.tar.gz"

echo "==> cargo release build"
cd "$ROOT"
cargo build --release

rm -rf "$OUT"
mkdir -p "$OUT"/{bin,lib,models,share/web,config,systemd}

cp -a target/release/vigil target/release/vigil-infer "$OUT/bin/"
cp -a config/default.yaml "$OUT/config/"
cp -a web/. "$OUT/share/web/"
cp -a packaging/vigil.service "$OUT/systemd/"
cp -a packaging/install.sh "$OUT/"
chmod +x "$OUT/install.sh" "$OUT/bin/"*

# Bundle ffmpeg if present on PATH
if command -v ffmpeg >/dev/null 2>&1; then
  cp "$(command -v ffmpeg)" "$OUT/bin/ffmpeg"
  command -v ffprobe >/dev/null 2>&1 && cp "$(command -v ffprobe)" "$OUT/bin/ffprobe" || true
else
  echo "WARN: ffmpeg not found; place binaries into bundle bin/ before shipping"
fi

# Bundle OpenVINO libs if OPENVINO_DIR or /opt/intel/openvino set
OV_ROOT="${OPENVINO_DIR:-${INTEL_OPENVINO_DIR:-/opt/intel/openvino}}"
if [[ -d "$OV_ROOT/runtime/lib/intel64" ]]; then
  echo "==> copying OpenVINO libs from $OV_ROOT"
  cp -a "$OV_ROOT/runtime/lib/intel64/"*.so* "$OUT/lib/" || true
elif [[ -d "$OV_ROOT/lib" ]]; then
  cp -a "$OV_ROOT/lib/"*openvino* "$OUT/lib/" || true
else
  echo "WARN: OpenVINO not found; infer will run in stub until libs+model added"
fi

# Models: copy if present
if [[ -f models/yolo11n_coco.xml ]]; then
  cp -a models/yolo11n_coco.xml models/yolo11n_coco.bin "$OUT/models/" 2>/dev/null || true
else
  echo "WARN: models/yolo11n_coco.xml missing — see docs/DEV.md"
  printf '%s\n' "Place yolo11n_coco.xml + .bin here" > "$OUT/models/README.txt"
fi

# Linux default infer endpoint in packaged config
sed -i 's|endpoint: "127.0.0.1:9870"|endpoint: "/run/vigil/infer.sock"|' "$OUT/config/default.yaml" || true

mkdir -p "$(dirname "$TARBALL")"
tar -C "$(dirname "$OUT")" -czf "$TARBALL" "$(basename "$OUT")"
echo "==> wrote $TARBALL"
ls -lh "$TARBALL"
