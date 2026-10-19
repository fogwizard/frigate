#!/usr/bin/env python3
"""Export YOLO11n to ONNX for Vigil (person/cat/dog via COCO labels)."""

from pathlib import Path


def main() -> None:
    from ultralytics import YOLO

    root = Path(__file__).resolve().parents[1]
    models = root / "models"
    models.mkdir(exist_ok=True)

    m = YOLO("yolo11n.pt")
    out = m.export(format="onnx", dynamic=False, simplify=True, opset=12)
    src = Path(out)
    dst = models / "yolo11n.onnx"
    if src.resolve() != dst.resolve():
        dst.write_bytes(src.read_bytes())
    print("wrote", dst)

    # Optional OpenVINO IR for Linux packaging with Intel runtime
    try:
        ov = m.export(format="openvino", dynamic=False, half=False)
        print("also exported OpenVINO:", ov)
        print("Rename/copy IR to models/yolo11n_coco.xml|.bin if needed")
    except Exception as e:
        print("OpenVINO export skipped:", e)


if __name__ == "__main__":
    main()
