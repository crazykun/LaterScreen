//! Windows / macOS 截屏：委托 xcap（这两个平台上走系统 API，无 C 编译依赖）。

use crate::{CaptureError, Result, Screenshot};
use xcap::Monitor;

fn err<E: std::fmt::Display>(e: E) -> CaptureError {
    CaptureError(e.to_string())
}

fn shoot(monitor: &Monitor) -> Result<Screenshot> {
    let img = monitor.capture_image().map_err(err)?;
    let (width, height) = (img.width(), img.height());
    Ok(Screenshot {
        rgba: img.into_raw(),
        width,
        height,
        origin: (monitor.x().map_err(err)?, monitor.y().map_err(err)?),
        scale: monitor.scale_factor().map_err(err)?,
        is_primary: monitor.is_primary().unwrap_or(false),
    })
}

pub fn capture_primary() -> Result<Screenshot> {
    let monitors = Monitor::all().map_err(err)?;
    let primary = monitors
        .iter()
        .find(|m| m.is_primary().unwrap_or(false))
        .or_else(|| monitors.first())
        .ok_or_else(|| CaptureError("no monitor found".into()))?;
    shoot(primary)
}

pub fn capture_at(x: i32, y: i32) -> Result<Screenshot> {
    shoot(&Monitor::from_point(x, y).map_err(err)?)
}

pub fn capture_all() -> Result<Vec<Screenshot>> {
    Monitor::all().map_err(err)?.iter().map(shoot).collect()
}

/// Win/mac：xcap 未暴露指针查询；返回 None 时上层回退主显示器。
/// TODO(M5): windows-rs GetCursorPos / objc2 NSEvent.mouseLocation
pub fn cursor_position() -> Option<(i32, i32)> {
    None
}

// ------------------------------------------------- 窗口枚举（M9，委托 xcap）

use crate::WindowInfo;

/// xcap 的 `Window::all()` 两平台都按 Z 序自顶向下返回
/// （Win = EnumWindows；mac = CGWindowListCopyWindowInfo），
/// 且已过滤不可见/工具窗口/本进程窗口。
pub fn list_windows() -> Vec<WindowInfo> {
    let Ok(windows) = xcap::Window::all() else {
        return Vec::new();
    };
    // 排除自家其他进程（贴图/录制状态窗）：Windows 的 app_name = 进程 exe 名，
    // mac = 应用名；与自身可执行文件名比对（大小写不敏感）
    let own_exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|s| s.to_string_lossy().into_owned()));
    let n = windows.len();
    let mut out = Vec::new();
    for (i, w) in windows.iter().enumerate() {
        let (Ok(width), Ok(height)) = (w.width(), w.height()) else {
            continue;
        };
        if width == 0 || height == 0 || w.is_minimized().unwrap_or(false) {
            continue;
        }
        if let (Some(own), Ok(app)) = (&own_exe, w.app_name()) {
            if app.eq_ignore_ascii_case(own) {
                continue;
            }
        }
        // 仅在 macOS 缩放换算时会被重新赋值；Windows 无此逻辑，mut 会触发
        // unused_mut 警告，故按平台条件加 mut（width/height 复用 66 行的绑定）
        #[cfg(target_os = "macos")]
        let (mut x, mut y) = (w.x().unwrap_or(0), w.y().unwrap_or(0));
        #[cfg(not(target_os = "macos"))]
        let (x, y) = (w.x().unwrap_or(0), w.y().unwrap_or(0));
        #[cfg(target_os = "macos")]
        let (mut width, mut height) = (width, height);
        // macOS：CG 坐标是逻辑点，按窗口中心所在显示器的缩放比换算物理像素，
        // 与 Screenshot 的坐标系（xcap Monitor 原点 + 物理像素位图）对齐
        #[cfg(target_os = "macos")]
        if let Ok(scale) = Monitor::from_point(x + width as i32 / 2, y + height as i32 / 2)
            .and_then(|m| m.scale_factor())
        {
            if scale > 0.0 {
                x = (x as f32 * scale).round() as i32;
                y = (y as f32 * scale).round() as i32;
                width = (width as f32 * scale).round() as u32;
                height = (height as f32 * scale).round() as u32;
            }
        }
        out.push(WindowInfo {
            id: w.id().unwrap_or(0) as u64,
            title: w.title().unwrap_or_default(),
            x,
            y,
            width,
            height,
            z_order: (n - i) as u32,
            is_minimized: false,
        });
    }
    out
}

pub fn frontmost_window() -> Option<WindowInfo> {
    let list = list_windows();
    if list.is_empty() {
        return None;
    }
    // Win = GetForegroundWindow；mac = 当前活跃 App 的窗口。
    // 取不到（无前台/活跃窗口）退回列表首个（Z 序最顶）
    xcap::Window::all()
        .ok()
        .and_then(|ws| {
            ws.iter()
                .find(|w| w.is_focused().unwrap_or(false))
                .and_then(|w| w.id().ok())
        })
        .and_then(|id| list.iter().find(|i| i.id == id as u64))
        .cloned()
        .or_else(|| list.first().cloned())
}

pub fn capture_region(x: i32, y: i32, w: u32, h: u32) -> Result<Screenshot> {
    // xcap 的 capture_region 是显示器相对坐标；先定位区域左上角所在显示器
    let monitor = Monitor::from_point(x, y).map_err(err)?;
    let (mx, my) = (monitor.x().map_err(err)?, monitor.y().map_err(err)?);
    let rel_x = (x - mx).max(0) as u32;
    let rel_y = (y - my).max(0) as u32;
    let img = monitor.capture_region(rel_x, rel_y, w, h).map_err(err)?;
    let (width, height) = (img.width(), img.height());
    Ok(Screenshot {
        rgba: img.into_raw(),
        width,
        height,
        origin: (x, y),
        scale: monitor.scale_factor().map_err(err)?,
        is_primary: false,
    })
}

/// Win/mac 的滚轮合成（SendInput / CGEvent）随 M4 系统编码器一并实现
pub fn scroll_wheel(_clicks: i32) -> Result<()> {
    Err(CaptureError("当前平台暂不支持滚动截图".into()))
}

pub fn warp_pointer(_x: i32, _y: i32) -> Result<()> {
    Err(CaptureError("当前平台暂不支持指针移动".into()))
}

pub fn set_window_class(_window_id: u32, _class: &str) -> Result<()> {
    Err(CaptureError("当前平台无 X11 WM_CLASS 语义".into()))
}

pub fn set_window_icon(_window_id: u32, _rgba: &[u8], _w: u32, _h: u32) -> Result<()> {
    Err(CaptureError("当前平台无 X11 窗口图标语义".into()))
}

pub fn set_fullscreen_span(_window_id: u32) -> Result<()> {
    Err(CaptureError("当前平台无 X11 跨屏 fullscreen 语义".into()))
}

// --------------------------------------------- 录制选区边框（M10）

/// Win/mac 暂无录制边框（与滚动截图同平台策略）：仅占位使 API 跨平台可用
pub struct RecordBorder;

impl RecordBorder {
    pub fn set_color(&self, _pixel: u32) {}
}

/// 虚拟桌面（全部显示器并集）的 (x, y, w, h)，物理像素。
pub fn monitor_bounds() -> Result<(i32, i32, u32, u32)> {
    let monitors = Monitor::all().map_err(err)?;
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
    for m in &monitors {
        let (x, y) = (m.x().map_err(err)?, m.y().map_err(err)?);
        let (w, h) = (m.width().map_err(err)?, m.height().map_err(err)?);
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        max_x = max_x.max(x.saturating_add(w.min(i32::MAX as u32) as i32));
        max_y = max_y.max(y.saturating_add(h.min(i32::MAX as u32) as i32));
    }
    if max_x <= min_x || max_y <= min_y {
        return Err(CaptureError("no monitor found".into()));
    }
    Ok((min_x, min_y, (max_x - min_x) as u32, (max_y - min_y) as u32))
}

/// 主显示器的 (x, y, w, h)，物理像素。无主屏标记时退回第一台（Win/mac 由
/// xcap 的 `Monitor::is_primary()` 提供；macOS 上主屏恒为内置屏）。
pub fn primary_monitor_bounds() -> Result<(i32, i32, u32, u32)> {
    let monitors = Monitor::all().map_err(err)?;
    let m = monitors
        .iter()
        .find(|m| m.is_primary().unwrap_or(false))
        .or_else(|| monitors.first())
        .ok_or_else(|| CaptureError("no monitor found".into()))?;
    Ok((
        m.x().map_err(err)?,
        m.y().map_err(err)?,
        m.width().map_err(err)?,
        m.height().map_err(err)?,
    ))
}

pub fn record_border(_x: i32, _y: i32, _w: u32, _h: u32) -> Option<RecordBorder> {
    None
}
