//! 二维码识别与生成：rqrr 识别 + qrcode 生成，均纯 Rust。

pub struct QrResult {
    pub content: String,
}

/// 纠错级别（生成用）：L 7% / M 15% / Q 25% / H 30%。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ecc {
    L,
    M,
    Q,
    H,
}

impl Ecc {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_uppercase().as_str() {
            "L" => Some(Ecc::L),
            "M" => Some(Ecc::M),
            "Q" => Some(Ecc::Q),
            "H" => Some(Ecc::H),
            _ => None,
        }
    }

    fn level(self) -> qrcode::EcLevel {
        match self {
            Ecc::L => qrcode::EcLevel::L,
            Ecc::M => qrcode::EcLevel::M,
            Ecc::Q => qrcode::EcLevel::Q,
            Ecc::H => qrcode::EcLevel::H,
        }
    }
}

/// 生成二维码位图（M13）：`px_per_module` 每模块像素（决定整图尺寸），
/// `margin` 四周静区的模块数（识别推荐 ≥4）。内容为空或超容量返回 Err
/// （文案面向用户）。底色白、码点黑——贴浅色截图可读，深色截图上用户
/// 可先加文字背景/矩形衬底（与 Snipaste 习惯一致）。
pub fn generate(
    text: &str,
    ecc: Ecc,
    px_per_module: u32,
    margin: u32,
) -> Result<image::RgbaImage, String> {
    if text.is_empty() {
        return Err("二维码内容不能为空".to_string());
    }
    if px_per_module == 0 || px_per_module > 64 {
        return Err("每模块像素需在 1-64 之间".to_string());
    }
    let code = qrcode::QrCode::with_error_correction_level(text.as_bytes(), ecc.level())
        .map_err(|e| format!("生成二维码失败（内容过长或编码错误）: {e}"))?;
    let modules = code.width() as u32;
    let dim = (modules + margin * 2) * px_per_module;
    let mut img = image::RgbaImage::from_pixel(dim, dim, image::Rgba([255, 255, 255, 255]));
    // to_colors：行主序的模块颜色矩阵（Dark = 深色）
    for (i, color) in code.to_colors().into_iter().enumerate() {
        if color != qrcode::Color::Dark {
            continue;
        }
        let (mx, my) = (i % modules as usize, i / modules as usize);
        let (mx, my) = (mx as u32, my as u32);
        for dy in 0..px_per_module {
            for dx in 0..px_per_module {
                let (px, py) = (
                    (mx + margin) * px_per_module + dx,
                    (my + margin) * px_per_module + dy,
                );
                img.put_pixel(px, py, image::Rgba([0, 0, 0, 255]));
            }
        }
    }
    Ok(img)
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
        ((rgba[i] as u32 * 299 + rgba[i + 1] as u32 * 587 + rgba[i + 2] as u32 * 114) / 1000) as u8
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

    /// 自家 generate 的产物要能被自家 detect 识回（M13 识别+生成闭环）。
    #[test]
    fn generate_then_detect_roundtrip() {
        for (text, ecc) in [
            ("https://ailater.com/lscreen", Ecc::M),
            ("hello world", Ecc::L),
            ("纠错级别 H 的中文内容", Ecc::H),
        ] {
            let img = generate(text, ecc, 8, 4).expect("生成失败");
            let found = detect(img.as_raw(), img.width(), img.height());
            assert_eq!(found.len(), 1, "{text}");
            assert_eq!(found[0].content, text);
        }
    }

    #[test]
    fn generate_validates_and_sizes() {
        assert!(generate("", Ecc::M, 8, 4).is_err());
        assert!(generate("x", Ecc::M, 0, 4).is_err());
        assert!(generate("x", Ecc::M, 8, 4).is_ok());
        // 尺寸 = (模块数 + 2*静区) * 每模块像素；QR v1 = 21 模块
        let img = generate("a", Ecc::M, 8, 4).unwrap();
        assert_eq!(img.width(), img.height());
        assert_eq!((img.width() - 8 * 4 * 2) % 8, 0);
        // 内容过长（超 v40 容量）报错而非 panic
        let long = "x".repeat(4000);
        assert!(generate(&long, Ecc::H, 8, 4).is_err());
        // Ecc 解析
        assert_eq!(Ecc::parse("m"), Some(Ecc::M));
        assert_eq!(Ecc::parse("Q"), Some(Ecc::Q));
        assert_eq!(Ecc::parse("x"), None);
    }
}
