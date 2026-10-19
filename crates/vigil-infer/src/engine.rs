//! YOLO inference via ONNX Runtime (compatible with YOLO11n / YOLOv8n exports).

use crate::yolo;
use anyhow::{Context, Result};
use ndarray::ArrayD;
use ort::session::Session;
use ort::value::TensorRef;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use vigil_common::Detection;

pub struct InferEngine {
    device: String,
    labels: Vec<String>,
    threshold: f32,
    stub: bool,
    session: Option<Mutex<Session>>,
}

impl InferEngine {
    pub fn load(
        model: &str,
        preferred_device: &str,
        labels: Vec<String>,
        threshold: f32,
    ) -> Result<Self> {
        let model_path = resolve_model_path(model);
        let path_str = model_path.to_string_lossy().to_string();

        if !model_path.exists() {
            if std::env::var("VIGIL_AUTO_MODEL").ok().as_deref() == Some("1") {
                if let Err(e) = download_demo_model(&model_path) {
                    tracing::warn!(error = %e, "auto model download failed — stub mode");
                    return Ok(Self::stub(labels, threshold));
                }
            } else {
                tracing::warn!(
                    model = %path_str,
                    "ONNX model not found — stub mode. Run scripts/export_model.py or scripts/download_demo_model.ps1"
                );
                return Ok(Self::stub(labels, threshold));
            }
        }

        match build_session(&model_path, preferred_device) {
            Ok((session, device)) => {
                tracing::info!(model = %path_str, %device, "YOLO ONNX session ready");
                Ok(Self {
                    device,
                    labels,
                    threshold,
                    stub: false,
                    session: Some(Mutex::new(session)),
                })
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to load ONNX session — stub mode");
                Ok(Self::stub(labels, threshold))
            }
        }
    }

    fn stub(labels: Vec<String>, threshold: f32) -> Self {
        Self {
            device: "STUB".into(),
            labels,
            threshold,
            stub: true,
            session: None,
        }
    }

    pub fn device(&self) -> &str {
        &self.device
    }

    pub fn is_stub(&self) -> bool {
        self.stub
    }

    pub fn infer_frame(
        &self,
        format: &str,
        width: u32,
        height: u32,
        data: &[u8],
    ) -> Result<Vec<Detection>> {
        if self.stub {
            return self.stub_infer(format, width, height, data);
        }
        let img = decode_image(format, width, height, data)?;
        let (input, lb) = yolo::preprocess(&img);

        let Some(ref session_mu) = self.session else {
            return self.stub_infer(format, width, height, data);
        };
        let mut session = session_mu
            .lock()
            .map_err(|_| anyhow::anyhow!("session lock poisoned"))?;

        let input_name = session
            .inputs()
            .first()
            .map(|i| i.name().to_string())
            .unwrap_or_else(|| "images".into());
        let outputs = session
            .run(ort::inputs![input_name.as_str() => TensorRef::from_array_view(input.view())?])
            .context("ort run")?;

        let (_name, value) = outputs.iter().next().context("no model outputs")?;
        let cow = value.try_extract_array::<f32>().context("extract output")?;
        let arr: ArrayD<f32> = cow.into_owned();

        let raw = yolo::normalize_output(arr);
        Ok(yolo::postprocess(raw, &lb, &self.labels, self.threshold))
    }

    fn stub_infer(
        &self,
        format: &str,
        width: u32,
        height: u32,
        data: &[u8],
    ) -> Result<Vec<Detection>> {
        let _img = decode_image(format, width, height, data)?;
        if std::env::var("VIGIL_INFER_MOCK").ok().as_deref() == Some("1")
            && self.labels.iter().any(|l| l == "person")
        {
            return Ok(vec![Detection {
                label: "person".into(),
                score: 0.9,
                xyxy: [0.35, 0.2, 0.65, 0.9],
            }]);
        }
        Ok(vec![])
    }
}

fn decode_image(
    format: &str,
    width: u32,
    height: u32,
    data: &[u8],
) -> Result<image::DynamicImage> {
    match format {
        "rgb8" => {
            let expect = width as usize * height as usize * 3;
            if data.len() != expect {
                anyhow::bail!("rgb8 size {} != {}x{}x3={}", data.len(), width, height, expect);
            }
            let img = image::RgbImage::from_raw(width, height, data.to_vec())
                .context("RgbImage::from_raw")?;
            Ok(image::DynamicImage::ImageRgb8(img))
        }
        "jpeg" | "jpg" => image::load_from_memory(data).context("decode jpeg"),
        other => anyhow::bail!("unsupported format {other}"),
    }
}

fn resolve_model_path(model: &str) -> PathBuf {
    let p = PathBuf::from(model);
    if p.extension().and_then(|e| e.to_str()) == Some("xml") {
        let onnx = p.with_extension("onnx");
        if onnx.exists() || !p.exists() {
            return onnx;
        }
    }
    if p.extension().and_then(|e| e.to_str()) == Some("bin") {
        return p.with_extension("onnx");
    }
    if !p.exists() {
        for name in ["models/yolo11n.onnx", "models/yolov8n.onnx", "models/yolov8m.onnx"] {
            let c = PathBuf::from(name);
            if c.exists() {
                return c;
            }
        }
    }
    p
}

fn build_session(path: &Path, preferred: &str) -> Result<(Session, String)> {
    let _ = preferred;
    let env = ort::init().with_name("vigil").build()?;
    let session = Session::builder(&env)
        .context("ort session builder")?
        .commit_from_file(path)
        .with_context(|| format!("load onnx {}", path.display()))?;
    Ok((session, "CPU".into()))
}

fn download_demo_model(dest: &Path) -> Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    const URL: &str = "https://cdn.pyke.io/0/pyke:ort-rs/example-models@0.0.0/yolov8m.onnx";
    tracing::info!(%URL, dest = %dest.display(), "downloading demo YOLO ONNX");
    #[cfg(windows)]
    {
        let status = std::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                &format!(
                    "Invoke-WebRequest -Uri '{}' -OutFile '{}'",
                    URL,
                    dest.to_string_lossy().replace('\'', "''")
                ),
            ])
            .status()
            .context("powershell download")?;
        if !status.success() || !dest.exists() {
            anyhow::bail!("download failed");
        }
    }
    #[cfg(not(windows))]
    {
        let status = std::process::Command::new("curl")
            .args(["-fsSL", URL, "-o", &dest.to_string_lossy()])
            .status()
            .context("curl download")?;
        if !status.success() || !dest.exists() {
            anyhow::bail!("download failed");
        }
    }
    Ok(())
}
