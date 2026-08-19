//! 系统字体定位：加载一个支持中文的系统字体供 UI 与导出渲染共用。
//! 不在二进制里捆绑字体（CJK 字体动辄 10MB+），运行时从系统读取。

use eframe::egui;
use std::path::PathBuf;
use std::process::Command;

/// 返回字体文件字节。找不到时返回 None（UI 退化为 egui 默认字体，文本工具受限）。
pub fn load_system_font() -> Option<Vec<u8>> {
    for path in candidates() {
        if let Ok(data) = std::fs::read(&path) {
            return Some(data);
        }
    }
    None
}

/// 把字体字节挂进 egui 字体族（Proportional + Monospace 追加在内置字体后，
/// 拉丁字形仍用内置、CJK 落到系统字体）。
/// 调用方需先用 core Renderer 验证字节可解析：epaint 内部解析失败是 panic 而非 Err。
pub fn setup_egui_fonts(ctx: &egui::Context, bytes: Vec<u8>) {
    let mut fonts = egui::FontDefinitions::default();
    fonts
        .font_data
        .insert("system".into(), egui::FontData::from_owned(bytes).into());
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts
            .families
            .entry(family)
            .or_default()
            .push("system".into());
    }
    ctx.set_fonts(fonts);
}

fn candidates() -> Vec<PathBuf> {
    let mut list: Vec<PathBuf> = Vec::new();

    #[cfg(target_os = "linux")]
    {
        // fontconfig 是 Linux 桌面标配，优先让它挑一个中文 sans
        if let Ok(out) = Command::new("fc-match")
            .args(["-f", "%{file}", "sans:lang=zh"])
            .output()
        {
            if out.status.success() {
                let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !p.is_empty() {
                    list.push(PathBuf::from(p));
                }
            }
        }
        for p in [
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
            "/usr/share/fonts/wenquanyi/wqy-microhei/wqy-microhei.ttc",
        ] {
            list.push(PathBuf::from(p));
        }
    }

    #[cfg(target_os = "macos")]
    {
        for p in [
            "/System/Library/Fonts/PingFang.ttc",
            "/System/Library/Fonts/Hiragino Sans GB.ttc",
            "/System/Library/Fonts/STHeiti Light.ttc",
        ] {
            list.push(PathBuf::from(p));
        }
    }

    #[cfg(target_os = "windows")]
    {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
        for f in ["msyh.ttc", "msyh.ttf", "simhei.ttf", "simsun.ttc"] {
            list.push(PathBuf::from(&root).join("Fonts").join(f));
        }
    }

    // 通用兜底，避免非以上平台时未使用变量警告
    let _ = &mut list;
    list
}
