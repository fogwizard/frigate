# Download a demo YOLOv8m ONNX (COCO) for local testing when you have not exported yolo11n yet.
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$destDir = Join-Path $root "models"
New-Item -ItemType Directory -Force -Path $destDir | Out-Null
$dest = Join-Path $destDir "yolo11n.onnx"
$url = "https://cdn.pyke.io/0/pyke:ort-rs/example-models@0.0.0/yolov8m.onnx"
Write-Host "Downloading demo model to $dest"
Invoke-WebRequest -Uri $url -OutFile $dest
Write-Host "Done. (This is yolov8m weights saved as yolo11n.onnx for path compatibility.)"
Write-Host "Prefer: python scripts/export_model.py  for real yolo11n."
