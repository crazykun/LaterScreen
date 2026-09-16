//! 上次交付选区的记忆（M13）：`<cache>/screen.sel` 存上次**交付**
//! （复制/保存/贴图/二维码/OCR/录屏框选）的区域。
//!
//! 语义：
//! - 存**绝对物理像素**坐标（虚拟桌面坐标系），与覆盖层截图来源无关；
//! - 附带显示器布局指纹（union + 主屏拼串哈希），布局变了（拔插屏/
//!   改分辨率）记录作废，覆盖层回退「最前窗口」预选，静默降级不提示；
//! - 换算到某次覆盖层的图像坐标时要求矩形**完整落在截图内**——
//!   跨屏缓冲天然满足；单屏回退路径下选区在另一块屏时直接判无效，
//!   不做「裁剪到边缘」的半吊子预选（裁出来的细条比没有更迷惑）。
//!
//! 走缓存目录（与历史副本同理：可再生的派生数据，删了只丢便利不丢配置）。
//! 写入 tmp+rename 原子替换，读侧永远不会看到半截文件。

use std::io::Write;
use std::path::PathBuf;

use lscreen_core::{RectF, P2};

/// 记录一次交付选区（绝对物理像素）。失败静默——记忆是尽力而为的便利。
pub fn remember(abs: (i32, i32, u32, u32)) {
    let Some(fp) = layout_fingerprint() else {
        return;
    };
    let path = sel_path();
    let line = format!("v1 {} {} {} {} {}\n", fp, abs.0, abs.1, abs.2, abs.3);
    // 同目录 tmp + rename：并发写互不撕裂；rename 跨平台替换语义同历史索引
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    let write = || -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(line.as_bytes())?;
        f.sync_all().ok();
        std::fs::rename(&tmp, &path)
    };
    let _ = write();
}

/// 读取上次选区；文件缺失/损坏/指纹不符/布局探测失败一律 None（静默）。
pub fn recalled() -> Option<(i32, i32, u32, u32)> {
    let fp = layout_fingerprint()?;
    let text = std::fs::read_to_string(sel_path()).ok()?;
    let mut it = text.split_whitespace();
    match (
        it.next()?,
        it.next()?,
        it.next()?,
        it.next()?,
        it.next()?,
        it.next()?,
    ) {
        ("v1", f, x, y, w, h) => {
            let (f, x, y): (u64, i32, i32) = (f.parse().ok()?, x.parse().ok()?, y.parse().ok()?);
            let (w, h): (u32, u32) = (w.parse().ok()?, h.parse().ok()?);
            (f == fp && w >= 1 && h >= 1).then_some((x, y, w, h))
        }
        _ => None,
    }
}

/// 绝对像素矩形 → 覆盖层图像像素矩形。要求完整落在截图内（±1px 容差，
/// 防浮点边缘抖动），否则 None。纯函数，供单测。
pub fn to_image_rect(
    abs: (i32, i32, u32, u32),
    origin: (i32, i32),
    img_w: u32,
    img_h: u32,
) -> Option<RectF> {
    let (x0, y0) = (
        abs.0 as f32 - origin.0 as f32,
        abs.1 as f32 - origin.1 as f32,
    );
    let (x1, y1) = (x0 + abs.2 as f32, y0 + abs.3 as f32);
    let (w, h) = (img_w as f32, img_h as f32);
    // 完整在内（1px 容差）；顺带拒绝退化矩形
    let inside = x0 >= -1.0 && y0 >= -1.0 && x1 <= w + 1.0 && y1 <= h + 1.0;
    let min = P2::new(x0.max(0.0), y0.max(0.0));
    let max = P2::new(x1.min(w), y1.min(h));
    (inside && max.x - min.x >= 1.0 && max.y - min.y >= 1.0).then_some(RectF { min, max })
}

/// 显示器布局指纹：union + 主屏几何拼串的 FNV-1a。任一探测失败 = None
/// （拿不到布局就不该信任旧记录，也不该写入新记录）。
fn layout_fingerprint() -> Option<u64> {
    let (ux, uy, uw, uh) = lscreen_capture::monitor_bounds()?;
    let primary = lscreen_capture::primary_monitor_bounds();
    let mut s = format!("{ux},{uy},{uw},{uh}");
    if let Some((px, py, pw, ph)) = primary {
        s.push_str(&format!("|{px},{py},{pw},{ph}"));
    }
    Some(fnv1a(s.as_bytes()))
}

fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
        (h ^ *b as u64).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

fn sel_path() -> PathBuf {
    // 测试注入：指向独立临时目录，避免动到开发者本机真实记录
    #[cfg(test)]
    if let Some(d) = TEST_DIR.lock().unwrap().clone() {
        return d.join("screen.sel");
    }
    crate::config::cache_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("screen.sel")
}

#[cfg(test)]
static TEST_DIR: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);
#[cfg(test)]
static TEST_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    /// 串行测试守护（同 history：TEST_DIR 是进程级全局，并行测试会互踩）。
    /// `_serial` 字段不读是刻意的：存在即持锁，drop 释放。
    struct Guard {
        _serial: std::sync::MutexGuard<'static, ()>,
    }

    impl Guard {
        fn new(dir: PathBuf) -> Self {
            let g = TEST_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
            *TEST_DIR.lock().unwrap_or_else(|e| e.into_inner()) = Some(dir);
            Guard { _serial: g }
        }
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            *TEST_DIR.lock().unwrap_or_else(|e| e.into_inner()) = None;
        }
    }

    #[test]
    fn to_image_rect_requires_fully_inside() {
        let o = (0, 0);
        // 完整在内
        assert!(to_image_rect((10, 10, 100, 50), o, 1920, 1080).is_some());
        // 原点非零的截图（副屏在左侧，origin=(-1920,0)）
        assert!(to_image_rect((-1920, 0, 800, 600), (-1920, 0), 1920, 1080).is_some());
        // 越出右/下边界（含恰好贴边，允许 ±1px）
        assert!(to_image_rect((1000, 1000, 1000, 100), o, 1920, 1080).is_none());
        assert!(to_image_rect((0, 1079, 10, 2), o, 1920, 1080).is_some());
        // 选区在另一块屏（单屏回退路径）：完整在内判负
        assert!(to_image_rect((2000, 0, 400, 400), o, 1920, 1080).is_none());
        // 负坐标越界
        assert!(to_image_rect((-5, 0, 10, 10), o, 1920, 1080).is_none());
    }

    #[test]
    fn parse_roundtrip_and_garbage() {
        let dir = std::env::temp_dir().join(format!("lscreen-selcache-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let _g = Guard::new(dir.clone());

        // 垃圾/缺失 → None
        assert!(recalled().is_none());
        std::fs::write(dir.join("screen.sel"), "garbage").unwrap();
        assert!(recalled().is_none());

        // 正常行可解析（fingerprint 与当前布局一致才有效；布局探测在无头
        // CI 上可能失败，这里只测「指纹不符 → None」与文件内容格式）
        std::fs::write(
            dir.join("screen.sel"),
            format!("v1 {} 10 20 300 200\n", u64::MAX),
        )
        .unwrap();
        // u64::MAX 几乎不可能等于真实指纹 → 视为布局已变
        if lscreen_capture::monitor_bounds().is_some() {
            assert!(recalled().is_none());
        }

        // 写入格式：v1 + 指纹 + 四元组
        std::fs::remove_file(dir.join("screen.sel")).ok();
        remember((5, 6, 100, 40));
        if let Ok(text) = std::fs::read_to_string(dir.join("screen.sel")) {
            let mut it = text.split_whitespace();
            assert_eq!(it.next(), Some("v1"));
            assert!(it.next().unwrap().parse::<u64>().is_ok());
            assert_eq!(it.next(), Some("5"));
            assert_eq!(it.next(), Some("6"));
            assert_eq!(it.next(), Some("100"));
            assert_eq!(it.next(), Some("40"));
        } else if lscreen_capture::monitor_bounds().is_none() {
            // 布局探测失败时不写文件——同样合规
        } else {
            panic!("remember 应当落盘");
        }
        std::fs::remove_dir_all(&dir).ok();
    }
}
