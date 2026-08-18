//! Linux：tesseract 子进程封装。
//!
//! 走 stdin/stdout 管道（`tesseract stdin stdout`），不落临时文件；
//! TSV 输出模式解析出词级置信度，按行聚合成文本块。

use std::io::Write;
use std::process::{Command, Stdio};

use crate::{OcrError, OcrOutput, Result, TextBlock, TextRecognizer};

pub struct Tesseract {
    langs: String,
}

impl Tesseract {
    pub fn new(languages: &[String]) -> Self {
        let langs = if languages.is_empty() {
            // 中文简体 + 英文是国内桌面的合理默认；未装语言包时 tesseract 会明确报错
            "chi_sim+eng".to_string()
        } else {
            languages.join("+")
        };
        Self { langs }
    }

    fn binary() -> Option<String> {
        // PATH 探测 + 常见安装位置兜底
        let candidates = [
            "tesseract",
            "/usr/bin/tesseract",
            "/usr/local/bin/tesseract",
        ];
        for c in candidates {
            if Command::new(c)
                .arg("--version")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
            {
                return Some(c.to_string());
            }
        }
        None
    }
}

impl TextRecognizer for Tesseract {
    fn available(&self) -> bool {
        Self::binary().is_some()
    }

    fn describe(&self) -> String {
        match Self::binary() {
            Some(bin) => format!("tesseract ({bin}), 语言: {}", self.langs),
            None => "未找到 tesseract。安装: sudo apt install tesseract-ocr tesseract-ocr-chi-sim"
                .into(),
        }
    }

    fn recognize(&self, rgba: &[u8], width: u32, height: u32) -> Result<OcrOutput> {
        let bin = Self::binary().ok_or_else(|| OcrError(self.describe()))?;

        // 编码为 PNG 经 stdin 传入
        let img = image::RgbaImage::from_raw(width, height, rgba.to_vec())
            .ok_or_else(|| OcrError("无效的图像缓冲".into()))?;
        let mut png = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .map_err(|e| OcrError(format!("PNG 编码失败: {e}")))?;

        let mut child = Command::new(&bin)
            .args(["stdin", "stdout", "-l", &self.langs, "tsv"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| OcrError(format!("启动 tesseract 失败: {e}")))?;

        child
            .stdin
            .take()
            .ok_or_else(|| OcrError("无法写入 tesseract stdin".into()))?
            .write_all(&png)
            .map_err(|e| OcrError(format!("写入图像失败: {e}")))?;

        let out = child
            .wait_with_output()
            .map_err(|e| OcrError(format!("等待 tesseract 失败: {e}")))?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            return Err(OcrError(format!(
                "tesseract 退出异常: {}",
                stderr.lines().last().unwrap_or("未知错误")
            )));
        }

        Ok(parse_tsv(&String::from_utf8_lossy(&out.stdout)))
    }
}

/// 拼接行内词：tesseract 把中文按字/词切成独立 word，直接 join(" ") 会得到
/// "中 文 识 别"。规则是只在两侧都不是 CJK 时补空格（西文才需要词间空格）。
fn join_words(words: &[String]) -> String {
    let mut text = String::new();
    let mut prev_cjk_or_empty = true;
    for w in words {
        let starts_cjk = w.chars().next().is_some_and(is_cjk);
        if !text.is_empty() && !prev_cjk_or_empty && !starts_cjk {
            text.push(' ');
        }
        text.push_str(w);
        prev_cjk_or_empty = w.chars().last().is_some_and(is_cjk);
    }
    text
}

/// CJK 及全角标点：这些字符前后不需要空格。
fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x1100..=0x11FF     // 韩文字母
        | 0x2E80..=0x2FFF   // 部首、康熙部首
        | 0x3000..=0x303F   // CJK 标点（。、「」等）
        | 0x3040..=0x30FF   // 平假名、片假名
        | 0x3130..=0x318F   // 韩文兼容字母
        | 0x3400..=0x4DBF   // 扩展 A
        | 0x4E00..=0x9FFF   // 基本区
        | 0xAC00..=0xD7AF   // 韩文音节
        | 0xF900..=0xFAFF   // 兼容表意
        | 0xFF00..=0xFFEF   // 全角形式
        | 0x20000..=0x2FA1F // 扩展 B–F、兼容补充
    )
}

/// 解析 tesseract TSV：level=5 为词，按 (block, par, line) 聚合成行文本块。
/// 词置信度取行内均值。
fn parse_tsv(tsv: &str) -> OcrOutput {
    let mut blocks: Vec<TextBlock> = Vec::new();
    let mut cur_key = (u32::MAX, u32::MAX, u32::MAX);
    let mut cur_words: Vec<String> = Vec::new();
    let mut cur_confs: Vec<f32> = Vec::new();

    let flush = |words: &mut Vec<String>, confs: &mut Vec<f32>, blocks: &mut Vec<TextBlock>| {
        if words.is_empty() {
            return;
        }
        let text = join_words(&words[..]);
        let confidence = if confs.is_empty() {
            None
        } else {
            Some(confs.iter().sum::<f32>() / confs.len() as f32 / 100.0)
        };
        blocks.push(TextBlock { text, confidence });
        words.clear();
        confs.clear();
    };

    for line in tsv.lines().skip(1) {
        let cols: Vec<&str> = line.split('\t').collect();
        // level page block par line word left top width height conf text
        if cols.len() < 12 || cols[0] != "5" {
            continue;
        }
        let key = (
            cols[2].parse().unwrap_or(0),
            cols[3].parse().unwrap_or(0),
            cols[4].parse().unwrap_or(0),
        );
        if key != cur_key {
            flush(&mut cur_words, &mut cur_confs, &mut blocks);
            cur_key = key;
        }
        let word = cols[11].trim();
        if word.is_empty() {
            continue;
        }
        cur_words.push(word.to_string());
        if let Ok(c) = cols[10].parse::<f32>() {
            if c >= 0.0 {
                cur_confs.push(c);
            }
        }
    }
    flush(&mut cur_words, &mut cur_confs, &mut blocks);
    OcrOutput { blocks }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cjk_words_join_without_spaces() {
        // tesseract 把中文切成 中/文/识别/测试 四个 word
        let words: Vec<String> = ["中", "文", "识别", "测试", "hello", "world"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(join_words(&words), "中文识别测试hello world");
    }

    #[test]
    fn latin_only_keeps_spaces() {
        let words: Vec<String> = ["hello", "world"].iter().map(|s| s.to_string()).collect();
        assert_eq!(join_words(&words), "hello world");
    }

    #[test]
    fn tsv_parsing_groups_lines() {
        let tsv = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n\
                   5\t1\t1\t1\t1\t1\t0\t0\t10\t10\t95.5\thello\n\
                   5\t1\t1\t1\t1\t2\t12\t0\t10\t10\t90.5\tworld\n\
                   5\t1\t1\t1\t2\t1\t0\t20\t10\t10\t80.0\tnext\n";
        let out = parse_tsv(tsv);
        assert_eq!(out.blocks.len(), 2);
        assert_eq!(out.blocks[0].text, "hello world");
        assert!((out.blocks[0].confidence.unwrap() - 0.93).abs() < 0.01);
        assert_eq!(out.blocks[1].text, "next");
        assert_eq!(out.plain_text(), "hello world\nnext");
    }
}
