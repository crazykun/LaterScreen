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
        let (mut x, mut y) = (w.x().unwrap_or(0), w.y().unwrap_or(0));
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
