//! macOS：Vision 框架 `VNRecognizeTextRequest` 系统引擎。
//!
//! 系统自带（macOS 10.15+），准确模式 + 语言校正；zh-Hans 识别需
//! macOS 13（revision 3），更老系统设置中文会报错——识别失败时用
//! 无语言约束（Vision 自动按支持的默认语言）重试一次。
//!
//! 图像路径：RGBA → PNG → `NSData` → `VNImageRequestHandler`，
//! 不经 CGImage/CIImage（CoreGraphics 是 C API，无 objc2 绑定）。

use objc2::rc::Retained;
use objc2::AnyThread;
use objc2_foundation::{NSArray, NSData, NSDictionary, NSString};
use objc2_vision::{
    VNImageRequestHandler, VNRecognizeTextRequest, VNRequest, VNRequestTextRecognitionLevel,
};

use crate::lang::tess_to_bcp47;
use crate::{OcrError, OcrOutput, Result, TextBlock, TextRecognizer};

/// 未指定语言时的默认（国内桌面场景与 tesseract 默认 chi_sim+eng 对齐）
const DEFAULT_LANGS: [&str; 2] = ["zh-Hans", "en-US"];

pub struct Vision {
    languages: Vec<&'static str>,
}

impl Vision {
    pub fn new(languages: &[String]) -> Self {
        let mut langs: Vec<&'static str> =
            languages.iter().filter_map(|l| tess_to_bcp47(l)).collect();
        if langs.is_empty() {
            langs = DEFAULT_LANGS.to_vec();
        }
        Self { languages: langs }
    }

    fn attempt(&self, rgba: &[u8], width: u32, height: u32, langs: &[&str]) -> Result<OcrOutput> {
        // RGBA → PNG bytes：Vision 走 initWithData 支持的解码格式
        let img = image::RgbaImage::from_raw(width, height, rgba.to_vec())
            .ok_or_else(|| OcrError("无效的图像缓冲".into()))?;
        let mut png = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .map_err(|e| OcrError(format!("PNG 编码失败: {e}")))?;

        let handler = VNImageRequestHandler::initWithData_options(
            VNImageRequestHandler::alloc(),
            &NSData::with_bytes(&png),
            &NSDictionary::new(),
        );
        let request = VNRecognizeTextRequest::new();
        request.setRecognitionLevel(VNRequestTextRecognitionLevel::Accurate);
        if !langs.is_empty() {
            let tags: Vec<Retained<NSString>> =
                langs.iter().map(|s| NSString::from_str(s)).collect();
            request.setRecognitionLanguages(&NSArray::from_retained_slice(&tags));
        }
        request.setUsesLanguageCorrection(true);

        // SAFETY: VNRecognizeTextRequest 是 VNRequest 的子类
        //（生成代码 #[unsafe(super(...))] 声明），向上转型恒有效
        let req: Retained<VNRequest> = unsafe { Retained::cast_unchecked(request.clone()) };
        let requests = NSArray::from_retained_slice(&[req]);
        handler
            .performRequests_error(&requests)
            .map_err(|e| OcrError(format!("Vision 识别失败: {e:?}")))?;

        let observations = request.results().unwrap_or_default();
        let mut blocks = Vec::new();
        for obs in observations.iter() {
            // 每个观察 = 一行文本；topCandidates(1) 给出最优候选
            let candidates = obs.topCandidates(1);
            if let Some(text) = candidates.iter().next() {
                let s = text.string().to_string();
                if !s.trim().is_empty() {
                    blocks.push(TextBlock {
                        text: s,
                        confidence: Some(text.confidence()),
                    });
                }
            }
        }
        Ok(OcrOutput { blocks })
    }
}

impl TextRecognizer for Vision {
    fn available(&self) -> bool {
        // Vision 框架随系统分发，macOS 10.15+ 恒可用
        true
    }

    fn describe(&self) -> String {
        format!(
            "macOS Vision 系统 OCR（准确模式；语言 {}，中文需 macOS 13+）",
            self.languages.join(", ")
        )
    }

    fn recognize(&self, rgba: &[u8], width: u32, height: u32) -> Result<OcrOutput> {
        match self.attempt(rgba, width, height, &self.languages) {
            Ok(out) => Ok(out),
            // 指定语言在本机不受支持（如 macOS 12 上的 zh-Hans）：去掉语言
            // 约束重试，Vision 用系统默认可识别语言
            Err(e) => self.attempt(rgba, width, height, &[]).or(Err(e)),
        }
    }
}
