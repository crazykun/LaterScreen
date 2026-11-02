//! lscreen-ocr: 文字识别抽象层。
//!
//! 引擎策略（显式指定 > 模型已在盘的 PP-OCR > 系统优先 + 内置兜底）：
//! - PP-OCR（M19）：PaddleOCR v4 mobile ONNX + tract 纯 Rust 本地中英文推理；
//!   模型约 15.6MB 按需下载到 `~/.cache/lscreen/ppocr/v1`，三平台通用
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

/// 相对于 OCR 输入图像左上角的物理像素包围框。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoundingBox {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// 一个识别出的文本块。
#[derive(Debug, Clone)]
pub struct TextBlock {
    pub text: String,
    /// 置信度 0.0..=1.0；引擎不提供时为 None
    pub confidence: Option<f32>,
    /// 引擎能提供时保留文字在输入图像中的位置。
    pub bbox: Option<BoundingBox>,
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
pub mod paddle;
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
    for px in bgra.as_chunks_mut::<4>().0 {
        px.swap(0, 2);
    }
    bgra
}

/// 选择结果与引擎构造分离：纯策略测试不读取环境、不访问模型/网络。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EngineKind {
    Ppocr,
    System,
    Tesseract,
    Ocrs,
}

/// None = 当前平台未实现；Some(false) = 已实现但当前不可用。
/// 显式 system 保留旧行为（不可用也返回该引擎，让识别给出准确错误）；
/// 显式 tesseract 则保留不可用时走自动链的行为。
#[derive(Debug, Clone, Copy)]
struct PlatformAvailability {
    system: Option<bool>,
    tesseract: Option<bool>,
}

/// env 逃生口优先于配置；未知、空串、auto 和本平台未实现的引擎均为未指定。
/// 先校验再归并，避免一个拼错的环境变量遮住合法配置。
fn effective_pref(
    env: Option<&str>,
    pref: Option<&str>,
    platform: PlatformAvailability,
) -> Option<EngineKind> {
    let parse = |s: &str| match s.trim() {
        "ppocr" | "paddle" => Some(EngineKind::Ppocr),
        "ocrs" => Some(EngineKind::Ocrs),
        "system" if platform.system.is_some() => Some(EngineKind::System),
        "tesseract" if platform.tesseract.is_some() => Some(EngineKind::Tesseract),
        _ => None,
    };
    env.and_then(parse).or_else(|| pref.and_then(parse))
}

fn screenshot_languages(
    env: Option<&str>,
    pref: Option<&str>,
    platform: PlatformAvailability,
) -> &'static [&'static str] {
    if effective_pref(env, pref, platform) == Some(EngineKind::Ocrs) {
        &["eng"]
    } else {
        &["chi_sim", "eng"]
    }
}

/// GUI 默认请求中英文；仅用户显式选择拉丁引擎时改为英文。
/// 自动链仍携带中文，不能把缺少中文能力的回退误当成成功。
pub fn screenshot_engine(engine_pref: Option<&str>) -> Box<dyn TextRecognizer> {
    let env = std::env::var("LSCREEN_OCR_ENGINE").ok();
    let platform = PlatformAvailability {
        system: cfg!(any(target_os = "windows", target_os = "macos")).then_some(false),
        tesseract: cfg!(target_os = "linux").then_some(false),
    };
    let languages = screenshot_languages(env.as_deref(), engine_pref, platform)
        .iter()
        .map(|s| (*s).to_owned())
        .collect::<Vec<_>>();
    engine_with_env(&languages, engine_pref, env.as_deref())
}

/// 完整选择策略：所有外部状态由调用方提供，模型状态只需廉价 metadata 探测。
fn select_engine(
    env: Option<&str>,
    pref: Option<&str>,
    models_present: bool,
    platform: PlatformAvailability,
) -> EngineKind {
    if let Some(explicit) = effective_pref(env, pref, platform) {
        // 仅旧 tesseract 的「二进制不可用」有自动回退；ppocr 即使模型缺失
        // 也保持显式选择，下载/加载/推理错误不能被换引擎静默掩盖。
        if explicit != EngineKind::Tesseract || platform.tesseract == Some(true) {
            return explicit;
        }
    }
    if models_present {
        EngineKind::Ppocr
    } else if platform.system == Some(true) {
        EngineKind::System
    } else if platform.tesseract == Some(true) {
        EngineKind::Tesseract
    } else {
        EngineKind::Ocrs
    }
}

/// 返回当前平台的识别引擎。
/// languages 形如 `["chi_sim", "eng"]`（tesseract 语言码），各平台自行映射
/// 到引擎的语言标识；ppocr 忽略该参数（中英混训）。
///
/// 合法显式偏好依次来自 env `LSCREEN_OCR_ENGINE`、配置 `[ocr].engine`；
/// None/空串/auto/未知值视为未指定，不遮住下一层配置。不支持的平台选项
/// （Linux 的 system、Windows/macOS 的 tesseract）也视为未指定。
/// 显式 ppocr 不检查模型是否已下载，首次识别才按需准备模型，失败不回退。
///
/// 自动链：模型已在盘的 PP-OCR → 可用的系统引擎/系统 tesseract → 内置 ocrs。
/// 选择本身不下载、不哈希、不加载模型；PP-OCR 缺失时不会为了自动选择下载。
/// 显式 system/ocrs 保持旧行为，不因不可用而回退；显式 tesseract 不可用
/// 时仍走自动链。系统引擎/命令的可用性探测仅在自动选择确有需要时发生。
pub fn default_engine(languages: &[String], engine_pref: Option<&str>) -> Box<dyn TextRecognizer> {
    let env = std::env::var("LSCREEN_OCR_ENGINE").ok();
    engine_with_env(languages, engine_pref, env.as_deref())
}

fn engine_with_env(
    languages: &[String],
    engine_pref: Option<&str>,
    env: Option<&str>,
) -> Box<dyn TextRecognizer> {
    let mut platform = PlatformAvailability {
        system: cfg!(any(target_os = "windows", target_os = "macos")).then_some(false),
        tesseract: cfg!(target_os = "linux").then_some(false),
    };
    let explicit = effective_pref(env, engine_pref, platform);
    // 显式 ppocr/ocrs/system 无需模型状态，只在可能走自动链时探测 metadata。
    let cached_models = if matches!(explicit, None | Some(EngineKind::Tesseract)) {
        paddle::models_dir().filter(|_| paddle::models_present())
    } else {
        None
    };
    let needs_native = matches!(explicit, Some(EngineKind::System | EngineKind::Tesseract))
        || (explicit.is_none() && cached_models.is_none());
    let native: Option<Box<dyn TextRecognizer>> = if needs_native {
        #[cfg(any(target_os = "windows", target_os = "macos"))]
        {
            Some(system_engine(languages))
        }
        #[cfg(target_os = "linux")]
        {
            Some(Box::new(tesseract::Tesseract::new(languages)))
        }
        #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
        {
            None
        }
    } else {
        None
    };
    if explicit != Some(EngineKind::System) {
        if let Some(engine) = &native {
            let available = engine.available();
            platform.system = platform.system.map(|_| available);
            platform.tesseract = platform.tesseract.map(|_| available);
        }
    }
    match select_engine(env, engine_pref, cached_models.is_some(), platform) {
        EngineKind::Ppocr if explicit == Some(EngineKind::Ppocr) => {
            Box::new(paddle::PaddleOcr::new(languages))
        }
        EngineKind::Ppocr => {
            // 自动选择只允许本地模型。选择后文件被删除/损坏也报错，不转为下载。
            Box::new(paddle::PaddleOcr::with_models_dir(
                cached_models.expect("自动选择 PP-OCR 时必须已有模型目录"),
            ))
        }
        EngineKind::Ocrs => Box::new(ocrs_engine::OcrsEngine::new(languages)),
        EngineKind::System | EngineKind::Tesseract => {
            native.expect("选择策略只返回当前平台已实现的引擎")
        }
    }
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
    use super::{effective_pref, rgba_to_bgra, select_engine, EngineKind, PlatformAvailability};
    use EngineKind::{Ocrs, Ppocr, System, Tesseract};

    const LINUX: PlatformAvailability = PlatformAvailability {
        system: None,
        tesseract: Some(true),
    };
    const NATIVE: PlatformAvailability = PlatformAvailability {
        system: Some(true),
        tesseract: None,
    };
    const NO_NATIVE: PlatformAvailability = PlatformAvailability {
        system: None,
        tesseract: None,
    };

    #[test]
    fn bgra_swaps_r_and_b_only() {
        let rgba = [1u8, 2, 3, 4, 5, 6, 7, 8];
        assert_eq!(rgba_to_bgra(&rgba), vec![3, 2, 1, 4, 7, 6, 5, 8]);
    }

    #[test]
    fn screenshot_languages_follow_effective_explicit_engine() {
        for platform in [LINUX, NATIVE] {
            assert_eq!(
                super::screenshot_languages(None, Some("ocrs"), platform),
                &["eng"]
            );
            assert_eq!(
                super::screenshot_languages(Some("ocrs"), Some("ppocr"), platform),
                &["eng"]
            );
            assert_eq!(
                super::screenshot_languages(Some("ppocr"), Some("ocrs"), platform),
                &["chi_sim", "eng"]
            );
            assert_eq!(
                super::screenshot_languages(Some("invalid"), Some("ocrs"), platform),
                &["eng"]
            );
            assert_eq!(
                super::screenshot_languages(None, Some("auto"), platform),
                &["chi_sim", "eng"]
            );
        }
    }

    #[test]
    fn engine_pref_resolution() {
        for unspecified in [
            None,
            Some(""),
            Some(" "),
            Some("auto"),
            Some(" auto "),
            Some("unknown"),
        ] {
            assert_eq!(
                effective_pref(unspecified, Some("ppocr"), LINUX),
                Some(Ppocr)
            );
            assert_eq!(effective_pref(None, unspecified, LINUX), None);
            assert_eq!(
                effective_pref(unspecified, Some("system"), NATIVE),
                Some(System)
            );
        }
        assert_eq!(
            effective_pref(Some("ocrs"), Some("ppocr"), LINUX),
            Some(Ocrs)
        );
        assert_eq!(
            effective_pref(Some("ppocr"), Some("ocrs"), LINUX),
            Some(Ppocr)
        );
    }

    #[test]
    fn selection_explicit_env_then_config_precede_automatic_models() {
        for (platform, native_name, native) in
            [(LINUX, "tesseract", Tesseract), (NATIVE, "system", System)]
        {
            for present in [false, true] {
                for (name, kind) in [("ppocr", Ppocr), ("ocrs", Ocrs), (native_name, native)] {
                    assert_eq!(
                        select_engine(Some(name), Some("unknown"), present, platform),
                        kind
                    );
                    assert_eq!(select_engine(None, Some(name), present, platform), kind);
                    assert_eq!(
                        select_engine(Some(name), Some("ppocr"), present, platform),
                        kind
                    );
                }
                assert_eq!(
                    select_engine(Some("ocrs"), Some(native_name), present, platform),
                    Ocrs
                );
                assert_eq!(
                    select_engine(Some(native_name), Some("ocrs"), present, platform),
                    native
                );
            }
        }
    }

    #[test]
    fn selection_ppocr_and_paddle_aliases_ignore_model_availability() {
        for platform in [LINUX, NATIVE, NO_NATIVE] {
            for present in [false, true] {
                for alias in ["ppocr", "paddle"] {
                    assert_eq!(
                        select_engine(Some(alias), Some("ocrs"), present, platform),
                        Ppocr
                    );
                    assert_eq!(select_engine(None, Some(alias), present, platform), Ppocr);
                }
            }
        }
    }

    #[test]
    fn selection_auto_only_uses_ppocr_when_models_are_present() {
        // 纯数据输入，无法下载/读文件/加载模型；不依赖宿主的实际缓存与环境。
        for (platform, fallback) in [
            (LINUX, Tesseract),
            (NATIVE, System),
            (NO_NATIVE, Ocrs),
            (
                PlatformAvailability {
                    tesseract: Some(false),
                    ..LINUX
                },
                Ocrs,
            ),
            (
                PlatformAvailability {
                    system: Some(false),
                    ..NATIVE
                },
                Ocrs,
            ),
        ] {
            for env in [None, Some(""), Some("auto"), Some("unknown")] {
                assert_eq!(select_engine(env, Some("auto"), true, platform), Ppocr);
                assert_eq!(select_engine(env, Some("auto"), false, platform), fallback);
                // 无效 env 不能遮住合法 config，哪怕模型尚未存在。
                assert_eq!(select_engine(env, Some("ppocr"), false, platform), Ppocr);
                assert_eq!(select_engine(env, Some("ocrs"), true, platform), Ocrs);
            }
        }
    }

    #[test]
    fn selection_preserves_legacy_explicit_unavailability_behavior() {
        let no_system = PlatformAvailability {
            system: Some(false),
            ..NATIVE
        };
        let no_tesseract = PlatformAvailability {
            tesseract: Some(false),
            ..LINUX
        };
        for present in [false, true] {
            // system 和 ocrs 显式选择不回退，由该引擎报告不可用原因。
            assert_eq!(
                select_engine(Some("system"), Some("ppocr"), present, no_system),
                System
            );
            assert_eq!(
                select_engine(None, Some("system"), present, no_system),
                System
            );
            assert_eq!(select_engine(Some("ocrs"), None, present, no_system), Ocrs);
            // Linux 显式 tesseract 二进制缺失仍走 auto，不改旧选择语义。
            let fallback = if present { Ppocr } else { Ocrs };
            assert_eq!(
                select_engine(Some("tesseract"), Some("ppocr"), present, no_tesseract),
                fallback
            );
            assert_eq!(
                select_engine(None, Some("tesseract"), present, no_tesseract),
                fallback
            );
        }
    }

    #[test]
    fn selection_unsupported_platform_options_do_not_mask_config() {
        for (platform, unsupported, native) in
            [(LINUX, "system", Tesseract), (NATIVE, "tesseract", System)]
        {
            assert_eq!(effective_pref(Some(unsupported), None, platform), None);
            assert_eq!(
                select_engine(Some(unsupported), Some("ocrs"), true, platform),
                Ocrs
            );
            assert_eq!(
                select_engine(Some(unsupported), Some("ppocr"), false, platform),
                Ppocr
            );
            assert_eq!(
                select_engine(None, Some(unsupported), true, platform),
                Ppocr
            );
            assert_eq!(
                select_engine(None, Some(unsupported), false, platform),
                native
            );
        }
        assert_eq!(
            select_engine(Some("system"), Some("tesseract"), false, NO_NATIVE),
            Ocrs
        );
    }
}
