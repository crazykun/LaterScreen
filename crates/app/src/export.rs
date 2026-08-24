//! 导出：图元合成 → PNG 文件 / 系统剪贴板。

use lscreen_core::render::Renderer;
use lscreen_core::{Element, RectF};
use std::borrow::Cow;
use std::path::{Path, PathBuf};

/// 从整幅 RGBA 中裁剪出 region（图像物理像素坐标）。
///
/// 空选区返回 `None` 而不是整幅图：退化选区静默变成全屏截图会把选区外的
/// 内容泄漏到剪贴板/文件里，必须让调用方显式提示。
pub fn crop_rgba(rgba: &[u8], w: u32, h: u32, region: RectF) -> Option<(Vec<u8>, u32, u32)> {
    let x0 = (region.min.x.round().max(0.0) as u32).min(w);
    let y0 = (region.min.y.round().max(0.0) as u32).min(h);
    let x1 = (region.max.x.round().max(0.0) as u32).max(x0).min(w);
    let y1 = (region.max.y.round().max(0.0) as u32).max(y0).min(h);
    let (cw, ch) = (x1 - x0, y1 - y0);
    if cw == 0 || ch == 0 {
        return None;
    }
    let mut out = Vec::with_capacity((cw as usize) * (ch as usize) * 4);
    for y in y0..y1 {
        let start = ((y as usize) * (w as usize) + x0 as usize) * 4;
        out.extend_from_slice(&rgba[start..start + (cw as usize) * 4]);
    }
    Some((out, cw, ch))
}

/// 合成并裁剪出选区的 RGBA。region 为图像物理像素坐标。
pub fn compose(
    renderer: &Renderer,
    rgba: &[u8],
    w: u32,
    h: u32,
    elements: &[Element],
    region: RectF,
) -> Option<(Vec<u8>, u32, u32)> {
    let full = renderer.render(rgba, w, h, elements);
    crop_rgba(&full, w, h, region)
}

/// 默认保存路径（读取用户配置）：目录来自 config.save_dir（空则 ~/Pictures
/// 存在时/否则家目录），文件名由模板渲染；同秒内多次保存自动追加序号，
/// 不覆盖既有文件。ext 为扩展名（"png" / "gif"）：查重针对最终写入的文件名，
/// 先查 png 再改后缀会让 GIF 路径的防覆盖检查失效。
pub fn default_save_path(ext: &str) -> PathBuf {
    save_path(&crate::config::Config::load(), ext)
}

/// 按配置生成保存路径。
pub fn save_path(cfg: &crate::config::Config, ext: &str) -> PathBuf {
    let name = crate::config::render_template(&cfg.filename_template, save_time());
    let dir = cfg.save_dir_override().unwrap_or_else(default_dir);
    let mut path = dir.join(format!("{name}.{ext}"));
    let mut n = 1;
    while path.exists() {
        path = dir.join(format!("{name}_{n}.{ext}"));
        n += 1;
    }
    path
}

/// 默认保存目录：~/Pictures（存在时）或家目录。
pub fn default_dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from);
    match &home {
        Some(home) if home.join("Pictures").is_dir() => home.join("Pictures"),
        Some(home) => home.clone(),
        None => PathBuf::from("."),
    }
}

/// 当前本地时间 (年,月,日,时,分,秒)。
fn save_time() -> (i64, u32, u32, u32, u32, u32) {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0) as i64;
    local_civil(secs)
}

/// unix 秒 → 本地时间 (年,月,日,时,分,秒)。
/// unix 平台走 libc localtime_r；其余平台退化为 UTC（civil-from-days 算法）。
fn local_civil(secs: i64) -> (i64, u32, u32, u32, u32, u32) {
    #[cfg(unix)]
    {
        let t = secs as libc::time_t;
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        if !unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
            return (
                tm.tm_year as i64 + 1900,
                tm.tm_mon as u32 + 1,
                tm.tm_mday as u32,
                tm.tm_hour as u32,
                tm.tm_min as u32,
                tm.tm_sec as u32,
            );
        }
    }
    utc_civil(secs)
}

/// UTC 的 civil-from-days（Howard Hinnant 算法）。
fn utc_civil(secs: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let (h, mi, s) = (rem / 3600, rem % 3600 / 60, rem % 60);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m as u32, d as u32, h as u32, mi as u32, s as u32)
}

/// 保存 PNG。无扩展名时补 `.png`；其他扩展名直接报错——
/// `image::save` 按扩展名猜格式，RGBA→JPEG 之类的失败信息非常费解。
/// 父目录不存在时自动创建（自定义保存目录首次使用）。
/// 返回实际写入的路径（可能补过扩展名）。
pub fn save_png(rgba: &[u8], w: u32, h: u32, path: &Path) -> Result<PathBuf, String> {
    let img = image::RgbaImage::from_raw(w, h, rgba.to_vec())
        .ok_or_else(|| "invalid image buffer".to_string())?;
    let mut out = path.to_path_buf();
    if out.extension().is_none() {
        out.set_extension("png");
    }
    if !out
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("png"))
    {
        return Err(format!("仅支持 PNG 输出: {}", out.display()));
    }
    if let Some(dir) = out.parent() {
        if !dir.as_os_str().is_empty() && !dir.is_dir() {
            std::fs::create_dir_all(dir).map_err(|e| format!("创建目录失败: {e}"))?;
        }
    }
    img.save_with_format(&out, image::ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    Ok(out)
}

/// 用系统默认程序打开文件（录屏点击播放、图片等按默认应用打开）。
/// 尽力而为，失败不报错。与 `open_in_file_manager` 的区别是这里把
/// 文件本身交给默认处理程序（如视频→播放器），而非只打开所在目录。
///
/// Linux 下对录屏产物（gif/mp4）优先用真正的视频播放器：`xdg-open` 把 GIF
/// 按 MIME 交给图像查看器，看到的只有首帧,和缩略图一模一样；先试常见
/// 播放器，失败再退回 xdg-open。
pub fn open_with_default(path: &Path) {
    #[cfg(target_os = "linux")]
    {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if matches!(ext.as_str(), "gif" | "mp4" | "webm" | "mkv") {
            for player in ["mpv", "vlc", "celluloid"] {
                if let Ok(mut c) = std::process::Command::new(player)
                    .arg(path)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                {
                    std::thread::spawn(move || {
                        let _ = c.wait();
                    });
                    return;
                }
            }
            // 常见播放器都没有：退回系统默认（Deepin 对 GIF 仍会开图片查看器，
            // 但至少尽力）
            let _ = std::process::Command::new("xdg-open")
                .arg(path)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn();
            return;
        }
        // 非视频（图片等）：直接 xdg-open
        let _ = std::process::Command::new("xdg-open")
            .arg(path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
    #[cfg(not(target_os = "linux"))]
    {
        let prog = if cfg!(target_os = "windows") {
            "explorer"
        } else {
            "open"
        };
        let _ = std::process::Command::new(prog)
            .arg(path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
}

/// 在文件管理器中打开文件所在目录（尽力而为，失败不报错）。
/// 平台命令：xdg-open / explorer / open；打开目录而非定位文件——
/// 定位接口三平台不一致（org.freedesktop.FileManager2 等），保持简单。
pub fn open_in_file_manager(dir: &Path) {
    let prog = if cfg!(target_os = "windows") {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let _ = std::process::Command::new(prog)
        .arg(dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

/// 在文件管理器中定位并选中某个文件（截图历史「打开目录并选中」用）。
/// 三平台命令：Windows `explorer /select`、macOS `open -R`、
/// Linux `org.freedesktop.FileManager1` D-Bus `ShowItems`（失败/无服务时
/// 降级为只开父目录，不定位）。
pub fn open_and_select(path: &Path) {
    if !path.exists() {
        // 文件已不存在：退化为打开父目录（若目录还在）
        if let Some(dir) = path.parent() {
            if dir.is_dir() {
                open_in_file_manager(dir);
            }
        }
        return;
    }
    let abspath = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("explorer")
            .arg(format!("/select,{}", abspath.display()))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open")
            .arg("-R")
            .arg(&abspath)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        // FileManager1 ShowItems 失败（无服务/无实现）时降级为只开父目录
        if !show_items_dbus(&abspath) {
            if let Some(dir) = abspath.parent() {
                if dir.is_dir() {
                    open_in_file_manager(dir);
                }
            }
        }
    }
}

/// Linux：`org.freedesktop.FileManager1` D-Bus `ShowItems` 定位文件。
/// 返回 false 表示未能定位（调用方降级为只开目录）。
#[cfg(all(unix, not(target_os = "macos")))]
fn show_items_dbus(path: &Path) -> bool {
    let Ok(conn) = zbus::blocking::Connection::session() else {
        return false;
    };
    let Ok(proxy) = zbus::blocking::Proxy::new(
        &conn,
        "org.freedesktop.FileManager1",
        "/org/freedesktop/FileManager1",
        "org.freedesktop.FileManager1",
    ) else {
        return false;
    };
    let uri = format!("file://{}", path.display());
    let result: Result<(), zbus::Error> = proxy.call("ShowItems", &(vec![uri], String::new()));
    result.is_ok()
}

pub fn copy_text_to_clipboard(text: &str) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        clipd_spawn(&format!("txt\n{text}").into_bytes())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let mut cb = arboard::Clipboard::new().map_err(|e| e.to_string())?;
        cb.set_text(text).map_err(|e| e.to_string())
    }
}

pub fn copy_to_clipboard(rgba: &[u8], w: u32, h: u32) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        let mut payload = format!("img {w} {h}\n").into_bytes();
        payload.extend_from_slice(rgba);
        clipd_spawn(&payload)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let mut cb = arboard::Clipboard::new().map_err(|e| e.to_string())?;
        cb.set_image(arboard::ImageData {
            width: w as usize,
            height: h as usize,
            bytes: Cow::Borrowed(rgba),
        })
        .map_err(|e| e.to_string())
    }
}

/// X11 剪贴板数据随所有者进程退出而消失。方案：用自身可执行文件拉起一个
/// 分离的守护子进程持有剪贴板（arboard wait()），内容被其他应用覆盖后自动退出。
/// 主进程写完数据立即返回，不必等剪贴板被接管。
#[cfg(target_os = "linux")]
fn clipd_spawn(payload: &[u8]) -> Result<(), String> {
    use std::io::{Read, Write};
    use std::process::{Command, Stdio};

    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut child = Command::new(exe)
        .arg(CLIPD_ARG)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("启动剪贴板守护进程失败: {e}"))?;
    child
        .stdin
        .take()
        .ok_or_else(|| "无法写入守护进程".to_string())?
        .write_all(payload)
        .map_err(|e| e.to_string())?;
    // stdin 已随上面的 take() 作用域结束关闭（EOF），子进程开始处理。
    // 等确认字节：子进程拿到剪贴板前若失败会直接退出 → stdout EOF，
    // 这里读到 0 字节即失败，不再出现「提示已复制但剪贴板是空的」。
    let mut ack = [0u8; 1];
    let got = child
        .stdout
        .take()
        .ok_or_else(|| "无法读取守护进程确认".to_string())?
        .read(&mut ack)
        .map_err(|e| e.to_string())?;
    if got == 0 || ack[0] != CLIPD_ACK {
        let _ = child.wait(); // 已退出，顺手收割
        return Err("剪贴板守护进程启动失败（X 连接不可用？）".into());
    }
    // 守护进程被覆盖退出后需要有人 wait 收割，否则 GUI 长会话中积累僵尸。
    // 用分离线程等它；若本进程先退出，子进程被 init 收养，同样无僵尸。
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// 隐藏子命令名：`lscreen __clipd`
#[cfg(target_os = "linux")]
pub const CLIPD_ARG: &str = "__clipd";
/// 守护进程持有剪贴板前回写的确认字节。
#[cfg(target_os = "linux")]
const CLIPD_ACK: u8 = b'k';

/// 守护进程入口：stdin 协议为一行头部 + 数据体。
/// `txt\n<utf8>` 或 `img W H\n<raw rgba>`
#[cfg(target_os = "linux")]
pub fn clipd_main() -> Result<(), String> {
    use arboard::SetExtLinux;
    use std::io::Read;

    let mut input = Vec::new();
    std::io::stdin()
        .read_to_end(&mut input)
        .map_err(|e| e.to_string())?;
    let nl = input
        .iter()
        .position(|&b| b == b'\n')
        .ok_or_else(|| "缺少协议头".to_string())?;
    let header = String::from_utf8_lossy(&input[..nl]).to_string();
    let body = &input[nl + 1..];

    // 先做完所有可失败的校验，再回执、再进入阻塞持有
    enum Payload<'a> {
        Text(String),
        Img { w: usize, h: usize, body: &'a [u8] },
    }
    let payload = if header == "txt" {
        Payload::Text(String::from_utf8_lossy(body).to_string())
    } else if let Some(dims) = header.strip_prefix("img ") {
        let mut it = dims.split_whitespace();
        let (w, h): (usize, usize) = match (
            it.next().and_then(|v| v.parse().ok()),
            it.next().and_then(|v| v.parse().ok()),
        ) {
            (Some(w), Some(h)) => (w, h),
            _ => return Err("协议头尺寸无效".into()),
        };
        if body.len() != w * h * 4 {
            return Err("图像数据长度不符".into());
        }
        Payload::Img { w, h, body }
    } else {
        return Err(format!("未知协议头: {header}"));
    };

    let mut cb = arboard::Clipboard::new().map_err(|e| e.to_string())?;

    // set().wait() 会阻塞到剪贴板被其他应用覆盖，无法在成功后再回写，
    // 因此确认点放在最后一个可失败步骤（X 连接 + 协议校验）之后、阻塞持有之前。
    {
        use std::io::Write;
        let mut out = std::io::stdout();
        out.write_all(&[CLIPD_ACK]).map_err(|e| e.to_string())?;
        out.flush().map_err(|e| e.to_string())?;
    }

    match payload {
        Payload::Text(text) => cb.set().wait().text(text).map_err(|e| e.to_string()),
        Payload::Img { w, h, body } => cb
            .set()
            .wait()
            .image(arboard::ImageData {
                width: w,
                height: h,
                bytes: Cow::Borrowed(body),
            })
            .map_err(|e| e.to_string()),
    }
}
