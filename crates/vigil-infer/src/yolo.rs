//! YOLO11 / YOLOv8 detect post-processing (COCO, ONNX `output0`).

use image::DynamicImage;
use ndarray::{Array2, Array4, Axis, s};
use vigil_common::Detection;

pub const INPUT_SIZE: u32 = 640;
pub const NMS_IOU: f32 = 0.7;

pub const COCO_NAMES: &[&str] = &[
    "person",
    "bicycle",
    "car",
    "motorcycle",
    "airplane",
    "bus",
    "train",
    "truck",
    "boat",
    "traffic light",
    "fire hydrant",
    "stop sign",
    "parking meter",
    "bench",
    "bird",
    "cat",
    "dog",
    "horse",
    "sheep",
    "cow",
    "elephant",
    "bear",
    "zebra",
    "giraffe",
    "backpack",
    "umbrella",
    "handbag",
    "tie",
    "suitcase",
    "frisbee",
    "skis",
    "snowboard",
    "sports ball",
    "kite",
    "baseball bat",
    "baseball glove",
    "skateboard",
    "surfboard",
    "tennis racket",
    "bottle",
    "wine glass",
    "cup",
    "fork",
    "knife",
    "spoon",
    "bowl",
    "banana",
    "apple",
    "sandwich",
    "orange",
    "broccoli",
    "carrot",
    "hot dog",
    "pizza",
    "donut",
    "cake",
    "chair",
    "couch",
    "potted plant",
    "bed",
    "dining table",
    "toilet",
    "tv",
    "laptop",
    "mouse",
    "remote",
    "keyboard",
    "cell phone",
    "microwave",
    "oven",
    "toaster",
    "sink",
    "refrigerator",
    "book",
    "clock",
    "vase",
    "scissors",
    "teddy bear",
    "hair drier",
    "toothbrush",
];

#[derive(Clone, Copy)]
pub struct Letterbox {
    pub scale: f32,
    pub pad_x: f32,
    pub pad_y: f32,
    pub orig_w: u32,
    pub orig_h: u32,
}

/// Letterbox resize to INPUT_SIZE, return CHW float tensor in [0,1] and mapping meta.
pub fn preprocess(img: &DynamicImage) -> (Array4<f32>, Letterbox) {
    let orig_w = img.width();
    let orig_h = img.height();
    let scale = (INPUT_SIZE as f32 / orig_w as f32).min(INPUT_SIZE as f32 / orig_h as f32);
    let new_w = (orig_w as f32 * scale).round() as u32;
    let new_h = (orig_h as f32 * scale).round() as u32;
    let pad_x = (INPUT_SIZE - new_w) as f32 / 2.0;
    let pad_y = (INPUT_SIZE - new_h) as f32 / 2.0;

    let resized = img.resize_exact(new_w, new_h, image::imageops::FilterType::Triangle);
    let rgb = resized.to_rgb8();

    let mut input = Array4::<f32>::zeros((1, 3, INPUT_SIZE as usize, INPUT_SIZE as usize));
    let pad = 114.0 / 255.0;
    input.fill(pad);

    let ox = pad_x.round() as u32;
    let oy = pad_y.round() as u32;
    for y in 0..new_h {
        for x in 0..new_w {
            let p = rgb.get_pixel(x, y).0;
            let yy = (y + oy) as usize;
            let xx = (x + ox) as usize;
            if yy < INPUT_SIZE as usize && xx < INPUT_SIZE as usize {
                input[[0, 0, yy, xx]] = p[0] as f32 / 255.0;
                input[[0, 1, yy, xx]] = p[1] as f32 / 255.0;
                input[[0, 2, yy, xx]] = p[2] as f32 / 255.0;
            }
        }
    }

    (
        input,
        Letterbox {
            scale,
            pad_x: ox as f32,
            pad_y: oy as f32,
            orig_w,
            orig_h,
        },
    )
}

#[derive(Clone)]
struct Cand {
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
    label: String,
    score: f32,
}

/// `raw` is YOLO output shaped [N, 84] (or [N, 4+nc]).
pub fn postprocess(
    raw: Array2<f32>,
    lb: &Letterbox,
    labels: &[String],
    threshold: f32,
) -> Vec<Detection> {
    let mut boxes = Vec::new();
    for row in raw.axis_iter(Axis(0)) {
        let row: Vec<f32> = row.iter().copied().collect();
        if row.len() < 5 {
            continue;
        }
        let (class_id, prob) = row
            .iter()
            .skip(4)
            .enumerate()
            .map(|(i, v)| (i, *v))
            .reduce(|a, b| if b.1 > a.1 { b } else { a })
            .unwrap_or((0, 0.0));
        if prob < threshold {
            continue;
        }
        let Some(name) = COCO_NAMES.get(class_id).copied() else {
            continue;
        };
        if !labels.iter().any(|l| l == name) {
            continue;
        }

        let xc = row[0];
        let yc = row[1];
        let w = row[2];
        let h = row[3];
        let x1 = (xc - w / 2.0 - lb.pad_x) / lb.scale;
        let y1 = (yc - h / 2.0 - lb.pad_y) / lb.scale;
        let x2 = (xc + w / 2.0 - lb.pad_x) / lb.scale;
        let y2 = (yc + h / 2.0 - lb.pad_y) / lb.scale;

        let ow = lb.orig_w as f32;
        let oh = lb.orig_h as f32;
        let x1 = x1.clamp(0.0, ow);
        let y1 = y1.clamp(0.0, oh);
        let x2 = x2.clamp(0.0, ow);
        let y2 = y2.clamp(0.0, oh);
        if x2 <= x1 || y2 <= y1 {
            continue;
        }

        boxes.push(Cand {
            x1,
            y1,
            x2,
            y2,
            label: name.to_string(),
            score: prob,
        });
    }

    boxes.sort_by(|a, b| b.score.total_cmp(&a.score));
    let kept = nms(boxes, NMS_IOU);

    let ow = lb.orig_w as f32;
    let oh = lb.orig_h as f32;
    kept.into_iter()
        .map(|b| Detection {
            label: b.label,
            score: b.score,
            xyxy: [b.x1 / ow, b.y1 / oh, b.x2 / ow, b.y2 / oh],
        })
        .collect()
}

fn nms(mut boxes: Vec<Cand>, iou_thr: f32) -> Vec<Cand> {
    let mut result = Vec::new();
    while let Some(best) = boxes.first().cloned() {
        result.push(best.clone());
        boxes = boxes
            .into_iter()
            .skip(1)
            .filter(|b| iou(&best, b) < iou_thr)
            .collect();
    }
    result
}

fn iou(a: &Cand, b: &Cand) -> f32 {
    let ix1 = a.x1.max(b.x1);
    let iy1 = a.y1.max(b.y1);
    let ix2 = a.x2.min(b.x2);
    let iy2 = a.y2.min(b.y2);
    let inter = (ix2 - ix1).max(0.0) * (iy2 - iy1).max(0.0);
    let uni = (a.x2 - a.x1) * (a.y2 - a.y1) + (b.x2 - b.x1) * (b.y2 - b.y1) - inter;
    if uni <= 0.0 {
        0.0
    } else {
        inter / uni
    }
}

/// Normalize ONNX output to [N, C] where C = 4 + num_classes.
pub fn normalize_output(arr: ndarray::ArrayD<f32>) -> Array2<f32> {
    let shape = arr.shape().to_vec();
    let to2 = |data: Vec<f32>, n: usize, c: usize| {
        Array2::from_shape_vec((n, c), data).unwrap_or_else(|_| Array2::zeros((0, 0)))
    };
    match shape.as_slice() {
        // [1, 84, 8400] -> [8400, 84]
        &[1, c, n] if c <= 512 && n > c => {
            let t = arr.slice(s![0, .., ..]).t().to_owned();
            to2(t.iter().copied().collect(), n, c)
        }
        // [1, 8400, 84] -> [8400, 84]
        &[1, n, c] if c <= 512 && n > c => {
            let t = arr.slice(s![0, .., ..]).to_owned();
            to2(t.iter().copied().collect(), n, c)
        }
        &[c, n] if c <= 512 && n > c => {
            let t = arr.t().to_owned();
            to2(t.iter().copied().collect(), n, c)
        }
        &[n, c] if c <= 512 => to2(arr.iter().copied().collect(), n, c),
        _ => {
            tracing::warn!(?shape, "unexpected YOLO output shape");
            Array2::zeros((0, 0))
        }
    }
}
