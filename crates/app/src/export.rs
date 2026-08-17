//! 导出：图元合成 → PNG 文件 / 系统剪贴板。

use lscreen_core::render::Renderer;
use lscreen_core::{Element, RectF};
use std::borrow::Cow;
use std::path::PathBuf;

/// 从整幅 RGBA 中裁剪出 region（图像物理像素坐标）。
pub fn crop_rgba(rgba: &[u8], w: u32, h: u32, region: RectF) -> (Vec<u8>, u32, u32) {
    let x0 = (region.min.x.round().max(0.0) as u32).min(w);
    let y0 = (region.min.y.round().max(0.0) as u32).min(h);
    let x1 = (region.max.x.round().max(0.0) as u32).clamp(x0, w);
    let y1 = (region.max.y.round().max(0.0) as u32).clamp(y0, h);
    let (cw, ch) = (x1 - x0, y1 - y0);
    if cw == 0 || ch == 0 {
        return (rgba.to_vec(), w, h);
    }
    let mut out = Vec::with_capacity((cw * ch * 4) as usize);
    for y in y0..y1 {
        let start = ((y * w + x0) * 4) as usize;
        out.extend_from_slice(&rgba[start..start + (cw * 4) as usize]);
    }
    (out, cw, ch)
}

/// 合成并裁剪出选区的 RGBA。region 为图像物理像素坐标。
pub fn compose(
    renderer: &Renderer,
    rgba: &[u8],
    w: u32,
    h: u32,
    elements: &[Element],
    region: RectF,
) -> (Vec<u8>, u32, u32) {
    let full = renderer.render(rgba, w, h, elements);
    crop_rgba(&full, w, h, region)
}

/// 默认保存路径：~/Pictures（存在时）或当前目录，文件名带时间戳。
pub fn default_save_path() -> PathBuf {
    let stamp = timestamp();
    let name = format!("lscreen_{stamp}.png");
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from);
    if let Some(home) = home {
        let pictures = home.join("Pictures");
        if pictures.is_dir() {
            return pictures.join(name);
        }
        return home.join(name);
    }
    PathBuf::from(name)
}

fn timestamp() -> String {
    // 避免引入 chrono：用 UNIX 秒数换算本地无关的紧凑时间戳
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{secs}")
}

pub fn save_png(rgba: &[u8], w: u32, h: u32, path: &PathBuf) -> Result<(), String> {
    let img = image::RgbaImage::from_raw(w, h, rgba.to_vec())
        .ok_or_else(|| "invalid image buffer".to_string())?;
    img.save(path).map_err(|e| e.to_string())
}

pub fn copy_text_to_clipboard(text: &str) -> Result<(), String> {
    let mut cb = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    cb.set_text(text).map_err(|e| e.to_string())?;
    #[cfg(target_os = "linux")]
    std::thread::sleep(std::time::Duration::from_millis(150));
    Ok(())
}

pub fn copy_to_clipboard(rgba: &[u8], w: u32, h: u32) -> Result<(), String> {
    let mut cb = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    cb.set_image(arboard::ImageData {
        width: w as usize,
        height: h as usize,
        bytes: Cow::Borrowed(rgba),
    })
    .map_err(|e| e.to_string())?;
    // X11 下剪贴板数据随进程退出丢失（除非有剪贴板管理器）。
    // 短暂驻留给管理器接管的时间窗口；彻底方案（fork 常驻直至被覆盖）在 M5。
    #[cfg(target_os = "linux")]
    std::thread::sleep(std::time::Duration::from_millis(300));
    Ok(())
}
