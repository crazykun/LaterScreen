//! 系统字体定位：加载一个支持中文的系统字体供 UI 与导出渲染共用。
//! 不在二进制里捆绑字体（CJK 字体动辄 10MB+），运行时从系统读取。

use eframe::egui;
use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
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

/// 把字体字节挂进 egui 字体族（Proportional + Monospace 均作为首选，
/// 内置字体保留作缺字 fallback），使预览与 core Renderer 导出使用同一主字体。
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
            .insert(0, "system".into());
    }
    ctx.set_fonts(fonts);
}

fn candidates() -> Vec<PathBuf> {
    let mut list: Vec<PathBuf> = Vec::new();

    #[cfg(target_os = "linux")]
    {
        // fc-list 只列真正声明支持中文的字体。不用 fc-match：实测部分发行版
        // （本机 CachyOS/Arch）上 `fc-match ':lang=zh'` 会把无汉字的 Noto Sans
        // 排在全部 CJK 字体之前（family 绑定压过 lang 评分），且该结果恒
        // status=success——「成功」加载一个中文全变方块的字体。
        if let Ok(out) = Command::new("fc-list")
            .args(["-f", "%{file}\n", ":lang=zh"])
            .output()
        {
            if out.status.success() {
                let files: Vec<String> = String::from_utf8_lossy(&out.stdout)
                    .lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty() && Path::new(l).is_file())
                    .map(str::to_string)
                    .collect();
                if let Some(p) = pick_cjk_font(&files) {
                    list.push(PathBuf::from(p));
                }
            }
        }
        // fc-match 次选（fc-list 异常的极简系统）；硬编码路径最后兜底
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

/// 从 `fc-list :lang=zh` 的文件列表里挑最合适的：已知中文字体家族优先；
/// 过重的字重（Black/Bold/Heavy，标注正文观感差）与衬线（Serif）降一档；
/// 同分取先出现者；无已知家族时任取第一个（列表本身已保证支持中文）。
fn pick_cjk_font(files: &[String]) -> Option<String> {
    const KNOWN: [&str; 7] = [
        "cjk",
        "yahei",
        "msyh",
        "wqy",
        "sourcehan",
        "simsun",
        "simhei",
    ];
    let score = |f: &str| -> i32 {
        let l = f.to_lowercase();
        let mut s = 0;
        if KNOWN.iter().any(|k| l.contains(k)) {
            s += 2;
            if ["black", "bold", "heavy"].iter().any(|k| l.contains(k)) {
                s -= 1;
            }
            if l.contains("serif") {
                s -= 1;
            }
        }
        s
    };
    let mut best: Option<(i32, &String)> = None;
    for f in files {
        let s = score(f);
        if best.is_none_or(|(bs, _)| s > bs) {
            best = Some((s, f));
        }
    }
    best.map(|(_, f)| f.clone())
}

#[cfg(test)]
mod tests {
    use super::pick_cjk_font;

    #[test]
    fn prefers_known_cjk_over_generic_latin() {
        // fc-match 曾把无汉字的 Noto Sans 排在前——picker 必须跳过它
        let files = [
            "/usr/share/fonts/noto/NotoSans-Regular.ttf".to_string(),
            "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc".to_string(),
        ];
        assert_eq!(
            pick_cjk_font(&files).as_deref(),
            Some("/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc")
        );
    }

    #[test]
    fn penalizes_heavy_weights_and_serif() {
        let files = [
            "/usr/share/fonts/noto-cjk/NotoSansCJK-Black.ttc".to_string(),
            "/usr/share/fonts/noto-cjk/NotoSerifCJK-Regular.ttc".to_string(),
            "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc".to_string(),
        ];
        assert_eq!(
            pick_cjk_font(&files).as_deref(),
            Some("/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc")
        );
    }

    #[test]
    fn falls_back_to_first_line_without_known_family() {
        let files = ["/x/a.ttf".to_string(), "/x/b.ttf".to_string()];
        assert_eq!(pick_cjk_font(&files).as_deref(), Some("/x/a.ttf"));
        assert_eq!(pick_cjk_font(&[]), None);
    }
}
