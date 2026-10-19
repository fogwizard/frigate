# Vigil development

## Prerequisites

- Rust 1.75+ (`rustup`)
- FFmpeg on `PATH`
- Optional: [OpenVINO](https://docs.openvino.ai/) runtime for real detection

## Build

```bash
cargo build
cargo build --release
```

Binaries: `target/debug/vigil`, `vigil-infer`.

## Run (dev)

Terminal 1 — inference sidecar:

```bash
# one-time: get a YOLO ONNX weights file
powershell -File scripts/download_demo_model.ps1
# or: python scripts/export_model.py

cargo run -p vigil-infer -- --endpoint 127.0.0.1:9870 --model models/yolo11n.onnx --device CPU
```

Terminal 2 — main daemon (from repo root):

```bash
cargo run -p vigil -- --config config/default.yaml --web-dir web
# equivalent: cargo run -p vigil -- run --config ...
```

UI: http://127.0.0.1:8970  
Ops (same binary): `cargo run -p vigil -- status` / `health` / `events` / `recordings`

Mock detections without a model:

```bash
VIGIL_INFER_MOCK=1 cargo run -p vigil-infer -- --endpoint 127.0.0.1:9870
```

## Model export (YOLO11n → ONNX)

```bash
pip install ultralytics
python scripts/export_model.py
# → models/yolo11n.onnx
```

Quick demo weights (YOLOv8m, COCO-compatible):

```powershell
powershell -File scripts/download_demo_model.ps1
```

Inference uses **ONNX Runtime** inside `vigil-infer` (letterbox + NMS, labels filtered to person/cat/dog).

## Packaging (Ubuntu x86-64)

```bash
export OPENVINO_DIR=/opt/intel/openvino   # optional
./packaging/build-bundle.sh
# → dist/vigil-linux-x86_64.tar.gz
```

Install:

```bash
tar xzf vigil-linux-x86_64.tar.gz
cd vigil-linux-x86_64
sudo ./install.sh
sudo edit /etc/vigil/config.yaml   # set RTSP URLs
sudo systemctl start vigil
```

## Architecture notes

| Process / command | Role |
|-------------------|------|
| `vigil` / `vigil run` | Record, detect schedule, events, HTTP+UI |
| `vigil status` / `health` / `events` / `recordings` | Ops CLI (same binary) |
| `vigil-infer` | OpenVINO (or stub) over length-prefixed JSON IPC |

**Frame path (Frigate-style):** JPEG bytes go into mmap ring `data/frames.shm`; socket only sends `{slot, seq, …}` notify. Response is still small JSON detections.

IPC: `u32 BE len` + JSON (`InferRequest` notify / `InferResponse`). TCP `host:port` or Unix socket path.
