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
    print("Vigil loads this ONNX with ONNX Runtime. OpenVINO IR is not required.")

    # Optional. Ultralytics treats a missing openvino package as an export error
    # and tries to pip-install it, which fails offline.
    try:
        import openvino  # noqa: F401
    except ImportError:
        print("OpenVINO IR skipped: package not installed")
        return

    ov = m.export(format="openvino", dynamic=False)
    print("also exported OpenVINO:", ov)


if __name__ == "__main__":
    main()
