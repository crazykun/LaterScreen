//! 颜色格式换算：取色器（M2）的 RGB / HEX / CMYK 输出。
//!
//! RGB→CMYK 没有唯一正确答案（严格换算依赖 ICC 色彩配置），
//! 取色器场景采用业界通用的朴素公式，与主流工具（Snipaste 等）行为一致。

use crate::model::Rgba;

pub fn to_hex(c: Rgba) -> String {
    format!("#{:02X}{:02X}{:02X}", c.r(), c.g(), c.b())
}

pub fn to_rgb_str(c: Rgba) -> String {
    format!("{}, {}, {}", c.r(), c.g(), c.b())
}

/// 返回 (c, m, y, k)，各分量 0..=100（百分比）。
pub fn to_cmyk(c: Rgba) -> (u8, u8, u8, u8) {
    let (r, g, b) = (
        c.r() as f32 / 255.0,
        c.g() as f32 / 255.0,
        c.b() as f32 / 255.0,
    );
    let k = 1.0 - r.max(g).max(b);
    if k >= 1.0 - f32::EPSILON {
        return (0, 0, 0, 100);
    }
    let cy = (1.0 - r - k) / (1.0 - k);
    let m = (1.0 - g - k) / (1.0 - k);
    let y = (1.0 - b - k) / (1.0 - k);
    let pct = |v: f32| (v * 100.0).round() as u8;
    (pct(cy), pct(m), pct(y), pct(k))
}

pub fn to_cmyk_str(c: Rgba) -> String {
    let (cy, m, y, k) = to_cmyk(c);
    format!("{}%, {}%, {}%, {}%", cy, m, y, k)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cmyk_known_values() {
        assert_eq!(to_cmyk(Rgba([255, 255, 255, 255])), (0, 0, 0, 0));
        assert_eq!(to_cmyk(Rgba([0, 0, 0, 255])), (0, 0, 0, 100));
        assert_eq!(to_cmyk(Rgba([255, 0, 0, 255])), (0, 100, 100, 0));
    }

    #[test]
    fn hex_format() {
        assert_eq!(to_hex(Rgba([0xe5, 0x39, 0x35, 0xff])), "#E53935");
    }
}
