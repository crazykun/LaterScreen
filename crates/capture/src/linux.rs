//! Linux X11 截屏：x11rb 纯 Rust 实现。
//! Wayland 会话暂不支持（M5 走 xdg-desktop-portal），检测到时给出明确报错。

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

fn ensure_x11() -> Result<()> {
    // 会话类型为 wayland，或无 DISPLAY 的纯 Wayland 会话（XDG_SESSION_TYPE
    // 并不总是被设置）都直接给明确报错，而不是让 x11rb 报底层连接错误
    let session_wayland = std::env::var("XDG_SESSION_TYPE").is_ok_and(|t| t == "wayland");
    let no_x11 = std::env::var_os("DISPLAY").is_none();
    if session_wayland || (no_x11 && std::env::var_os("WAYLAND_DISPLAY").is_some()) {
        return Err(CaptureError(
            "Wayland 会话暂不支持（X11 抓屏抓不到原生 Wayland 窗口），敬请期待".into(),
        ));
    }
    Ok(())
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
    let lsb = conn.setup().image_byte_order == ImageOrder::LSB_FIRST;
    let mut rgba = vec![0u8; expected];
    for (dst, src) in rgba.chunks_exact_mut(4).zip(data.chunks_exact(4)) {
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
