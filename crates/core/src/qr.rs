//! 二维码识别：rqrr 纯 Rust 实现。输入 RGBA 像素，输出解码内容。

pub struct QrResult {
    pub content: String,
}

/// 在 RGBA 图像中检测并解码所有二维码。
pub fn detect(rgba: &[u8], w: u32, h: u32) -> Vec<QrResult> {
    if rgba.len() < (w as usize) * (h as usize) * 4 || w == 0 || h == 0 {
        return Vec::new();
    }
    let (w, h) = (w as usize, h as usize);
    let mut img = rqrr::PreparedImage::prepare_from_greyscale(w, h, |x, y| {
        let i = (y * w + x) * 4;
        // ITU-R BT.601 亮度
        ((rgba[i] as u32 * 299 + rgba[i + 1] as u32 * 587 + rgba[i + 2] as u32 * 114) / 1000)
            as u8
    });
    img.detect_grids()
        .iter()
        .filter_map(|g| g.decode().ok())
        .map(|(_, content)| QrResult { content })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 生成 → 识别 全链路回归。
    #[test]
    fn roundtrip() {
        let code = qrcode::QrCode::new(b"https://ailater.com/lscreen").unwrap();
        // 按模块矩阵手工放大成像素图（含静区）
        let width = code.width();
        let scale = 8usize;
        let quiet = 4 * scale; // 静区
        let dim = width * scale + quiet * 2;
        let mut rgba = vec![255u8; dim * dim * 4];
        for y in 0..width {
            for x in 0..width {
                if code[(x, y)] == qrcode::Color::Dark {
                    for dy in 0..scale {
                        for dx in 0..scale {
                            let px = quiet + x * scale + dx;
                            let py = quiet + y * scale + dy;
                            let i = (py * dim + px) * 4;
                            rgba[i] = 0;
                            rgba[i + 1] = 0;
                            rgba[i + 2] = 0;
                        }
                    }
                }
            }
        }
        let found = detect(&rgba, dim as u32, dim as u32);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].content, "https://ailater.com/lscreen");
    }

    #[test]
    fn empty_image_no_panic() {
        assert!(detect(&[], 0, 0).is_empty());
    }
}
