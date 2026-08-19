//! lscreen-ocr: 文字识别抽象层。
//!
//! 引擎策略（按优先级回退）：
//! - Linux：探测系统 tesseract 可执行文件，子进程调用（无链接依赖，支持中文）
//! - 内置 ocrs：纯 Rust 引擎，零外部依赖兜底，Windows/macOS 首个可用引擎；
//!   模型约 4MB 按需下载到 `~/.cache/ocrs`，仅支持拉丁字母文字
//! - Windows `Windows.Media.Ocr` / macOS Vision 系统 OCR：计划中（M5）
//!
//! 识别结果统一为 [`OcrOutput`]，为后续「截图 → LLM 处理」（翻译/总结/问答）
//! 预留结构化扩展点：调用方拿到的是带置信度的文本块，而非拼接后的字符串。

use std::fmt;

#[derive(Debug)]
pub struct OcrError(pub String);

impl fmt::Display for OcrError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for OcrError {}

pub type Result<T> = std::result::Result<T, OcrError>;

/// 一个识别出的文本块。
#[derive(Debug, Clone)]
pub struct TextBlock {
    pub text: String,
    /// 置信度 0.0..=1.0；引擎不提供时为 None
    pub confidence: Option<f32>,
}

/// 一次识别的完整输出。
#[derive(Debug, Clone, Default)]
pub struct OcrOutput {
    pub blocks: Vec<TextBlock>,
}

impl OcrOutput {
    /// 拼接为纯文本（块间换行）。
    pub fn plain_text(&self) -> String {
        self.blocks
            .iter()
            .map(|b| b.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty() || self.blocks.iter().all(|b| b.text.trim().is_empty())
    }
}

/// 文字识别引擎抽象。实现方接收 RGBA 像素。
/// `Send` 约束：允许调用方把识别任务移入后台线程。
pub trait TextRecognizer: Send {
    /// 引擎是否可用（依赖是否就绪）。不可用时 `recognize` 返回引导性错误。
    fn available(&self) -> bool;

    /// 人类可读的引擎说明（用于 UI 展示与诊断）。
    fn describe(&self) -> String;

    fn recognize(&self, rgba: &[u8], width: u32, height: u32) -> Result<OcrOutput>;
}

mod ocrs_engine;
#[cfg(target_os = "linux")]
mod tesseract;

/// 返回当前平台的默认识别引擎。
/// languages 形如 `["chi_sim", "eng"]`，各平台自行映射到引擎的语言标识。
///
/// 选择逻辑：tesseract 可用且未被显式排除时优先（唯一支持中文的引擎），
/// 否则回退到内置 ocrs（零依赖，仅拉丁文字）。
pub fn default_engine(languages: &[String]) -> Box<dyn TextRecognizer> {
    let use_tesseract = std::env::var("LSCREEN_OCR_ENGINE")
        .ok()
        .map(|v| v == "tesseract")
        .unwrap_or(true);
    #[cfg(target_os = "linux")]
    if use_tesseract {
        let t = tesseract::Tesseract::new(languages);
        if t.available() {
            return Box::new(t);
        }
    }
    Box::new(ocrs_engine::OcrsEngine::new(languages))
}
