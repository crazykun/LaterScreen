//! lscreen-capture: 截屏封装。
//!
//! 平台策略（服务于「单文件、无动态库依赖」目标）：
//! - Linux X11：x11rb（纯 Rust XCB 协议实现），多显示器走 RandR
//! - Linux Wayland：计划走 xdg-desktop-portal（ashpd，纯 Rust D-Bus），M5 实现
//! - Windows / macOS：xcap（走系统 API，无 C 编译依赖）
//!
//! 刻意不用 xcap 的 Linux 路径：它强制链接 pipewire（C 动态库）。

#[cfg(target_os = "linux")]
mod linux;
#[cfg(not(target_os = "linux"))]
mod other;

#[cfg(target_os = "linux")]
use linux as platform;
#[cfg(not(target_os = "linux"))]
use other as platform;

use std::fmt;

#[derive(Debug)]
pub struct CaptureError(pub String);

impl fmt::Display for CaptureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for CaptureError {}

pub type Result<T> = std::result::Result<T, CaptureError>;

/// 一次截屏的结果。坐标体系：
/// - `origin` 是该显示器在虚拟桌面上的原点
/// - `width/height` 是物理像素尺寸
/// - `scale` = 物理像素 / 逻辑坐标（X11 下恒为 1.0）
pub struct Screenshot {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub origin: (i32, i32),
    pub scale: f32,
    pub is_primary: bool,
}

impl Screenshot {
    /// 读取物理像素坐标 (x, y) 处的颜色，越界返回 None。
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let i = ((y * self.width + x) * 4) as usize;
        self.rgba.get(i..i + 4).map(|s| [s[0], s[1], s[2], s[3]])
    }
}

/// 截主显示器。找不到主显示器时退回第一台。
pub fn capture_primary() -> Result<Screenshot> {
    platform::capture_primary()
}

/// 截包含指定虚拟桌面坐标的显示器。
pub fn capture_at(x: i32, y: i32) -> Result<Screenshot> {
    platform::capture_at(x, y)
}

/// 截所有显示器。
pub fn capture_all() -> Result<Vec<Screenshot>> {
    platform::capture_all()
}

/// 截取虚拟桌面上的任意矩形区域（物理像素坐标）。录屏采帧用。
pub fn capture_region(x: i32, y: i32, w: u32, h: u32) -> Result<Screenshot> {
    platform::capture_region(x, y, w, h)
}

/// 当前鼠标指针的虚拟桌面坐标。查询失败（平台未实现/无指针设备）返回 None。
pub fn cursor_position() -> Option<(i32, i32)> {
    platform::cursor_position()
}

/// 在指针当前位置发送滚轮事件（clicks > 0 向上，< 0 向下）。
/// 平台不支持（Win/mac 未实现）返回 Err。
pub fn scroll_wheel(clicks: i32) -> Result<()> {
    platform::scroll_wheel(clicks)
}

/// 把指针移动到虚拟桌面坐标 (x, y)。平台不支持返回 Err。
pub fn warp_pointer(x: i32, y: i32) -> Result<()> {
    platform::warp_pointer(x, y)
}

/// 设置 X11 窗口的 WM_CLASS（instance 与 class 均为 `class`）。
/// 任务栏/启动器据此把窗口关联到 .desktop 文件；仅 Linux X11 有意义，
/// 其余平台返回 Err。窗口创建后即可调用。
pub fn set_window_class(window_id: u32, class: &str) -> Result<()> {
    platform::set_window_class(window_id, class)
}

/// 设置 X11 窗口的 `_NET_WM_ICON`（RGBA8）。任务栏/alt-tab 直接显示此图标，
/// 不依赖 .desktop 图标缓存。仅 Linux X11 有意义，其余平台返回 Err。
pub fn set_window_icon(window_id: u32, rgba: &[u8], w: u32, h: u32) -> Result<()> {
    platform::set_window_icon(window_id, rgba, w, h)
}

/// 让一个 fullscreen 窗口经 `_NET_WM_FULLSCREEN_MONITORS` 跨全部显示器
/// （多屏覆盖层用）。给出四条边所在的显示器编号，WM 让窗口铺满其围成的
/// 矩形。仅 Linux X11 有意义；WM 不支持时无副作用（窗口维持单屏 fullscreen），
/// 其余平台返回 Err。窗口须已 map 且处于 fullscreen 状态时调用。
pub fn set_fullscreen_span(window_id: u32) -> Result<()> {
    platform::set_fullscreen_span(window_id)
}

// ---------------------------------------------------------------- 录制选区边框（M10）

/// 录制期间的选区边框 guard：Linux X11 在选区外侧显示 4 条置顶、点击穿透的
/// 红色边条，Drop 即销毁。Win/mac/Wayland 为空占位。
pub use platform::RecordBorder;

/// 在选区 (x, y, w, h) 周围显示录制边框，返回 RAII guard（guard 存活期间
/// 边框常显）。平台不支持或创建失败返回 None——录制行为不变，仅无边框。
pub fn record_border(x: i32, y: i32, w: u32, h: u32) -> Option<RecordBorder> {
    platform::record_border(x, y, w, h)
}

/// 虚拟桌面（全部显示器并集）的 (x, y, w, h)。上层摆放"避开录制选区"的
/// 窗口（状态窗等）用。失败返回 None。
pub fn monitor_bounds() -> Option<(i32, i32, u32, u32)> {
    platform::monitor_bounds().ok()
}

/// 主显示器的 (x, y, w, h)。上层摆放"靠近托盘/主屏右下"的窗口（历史面板）
/// 用——多屏下应贴主屏而不是虚拟桌面右边界（Deepin 的 dock 在主屏）。
/// 失败返回 None。
pub fn primary_monitor_bounds() -> Option<(i32, i32, u32, u32)> {
    platform::primary_monitor_bounds().ok()
}

// ---------------------------------------------------------------- 窗口枚举（M9）

/// 一个可交互的顶层窗口。坐标语义与 `Screenshot::origin` 同一坐标系同一单位
/// （Linux X11 = 根窗口物理像素；Windows = 屏幕物理像素；macOS = CG 全局点），
/// 与 `Screenshot` 的原点相减即可换算到图像像素。
#[derive(Debug, Clone)]
pub struct WindowInfo {
    pub id: u64,
    pub title: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    /// Z 序：越大越靠前。列表本身已按 Z 序自顶向下排序
    pub z_order: u32,
    /// 最小化窗口已在上游过滤，此字段恒 false（保留以符合数据模型）
    pub is_minimized: bool,
}

impl WindowInfo {
    pub fn contains(&self, x: i32, y: i32) -> bool {
        self.width > 0
            && self.height > 0
            && x >= self.x
            && y >= self.y
            && x < self.x + self.width as i32
            && y < self.y + self.height as i32
    }
}

/// 枚举顶层窗口，按 Z 序自顶向下。已过滤：最小化、其他桌面、面板/停靠区
/// 等辅助窗口、本程序自己的窗口（贴图/录制状态窗）。
/// 失败或平台不支持（如 Wayland）返回空列表——上层降级为纯手动框选，不报错。
pub fn list_windows() -> Vec<WindowInfo> {
    platform::list_windows()
}

/// 当前最前（活跃）窗口。取不到活跃窗口时退回 Z 序最顶的普通窗口。
pub fn frontmost_window() -> Option<WindowInfo> {
    platform::frontmost_window()
}

/// 虚拟桌面坐标 (x, y) 处最上面的窗口（Z 序自顶向下第一个含点者）。
pub fn window_at(x: i32, y: i32) -> Option<WindowInfo> {
    list_windows().into_iter().find(|w| w.contains(x, y))
}

/// 窗口矩形与一台显示器截图的交集，换算为图像像素坐标 (x, y, w, h)。
/// 无交集返回 None。macOS 的窗口矩形是 CG 逻辑点，这里按该显示器的
/// 缩放比换算成物理像素——平台差异收敛在本函数，不泄漏到 app。
pub fn window_rect_in_image(win: &WindowInfo, shot: &Screenshot) -> Option<(f32, f32, f32, f32)> {
    #[cfg(target_os = "macos")]
    let (x, y, w, h) = (
        (win.x as f32 - shot.origin.0 as f32) * shot.scale,
        (win.y as f32 - shot.origin.1 as f32) * shot.scale,
        win.width as f32 * shot.scale,
        win.height as f32 * shot.scale,
    );
    #[cfg(not(target_os = "macos"))]
    let (x, y, w, h) = (
        win.x as f32 - shot.origin.0 as f32,
        win.y as f32 - shot.origin.1 as f32,
        win.width as f32,
        win.height as f32,
    );
    let (sw, sh) = (shot.width as f32, shot.height as f32);
    let x0 = x.max(0.0).min(sw);
    let y0 = y.max(0.0).min(sh);
    let x1 = (x + w).min(sw).max(0.0);
    let y1 = (y + h).min(sh).max(0.0);
    if x1 <= x0 || y1 <= y0 {
        None
    } else {
        Some((x0, y0, x1 - x0, y1 - y0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shot(w: u32, h: u32, origin: (i32, i32)) -> Screenshot {
        Screenshot {
            rgba: vec![0; (w * h * 4) as usize],
            width: w,
            height: h,
            origin,
            scale: 1.0,
            is_primary: true,
        }
    }

    fn win(x: i32, y: i32, w: u32, h: u32) -> WindowInfo {
        WindowInfo {
            id: 1,
            title: String::new(),
            x,
            y,
            width: w,
            height: h,
            z_order: 0,
            is_minimized: false,
        }
    }

    #[test]
    fn window_contains() {
        let w = win(10, 20, 100, 50);
        assert!(w.contains(10, 20));
        assert!(w.contains(109, 69));
        assert!(!w.contains(110, 20));
        assert!(!w.contains(10, 70));
        assert!(!win(0, 0, 0, 10).contains(0, 0));
    }

    #[test]
    fn rect_in_image_full() {
        let s = shot(1920, 1080, (0, 0));
        assert_eq!(
            window_rect_in_image(&win(100, 50, 800, 600), &s),
            Some((100.0, 50.0, 800.0, 600.0))
        );
    }

    #[test]
    fn rect_in_image_negative_origin() {
        // 显示器在主屏左侧（原点为负）：窗口根坐标换算到图像坐标
        let s = shot(1920, 1080, (-1920, 0));
        assert_eq!(
            window_rect_in_image(&win(-1920, 100, 800, 600), &s),
            Some((0.0, 100.0, 800.0, 600.0))
        );
    }

    #[test]
    fn rect_in_image_clip_and_miss() {
        let s = shot(1920, 1080, (0, 0));
        // 越出右/下边界 → 裁剪
        assert_eq!(
            window_rect_in_image(&win(1500, 900, 800, 400), &s),
            Some((1500.0, 900.0, 420.0, 180.0))
        );
        // 完全在屏幕外 → None
        assert_eq!(window_rect_in_image(&win(2000, 0, 100, 100), &s), None);
        assert_eq!(window_rect_in_image(&win(-500, 0, 100, 100), &s), None);
    }
}
