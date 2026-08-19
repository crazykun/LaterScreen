//! 内置 ocrs 引擎：纯 Rust OCR（robertknight/ocrs + rten 推理运行时）。
//!
//! 定位是「零外部依赖的兜底」：Linux 无 tesseract 时顶上，Windows/macOS
//! 在系统 OCR（M5）落地前的首个可用引擎。**模型仅覆盖拉丁字母文字**，
//! 中文等 CJK 识别仍需 tesseract（引擎选择见 lib.rs default_engine）。
//!
//! 模型文件（检测 + 识别，共约 4MB）不打进二进制——产物体积约束，
//! 且多数用户用不到兜底路径。首次使用时经系统 curl 下载到 `~/.cache/ocrs`
//! （与 ocrs-cli 同路径，已装过的用户直接复用）；无 curl 时报错并给出
//! 手动下载的 URL 与目标路径。

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

use ocrs::{ImageSource, OcrEngine, OcrEngineParams};
use rten::Model;

use crate::{OcrError, OcrOutput, Result, TextBlock, TextRecognizer};

const MODEL_BASE_URL: &str = "https://ocrs-models.s3-accelerate.amazonaws.com";
const MODEL_FILES: [&str; 2] = ["text-detection.rten", "text-recognition.rten"];

pub struct OcrsEngine {
    /// 用户显式指定了 ocrs 不支持的语言（如 chi_sim）时记录之，
    /// available() 返回 false 并在 describe() 里解释，而不是跑出乱码
    unsupported_lang: Option<String>,
    /// 引擎缓存：模型加载约几百 ms，GUI 内重复识别不重复加载
    engine: OnceLock<OcrEngine>,
}

/// ocrs 默认模型的字母表不含这些文字（按 tesseract 语言码前缀匹配）。
/// 覆盖常见非拉丁文字即可——漏判的小语种会识别出乱码，但不会崩溃。
const NON_LATIN_LANG_PREFIXES: [&str; 10] = [
    "chi", "jpn", "kor", "ara", "tha", "heb", "rus", "ell", "hin", "ben",
];

impl OcrsEngine {
    pub fn new(languages: &[String]) -> Self {
        let unsupported_lang = languages
            .iter()
            .find(|l| NON_LATIN_LANG_PREFIXES.iter().any(|p| l.starts_with(p)))
            .cloned();
        Self {
            unsupported_lang,
            engine: OnceLock::new(),
        }
    }

    fn ensure_engine(&self) -> Result<&OcrEngine> {
        if let Some(e) = self.engine.get() {
            return Ok(e);
        }
        let dir = models_dir().ok_or_else(|| OcrError("无法定位用户主目录（HOME）".into()))?;
        let mut models = Vec::with_capacity(2);
        for file in MODEL_FILES {
            let path = dir.join(file);
            if !path.is_file() {
                download(&format!("{MODEL_BASE_URL}/{file}"), &path)?;
            }
            models.push(Model::load_file(&path).map_err(|e| {
                OcrError(format!(
                    "加载 OCR 模型失败: {e}（文件损坏可删除 {} 后重试）",
                    path.display()
                ))
            })?);
        }
        let recognition = models.pop().expect("两个模型刚压入");
        let detection = models.pop().expect("两个模型刚压入");
        let engine = OcrEngine::new(OcrEngineParams {
            detection_model: Some(detection),
            recognition_model: Some(recognition),
            ..Default::default()
        })
        .map_err(|e| OcrError(format!("初始化 ocrs 引擎失败: {e}")))?;
        Ok(self.engine.get_or_init(|| engine))
    }
}

impl TextRecognizer for OcrsEngine {
    fn available(&self) -> bool {
        if self.unsupported_lang.is_some() {
            return false;
        }
        models_present() || curl_available()
    }

    fn describe(&self) -> String {
        if let Some(lang) = &self.unsupported_lang {
            return format!(
                "内置 ocrs 引擎仅支持拉丁字母文字，无法识别 {lang}\
                 （中文方案：Linux 安装 tesseract；Windows/macOS 系统 OCR 规划中）"
            );
        }
        if models_present() {
            "内置 ocrs 引擎（纯 Rust；仅拉丁字母文字，中文请安装 tesseract）".into()
        } else if curl_available() {
            "内置 ocrs 引擎：首次使用将自动下载模型（约 4MB）到 ~/.cache/ocrs".into()
        } else {
            let dir = models_dir()
                .map(|d| d.display().to_string())
                .unwrap_or_else(|| "~/.cache/ocrs".into());
            format!(
                "内置 ocrs 引擎缺少模型且未找到 curl。请手动下载 {MODEL_BASE_URL}/\
                 {{text-detection,text-recognition}}.rten 放入 {dir}"
            )
        }
    }

    fn recognize(&self, rgba: &[u8], width: u32, height: u32) -> Result<OcrOutput> {
        if self.unsupported_lang.is_some() {
            return Err(OcrError(self.describe()));
        }
        let engine = self.ensure_engine()?;
        let source = ImageSource::from_bytes(rgba, (width, height))
            .map_err(|e| OcrError(format!("无效的图像缓冲: {e}")))?;
        let input = engine
            .prepare_input(source)
            .map_err(|e| OcrError(format!("图像预处理失败: {e}")))?;
        let words = engine
            .detect_words(&input)
            .map_err(|e| OcrError(format!("文本检测失败: {e}")))?;
        let lines = engine.find_text_lines(&input, &words);
        let texts = engine
            .recognize_text(&input, &lines)
            .map_err(|e| OcrError(format!("文本识别失败: {e}")))?;
        let blocks = texts
            .into_iter()
            .flatten()
            .map(|line| line.to_string())
            // 单字符行多为误检噪声，上游示例同此过滤
            .filter(|t| t.trim().chars().count() > 1)
            .map(|text| TextBlock {
                text,
                confidence: None,
            })
            .collect();
        Ok(OcrOutput { blocks })
    }
}

/// 模型缓存目录：`~/.cache/ocrs`，三平台统一（刻意与 ocrs-cli 相同，
/// 装过它的用户零下载复用模型）。
fn models_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|v| !v.is_empty())?;
    Some(PathBuf::from(home).join(".cache").join("ocrs"))
}

fn models_present() -> bool {
    models_dir().is_some_and(|d| MODEL_FILES.iter().all(|f| d.join(f).is_file()))
}

fn curl_available() -> bool {
    // Windows 10 1803+ / macOS / 绝大多数 Linux 发行版自带 curl
    Command::new("curl")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// 经系统 curl 下载到 `.part` 临时名，成功后原子改名——中断不会留下
/// 让 models_present() 误判的半截文件。
fn download(url: &str, dest: &Path) -> Result<()> {
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| OcrError(format!("创建 {} 失败: {e}", dir.display())))?;
    }
    eprintln!("首次使用内置 OCR：下载模型 {url} …");
    let part = dest.with_extension("rten.part");
    let status = Command::new("curl")
        .args(["-fsSL", "--retry", "2", "--connect-timeout", "15", "-o"])
        .arg(&part)
        .arg(url)
        .status()
        .map_err(|e| {
            OcrError(format!(
                "启动 curl 失败: {e}。可手动下载 {url} 到 {}",
                dest.display()
            ))
        })?;
    if !status.success() {
        let _ = std::fs::remove_file(&part);
        return Err(OcrError(format!(
            "下载模型失败（curl 退出码 {:?}）。可手动下载 {url} 到 {}",
            status.code(),
            dest.display()
        )));
    }
    std::fs::rename(&part, dest).map_err(|e| OcrError(format!("保存模型失败: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_latin_langs_are_rejected() {
        let e = OcrsEngine::new(&["chi_sim".into(), "eng".into()]);
        assert!(!e.available());
        assert!(e.describe().contains("chi_sim"));

        let e = OcrsEngine::new(&["eng".into()]);
        assert!(e.unsupported_lang.is_none());

        let e = OcrsEngine::new(&[]);
        assert!(e.unsupported_lang.is_none());
    }
}
