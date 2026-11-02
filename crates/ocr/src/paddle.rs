//! PP-OCRv4 mobile：按需下载模型、纯 Rust CPU 推理，不上传截图。
//! 自动选择只使用本地模型；显式选择才允许首次识别下载。下载/完整性校验
//! 见 paddle_models.rs，推理不依赖 GUI 或系统 Python/ONNX Runtime。
//!
//! det 为 960×960 等比补边；rec 按行宽分档（320..4096），保持高度 48。
//! 固定 320 宽只有 40 个 CTC 时间步，长行不能强行缩到其中。同行宽度分组
//! 后复用计划，最多保留一个 rec 计划。仅面向正立屏幕文字，不做旋转校正。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use image::{Rgb, RgbImage};
use tract_onnx::prelude::*;

use crate::{BoundingBox, OcrError, OcrOutput, Result, TextBlock, TextRecognizer};

#[path = "paddle_models.rs"]
mod models;
pub use models::{download_models, models_dir, models_present};
use models::{DET_FILE, DICT_FILE, REC_FILE};

const DET_SIZE: u32 = 960;
const REC_H: u32 = 48;
const REC_WIDTHS: [u32; 5] = [320, 640, 1280, 2560, 4096];
const DET_THRESH: f32 = 0.3;
const BOX_THRESH: f32 = 0.5;
const TEXT_THRESH: f32 = 0.5;
const MAX_BOXES: usize = 512;
const MAX_PIXELS: usize = 64 * 1024 * 1024;
const DET_MEAN: [f32; 3] = [0.485, 0.456, 0.406];
const DET_STD: [f32; 3] = [0.229, 0.224, 0.225];
type Runnable = Arc<RunnableModel<TypedFact, Box<dyn TypedOp>>>;

pub struct PaddleOcr {
    dir: Option<PathBuf>,
    allow_download: bool,
    bundle: OnceLock<ModelBundle>,
    init_lock: Mutex<()>,
}

struct ModelBundle {
    det: Runnable,
    rec: Mutex<Option<(u32, Runnable)>>,
    dict: Vec<String>,
}

impl PaddleOcr {
    /// 显式选择：允许首次使用下载固定的中英文模型组合。
    pub fn new(_languages: &[String]) -> Self {
        Self {
            dir: models_dir(),
            allow_download: true,
            bundle: OnceLock::new(),
            init_lock: Mutex::new(()),
        }
    }

    /// 自动选择或离线测试：固定目录，缺失/损坏只报错，绝不触发联网。
    pub fn with_models_dir(dir: PathBuf) -> Self {
        Self {
            dir: Some(dir),
            allow_download: false,
            ..Self::new(&[])
        }
    }

    fn ensure_bundle(&self) -> Result<&ModelBundle> {
        if let Some(bundle) = self.bundle.get() {
            return Ok(bundle);
        }
        let _guard = self
            .init_lock
            .lock()
            .map_err(|_| OcrError("OCR 初始化锁异常".into()))?;
        if let Some(bundle) = self.bundle.get() {
            return Ok(bundle);
        }
        let dir = self
            .dir
            .as_ref()
            .ok_or_else(|| OcrError("无法定位 PP-OCR 模型缓存目录".into()))?;
        if self.allow_download && !models::models_present_in(dir) {
            eprintln!("首次使用 PP-OCR：下载约 15.6MB 模型到 {}", dir.display());
            models::download_models_to(dir, &mut |_, _| {})?;
        }
        models::verify_models(dir)?;
        let dict = parse_dict(
            &std::fs::read_to_string(dir.join(DICT_FILE))
                .map_err(|e| OcrError(format!("读取 PP-OCR 字典失败: {e}")))?,
        );
        if dict.len() != 6623 || dict.iter().any(String::is_empty) {
            return Err(OcrError(
                "PP-OCR 字典与模型不匹配，请在设置中重新下载".into(),
            ));
        }
        let det = load_model(&dir.join(DET_FILE), DET_SIZE, DET_SIZE, "检测")?;
        Ok(self.bundle.get_or_init(|| ModelBundle {
            det,
            rec: Mutex::new(None),
            dict,
        }))
    }

    fn detect(&self, bundle: &ModelBundle, rgb: &RgbImage) -> Result<Vec<Rect>> {
        let (image, transform) = letterbox_rgb(rgb, DET_SIZE, DET_SIZE);
        let input = Tensor::from_shape(
            &[1, 3, DET_SIZE as usize, DET_SIZE as usize],
            &det_nchw(&image),
        )
        .map_err(|e| OcrError(format!("构造检测输入失败: {e}")))?;
        let output = bundle
            .det
            .run(tvec![input.into_tvalue()])
            .map_err(|e| OcrError(format!("文本检测推理失败: {e}")))?;
        let out = output
            .first()
            .ok_or_else(|| OcrError("检测模型未返回结果".into()))?;
        if out.shape() != [1, 1, DET_SIZE as usize, DET_SIZE as usize] {
            return Err(OcrError(format!("检测模型输出形状异常: {:?}", out.shape())));
        }
        let boxes = text_boxes(f32_slice(out)?, DET_SIZE, DET_SIZE)?;
        let mut boxes = merge_lines(
            boxes
                .into_iter()
                .filter_map(|r| r.to_orig(rgb.width(), rgb.height(), transform))
                .filter(|r| r.w >= 2 && r.h >= 3)
                .collect(),
        );
        sort_reading_order(&mut boxes);
        Ok(boxes)
    }

    fn recognize_line(
        &self,
        bundle: &ModelBundle,
        rgb: &RgbImage,
        r: Rect,
        width: u32,
    ) -> Result<(String, f32)> {
        let crop = image::imageops::crop_imm(rgb, r.x, r.y, r.w, r.h).to_image();
        let input = Tensor::from_shape(
            &[1, 3, REC_H as usize, width as usize],
            &recognition_input(&crop, width),
        )
        .map_err(|e| OcrError(format!("构造识别输入失败: {e}")))?;
        let mut cache = bundle
            .rec
            .lock()
            .map_err(|_| OcrError("OCR 推理锁异常".into()))?;
        if cache.as_ref().is_none_or(|(w, _)| *w != width) {
            // 先释放旧计划，避免不同宽度的模型同时驻留。
            *cache = None;
            let dir = self
                .dir
                .as_ref()
                .ok_or_else(|| OcrError("无法定位模型目录".into()))?;
            *cache = Some((
                width,
                load_model(&dir.join(REC_FILE), REC_H, width, "识别")?,
            ));
        }
        let plan = &cache.as_ref().expect("刚加载或命中计划").1;
        let output = plan
            .run(tvec![input.into_tvalue()])
            .map_err(|e| OcrError(format!("文本识别推理失败: {e}")))?;
        let out = output
            .first()
            .ok_or_else(|| OcrError("识别模型未返回结果".into()))?;
        let shape = out.shape();
        if shape.len() != 3 || shape[0] != 1 || shape[2] != bundle.dict.len() + 2 {
            return Err(OcrError(format!("识别模型输出与字典不匹配: {shape:?}")));
        }
        ctc_greedy(f32_slice(out)?, shape[1], shape[2], &bundle.dict)
    }
}

impl TextRecognizer for PaddleOcr {
    fn available(&self) -> bool {
        self.dir
            .as_ref()
            .is_some_and(|dir| self.allow_download || models::models_present_in(dir))
    }

    fn describe(&self) -> String {
        let Some(dir) = &self.dir else {
            return "无法定位 PP-OCR 模型缓存目录（HOME/USERPROFILE）".into();
        };
        if models::models_present_in(dir) {
            format!(
                "PP-OCRv4 本地中英文引擎（{}；加载时校验完整性）",
                dir.display()
            )
        } else if self.allow_download {
            format!(
                "PP-OCR 模型尚未下载，首次识别将下载约 15.6MB 到 {}；也可在设置中下载",
                dir.display()
            )
        } else {
            format!(
                "PP-OCR 模型缺失或不完整：{}。请在设置中下载/修复后重试",
                dir.display()
            )
        }
    }

    fn recognize(&self, rgba: &[u8], width: u32, height: u32) -> Result<OcrOutput> {
        validate_image(rgba, width, height)?;
        let started = std::time::Instant::now();
        let bundle = self.ensure_bundle()?;
        let rgb = composite_white(rgba, width, height);
        let boxes = self.detect(bundle, &rgb)?;
        let mut jobs = boxes
            .iter()
            .enumerate()
            .map(|(i, r)| Ok((i, recognition_width(r.w, r.h)?)))
            .collect::<Result<Vec<_>>>()?;
        jobs.sort_by_key(|(_, width)| *width);
        let mut blocks = Vec::new();
        for (i, width) in jobs {
            let r = boxes[i];
            let (text, confidence) = self.recognize_line(bundle, &rgb, r, width)?;
            if !text.trim().is_empty() && confidence >= TEXT_THRESH {
                blocks.push((
                    i,
                    TextBlock {
                        text,
                        confidence: Some(confidence),
                        bbox: Some(BoundingBox {
                            x: r.x,
                            y: r.y,
                            width: r.w,
                            height: r.h,
                        }),
                    },
                ));
            }
        }
        blocks.sort_by_key(|(i, _)| *i);
        eprintln!(
            "lscreen: PP-OCR 完成，{} 行，耗时 {}ms",
            blocks.len(),
            started.elapsed().as_millis()
        );
        Ok(OcrOutput {
            blocks: blocks.into_iter().map(|(_, b)| b).collect(),
        })
    }
}

fn validate_image(rgba: &[u8], width: u32, height: u32) -> Result<()> {
    let pixels = (width as usize).checked_mul(height as usize);
    if width == 0
        || height == 0
        || pixels.is_none_or(|n| n > MAX_PIXELS)
        || pixels.and_then(|n| n.checked_mul(4)) != Some(rgba.len())
    {
        return Err(OcrError(
            "无效或过大的图像缓冲（最多 64MP），请缩小选区".into(),
        ));
    }
    Ok(())
}

fn f32_slice(t: &Tensor) -> Result<&[f32]> {
    t.try_as_plain_ram()
        .and_then(|v| v.as_slice::<f32>())
        .map_err(|e| OcrError(format!("OCR 输出数据异常: {e}")))
}

fn load_model(path: &Path, h: u32, w: u32, name: &str) -> Result<Runnable> {
    let load = || -> TractResult<Runnable> {
        let mut model = tract_onnx::onnx().model_for_path(path)?;
        model.set_input_fact(0, f32::fact([1usize, 3, h as usize, w as usize]).into())?;
        model.into_optimized()?.into_runnable()
    };
    load().map_err(|e| {
        OcrError(format!(
            "PP-OCR {name}模型加载失败: {e}（{}）",
            path.display()
        ))
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Rect {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
}

#[derive(Debug, Clone, Copy)]
struct Scale {
    x: f64,
    y: f64,
}

impl Rect {
    fn right(self) -> u32 {
        self.x.saturating_add(self.w)
    }
    fn bottom(self) -> u32 {
        self.y.saturating_add(self.h)
    }

    fn to_orig(self, width: u32, height: u32, scale: Scale) -> Option<Self> {
        if !scale.x.is_finite() || !scale.y.is_finite() || scale.x <= 0.0 || scale.y <= 0.0 {
            return None;
        }
        let x = (self.x as f64 / scale.x).floor().min(width as f64) as u32;
        let y = (self.y as f64 / scale.y).floor().min(height as f64) as u32;
        let right = (self.right() as f64 / scale.x).ceil().min(width as f64) as u32;
        let bottom = (self.bottom() as f64 / scale.y).ceil().min(height as f64) as u32;
        (right > x && bottom > y).then_some(Self {
            x,
            y,
            w: right - x,
            h: bottom - y,
        })
    }

    fn union(self, other: Self) -> Self {
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        Self {
            x,
            y,
            w: self.right().max(other.right()) - x,
            h: self.bottom().max(other.bottom()) - y,
        }
    }
}

fn composite_white(rgba: &[u8], width: u32, height: u32) -> RgbImage {
    RgbImage::from_fn(width, height, |x, y| {
        let p = (y as usize * width as usize + x as usize) * 4;
        let a = rgba[p + 3] as u32;
        Rgb(std::array::from_fn(|c| {
            ((rgba[p + c] as u32 * a + 255 * (255 - a) + 127) / 255) as u8
        }))
    })
}

fn letterbox_rgb(src: &RgbImage, width: u32, height: u32) -> (RgbImage, Scale) {
    let scale = (width as f64 / src.width() as f64).min(height as f64 / src.height() as f64);
    let rw = ((src.width() as f64 * scale).round() as u32).clamp(1, width);
    let rh = ((src.height() as f64 * scale).round() as u32).clamp(1, height);
    let resized = image::imageops::resize(src, rw, rh, image::imageops::FilterType::Triangle);
    let mut out = RgbImage::from_pixel(width, height, Rgb([255; 3]));
    image::imageops::overlay(&mut out, &resized, 0, 0);
    (
        out,
        Scale {
            x: rw as f64 / src.width() as f64,
            y: rh as f64 / src.height() as f64,
        },
    )
}

fn det_nchw(img: &RgbImage) -> Vec<f32> {
    let n = img.width() as usize * img.height() as usize;
    let mut v = vec![0.0; 3 * n];
    for (i, px) in img.pixels().enumerate() {
        // RapidOCR/PaddleOCR 的 OpenCV 路径用 BGR。
        for c in 0..3 {
            v[c * n + i] = (px.0[2 - c] as f32 / 255.0 - DET_MEAN[c]) / DET_STD[c];
        }
    }
    v
}

fn recognition_width(width: u32, height: u32) -> Result<u32> {
    if height == 0 {
        return Err(OcrError("无效的文字行高度".into()));
    }
    let needed = (width as u64 * REC_H as u64).div_ceil(height as u64);
    REC_WIDTHS
        .into_iter()
        .find(|&w| w as u64 >= needed)
        .ok_or_else(|| OcrError("文字行过长，请分段选择后识别（不截断文字）".into()))
}

fn recognition_input(src: &RgbImage, width: u32) -> Vec<f32> {
    let rw = (src.width() as u64 * REC_H as u64)
        .div_ceil(src.height() as u64)
        .min(width as u64) as u32;
    let resized =
        image::imageops::resize(src, rw.max(1), REC_H, image::imageops::FilterType::Triangle);
    let n = (REC_H * width) as usize;
    // 上游 rec 填充为归一化后的 0，不是白色(+1)。
    let mut input = vec![0.0; 3 * n];
    for (x, y, px) in resized.enumerate_pixels() {
        for c in 0..3 {
            input[c * n + (y * width + x) as usize] = px.0[2 - c] as f32 / 127.5 - 1.0;
        }
    }
    input
}

fn text_boxes(prob: &[f32], width: u32, height: u32) -> Result<Vec<Rect>> {
    if prob.len() != (width * height) as usize
        || prob
            .iter()
            .any(|v| !v.is_finite() || !(0.0..=1.0).contains(v))
    {
        return Err(OcrError("检测模型概率图异常".into()));
    }
    // 固定模型末端已含 Sigmoid；不基于数值范围猜测并二次 sigmoid。
    let bin = prob.iter().map(|&p| p > DET_THRESH).collect::<Vec<_>>();
    let mut boxes = Vec::new();
    for r in connected_boxes(&bin, width, height) {
        if r.w < 2 || r.h < 2 {
            continue;
        }
        let score = (r.y..r.bottom())
            .flat_map(|y| (r.x..r.right()).map(move |x| prob[(y * width + x) as usize]))
            .sum::<f32>()
            / (r.w * r.h) as f32;
        if score < BOX_THRESH {
            continue;
        }
        // DB unclip 的轴对齐近似：distance = area * 1.6 / perimeter。
        let pad = ((r.w as f32 * r.h as f32 * 1.6) / (2.0 * (r.w + r.h) as f32)).ceil() as u32;
        let x = r.x.saturating_sub(pad);
        let y = r.y.saturating_sub(pad);
        boxes.push(Rect {
            x,
            y,
            w: r.right().saturating_add(pad).min(width) - x,
            h: r.bottom().saturating_add(pad).min(height) - y,
        });
        if boxes.len() > MAX_BOXES {
            return Err(OcrError("检测到过多文字区域，请缩小选区后重试".into()));
        }
    }
    Ok(boxes)
}

/// 8 连通域洪泛，访问一次标记一次，时间/空间均受 det 输入像素数限制。
fn connected_boxes(bin: &[bool], width: u32, height: u32) -> Vec<Rect> {
    let mut seen = vec![false; bin.len()];
    let mut stack = Vec::new();
    let mut boxes = Vec::new();
    for start in 0..bin.len() {
        if !bin[start] || seen[start] {
            continue;
        }
        seen[start] = true;
        stack.push(start);
        let mut r = Rect {
            x: start as u32 % width,
            y: start as u32 / width,
            w: 1,
            h: 1,
        };
        while let Some(i) = stack.pop() {
            let x = i as u32 % width;
            let y = i as u32 / width;
            r = r.union(Rect { x, y, w: 1, h: 1 });
            for ny in y.saturating_sub(1)..=(y + 1).min(height - 1) {
                for nx in x.saturating_sub(1)..=(x + 1).min(width - 1) {
                    let ni = (ny * width + nx) as usize;
                    if bin[ni] && !seen[ni] {
                        seen[ni] = true;
                        stack.push(ni);
                    }
                }
            }
        }
        boxes.push(r);
    }
    boxes
}

fn same_line(a: Rect, b: Rect) -> bool {
    let overlap = a.bottom().min(b.bottom()).saturating_sub(a.y.max(b.y));
    overlap * 2 > a.h.min(b.h) && a.h.max(b.h) <= a.h.min(b.h) * 2
}

fn merge_lines(mut boxes: Vec<Rect>) -> Vec<Rect> {
    loop {
        let pair = (0..boxes.len()).find_map(|i| {
            ((i + 1)..boxes.len())
                .find(|&j| {
                    let (a, b) = (boxes[i], boxes[j]);
                    let gap = a.x.max(b.x).saturating_sub(a.right().min(b.right()));
                    same_line(a, b) && gap * 2 < 3 * a.h.min(b.h)
                })
                .map(|j| (i, j))
        });
        let Some((i, j)) = pair else { return boxes };
        boxes[i] = boxes[i].union(boxes[j]);
        boxes.swap_remove(j);
    }
}

fn sort_reading_order(boxes: &mut [Rect]) {
    boxes.sort_by_key(|r| (r.y, r.x));
    // 使用固定行锚点，避免带模糊 y 判定的 comparator 破坏全序契约。
    let mut start = 0;
    while start < boxes.len() {
        let anchor = boxes[start];
        let end = (start + 1..boxes.len())
            .find(|&i| !same_line(anchor, boxes[i]))
            .unwrap_or(boxes.len());
        boxes[start..end].sort_by_key(|r| r.x);
        start = end;
    }
}

fn parse_dict(s: &str) -> Vec<String> {
    // 保留空行以便报告字典错误，绝不能过滤后导致后续类别整体错位。
    s.lines()
        .map(|s| s.trim_end_matches('\r').to_owned())
        .collect()
}

fn ctc_greedy(
    prob: &[f32],
    steps: usize,
    classes: usize,
    dict: &[String],
) -> Result<(String, f32)> {
    if classes != dict.len() + 2
        || steps.checked_mul(classes) != Some(prob.len())
        || prob
            .iter()
            .any(|v| !v.is_finite() || !(0.0..=1.0).contains(v))
    {
        return Err(OcrError("识别概率或字典维度异常".into()));
    }
    // 固定模型末端已含 Softmax。blank 重置 last，才能识别「哈哈」。
    let (mut text, mut sum, mut count, mut last) = (String::new(), 0.0, 0, 0);
    for row in prob.chunks_exact(classes) {
        let (i, &value) = row
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .expect("至少blank/space两类");
        if i != 0 && i != last {
            text.push_str(if i == classes - 1 { " " } else { &dict[i - 1] });
            sum += value;
            count += 1;
        }
        last = i;
    }
    Ok((text, if count > 0 { sum / count as f32 } else { 0.0 }))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn rect(x: u32, y: u32, w: u32, h: u32) -> Rect {
        Rect { x, y, w, h }
    }

    #[test]
    fn dict_parse_tolerates_missing_trailing_newline() {
        assert_eq!(parse_dict("一\n二\n三"), vec!["一", "二", "三"]);
        assert_eq!(parse_dict("一\r\n二\r\n"), vec!["一", "二"]);
        assert_eq!(parse_dict("一\n\n二"), vec!["一", "", "二"]);
    }

    fn decode(ids: &[usize]) -> (String, f32) {
        let mut prob = vec![0.0; ids.len() * 4];
        for (t, &i) in ids.iter().enumerate() {
            prob[t * 4 + i] = 1.0;
        }
        ctc_greedy(&prob, ids.len(), 4, &["中".into(), "文".into()]).unwrap()
    }
    #[test]
    fn ctc_collapses_repeats_and_skips_blank() {
        assert_eq!(decode(&[1, 1, 0, 2, 2]), ("中文".into(), 1.0));
        assert_eq!(decode(&[1, 0, 1]).0, "中中");
        assert_eq!(decode(&[1, 3, 2]).0, "中 文");
        assert_eq!(decode(&[0, 0]).0, "");
        assert_eq!(decode(&[]).0, "");
    }
    #[test]
    fn ctc_confidence_uses_only_emitted_steps() {
        let prob = [
            0.05, 0.9, 0.03, 0.02, 0.1, 0.8, 0.05, 0.05, 0.95, 0.02, 0.02, 0.01, 0.1, 0.1, 0.7, 0.1,
        ];
        let (text, conf) = ctc_greedy(&prob, 4, 4, &["中".into(), "文".into()]).unwrap();
        assert_eq!(text, "中文");
        assert!((conf - 0.8).abs() < 1e-6);
    }
    #[test]
    fn malformed_outputs_return_errors() {
        assert!(ctc_greedy(&[1.0], 1, 4, &["中".into(), "文".into()]).is_err());
        assert!(ctc_greedy(&[f32::NAN, 0.0], 1, 2, &[]).is_err());
        assert!(text_boxes(&[f32::NAN], 1, 1).is_err());
        assert!(text_boxes(&[1.0], 2, 2).is_err());
    }
    #[test]
    fn letterbox_scales_and_pads_bottom_right() {
        let src = RgbImage::from_pixel(500, 300, Rgb([0; 3]));
        let (out, scale) = letterbox_rgb(&src, 960, 960);
        assert_eq!((out.width(), out.height()), (960, 960));
        assert_eq!(scale.x, 1.92);
        assert_eq!(scale.y, 1.92);
        assert_eq!(out.get_pixel(959, 959), &Rgb([255; 3]));
        assert_eq!(out.get_pixel(0, 0), &Rgb([0; 3]));
    }
    #[test]
    fn orig_mapping_scales_back_and_clamps() {
        let s = Scale { x: 1.92, y: 1.92 };
        assert_eq!(
            rect(192, 96, 200, 40).to_orig(500, 300, s),
            Some(rect(100, 50, 105, 21))
        );
        assert_eq!(
            rect(0, 0, 960, 960).to_orig(500, 300, s),
            Some(rect(0, 0, 500, 300))
        );
        assert!(rect(0, 0, 10, 10)
            .to_orig(
                500,
                300,
                Scale {
                    x: f64::NAN,
                    y: 1.0
                }
            )
            .is_none());
        assert!(rect(950, 950, 10, 10).to_orig(500, 300, s).is_none());
    }
    #[test]
    fn merge_joins_same_line_but_not_stacked() {
        assert_eq!(
            merge_lines(vec![rect(10, 10, 50, 20), rect(80, 12, 60, 18)]),
            vec![rect(10, 10, 130, 20)]
        );
        assert_eq!(
            merge_lines(vec![rect(10, 10, 50, 20), rect(10, 60, 50, 20)]).len(),
            2
        );
        assert_eq!(
            merge_lines(vec![rect(10, 10, 50, 20), rect(800, 10, 60, 20)]).len(),
            2
        );
    }
    #[test]
    fn reading_order_tolerates_baseline_jitter() {
        let mut boxes = [
            rect(100, 9, 40, 20),
            rect(0, 10, 40, 20),
            rect(0, 60, 40, 20),
        ];
        sort_reading_order(&mut boxes);
        assert_eq!(boxes.map(|r| r.x), [0, 100, 0]);
        assert_eq!(boxes[2].y, 60);
    }
    #[test]
    fn connected_boxes_finds_two_blobs() {
        let mut bin = vec![false; 81];
        for p in [0, 1, 9, 10, 66, 67, 68] {
            bin[p] = true;
        }
        let boxes = connected_boxes(&bin, 9, 9);
        assert_eq!(boxes, vec![rect(0, 0, 2, 2), rect(3, 7, 3, 1)]);
    }
    #[test]
    fn det_normalizes_each_channel_and_uses_bgr() {
        let image = RgbImage::from_pixel(4, 4, Rgb([255; 3]));
        for (c, plane) in det_nchw(&image).as_chunks::<16>().0.iter().enumerate() {
            let expected = (1.0 - DET_MEAN[c]) / DET_STD[c];
            assert!(plane.iter().all(|x| (x - expected).abs() < 1e-5));
        }
        let v = det_nchw(&RgbImage::from_pixel(1, 1, Rgb([255, 0, 0])));
        assert!((v[0] + DET_MEAN[0] / DET_STD[0]).abs() < 1e-6);
        assert!((v[2] - (1.0 - DET_MEAN[2]) / DET_STD[2]).abs() < 1e-6);
    }
    #[test]
    fn recognition_width_preserves_long_lines() {
        assert_eq!(recognition_width(100, 48).unwrap(), 320);
        assert_eq!(recognition_width(1200, 48).unwrap(), 1280);
        assert!(recognition_width(10_000, 1).is_err());
    }
    #[test]
    fn invalid_inputs_fail_before_loading_models() {
        let engine = PaddleOcr::new(&[]);
        for (rgba, w, h) in [
            (&[0u8][..], 0, 1),
            (&[0u8][..], u32::MAX, u32::MAX),
            (&[0u8; 5][..], 1, 1),
        ] {
            assert!(engine.recognize(rgba, w, h).unwrap_err().0.contains("图像"));
        }
    }
    #[test]
    fn recognition_padding_is_zero_after_normalization() {
        let src = RgbImage::from_pixel(12, 48, Rgb([255, 0, 0]));
        let input = recognition_input(&src, 320);
        let n = 48 * 320;
        assert_eq!(input[0], -1.0);
        assert_eq!(input[2 * n], 1.0);
        assert_eq!(input[20], 0.0);
        assert_eq!(input[2 * n + 20], 0.0);
    }
    #[test]
    fn composite_white_blends_alpha() {
        let img = composite_white(&[0, 0, 0, 0, 0, 0, 0, 128], 2, 1);
        assert_eq!(img.get_pixel(0, 0), &Rgb([255; 3]));
        assert_eq!(img.get_pixel(1, 0), &Rgb([127; 3]));
    }
}
