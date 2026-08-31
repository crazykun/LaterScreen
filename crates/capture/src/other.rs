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

/// Win：`Monitor::from_point` 收物理像素（DPI 感知进程坐标系一致）。
#[cfg(not(target_os = "macos"))]
pub fn capture_at(x: i32, y: i32) -> Result<Screenshot> {
    shoot(&Monitor::from_point(x, y).map_err(err)?)
}

/// mac：上游传物理像素（cursor_position/CLI），`from_point` 却收 CG 逻辑点，
/// retina 下直接传会定位到错误的屏——先按物理包围盒定位再截。
#[cfg(target_os = "macos")]
pub fn capture_at(x: i32, y: i32) -> Result<Screenshot> {
    let monitors = Monitor::all().map_err(err)?;
    let screens = mac::screens(&monitors);
    let monitor = mac::idx_at_physical(&screens, x, y)
        .and_then(|i| monitors.get(i))
        .or_else(|| monitors.first())
        .ok_or_else(|| CaptureError("no monitor found".into()))?;
    shoot(monitor)
}

pub fn capture_all() -> Result<Vec<Screenshot>> {
    Monitor::all().map_err(err)?.iter().map(shoot).collect()
}

// ------------------------------------------------- macOS 坐标/事件辅助
//
// 坐标约定：本 crate 对外一律物理像素；而 macOS 上 xcap 的显示器几何
// （CGDisplayBounds）与 CG 事件 API 均为 CG 全局逻辑点（主屏左上原点）。
// 换算约定与 list_windows 一致：**全局逻辑点 × 所在屏 scale = 物理像素**
// （retina = 2.0 等），反向除回。截屏位图（CGWindowListCreateImage）本身
// 按 backing store 物理分辨率出图，与 Linux/Win 一致。
// 已知限制：多屏混合 DPI 时逻辑坐标系没有对应的全物理虚拟桌面，跨屏
// 选区的换算只在选区所在屏内精确（选区链路按物理像素设计，见
// pick_region_interactive）。
#[cfg(target_os = "macos")]
mod mac {
    use super::Monitor;

    /// CG CGPoint（repr(C) 与系统 ABI 一致）
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct CGPoint {
        pub x: f64,
        pub y: f64,
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        pub fn CFRelease(cf: *mut core::ffi::c_void);
    }

    // CoreGraphics：滚轮合成 / 指针控制 / 指针查询（系统框架直连，零新依赖）
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        pub fn CGEventCreate(source: *mut core::ffi::c_void) -> *mut core::ffi::c_void;
        pub fn CGEventGetLocation(event: *mut core::ffi::c_void) -> CGPoint;
        pub fn CGEventSourceCreate(state_id: u32) -> *mut core::ffi::c_void;
        pub fn CGEventCreateScrollWheelEvent(
            source: *mut core::ffi::c_void,
            units: u32,
            wheel_count: u32,
            ...
        ) -> *mut core::ffi::c_void;
        pub fn CGEventPost(tap: u32, event: *mut core::ffi::c_void);
        pub fn CGWarpMouseCursorPosition(new_position: CGPoint) -> i32;
    }

    /// kCGEventSourceStateHIDSystemState（合成滚轮事件源的标准状态）
    pub const SOURCE_HID: u32 = 1;
    /// kCGScrollEventUnitLine（行单位，一格滚轮 ≈ 3 行，对齐 X11 notch 语义）
    pub const SCROLL_UNIT_LINE: u32 = 1;
    /// kCGSessionEventTap：注入会话层，方向不被「自然滚动」系统设置翻转
    pub const SESSION_TAP: u32 = 1;

    /// 一台显示器的几何快照：scale + CG 逻辑包围盒
    pub struct Screen {
        pub scale: f32,
        pub x: f64,
        pub y: f64,
        pub w: f64,
        pub h: f64,
    }

    pub fn screens(monitors: &[Monitor]) -> Vec<Screen> {
        monitors
            .iter()
            .filter_map(|m| {
                let scale = m.scale_factor().ok()?;
                Some(Screen {
                    scale: if scale > 0.0 { scale } else { 1.0 },
                    x: m.x().ok()? as f64,
                    y: m.y().ok()? as f64,
                    w: m.width().ok()? as f64,
                    h: m.height().ok()? as f64,
                })
            })
            .collect()
    }

    /// 物理点落在哪台屏（物理包围盒 = 逻辑包围盒 × 各自 scale），返回下标
    pub fn idx_at_physical(screens: &[Screen], x: i32, y: i32) -> Option<usize> {
        let (xf, yf) = (x as f64, y as f64);
        screens.iter().position(|s| {
            let k = s.scale as f64;
            xf >= s.x * k && xf < (s.x + s.w) * k && yf >= s.y * k && yf < (s.y + s.h) * k
        })
    }

    /// CG 逻辑点落在哪台屏，返回下标
    pub fn idx_at_logical(screens: &[Screen], x: f64, y: f64) -> Option<usize> {
        screens
            .iter()
            .position(|s| x >= s.x && x < s.x + s.w && y >= s.y && y < s.y + s.h)
    }
}

/// Win/mac 无 Wayland 概念，恒 false。
pub fn is_wayland() -> bool {
    false
}

/// 交互式截图仅 Wayland portal 路径需要；Win/mac 走自绘覆盖层。
pub fn capture_interactive() -> Result<Screenshot> {
    Err(CaptureError("当前平台不支持交互式 portal 截图".into()))
}

/// Win：GetCursorPos，物理像素（截屏/边框同一坐标系）。
#[cfg(windows)]
pub fn cursor_position() -> Option<(i32, i32)> {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;
    let mut pt = POINT { x: 0, y: 0 };
    (unsafe { GetCursorPos(&mut pt) } != 0).then_some((pt.x, pt.y))
}

/// mac：CGEventGetLocation 拿 CG 全局逻辑点，× 所在屏 scale 换算回物理像素
/// （与截屏/选区同一坐标系）。
#[cfg(target_os = "macos")]
pub fn cursor_position() -> Option<(i32, i32)> {
    let ev = unsafe { mac::CGEventCreate(std::ptr::null_mut()) };
    if ev.is_null() {
        return None;
    }
    let pt = unsafe { mac::CGEventGetLocation(ev) };
    unsafe { mac::CFRelease(ev) };
    let monitors = Monitor::all().ok()?;
    let screens = mac::screens(&monitors);
    let s = mac::idx_at_logical(&screens, pt.x, pt.y).and_then(|i| screens.get(i))?;
    let k = s.scale as f64;
    Some(((pt.x * k).round() as i32, (pt.y * k).round() as i32))
}

/// 其余平台（理论上仅剩非 Win/mac 的移植目标）：返回 None 时上层回退主显示器
#[cfg(not(any(windows, target_os = "macos")))]
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

#[cfg(not(target_os = "macos"))]
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

/// mac：上游（覆盖层选区/CLI --region）全是物理像素，而 xcap mac 的
/// `capture_region` 收显示器相对的 **CG 逻辑点**——retina 下原样传会采到
/// 2 倍大且偏移的区域，全屏选区更会被边界校验直接拒绝。先按物理包围盒
/// 定位显示器，再把选区换算成显示器相对逻辑矩形（round 保证同输入同
/// 输出，滚动拼接要求帧宽恒定）。返回帧按 backing store 物理分辨率出图。
#[cfg(target_os = "macos")]
pub fn capture_region(x: i32, y: i32, w: u32, h: u32) -> Result<Screenshot> {
    let monitors = Monitor::all().map_err(err)?;
    let screens = mac::screens(&monitors);
    let Some(i) = mac::idx_at_physical(&screens, x, y) else {
        return Err(CaptureError("区域超出屏幕范围".into()));
    };
    let (monitor, s) = (&monitors[i], screens[i].scale as f64);
    let (lx, ly, lw, lh) = (screens[i].x, screens[i].y, screens[i].w, screens[i].h);
    // 物理 → 显示器相对逻辑点（全局逻辑 = 物理 ÷ 所在屏 scale）
    let rel_x = ((x as f64 / s) - lx).round().max(0.0);
    let rel_y = ((y as f64 / s) - ly).round().max(0.0);
    let rel_w = ((w as f64 / s).round().max(1.0)).min(lw - rel_x).max(1.0);
    let rel_h = ((h as f64 / s).round().max(1.0)).min(lh - rel_y).max(1.0);
    let img = monitor
        .capture_region(rel_x as u32, rel_y as u32, rel_w as u32, rel_h as u32)
        .map_err(err)?;
    let (width, height) = (img.width(), img.height());
    Ok(Screenshot {
        rgba: img.into_raw(),
        width,
        height,
        origin: (x, y),
        scale: screens[i].scale,
        is_primary: false,
    })
}

/// Win：SendInput 合成滚轮（事件落在指针当前所在的窗口）。clicks > 0 向上，
/// < 0 向下，与 Linux 的 XTest M4/M5 语义一致。
#[cfg(windows)]
pub fn scroll_wheel(clicks: i32) -> Result<()> {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_WHEEL, MOUSEINPUT,
    };
    /// 滚轮一格的标准增量（Win32 约定 WHEEL_DELTA = 120）
    const WHEEL_DELTA: u32 = 120;

    for _ in 0..clicks.unsigned_abs() {
        let input = INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx: 0,
                    dy: 0,
                    // 正增量向上、负增量向下
                    mouseData: if clicks > 0 {
                        WHEEL_DELTA
                    } else {
                        WHEEL_DELTA.wrapping_neg()
                    },
                    dwFlags: MOUSEEVENTF_WHEEL,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        let sent = unsafe { SendInput(1, &input, std::mem::size_of::<INPUT>() as i32) };
        if sent != 1 {
            // SendInput 被 UIPI 拦截（目标窗口以管理员运行）时返回 0
            return Err(CaptureError(
                "SendInput 注入滚轮失败（目标窗口可能以管理员权限运行）".into(),
            ));
        }
    }
    Ok(())
}

/// mac：CGEvent 合成滚轮。行单位（1 格 = 3 行）对齐 X11 button4/5 的 notch
/// 语义；clicks > 0 向上、< 0 向下。经会话层注入，方向不受「自然滚动」
/// 设置影响。指针需已在目标窗口上（滚动截图流程先 warp 到选区中心）。
#[cfg(target_os = "macos")]
pub fn scroll_wheel(clicks: i32) -> Result<()> {
    /// 一格滚轮的行数（经典滚轮 notch 约定）
    const LINES_PER_NOTCH: i32 = 3;
    let source = unsafe { mac::CGEventSourceCreate(mac::SOURCE_HID) };
    if source.is_null() {
        return Err(CaptureError("CGEventSourceCreate 失败".into()));
    }
    let delta = clicks * LINES_PER_NOTCH;
    let mut ok = true;
    for _ in 0..clicks.unsigned_abs() {
        let ev =
            unsafe { mac::CGEventCreateScrollWheelEvent(source, mac::SCROLL_UNIT_LINE, 1, delta) };
        if ev.is_null() {
            ok = false;
            break;
        }
        unsafe {
            mac::CGEventPost(mac::SESSION_TAP, ev);
            mac::CFRelease(ev);
        }
    }
    unsafe { mac::CFRelease(source) };
    if !ok {
        return Err(CaptureError("CGEventCreateScrollWheelEvent 失败".into()));
    }
    Ok(())
}

/// 其余平台暂未实现
#[cfg(not(any(windows, target_os = "macos")))]
pub fn scroll_wheel(_clicks: i32) -> Result<()> {
    Err(CaptureError("当前平台暂不支持滚动截图".into()))
}

/// Win：把指针移到虚拟桌面坐标（滚轮事件落在指针下的窗口，滚动截图
/// 依赖先把指针移到选区中心）。
#[cfg(windows)]
pub fn warp_pointer(x: i32, y: i32) -> Result<()> {
    use windows_sys::Win32::UI::WindowsAndMessaging::SetCursorPos;
    if unsafe { SetCursorPos(x, y) } == 0 {
        return Err(CaptureError("SetCursorPos 移动指针失败".into()));
    }
    Ok(())
}

/// mac：CGWarpMouseCursorPosition 收 CG 全局逻辑点，物理输入先 ÷ 所在屏
/// scale 换算（找不到所在屏时退回主屏 scale，单屏 retina 即主场景）。
#[cfg(target_os = "macos")]
pub fn warp_pointer(x: i32, y: i32) -> Result<()> {
    let monitors = Monitor::all().map_err(err)?;
    let screens = mac::screens(&monitors);
    let scale = mac::idx_at_physical(&screens, x, y)
        .and_then(|i| screens.get(i))
        .or_else(|| screens.first())
        .map(|s| s.scale as f64)
        .ok_or_else(|| CaptureError("no monitor found".into()))?;
    let rc = unsafe {
        mac::CGWarpMouseCursorPosition(mac::CGPoint {
            x: x as f64 / scale,
            y: y as f64 / scale,
        })
    };
    if rc != 0 {
        return Err(CaptureError(format!(
            "CGWarpMouseCursorPosition 失败（错误码 {rc}）"
        )));
    }
    Ok(())
}

/// 其余平台暂未实现
#[cfg(not(any(windows, target_os = "macos")))]
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

/// macOS 的录制选区边框：NSWindow 四条 2px 细条围在选区**外侧**，RAII。
/// 关键约束与 Linux/Win 版一致：**边条绝不覆盖选区像素**（采帧抓选区实际
/// 屏幕像素，选区内装饰都会进成品），贴屏幕边缘时宁可缺侧也不污染成品。
///
/// 线程模型：AppKit 只许主线程操作窗口——创建在调用线程（run_record/
/// run_scroll 都在主线程、且先于 eframe 事件循环）；闪烁线程的改色经
/// performSelectorOnMainThread 编组回主队列；guard 在主线程 drop 时直接
/// orderOut 收尾（滚动截图流程，事件循环已停、perform 无人泵），在闪烁
/// 线程 drop 时 orderOut 入队后放弃所有权——窗口随录制子进程退出销毁。
#[cfg(target_os = "macos")]
pub use mac_border::{record_border, RecordBorder};

#[cfg(target_os = "macos")]
mod mac_border {
    use super::mac;
    use super::Monitor;
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::{msg_send, sel, MainThreadMarker};
    use objc2_app_kit::{
        NSApplication, NSBackingStoreType, NSColor, NSWindow, NSWindowCollectionBehavior,
        NSWindowLevel, NSWindowStyleMask,
    };
    use objc2_foundation::{NSPoint, NSRect, NSSize};

    /// 初始颜色（红），与 app 闪烁线程的 RED 常量一致：armed 阶段静态红边
    const INITIAL: u32 = 0xE5_39_35;
    /// 边条厚度（物理像素；retina 下 1 逻辑点 = 2 物理 px，线条仍清晰）
    const T: i32 = 2;
    /// NSFloatingWindowLevel / kCGFloatingWindowLevel：悬浮在普通窗口之上
    const FLOATING: NSWindowLevel = 3;

    /// `Retained<NSWindow>` 非 Send，而 guard 会被 move 进闪烁线程：
    /// 包一层承诺只在主线程触碰（改色/收尾都经主线程编组）
    struct MainWin(Retained<NSWindow>);
    unsafe impl Send for MainWin {}

    pub struct RecordBorder {
        wins: Vec<MainWin>,
    }

    impl RecordBorder {
        /// 更新边条颜色（0xRRGGBB）。NSColor 是不可变值对象，后台线程构造
        /// 安全；`setBackgroundColor:` 必须在主线程执行，交给 perform。
        pub fn set_color(&self, pixel: u32) {
            let color = ns_color(pixel);
            let sel = sel!(setBackgroundColor:);
            for w in &self.wins {
                unsafe {
                    let _: () = msg_send![
                        &*w.0,
                        performSelectorOnMainThread: sel,
                        withObject: Some(&*color),
                        waitUntilDone: false
                    ];
                }
            }
        }
    }

    impl Drop for RecordBorder {
        fn drop(&mut self) {
            if MainThreadMarker::new().is_some() {
                // 主线程（滚动截图流程，事件循环已停）：就地隐藏并释放
                for w in self.wins.drain(..) {
                    w.0.orderOut(None);
                }
            } else {
                // 闪烁线程（录制流程的 guard move 进了线程）：orderOut 入队
                // 主线程；Retained 就地放弃所有权，避免跨线程 dealloc
                // （录制子进程随即退出，毫秒级窗口由 OS 回收）
                let sel = sel!(orderOut:);
                for w in self.wins.drain(..) {
                    unsafe {
                        let _: () = msg_send![
                            &*w.0,
                            performSelectorOnMainThread: sel,
                            withObject: Option::<&AnyObject>::None,
                            waitUntilDone: false
                        ];
                    }
                    std::mem::forget(w.0);
                }
            }
        }
    }

    fn ns_color(rgb: u32) -> Retained<NSColor> {
        NSColor::colorWithRed_green_blue_alpha(
            ((rgb >> 16) & 0xff) as f64 / 255.0,
            ((rgb >> 8) & 0xff) as f64 / 255.0,
            (rgb & 0xff) as f64 / 255.0,
            1.0,
        )
    }

    pub fn record_border(x: i32, y: i32, w: u32, h: u32) -> Option<RecordBorder> {
        if w == 0 || h == 0 {
            return None;
        }
        // AppKit 仅主线程；调用方（run_record/run_scroll）都在主线程
        let mtm = MainThreadMarker::new()?;
        // NSWindow 前置：显式初始化 NSApplication（eframe 稍后接管同一实例）
        let _app = NSApplication::sharedApplication(mtm);

        let monitors = Monitor::all().ok()?;
        let screens = mac::screens(&monitors);
        // 物理虚拟桌面（各屏逻辑包围盒 × scale 的并集）
        let (mut min_x, mut min_y, mut max_x, mut max_y) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for s in &screens {
            let k = s.scale as f64;
            min_x = min_x.min(s.x * k);
            min_y = min_y.min(s.y * k);
            max_x = max_x.max((s.x + s.w) * k);
            max_y = max_y.max((s.y + s.h) * k);
        }
        // 主屏逻辑高：AppKit 全局坐标 y 自下而上，与 CG（自上而下）相反
        let primary_lh = monitors
            .iter()
            .find(|m| m.is_primary().unwrap_or(false))
            .or_else(|| monitors.first())
            .and_then(|m| m.height().ok())
            .map(|h| h as f64)?;

        let wi = w.min(i32::MAX as u32) as i32;
        let hi = h.min(i32::MAX as u32) as i32;
        // 上/下/左/右四条边条的理想矩形（物理像素，选区外扩 T px）
        let strips = [
            (
                x.saturating_sub(T),
                y.saturating_sub(T),
                wi.saturating_add(2 * T),
                T,
            ),
            (
                x.saturating_sub(T),
                y.saturating_add(hi),
                wi.saturating_add(2 * T),
                T,
            ),
            (x.saturating_sub(T), y, T, hi),
            (x.saturating_add(wi), y, T, hi),
        ];

        let mut wins = Vec::new();
        for (sx, sy, sw, sh) in strips {
            // 钳到物理虚拟桌面：贴边选区的外扩条可能越界，裁掉越界部分
            let x0 = (sx as f64).max(min_x);
            let y0 = (sy as f64).max(min_y);
            let x1 = (sx.saturating_add(sw) as f64).min(max_x);
            let y1 = (sy.saturating_add(sh) as f64).min(max_y);
            if x1 <= x0 || y1 <= y0 {
                continue; // 该侧贴屏幕边缘，接受缺边
            }
            // 物理矩形 → CG 全局逻辑（÷ 所在屏 scale）
            let Some(s) =
                mac::idx_at_physical(&screens, x0 as i32, y0 as i32).and_then(|i| screens.get(i))
            else {
                continue;
            };
            let k = s.scale as f64;
            let (lx, ly) = (x0 / k, y0 / k);
            let (lw, lh) = ((x1 - x0) / k, (y1 - y0) / k);
            // CG 逻辑（y 自上而下）→ AppKit 逻辑（y 自下而上）
            let rect = NSRect::new(
                NSPoint::new(lx, primary_lh - (ly + lh)),
                NSSize::new(lw.max(1.0), lh.max(1.0)),
            );
            let win = unsafe {
                NSWindow::initWithContentRect_styleMask_backing_defer(
                    mtm.alloc::<NSWindow>(),
                    rect,
                    NSWindowStyleMask::Borderless,
                    NSBackingStoreType::Buffered,
                    false,
                )
            };
            // 不透明纯色、无阴影、点击穿透、跨空间可见、悬浮置顶、不激活 App
            win.setOpaque(true);
            win.setBackgroundColor(Some(&ns_color(INITIAL)));
            win.setHasShadow(false);
            win.setIgnoresMouseEvents(true);
            win.setCollectionBehavior(NSWindowCollectionBehavior::CanJoinAllSpaces);
            win.setLevel(FLOATING);
            win.orderFrontRegardless();
            wins.push(MainWin(win));
        }
        (!wins.is_empty()).then_some(RecordBorder { wins })
    }
}

/// 其余平台暂无录制边框（仅占位使 API 跨平台可用）
#[cfg(not(any(windows, target_os = "macos")))]
pub struct RecordBorder;

#[cfg(not(any(windows, target_os = "macos")))]
impl RecordBorder {
    pub fn set_color(&self, _pixel: u32) {}
}

/// Win 的录制选区边框：4 条 2px 细条围在选区**外侧**，RAII（Drop 即销毁，
/// 录制结束/出错/进程退出都不留残影）。
///
/// 关键约束与 Linux 版一致：**边条绝不覆盖选区像素**——采帧抓的是选区矩形
/// 的实际屏幕像素，选区内任何装饰都会被录进成品，因此边条画在选区外扩
/// 2px 的位置，选区贴屏幕边缘时钳到虚拟桌面范围、缺一侧就缺一侧（宁可
/// 缺边也不污染成品）。
///
/// 线程模型：Win32 窗口只能被创建它的线程销毁，而边框 guard 在调用方线程
/// 创建、却常在闪烁线程 drop——所以窗口的创建/改色/销毁全部收进一个专属
/// 线程，外界只经 mpsc 通信。`RecordBorder` drop = 关 channel = 归属线程
/// 销毁窗口后退出，`join` 保证进程退出前绝无残影。
#[cfg(windows)]
pub struct RecordBorder {
    /// 归属线程的邮箱：发 u32 = 改色（0xRRGGBB）；Sender 关闭 = 销毁退出
    tx: Option<std::sync::mpsc::Sender<u32>>,
    join: Option<std::thread::JoinHandle<()>>,
}

#[cfg(windows)]
impl RecordBorder {
    /// 更新全部边条颜色（0xRRGGBB，如 0xE53935 红）。上层的闪烁线程定时
    /// 调用（红/蓝交替）实现"录制中"的视觉提示；归属线程已退出则静默忽略。
    pub fn set_color(&self, pixel: u32) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(pixel);
        }
    }
}

#[cfg(windows)]
impl Drop for RecordBorder {
    fn drop(&mut self) {
        // 关闭 channel：归属线程 recv 失败 → 销毁窗口、注销窗口类、退出
        self.tx.take();
        if let Some(h) = self.join.take() {
            let _ = h.join();
        }
    }
}

/// RGB(0xRRGGBB) → COLORREF(0x00BBGGRR)
#[cfg(windows)]
fn to_colorref(rgb: u32) -> u32 {
    let r = (rgb >> 16) & 0xff;
    let g = (rgb >> 8) & 0xff;
    let b = rgb & 0xff;
    (b << 16) | (g << 8) | r
}

/// 在选区 (x, y, w, h) 周围显示录制边框。任何失败（选区外无可用空间/
/// 建窗失败）都返回 None：录制行为不变，仅无边框。
#[cfg(windows)]
pub fn record_border(x: i32, y: i32, w: u32, h: u32) -> Option<RecordBorder> {
    use std::sync::mpsc;

    if w == 0 || h == 0 {
        return None;
    }
    let (tx, rx) = mpsc::channel::<u32>();
    // 就绪握手：归属线程建好（或建失败）后回报，失败返回 None——与 Linux 版
    // 「失败即无边框、录制行为不变」的语义一致
    let (ready_tx, ready_rx) = mpsc::sync_channel::<bool>(1);
    let join = std::thread::Builder::new()
        .name("lscreen-record-border".into())
        .spawn(move || border_thread(rx, ready_tx, x, y, w, h))
        .ok()?;
    if ready_rx.recv().unwrap_or(false) {
        Some(RecordBorder {
            tx: Some(tx),
            join: Some(join),
        })
    } else {
        drop(tx);
        let _ = join.join();
        None
    }
}

/// 边框窗口归属线程的主体：建窗（回报就绪）→ 收消息改色 → channel 关闭后
/// 销毁窗口退出。所有 Win32 窗口操作都限制在本线程。
#[cfg(windows)]
fn border_thread(
    rx: std::sync::mpsc::Receiver<u32>,
    ready_tx: std::sync::mpsc::SyncSender<bool>,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
) {
    use std::sync::atomic::{AtomicU32, Ordering};
    use windows_sys::Win32::Foundation::{HMODULE, HWND};
    use windows_sys::Win32::Graphics::Gdi::{
        CreateSolidBrush, DeleteObject, InvalidateRect, UpdateWindow, HBRUSH,
    };
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, PeekMessageW,
        RegisterClassW, SetClassLongPtrW, SetWindowPos, ShowWindow, TranslateMessage,
        UnregisterClassW, GCLP_HBRBACKGROUND, HWND_TOPMOST, MSG, PM_REMOVE, SWP_NOACTIVATE,
        SWP_NOMOVE, SWP_NOSIZE, SW_SHOWNOACTIVATE, WNDCLASSW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
        WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
    };

    /// 初始颜色（红），与 app 闪烁线程的 RED 常量一致：armed 阶段静态红边
    const INITIAL: u32 = 0xE5_39_35;
    /// 边条厚度
    const T: i32 = 2;

    /// 泵掉队列里的消息（含 WM_PAINT）：边条背景由窗口类画刷在默认
    /// WM_PAINT 里填充，无需自绘
    fn pump_paint() {
        unsafe {
            let mut msg: MSG = std::mem::zeroed();
            while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }

    let setup = (|| -> Option<(Vec<HWND>, HBRUSH, Vec<u16>, HMODULE)> {
        let (min_x, min_y, bw, bh) = monitor_bounds().ok()?;
        let (max_x, max_y) = (
            min_x.saturating_add(bw.min(i32::MAX as u32) as i32),
            min_y.saturating_add(bh.min(i32::MAX as u32) as i32),
        );
        let wi = w.min(i32::MAX as u32) as i32;
        let hi = h.min(i32::MAX as u32) as i32;
        // 上/下/左/右四条边条的理想矩形（选区外扩 T px；u32→i32 先钳再算防溢出）
        let strips = [
            (
                x.saturating_sub(T),
                y.saturating_sub(T),
                wi.saturating_add(2 * T),
                T,
            ),
            (
                x.saturating_sub(T),
                y.saturating_add(hi),
                wi.saturating_add(2 * T),
                T,
            ),
            (x.saturating_sub(T), y, T, hi),
            (x.saturating_add(wi), y, T, hi),
        ];

        let hinstance = unsafe { GetModuleHandleW(std::ptr::null()) };
        // 每实例一个窗口类（类画刷即边条颜色，多实例不互扰）
        static SEQ: AtomicU32 = AtomicU32::new(0);
        let class_name: Vec<u16> = format!(
            "lscreen-border-{}-{}\0",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        )
        .encode_utf16()
        .collect();
        let brush = unsafe { CreateSolidBrush(to_colorref(INITIAL)) };
        if brush.is_null() {
            return None;
        }
        let wc = WNDCLASSW {
            lpfnWndProc: Some(DefWindowProcW),
            hInstance: hinstance,
            hbrBackground: brush,
            lpszClassName: class_name.as_ptr(),
            ..Default::default()
        };
        if unsafe { RegisterClassW(&wc) } == 0 {
            unsafe { DeleteObject(brush) };
            return None;
        }
        // 工具窗（不进任务栏/alt-tab）+ 置顶 + 点击穿透（不抢鼠标事件）+ 不抢焦点
        let ex = WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE;
        let mut wins = Vec::new();
        for (sx, sy, sw, sh) in strips {
            // 钳到虚拟桌面：贴边选区的外扩条可能越界，裁掉越界部分
            let x0 = sx.max(min_x);
            let y0 = sy.max(min_y);
            let x1 = sx.saturating_add(sw).min(max_x);
            let y1 = sy.saturating_add(sh).min(max_y);
            if x1 <= x0 || y1 <= y0 {
                continue; // 该侧贴屏幕边缘，接受缺边
            }
            let win = unsafe {
                CreateWindowExW(
                    ex,
                    class_name.as_ptr(),
                    std::ptr::null(),
                    WS_POPUP,
                    x0,
                    y0,
                    x1 - x0,
                    y1 - y0,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    hinstance,
                    std::ptr::null(),
                )
            };
            if win.is_null() {
                break;
            }
            unsafe {
                ShowWindow(win, SW_SHOWNOACTIVATE);
                // 显式置顶（ShowWindow 后 topmost 偶被同类窗口顶掉，补一刀）
                SetWindowPos(
                    win,
                    HWND_TOPMOST,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                );
            }
            wins.push(win);
        }
        if wins.is_empty() {
            unsafe {
                DeleteObject(brush);
                UnregisterClassW(class_name.as_ptr(), hinstance);
            }
            return None;
        }
        pump_paint(); // 立即画上初始红边
        Some((wins, brush, class_name, hinstance))
    })();

    let Some((wins, mut brush, class_name, hinstance)) = setup else {
        let _ = ready_tx.send(false);
        return;
    };
    let _ = ready_tx.send(true);

    while let Ok(pixel) = rx.recv() {
        let new = unsafe { CreateSolidBrush(to_colorref(pixel)) };
        if !new.is_null() {
            // 类画刷全类共享：改一处四条全变；换下的旧画刷立即删
            unsafe { SetClassLongPtrW(wins[0], GCLP_HBRBACKGROUND, new as isize) };
            let old = std::mem::replace(&mut brush, new);
            unsafe { DeleteObject(old) };
            for &win in &wins {
                unsafe {
                    InvalidateRect(win, std::ptr::null(), 1);
                    UpdateWindow(win);
                }
            }
        }
        pump_paint();
    } // channel 关闭（RecordBorder drop）→ 收尾
    for &win in &wins {
        unsafe { DestroyWindow(win) };
    }
    unsafe {
        DeleteObject(brush);
        UnregisterClassW(class_name.as_ptr(), hinstance);
    }
}

/// 其余平台暂无边框（仅占位使 API 跨平台可用）
#[cfg(not(any(windows, target_os = "macos")))]
pub fn record_border(_x: i32, _y: i32, _w: u32, _h: u32) -> Option<RecordBorder> {
    None
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
