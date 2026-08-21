//! Linux X11 截屏：x11rb 纯 Rust 实现。
//! Wayland 会话走 xdg-desktop-portal（M5，纯 D-Bus，只支持整屏快照——
//! 区域采帧/录屏/指针查询在 Wayland 下明确报错降级）。

use crate::{CaptureError, Result, Screenshot};

use x11rb::connection::Connection;
use x11rb::protocol::randr::ConnectionExt as _;
use x11rb::protocol::xproto::{ConnectionExt as _, ImageFormat, ImageOrder, Screen};

struct MonitorInfo {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    primary: bool,
}

fn err<E: std::fmt::Display>(e: E) -> CaptureError {
    CaptureError(e.to_string())
}

/// 当前会话是否纯 Wayland（无 X11 可用）
fn is_wayland() -> bool {
    let session_wayland = std::env::var("XDG_SESSION_TYPE").is_ok_and(|t| t == "wayland");
    let no_x11 = std::env::var_os("DISPLAY").is_none();
    session_wayland || (no_x11 && std::env::var_os("WAYLAND_DISPLAY").is_some())
}

/// X11 直连前置检查（区域采帧/录屏/指针路径仍走 X11）
fn ensure_x11() -> Result<()> {
    if is_wayland() {
        return Err(CaptureError(
            "Wayland 会话仅支持整屏截图（portal），区域采帧/录屏暂不支持".into(),
        ));
    }
    Ok(())
}

/// 经 xdg-desktop-portal 截整屏（M5 Wayland 路径）。
/// portal 只回整幅 PNG 文件（可能是全部显示器的拼接，视 DE 而定），
/// origin 固定 (0,0)。首次调用可能触发权限确认（视 DE 的 portal 后端策略）。
fn portal_screenshot() -> Result<Screenshot> {
    async_io::block_on(async {
        use ashpd::desktop::screenshot::{ScreenshotOptions, ScreenshotProxy};

        let proxy = ScreenshotProxy::new()
            .await
            .map_err(|e| CaptureError(format!("连接 xdg-desktop-portal 失败: {e}")))?;
        // request() 内部已 await Response 信号；response() 同步取结果
        let opts = ScreenshotOptions::default()
            .set_interactive(false)
            .set_modal(true);
        let request = proxy
            .screenshot(None, opts)
            .await
            .map_err(|e| CaptureError(format!("portal 请求失败: {e}")))?;
        let response = request
            .response()
            .map_err(|e| CaptureError(format!("portal 响应失败（未授权？）: {e}")))?;
        let uri = response.uri().as_str();
        let path = uri.strip_prefix("file://").unwrap_or(uri);
        let data = std::fs::read(path)
            .map_err(|e| CaptureError(format!("读取 portal 截图失败: {e}（{path}）")))?;
        let _ = std::fs::remove_file(path); // portal 产物在临时目录，读后即清
        let img = image::load_from_memory(&data)
            .map_err(|e| CaptureError(format!("portal 截图解码失败: {e}")))?
            .into_rgba8();
        let (width, height) = (img.width(), img.height());
        Ok(Screenshot {
            rgba: img.into_raw(),
            width,
            height,
            origin: (0, 0),
            scale: 1.0,
            is_primary: true,
        })
    })
}

fn monitors(conn: &impl Connection, screen: &Screen) -> Result<Vec<MonitorInfo>> {
    let reply = conn
        .randr_get_monitors(screen.root, true)
        .map_err(err)?
        .reply()
        .map_err(err)?;
    let mut list: Vec<MonitorInfo> = reply
        .monitors
        .iter()
        .map(|m| MonitorInfo {
            x: m.x as i32,
            y: m.y as i32,
            width: m.width as u32,
            height: m.height as u32,
            primary: m.primary,
        })
        .collect();
    if list.is_empty() {
        // 无 RandR 信息时退化为整个根窗口
        list.push(MonitorInfo {
            x: 0,
            y: 0,
            width: screen.width_in_pixels as u32,
            height: screen.height_in_pixels as u32,
            primary: true,
        });
    }
    Ok(list)
}

fn grab(conn: &impl Connection, screen: &Screen, m: &MonitorInfo) -> Result<Screenshot> {
    let img = conn
        .get_image(
            ImageFormat::Z_PIXMAP,
            screen.root,
            m.x as i16,
            m.y as i16,
            m.width as u16,
            m.height as u16,
            !0,
        )
        .map_err(err)?
        .reply()
        .map_err(err)?;

    let data = img.data;
    let expected = (m.width * m.height * 4) as usize;
    if data.len() < expected {
        return Err(CaptureError(format!(
            "GetImage 返回数据不足: {} < {expected}（不支持的像素格式 depth={}）",
            data.len(),
            img.depth
        )));
    }

    // ZPixmap depth24/32 每像素 4 字节。LSB 序为 BGRX，MSB 序为 XRGB。
    // data 可能因扫描线填充略长于 expected，先裁到 expected（w*h*4，必为 4 倍数）
    let lsb = conn.setup().image_byte_order == ImageOrder::LSB_FIRST;
    let mut rgba = vec![0u8; expected];
    for (dst, src) in rgba
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(data[..expected].as_chunks::<4>().0.iter())
    {
        let (r, g, b) = if lsb {
            (src[2], src[1], src[0])
        } else {
            (src[1], src[2], src[3])
        };
        dst[0] = r;
        dst[1] = g;
        dst[2] = b;
        dst[3] = 255;
    }

    Ok(Screenshot {
        rgba,
        width: m.width,
        height: m.height,
        origin: (m.x, m.y),
        scale: 1.0,
        is_primary: m.primary,
    })
}

fn with_conn<T>(
    f: impl FnOnce(&x11rb::rust_connection::RustConnection, &Screen) -> Result<T>,
) -> Result<T> {
    ensure_x11()?;
    let (conn, screen_num) = x11rb::connect(None).map_err(err)?;
    let screen = conn.setup().roots[screen_num].clone();
    f(&conn, &screen)
}

pub fn capture_primary() -> Result<Screenshot> {
    if is_wayland() {
        return portal_screenshot();
    }
    with_conn(|conn, screen| {
        let mons = monitors(conn, screen)?;
        let m = mons
            .iter()
            .find(|m| m.primary)
            .or_else(|| mons.first())
            .ok_or_else(|| CaptureError("no monitor found".into()))?;
        grab(conn, screen, m)
    })
}

pub fn capture_at(x: i32, y: i32) -> Result<Screenshot> {
    // portal 无显示器区分：Wayland 下任何坐标都给整幅快照
    if is_wayland() {
        return portal_screenshot();
    }
    with_conn(|conn, screen| {
        let mons = monitors(conn, screen)?;
        let m = mons
            .iter()
            .find(|m| x >= m.x && x < m.x + m.width as i32 && y >= m.y && y < m.y + m.height as i32)
            .or_else(|| mons.first())
            .ok_or_else(|| CaptureError("no monitor found".into()))?;
        grab(conn, screen, m)
    })
}

pub fn capture_all() -> Result<Vec<Screenshot>> {
    if is_wayland() {
        return Ok(vec![portal_screenshot()?]);
    }
    with_conn(|conn, screen| {
        monitors(conn, screen)?
            .iter()
            .map(|m| grab(conn, screen, m))
            .collect()
    })
}

pub fn cursor_position() -> Option<(i32, i32)> {
    with_conn(|conn, screen| {
        let reply = conn
            .query_pointer(screen.root)
            .map_err(err)?
            .reply()
            .map_err(err)?;
        Ok((reply.root_x as i32, reply.root_y as i32))
    })
    .ok()
}

// ------------------------------------------------------- 窗口枚举（M9，EWMH）

use x11rb::protocol::xproto::AtomEnum;

use crate::WindowInfo;

/// 本次枚举要用到的 EWMH 原子。intern 失败（WM 不支持 EWMH）时值为 0。
struct WinAtoms {
    supporting_wm_check: u32,
    client_list_stacking: u32,
    current_desktop: u32,
    net_wm_desktop: u32,
    net_wm_state: u32,
    net_wm_state_hidden: u32,
    net_wm_name: u32,
    wm_name: u32,
    net_wm_pid: u32,
    frame_extents: u32,
    window_type: u32,
    /// 这些类型的窗口不参与「窗口吸附选区」：面板/桌面/菜单/气泡等
    skip_types: Vec<u32>,
}

fn intern(conn: &impl Connection, name: &str) -> u32 {
    conn.intern_atom(true, name.as_bytes())
        .ok()
        .and_then(|c| c.reply().ok())
        .map(|r| r.atom)
        .unwrap_or(0)
}

impl WinAtoms {
    fn intern_all(conn: &impl Connection) -> Self {
        let skip_types = [
            "_NET_WM_WINDOW_TYPE_DOCK",
            "_NET_WM_WINDOW_TYPE_DESKTOP",
            "_NET_WM_WINDOW_TYPE_TOOLBAR",
            "_NET_WM_WINDOW_TYPE_MENU",
            "_NET_WM_WINDOW_TYPE_SPLASH",
            "_NET_WM_WINDOW_TYPE_NOTIFICATION",
            "_NET_WM_WINDOW_TYPE_TOOLTIP",
            "_NET_WM_WINDOW_TYPE_COMBO",
            "_NET_WM_WINDOW_TYPE_DND",
        ]
        .iter()
        .map(|n| intern(conn, n))
        .filter(|a| *a != 0)
        .collect();
        Self {
            supporting_wm_check: intern(conn, "_NET_SUPPORTING_WM_CHECK"),
            client_list_stacking: intern(conn, "_NET_CLIENT_LIST_STACKING"),
            current_desktop: intern(conn, "_NET_CURRENT_DESKTOP"),
            net_wm_desktop: intern(conn, "_NET_WM_DESKTOP"),
            net_wm_state: intern(conn, "_NET_WM_STATE"),
            net_wm_state_hidden: intern(conn, "_NET_WM_STATE_HIDDEN"),
            net_wm_name: intern(conn, "_NET_WM_NAME"),
            wm_name: intern(conn, "WM_NAME"),
            net_wm_pid: intern(conn, "_NET_WM_PID"),
            frame_extents: intern(conn, "_NET_FRAME_EXTENTS"),
            window_type: intern(conn, "_NET_WM_WINDOW_TYPE"),
            skip_types,
        }
    }
}

/// 读窗口属性原始字节；属性不存在或出错返回 None。
fn prop(
    conn: &impl Connection,
    window: u32,
    property: u32,
) -> Option<x11rb::protocol::xproto::GetPropertyReply> {
    if property == 0 {
        return None;
    }
    conn.get_property(false, window, property, AtomEnum::ANY, 0, 4096)
        .ok()?
        .reply()
        .ok()
}

/// 属性值的 u32 迭代视图。x11rb 的 value32() 在格式不符时返回 None，
/// 这里展平为空迭代，缺失属性一律当「无值」处理。
fn values32(r: &x11rb::protocol::xproto::GetPropertyReply) -> impl Iterator<Item = u32> + '_ {
    r.value32().into_iter().flatten()
}

fn window_title(conn: &impl Connection, win: u32, atoms: &WinAtoms) -> String {
    for prop_atom in [atoms.net_wm_name, atoms.wm_name] {
        if let Some(r) = prop(conn, win, prop_atom) {
            if !r.value.is_empty() {
                return String::from_utf8_lossy(&r.value).into_owned();
            }
        }
    }
    String::new()
}

/// 是否本程序自己的窗口（贴图/录制状态/配置面板）：贴图是独立进程，
/// 按「窗口 pid 的可执行文件 == 自身可执行文件」识别。
/// 覆盖层自身在窗口列表采集之后才建窗，不会出现在列表里。
fn is_own_process(conn: &impl Connection, win: u32, atoms: &WinAtoms) -> bool {
    let Some(pid) = prop(conn, win, atoms.net_wm_pid).and_then(|r| values32(&r).next()) else {
        return false;
    };
    if pid == std::process::id() {
        return true;
    }
    let Some(my_exe) = std::env::current_exe()
        .ok()
        .and_then(|p| p.canonicalize().ok())
    else {
        return false;
    };
    // /proc/<pid>/exe 是符号链接；同用户可读，异用户 Err → 不排除
    std::fs::read_link(format!("/proc/{pid}/exe")).is_ok_and(|p| p == my_exe)
}

/// 客户区根坐标 + 装饰边框（_NET_FRAME_EXTENTS）= 可见窗口矩形。
fn window_geometry(
    conn: &impl Connection,
    screen: &Screen,
    win: u32,
    atoms: &WinAtoms,
) -> Option<(i32, i32, u32, u32)> {
    let geo = conn.get_geometry(win).ok()?.reply().ok()?;
    let tr = conn
        .translate_coordinates(win, screen.root, 0, 0)
        .ok()?
        .reply()
        .ok()?;
    let border = geo.border_width as i32;
    let (mut x, mut y) = (tr.dst_x as i32 - border, tr.dst_y as i32 - border);
    let (mut w, mut h) = (geo.width as u32, geo.height as u32);
    if let Some(ext) = prop(conn, win, atoms.frame_extents) {
        let v: Vec<u32> = values32(&ext).collect();
        if v.len() == 4 {
            // _NET_FRAME_EXTENTS = left, right, top, bottom
            x -= v[0] as i32;
            y -= v[2] as i32;
            w = w.saturating_add(v[0]).saturating_add(v[1]);
            h = h.saturating_add(v[2]).saturating_add(v[3]);
        }
    }
    if w == 0 || h == 0 {
        return None;
    }
    Some((x, y, w, h))
}

fn list_windows_inner(conn: &impl Connection, screen: &Screen) -> Vec<WindowInfo> {
    let atoms = WinAtoms::intern_all(conn);
    // 无 EWMH 的 WM（裸 WM 等）给不了 Z 序/状态，降级为手动框选
    if atoms.supporting_wm_check == 0 || atoms.client_list_stacking == 0 {
        return Vec::new();
    }
    let Some(list_reply) = prop(conn, screen.root, atoms.client_list_stacking) else {
        return Vec::new();
    };
    // _NET_CLIENT_LIST_STACKING 自底向上
    let stacking: Vec<u32> = values32(&list_reply).collect();
    let current_desktop =
        prop(conn, screen.root, atoms.current_desktop).and_then(|r| values32(&r).next());

    let mut out = Vec::new();
    let n = stacking.len();
    for (i, &win) in stacking.iter().enumerate() {
        // 非当前桌面（0xFFFFFFFF = sticky，出现在所有桌面，保留）
        if atoms.net_wm_desktop != 0 {
            if let Some(desktop) =
                prop(conn, win, atoms.net_wm_desktop).and_then(|r| values32(&r).next())
            {
                if desktop != 0xFFFF_FFFF && Some(desktop) != current_desktop {
                    continue;
                }
            }
        }
        // 最小化（_NET_WM_STATE_HIDDEN）
        if atoms.net_wm_state != 0
            && atoms.net_wm_state_hidden != 0
            && prop(conn, win, atoms.net_wm_state)
                .map(|r| values32(&r).any(|a| a == atoms.net_wm_state_hidden))
                .unwrap_or(false)
        {
            continue;
        }
        // 面板/桌面/菜单等辅助窗口
        if atoms.window_type != 0
            && !atoms.skip_types.is_empty()
            && prop(conn, win, atoms.window_type)
                .map(|r| values32(&r).any(|t| atoms.skip_types.contains(&t)))
                .unwrap_or(false)
        {
            continue;
        }
        if is_own_process(conn, win, &atoms) {
            continue;
        }
        let Some((x, y, w, h)) = window_geometry(conn, screen, win, &atoms) else {
            continue;
        };
        out.push(WindowInfo {
            id: win as u64,
            title: window_title(conn, win, &atoms),
            x,
            y,
            width: w,
            height: h,
            z_order: (n - 1 - i) as u32,
            is_minimized: false,
        });
    }
    // stacking 自底向上 → 输出自顶向下（Z 序优先命中测试的顺序）
    out.reverse();
    out
}

pub fn list_windows() -> Vec<WindowInfo> {
    // Wayland 会话直接降级（不报错），由上层走纯手动框选
    with_conn(|conn, screen| Ok(list_windows_inner(conn, screen))).unwrap_or_default()
}

pub fn frontmost_window() -> Option<WindowInfo> {
    with_conn(|conn, screen| {
        let list = list_windows_inner(conn, screen);
        if list.is_empty() {
            return Ok(None);
        }
        // _NET_ACTIVE_WINDOW = 当前聚焦窗口；不在列表里（自家/已过滤）时
        // 退回 Z 序最顶
        if let Some(r) = prop(conn, screen.root, intern(conn, "_NET_ACTIVE_WINDOW")) {
            if let Some(active) = values32(&r).next() {
                if active != 0 {
                    if let Some(found) = list.iter().find(|w| w.id == active as u64) {
                        return Ok(Some(found.clone()));
                    }
                }
            }
        }
        Ok(list.first().cloned())
    })
    .ok()
    .flatten()
}

pub fn capture_region(x: i32, y: i32, w: u32, h: u32) -> Result<Screenshot> {
    with_conn(|conn, screen| {
        // 可截区域 = 全部显示器的并集。显示器原点可为负（位于主屏左侧/上方），
        // 钳到根窗口 [0, sw] 会把负坐标区域错误地折进主屏。
        let mons = monitors(conn, screen)?;
        let (mut min_x, mut min_y, mut max_x, mut max_y) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
        for m in &mons {
            min_x = min_x.min(m.x);
            min_y = min_y.min(m.y);
            max_x = max_x.max(m.x + m.width as i32);
            max_y = max_y.max(m.y + m.height as i32);
        }
        if max_x <= min_x || max_y <= min_y {
            return Err(CaptureError("no monitor found".into()));
        }
        let x0 = x.max(min_x).min(max_x);
        let y0 = y.max(min_y).min(max_y);
        // w/h 来自 u32，转 i32 可能为负；saturating_add 避免 debug 下溢出 panic
        let x1 = x
            .saturating_add(w.min(i32::MAX as u32) as i32)
            .max(x0)
            .min(max_x);
        let y1 = y
            .saturating_add(h.min(i32::MAX as u32) as i32)
            .max(y0)
            .min(max_y);
        if x1 - x0 < 1 || y1 - y0 < 1 {
            return Err(CaptureError("区域超出屏幕范围".into()));
        }
        grab(
            conn,
            screen,
            &MonitorInfo {
                x: x0,
                y: y0,
                width: (x1 - x0) as u32,
                height: (y1 - y0) as u32,
                primary: false,
            },
        )
    })
}

// ------------------------------------------------- 指针控制（M4 滚动截图）

/// 在指针当前位置发送滚轮事件（XTest FakeInput，root=0 即指针所在屏，
/// 事件天然落在指针下的窗口上）。clicks > 0 向上，< 0 向下。
pub fn scroll_wheel(clicks: i32) -> Result<()> {
    use x11rb::protocol::xproto::ButtonIndex;
    use x11rb::protocol::xtest::ConnectionExt as _;
    use x11rb::wrapper::ConnectionExt as _;
    const PRESS: u8 = 4; // ButtonPress
    const RELEASE: u8 = 5; // ButtonRelease
    let button: u8 = if clicks > 0 {
        ButtonIndex::M4.into()
    } else {
        ButtonIndex::M5.into()
    };
    with_conn(|conn, _| {
        for _ in 0..clicks.unsigned_abs() {
            conn.xtest_fake_input(PRESS, button, 0, x11rb::NONE, 0, 0, 0)
                .map_err(err)?;
            conn.xtest_fake_input(RELEASE, button, 0, x11rb::NONE, 0, 0, 0)
                .map_err(err)?;
        }
        // FakeInput 是 Void 请求：sync 确保服务器已处理完再返回，
        // 否则紧随其后的截屏可能发生在滚动生效之前
        conn.sync().map_err(err)
    })
}

/// 把指针移动到虚拟桌面坐标 (x, y)。
pub fn warp_pointer(x: i32, y: i32) -> Result<()> {
    use x11rb::wrapper::ConnectionExt as _;
    with_conn(|conn, screen| {
        conn.warp_pointer(x11rb::NONE, screen.root, 0, 0, 0, 0, x as i16, y as i16)
            .map_err(err)?;
        conn.sync().map_err(err)
    })
}

/// 设置指定窗口的 WM_CLASS（instance=class=传入值）。
/// 任务栏靠 WM_CLASS 匹配 .desktop 文件来决定图标；egui/winit 在 X11 下
/// 不设置 WM_CLASS（回落为窗口标题或 argv[0]），导致图标对不上。
/// 独立开连接设置（窗口可能由 winit 用另一连接创建，属性不挑连接）。
pub fn set_window_class(window_id: u32, class: &str) -> Result<()> {
    use x11rb::protocol::xproto::{AtomEnum, PropMode};
    use x11rb::wrapper::ConnectionExt as _;
    with_conn(|conn, _| {
        let atom = AtomEnum::WM_CLASS;
        // X11 WM_CLASS 是 "\0" 分隔的 instance\0class\0，两段同值即可
        let value = format!("{class}\0{class}\0");
        conn.change_property8(
            PropMode::REPLACE,
            window_id,
            atom,
            AtomEnum::STRING,
            value.as_bytes(),
        )
        .map_err(err)?;
        conn.sync().map_err(err)
    })
}
