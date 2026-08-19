//! lscreen: 跨平台截图标注工具。
//!
//! 当前形态：单次调用，进程只在截图/标注期间存活。全局唤起快捷键请在系统/桌面
//! 环境中绑定到 `lscreen` 命令；托盘常驻模式见 `doc/PLAN.md` M8。

mod export;
mod font;
mod pin;
mod ui;

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "lscreen",
    version,
    about = "LaterScreen - 跨平台截图标注工具（截图/标注/取色/OCR）"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// 交互式截图（默认行为）
    Gui,
    /// 屏幕取色器：放大镜取景，单击复制 HEX，Ctrl+R/H/K 复制 RGB/HEX/CMYK
    Pick,
    /// 识别二维码：从屏幕区域或图片文件
    Qr {
        /// 识别区域，格式 X,Y,W,H（物理像素）；缺省为整个主屏
        #[arg(long, value_name = "X,Y,W,H", allow_hyphen_values = true)]
        region: Option<String>,
        /// 从图片文件识别（PNG/JPEG），指定时忽略 --region
        #[arg(short, long)]
        input: Option<PathBuf>,
    },
    /// 文字识别（OCR）：从屏幕区域或图片文件，结果输出到 stdout
    Ocr {
        /// 识别区域，格式 X,Y,W,H（物理像素）；缺省为整个主屏
        #[arg(long, value_name = "X,Y,W,H", allow_hyphen_values = true)]
        region: Option<String>,
        /// 从图片文件识别（PNG/JPEG），指定时忽略 --region
        #[arg(short, long)]
        input: Option<PathBuf>,
        /// 识别语言（可多次指定），如 --lang chi_sim --lang eng
        #[arg(long = "lang")]
        languages: Vec<String>,
    },
    /// 录制屏幕为 GIF（Ctrl+C 或时长到达后停止）
    Record {
        /// 录制区域，格式 X,Y,W,H（物理像素）；缺省为整个主屏
        #[arg(long, value_name = "X,Y,W,H", allow_hyphen_values = true)]
        region: Option<String>,
        /// 最长录制时长（秒）
        #[arg(long, default_value_t = 30.0)]
        duration: f32,
        /// 帧率 1-30
        #[arg(long, default_value_t = 10)]
        fps: u32,
        /// 编码质量 1-100
        #[arg(long, default_value_t = 90)]
        quality: u8,
        /// 输出文件路径（.gif）；缺省输出到 ~/Pictures
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// 贴图：把图片钉在屏幕上置顶悬浮（独立进程，支持多个并存）
    Pin {
        /// 图片文件路径（PNG）；缺省从 stdin 读 PNG（覆盖层 spawn 时走此通道）
        #[arg(short, long)]
        input: Option<PathBuf>,
        /// 窗口左上角屏幕坐标（逻辑点），格式 X,Y
        #[arg(long, value_name = "X,Y", allow_hyphen_values = true)]
        pos: Option<String>,
        /// 屏幕缩放比（物理像素/逻辑点），用于换算窗口初始尺寸；缺省 1.0
        #[arg(long, default_value_t = 1.0, allow_hyphen_values = true)]
        scale: f32,
    },
    /// 无界面截屏：立即截取并保存/复制
    Shot {
        /// 截取区域，格式 X,Y,W,H（物理像素）；缺省为整个主屏
        #[arg(long, value_name = "X,Y,W,H", allow_hyphen_values = true)]
        region: Option<String>,
        /// 输出文件路径（PNG）；缺省输出到 ~/Pictures
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// 同时复制到剪贴板
        #[arg(short, long)]
        clipboard: bool,
    },
}

fn main() {
    // 剪贴板守护进程（内部机制，见 export::clipd_main）：在 clap 之前拦截，
    // 不出现在 --help 中
    #[cfg(target_os = "linux")]
    if std::env::args_os()
        .nth(1)
        .is_some_and(|a| a.as_os_str() == std::ffi::OsStr::new(export::CLIPD_ARG))
    {
        if let Err(e) = export::clipd_main() {
            eprintln!("lscreen clipd: {e}");
            std::process::exit(1);
        }
        return;
    }

    let cli = Cli::parse();
    let result = match cli.cmd {
        None | Some(Cmd::Gui) => run_gui(ui::Mode::Snip),
        Some(Cmd::Pick) => run_gui(ui::Mode::Pick),
        Some(Cmd::Qr { region, input }) => run_qr(region, input),
        Some(Cmd::Ocr {
            region,
            input,
            languages,
        }) => run_ocr(region, input, languages),
        Some(Cmd::Record {
            region,
            duration,
            fps,
            quality,
            output,
        }) => run_record(region, duration, fps, quality, output),
        Some(Cmd::Shot {
            region,
            output,
            clipboard,
        }) => run_shot(region, output, clipboard),
        Some(Cmd::Pin { input, pos, scale }) => run_pin(input, pos, scale),
    };
    if let Err(e) = result {
        eprintln!("lscreen: {e}");
        std::process::exit(1);
    }
}

fn run_gui(mode: ui::Mode) -> Result<(), String> {
    // 先截屏再开窗口，避免把自己截进去。
    // 多显示器：截鼠标所在屏，并把覆盖层窗口钉到同一块屏（指针查询不可用时回退主屏）。
    let shot = match lscreen_capture::cursor_position() {
        Some((x, y)) => {
            lscreen_capture::capture_at(x, y).or_else(|_| lscreen_capture::capture_primary())
        }
        None => lscreen_capture::capture_primary(),
    }
    .map_err(|e| e.to_string())?;
    let font = font::load_system_font();

    // origin 为物理像素；X11 下 winit 逻辑坐标与物理一致（scale=1），
    // 其余平台按截屏缩放比换算
    let scale = if shot.scale > 0.0 { shot.scale } else { 1.0 };
    let pos = eframe::egui::Pos2::new(shot.origin.0 as f32 / scale, shot.origin.1 as f32 / scale);
    let size = eframe::egui::Vec2::new(shot.width as f32 / scale, shot.height as f32 / scale);
    let viewport = eframe::egui::ViewportBuilder::default()
        .with_position(pos)
        .with_inner_size(size)
        .with_fullscreen(true)
        .with_decorations(false)
        .with_always_on_top();
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "lscreen",
        options,
        Box::new(move |cc| Ok(Box::new(ui::SnipApp::new(cc, shot, font, mode)))),
    )
    .map_err(|e| e.to_string())
}

/// 无头模式的图像来源：文件优先，否则截屏（可选区域裁剪）。
fn acquire_image(
    region: Option<String>,
    input: Option<PathBuf>,
) -> Result<(Vec<u8>, u32, u32), String> {
    match input {
        Some(path) => {
            let img = image::open(&path)
                .map_err(|e| format!("无法读取 {}: {e}", path.display()))?
                .into_rgba8();
            let (w, h) = (img.width(), img.height());
            Ok((img.into_raw(), w, h))
        }
        None => {
            let shot = lscreen_capture::capture_primary().map_err(|e| e.to_string())?;
            match region {
                Some(s) => {
                    let r = parse_region(&s)?;
                    export::crop_rgba(&shot.rgba, shot.width, shot.height, r)
                        .ok_or_else(|| "选区为空（region 与屏幕无交集）".to_string())
                }
                None => Ok((shot.rgba, shot.width, shot.height)),
            }
        }
    }
}

fn run_qr(region: Option<String>, input: Option<PathBuf>) -> Result<(), String> {
    let (rgba, w, h) = acquire_image(region, input)?;
    let found = lscreen_core::qr::detect(&rgba, w, h);
    if found.is_empty() {
        return Err("未识别到二维码".into());
    }
    for r in found {
        println!("{}", r.content);
    }
    Ok(())
}

fn run_ocr(
    region: Option<String>,
    input: Option<PathBuf>,
    languages: Vec<String>,
) -> Result<(), String> {
    let engine = lscreen_ocr::default_engine(&languages);
    if !engine.available() {
        return Err(engine.describe());
    }
    let (rgba, w, h) = acquire_image(region, input)?;
    let out = engine.recognize(&rgba, w, h).map_err(|e| e.to_string())?;
    if out.is_empty() {
        return Err("未识别到文字".into());
    }
    println!("{}", out.plain_text());
    Ok(())
}

fn run_shot(
    region: Option<String>,
    output: Option<PathBuf>,
    clipboard: bool,
) -> Result<(), String> {
    let shot = lscreen_capture::capture_primary().map_err(|e| e.to_string())?;
    let region = match region {
        Some(s) => parse_region(&s)?,
        None => lscreen_core::RectF::from_points(
            lscreen_core::P2::new(0.0, 0.0),
            lscreen_core::P2::new(shot.width as f32, shot.height as f32),
        ),
    };
    let renderer = lscreen_core::render::Renderer::new(None);
    let (rgba, w, h) = export::compose(&renderer, &shot.rgba, shot.width, shot.height, &[], region)
        .ok_or_else(|| "选区为空（region 与屏幕无交集）".to_string())?;

    if clipboard {
        export::copy_to_clipboard(&rgba, w, h)?;
    }
    if output.is_some() || !clipboard {
        let path = output.unwrap_or_else(|| export::default_save_path("png"));
        let saved = export::save_png(&rgba, w, h, &path)?;
        println!("{}", saved.display());
    }
    Ok(())
}

fn run_record(
    region: Option<String>,
    duration: f32,
    fps: u32,
    quality: u8,
    output: Option<PathBuf>,
) -> Result<(), String> {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    // 参数校验放在截屏之前：无头环境也能快速报错
    // NaN 会穿过 clamp，而 from_secs_f32 对 NaN/超范围值 panic
    if !duration.is_finite() {
        return Err("无效的 --duration（应为有限秒数）".into());
    }
    let secs = duration.clamp(0.1, 86_400.0);
    // 提前校验而不是静默 clamp：用户给 60fps 却录出 30fps 会非常困惑
    if !(1..=30).contains(&fps) {
        return Err("无效的 --fps（应为 1-30）".into());
    }
    if !(1..=100).contains(&quality) {
        return Err("无效的 --quality（应为 1-100）".into());
    }

    // 录制区域：指定值或整个主屏
    let (x, y, w, h) = match region {
        Some(s) => {
            let r = parse_region(&s)?;
            (
                r.min.x as i32,
                r.min.y as i32,
                r.width() as u32,
                r.height() as u32,
            )
        }
        None => {
            let shot = lscreen_capture::capture_primary().map_err(|e| e.to_string())?;
            (shot.origin.0, shot.origin.1, shot.width, shot.height)
        }
    };

    let path = output.unwrap_or_else(|| export::default_save_path("gif"));
    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = stop.clone();
        ctrlc::set_handler(move || stop.store(true, Ordering::Relaxed))
            .map_err(|e| e.to_string())?;
    }

    eprintln!("录制中 {w}x{h}@{fps}fps，Ctrl+C 停止（最长 {secs} 秒）…");
    let frames = lscreen_record::record_gif(
        || {
            lscreen_capture::capture_region(x, y, w, h)
                .map(|s| (s.rgba, s.width, s.height))
                .map_err(|e| lscreen_record::RecordError(e.to_string()))
        },
        &lscreen_record::GifOptions { fps, quality },
        std::time::Duration::from_secs_f32(secs),
        &stop,
        &path,
    )
    .map_err(|e| e.to_string())?;
    eprintln!("已录制 {frames} 帧");
    println!("{}", path.display());
    Ok(())
}

/// 贴图入口：读图（文件或 stdin）→ 置顶无边框窗口。
fn run_pin(input: Option<PathBuf>, pos: Option<String>, scale: f32) -> Result<(), String> {
    // 参数校验前置：避免交互式调用在 stdin 上阻塞等待 EOF
    if !scale.is_finite() || scale <= 0.0 {
        return Err("无效的 --scale（应为正数）".into());
    }
    let pos = match pos {
        Some(s) => parse_pos(&s)?,
        None => eframe::egui::Pos2::new(80.0, 80.0),
    };
    if input.is_none() && is_stdin_tty() {
        return Err("缺少图片：用 -i 指定 PNG 文件，或经管道传入（覆盖层自动走此通道）".into());
    }
    let img = match input {
        Some(path) => image::open(&path)
            .map_err(|e| format!("无法读取 {}: {e}", path.display()))?
            .into_rgba8(),
        None => {
            // stdin 由父进程写完才退出，读到的就是完整 PNG
            let mut buf = Vec::new();
            std::io::Read::read_to_end(&mut std::io::stdin(), &mut buf)
                .map_err(|e| format!("读取 stdin 失败: {e}"))?;
            image::load_from_memory(&buf)
                .map_err(|e| format!("stdin 不是有效图片: {e}（贴图仅支持 PNG）"))?
                .into_rgba8()
        }
    };
    let (w, h) = (img.width(), img.height());
    let rgba = img.into_raw();
    if w == 0 || h == 0 {
        return Err("空图片".into());
    }
    let viewport = eframe::egui::ViewportBuilder::default()
        .with_position(pos)
        // 窗口高度含底部工具条条带（pin::BAR_H）
        .with_inner_size(pin::window_size(w, h, scale))
        .with_decorations(false)
        .with_always_on_top()
        .with_resizable(false);
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "lscreen-pin",
        options,
        Box::new(move |cc| {
            let font = font::load_system_font();
            Ok(Box::new(pin::PinApp::new(cc, rgba, w, h, scale, font)))
        }),
    )
    .map_err(|e| e.to_string())
}

/// stdin 是否为交互终端（tty）：是则没有管道数据可读，直接报错而非阻塞。
/// 标准库 IsTerminal 三平台可用——cfg(unix) + libc 版本会让 Windows
/// 恒返回 false，控制台直接敲 `lscreen pin` 就挂在等 stdin EOF 上。
fn is_stdin_tty() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal()
}

fn parse_pos(s: &str) -> Result<eframe::egui::Pos2, String> {
    let parts: Vec<f32> = s
        .split(',')
        .map(|v| v.trim().parse::<f32>())
        .collect::<Result<_, _>>()
        .map_err(|_| format!("无效的坐标: {s}（应为 X,Y）"))?;
    if parts.len() != 2 || !parts.iter().all(|v| v.is_finite()) {
        return Err(format!("无效的坐标: {s}（应为 X,Y）"));
    }
    Ok(eframe::egui::Pos2::new(parts[0], parts[1]))
}

fn parse_region(s: &str) -> Result<lscreen_core::RectF, String> {
    let parts: Vec<f32> = s
        .split(',')
        .map(|v| v.trim().parse::<f32>())
        .collect::<Result<_, _>>()
        .map_err(|_| format!("无效的区域参数: {s}（应为 X,Y,W,H）"))?;
    // NaN 能通过 `<= 0.0` 判定（任何比较都为假），必须显式挡掉
    if parts.len() != 4
        || !parts.iter().all(|v| v.is_finite())
        || parts[2] <= 0.0
        || parts[3] <= 0.0
    {
        return Err(format!("无效的区域参数: {s}（应为 X,Y,W,H）"));
    }
    Ok(lscreen_core::RectF::from_points(
        lscreen_core::P2::new(parts[0], parts[1]),
        lscreen_core::P2::new(parts[0] + parts[2], parts[1] + parts[3]),
    ))
}

#[cfg(test)]
mod tests {
    use super::parse_pos;

    #[test]
    fn pos_parsing() {
        let p = parse_pos("100,200").unwrap();
        assert_eq!((p.x, p.y), (100.0, 200.0));
        // 负坐标（显示器在主屏左侧）
        let p = parse_pos("-1920,0").unwrap();
        assert_eq!((p.x, p.y), (-1920.0, 0.0));
        assert!(parse_pos("abc").is_err());
        assert!(parse_pos("1,2,3").is_err());
        assert!(parse_pos("nan,2").is_err());
    }
}
