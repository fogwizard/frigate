# Models

Place an ONNX detect model here:

- **Preferred:** `yolo11n.onnx` — `python scripts/export_model.py`
- **Quick demo:** `powershell -File scripts/download_demo_model.ps1` (saves YOLOv8m as `yolo11n.onnx`)

Optional OpenVINO IR (Linux packaging): `yolo11n_coco.xml` + `.bin` from the same export script.

Without a model file, `vigil-infer` runs in **stub** mode (empty detections). Set `VIGIL_INFER_MOCK=1` for a fake person box, or `VIGIL_AUTO_MODEL=1` to auto-download the demo ONNX.
