//! lscreen: 跨平台截图标注工具。
//!
//! 用完即走：进程只在截图/标注期间存活。全局唤起快捷键请在系统/桌面环境中
//! 绑定到 `lscreen` 命令。

mod export;
mod font;
mod ui;

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "lscreen",
    version,
    about = "LaterScreen (ailater.com) - 跨平台截图标注工具（截图/标注/取色/OCR）"
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
        #[arg(long, value_name = "X,Y,W,H")]
        region: Option<String>,
        /// 从图片文件识别（PNG/JPEG），指定时忽略 --region
        #[arg(short, long)]
        input: Option<PathBuf>,
    },
    /// 无界面截屏：立即截取并保存/复制
    Shot {
        /// 截取区域，格式 X,Y,W,H（物理像素）；缺省为整个主屏
        #[arg(long, value_name = "X,Y,W,H")]
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
    let cli = Cli::parse();
    let result = match cli.cmd {
        None | Some(Cmd::Gui) => run_gui(ui::Mode::Snip),
        Some(Cmd::Pick) => run_gui(ui::Mode::Pick),
        Some(Cmd::Qr { region, input }) => run_qr(region, input),
        Some(Cmd::Shot {
            region,
            output,
            clipboard,
        }) => run_shot(region, output, clipboard),
    };
    if let Err(e) = result {
        eprintln!("lscreen: {e}");
        std::process::exit(1);
    }
}

fn run_gui(mode: ui::Mode) -> Result<(), String> {
    // 先截屏再开窗口，避免把自己截进去
    let shot = lscreen_capture::capture_primary().map_err(|e| e.to_string())?;
    let font = font::load_system_font();

    let viewport = eframe::egui::ViewportBuilder::default()
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

fn run_qr(region: Option<String>, input: Option<PathBuf>) -> Result<(), String> {
    let (rgba, w, h) = match input {
        Some(path) => {
            let img = image::open(&path)
                .map_err(|e| format!("无法读取 {}: {e}", path.display()))?
                .into_rgba8();
            let (w, h) = (img.width(), img.height());
            (img.into_raw(), w, h)
        }
        None => {
            let shot = lscreen_capture::capture_primary().map_err(|e| e.to_string())?;
            match region {
                Some(s) => {
                    let r = parse_region(&s)?;
                    export::crop_rgba(&shot.rgba, shot.width, shot.height, r)
                }
                None => (shot.rgba, shot.width, shot.height),
            }
        }
    };
    let found = lscreen_core::qr::detect(&rgba, w, h);
    if found.is_empty() {
        return Err("未识别到二维码".into());
    }
    for r in found {
        println!("{}", r.content);
    }
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
    let (rgba, w, h) = export::compose(&renderer, &shot.rgba, shot.width, shot.height, &[], region);

    if clipboard {
        export::copy_to_clipboard(&rgba, w, h)?;
    }
    if output.is_some() || !clipboard {
        let path = output.unwrap_or_else(export::default_save_path);
        export::save_png(&rgba, w, h, &path)?;
        println!("{}", path.display());
    }
    Ok(())
}

fn parse_region(s: &str) -> Result<lscreen_core::RectF, String> {
    let parts: Vec<f32> = s
        .split(',')
        .map(|v| v.trim().parse::<f32>())
        .collect::<Result<_, _>>()
        .map_err(|_| format!("无效的区域参数: {s}（应为 X,Y,W,H）"))?;
    if parts.len() != 4 || parts[2] <= 0.0 || parts[3] <= 0.0 {
        return Err(format!("无效的区域参数: {s}（应为 X,Y,W,H）"));
    }
    Ok(lscreen_core::RectF::from_points(
        lscreen_core::P2::new(parts[0], parts[1]),
        lscreen_core::P2::new(parts[0] + parts[2], parts[1] + parts[3]),
    ))
}
