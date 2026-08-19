//! Windows：`Windows.Media.Ocr` 系统引擎。
//!
//! 引擎与语言包随系统分发（Win10+，中文系统自带 zh OCR 包），零体积零下载。
//! 每个引擎单语言（`MaxRecognizerLanguageCount` 通常为 1），语言未装时
//! 回退用户系统语言列表。词级结果无置信度（confidence = None）。

use windows::core::{Interface, HSTRING};
use windows::Globalization::Language;
use windows::Graphics::Imaging::{BitmapPixelFormat, SoftwareBitmap};
use windows::Media::Ocr::OcrEngine;
use windows::Storage::Streams::Buffer;
use windows::Win32::System::WinRT::IBufferByteAccess;

use crate::lang::tess_to_bcp47;
use crate::{OcrError, OcrOutput, Result, TextBlock, TextRecognizer};

pub struct WinOcr {
    /// BCP-47 标签；None = 未指定或不可映射，跟随用户系统语言列表
    tag: Option<String>,
}

impl WinOcr {
    pub fn new(languages: &[String]) -> Self {
        let tag = languages
            .iter()
            .find_map(|l| tess_to_bcp47(l).map(str::to_string));
        Self { tag }
    }

    fn create_engine(&self) -> windows::core::Result<OcrEngine> {
        if let Some(tag) = &self.tag {
            let lang = Language::CreateLanguage(&HSTRING::from(tag.as_str()))?;
            // 指定语言未装 OCR 包时 TryCreate 失败，回退用户语言列表
            if let Ok(engine) = OcrEngine::TryCreateFromLanguage(&lang) {
                return Ok(engine);
            }
        }
        OcrEngine::TryCreateFromUserProfileLanguages()
    }
}

impl TextRecognizer for WinOcr {
    fn available(&self) -> bool {
        self.create_engine().is_ok()
    }

    fn describe(&self) -> String {
        match &self.tag {
            Some(tag) => format!("Windows 系统 OCR（语言 {tag}，未装语言包时回退系统语言）"),
            None => "Windows 系统 OCR（Windows.Media.Ocr，跟随系统语言）".into(),
        }
    }

    fn recognize(&self, rgba: &[u8], width: u32, height: u32) -> Result<OcrOutput> {
        let engine = self
            .create_engine()
            .map_err(|e| OcrError(format!("创建 Windows OCR 引擎失败: {e}")))?;

        // RGBA → BGRA（WinRT 位图字节序），然后经 IBufferByteAccess 直写像素
        let bgra = crate::rgba_to_bgra(rgba);
        let len = bgra.len() as u32;
        let buffer = Buffer::Create(len).map_err(|e| OcrError(format!("创建缓冲失败: {e}")))?;
        buffer
            .SetLength(len)
            .map_err(|e| OcrError(format!("设置缓冲长度失败: {e}")))?;
        let access: IBufferByteAccess = buffer
            .cast()
            .map_err(|e| OcrError(format!("获取缓冲指针失败: {e}")))?;
        unsafe {
            let ptr = access
                .Buffer()
                .map_err(|e| OcrError(format!("锁定像素内存失败: {e}")))?;
            std::ptr::copy_nonoverlapping(bgra.as_ptr(), ptr, bgra.len());
        }
        let bitmap = SoftwareBitmap::CreateCopyFromBuffer(
            &buffer,
            BitmapPixelFormat::Bgra8,
            width as i32,
            height as i32,
        )
        .map_err(|e| OcrError(format!("创建位图失败: {e}")))?;

        // join() 阻塞等结果；调用线程未显式初始化 COM 时 WinRT 隐式按 MTA 处理，
        // 后台线程阻塞等待安全（无 STA 重入死锁）
        let result = engine
            .RecognizeAsync(&bitmap)
            .map_err(|e| OcrError(format!("提交识别失败: {e}")))?
            .join()
            .map_err(|e| OcrError(format!("识别失败: {e}")))?;

        let mut blocks = Vec::new();
        for line in result
            .Lines()
            .map_err(|e| OcrError(format!("读取结果失败: {e}")))?
        {
            let text = line
                .Text()
                .map_err(|e| OcrError(format!("读取文本失败: {e}")))?
                .to_string_lossy();
            if !text.trim().is_empty() {
                blocks.push(TextBlock {
                    text,
                    confidence: None,
                });
            }
        }
        Ok(OcrOutput { blocks })
    }
}
