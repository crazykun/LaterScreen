//! lscreen-ocr: 文字识别抽象层。
//!
//! 引擎策略（系统优先 + 内置兜底）：
//! - Windows：`Windows.Media.Ocr` 系统引擎（Win10+，语言包随系统分发）
//! - macOS：Vision `VNRecognizeTextRequest` 系统引擎（10.15+，中文需 13+）
//! - Linux：探测系统 tesseract 可执行文件，子进程调用（无链接依赖，支持中文）
//! - 内置 ocrs：纯 Rust 引擎，零外部依赖兜底；英文模型约 4MB 按需下载到
//!   `~/.cache/ocrs`，仅支持拉丁字母文字
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

// 语言码映射仅系统引擎（Win/mac）使用；带 test 使 Linux 宿主也能跑映射单测
#[cfg(any(target_os = "windows", target_os = "macos", test))]
mod lang;
mod ocrs_engine;
#[cfg(target_os = "linux")]
mod tesseract;
#[cfg(target_os = "macos")]
mod vision;
#[cfg(target_os = "windows")]
mod win_ocr;

/// RGBA → BGRA（原地交换 R/B 通道）。Windows WinRT 位图字节序是 BGRA。
#[cfg(any(target_os = "windows", test))]
pub(crate) fn rgba_to_bgra(rgba: &[u8]) -> Vec<u8> {
    let mut bgra = rgba.to_vec();
    for px in bgra.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    bgra
}

/// 返回当前平台的默认识别引擎。
/// languages 形如 `["chi_sim", "eng"]`（tesseract 语言码），各平台自行映射
/// 到引擎的语言标识。
///
/// 选择逻辑（PLAN「系统 API 优先 + 内置兜底」）：
/// 1. Windows/macOS：系统引擎可用时优先（唯一支持中文且零依赖）
/// 2. Linux：tesseract 可用时优先（中文质量最稳）
/// 3. 回退内置 ocrs（零依赖，仅拉丁文字）
///
/// `LSCREEN_OCR_ENGINE` 显式指定引擎：`ocrs` / `tesseract`（Linux）/
/// `system`（Windows/macOS），指定不可用时自动落回默认序。
pub fn default_engine(languages: &[String]) -> Box<dyn TextRecognizer> {
    match std::env::var("LSCREEN_OCR_ENGINE").ok().as_deref() {
        Some("ocrs") => return Box::new(ocrs_engine::OcrsEngine::new(languages)),
        Some("system") => {
            #[cfg(any(target_os = "windows", target_os = "macos"))]
            {
                return system_engine(languages);
            }
            #[cfg(not(any(target_os = "windows", target_os = "macos")))]
            {
                // Linux 无系统引擎，等价未指定，走默认序
            }
        }
        _ => {}
    }

    #[cfg(any(target_os = "windows", target_os = "macos"))]
    {
        let sys = system_engine(languages);
        if sys.available() {
            return sys;
        }
    }

    #[cfg(target_os = "linux")]
    {
        let t = tesseract::Tesseract::new(languages);
        if t.available() {
            return Box::new(t);
        }
    }

    Box::new(ocrs_engine::OcrsEngine::new(languages))
}

/// 当前平台的系统引擎构造（仅 Windows/macOS 存在）。
#[cfg(any(target_os = "windows", target_os = "macos"))]
fn system_engine(languages: &[String]) -> Box<dyn TextRecognizer> {
    #[cfg(target_os = "windows")]
    {
        Box::new(win_ocr::WinOcr::new(languages))
    }
    #[cfg(target_os = "macos")]
    {
        Box::new(vision::Vision::new(languages))
    }
}

#[cfg(test)]
mod tests {
    use super::rgba_to_bgra;

    #[test]
    fn bgra_swaps_r_and_b_only() {
        let rgba = [1u8, 2, 3, 4, 5, 6, 7, 8];
        assert_eq!(rgba_to_bgra(&rgba), vec![3, 2, 1, 4, 7, 6, 5, 8]);
    }
}
