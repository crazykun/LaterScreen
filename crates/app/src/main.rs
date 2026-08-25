//! lscreen: 跨平台截图标注工具。
//!
//! 默认行为：无参数启动 = 静默驻留后台的托盘进程（截图等动作由全局热键
//! 或托盘菜单按需唤起，子进程用完即退）。`lscreen gui` 直接进交互截图；
//! 其余子命令单次调用即起即退。

// Windows 用 GUI 子系统：双击/快捷方式/托盘 spawn 子进程都不再弹控制台黑框。
// CLI 场景由 main() 里的 attach_parent_console 兜底。
#![cfg_attr(windows, windows_subsystem = "windows")]

mod config;
mod export;
mod font;
mod history;
mod pin;
mod record_ui;
mod settings_ui;
mod tray;
mod ui;

use clap::{Parser, Subcommand};
use std::path::PathBuf;
use ui::SharedRegion;

/// 让任务栏/启动器把本窗口关联到 lscreen.desktop（决定显示的图标）。
///
/// 根因：egui 的 `with_app_id` 只在 Wayland 生效；X11 下 winit 不设 WM_CLASS，
/// 回落为窗口标题或 argv[0]，任务栏匹配不到 .desktop 文件就用了别的图标
/// （实测显示成了启动来源的 VS Code 图标）。这里从原始窗口句柄取出 X11
/// window id，显式把 WM_CLASS 设为 "lscreen"，与 .desktop 的 StartupWMClass 对齐。
#[cfg(target_os = "linux")]
pub(crate) fn apply_window_class(cc: &eframe::CreationContext<'_>) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    if let Ok(handle) = cc.window_handle() {
        if let RawWindowHandle::Xlib(h) = handle.as_raw() {
            let wid = h.window as u32;
            // 失败不致命：图标归属只是体验问题，不影响功能
            let _ = lscreen_capture::set_window_class(wid, "lscreen");
            // _NET_WM_ICON 直接给任务栏/alt-tab 图标，不依赖 .desktop 缓存
            if let Some(icon) = tray::window_icon() {
                let _ = lscreen_capture::set_window_icon(
                    wid,
                    icon.as_raw(),
                    icon.width(),
                    icon.height(),
                );
            }
        }
    }
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn apply_window_class(_cc: &eframe::CreationContext<'_>) {}

#[derive(Parser)]
#[command(
    name = "lscreen",
    version,
    about = "LaterScreen - 跨平台截图标注工具（截图/标注/取色/OCR/录屏/贴图/托盘）"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// 常驻托盘（默认行为）：后台常驻，热键/菜单随时唤起各功能
    Tray {
        /// 前台运行（调试/自启动用；默认分离到后台，终端立即返回）
        #[arg(long)]
        foreground: bool,
    },
    /// 交互式截图（立即打开截图覆盖层）
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
    /// 录制屏幕为 GIF/MP4（Ctrl+C 或时长到达后停止）
    Record {
        /// 录制区域，格式 X,Y,W,H（物理像素）；缺省为整个主屏
        #[arg(long, value_name = "X,Y,W,H", allow_hyphen_values = true)]
        region: Option<String>,
        /// 先交互框选录制区域（框完立即开始录制）
        #[arg(long)]
        select: bool,
        /// 编码为 MP4/H.264（缺省 GIF；MP4 目前 Linux 可用）
        #[arg(long)]
        mp4: bool,
        /// 最长录制时长（秒）
        #[arg(long, default_value_t = 30.0)]
        duration: f32,
        /// 帧率 1-30
        #[arg(long, default_value_t = 10)]
        fps: u32,
        /// 编码质量 1-100（GIF，缺省 90）；MP4 为目标码率 kbps 200-50000（缺省 4000）
        #[arg(long)]
        quality: Option<u64>,
        /// 输出文件路径（.gif/.mp4）；缺省输出到 ~/Pictures
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
    /// 滚动长截图：框选区域后自动滚动内容并拼接为长图（Linux X11）
    Scroll {
        /// 截取区域，格式 X,Y,W,H（物理像素）；缺省为交互框选
        #[arg(long, value_name = "X,Y,W,H", allow_hyphen_values = true)]
        region: Option<String>,
        /// 最大滚动步数（每步滚动一次滚轮）
        #[arg(long, default_value_t = 60)]
        steps: u32,
        /// 每步滚轮格数
        #[arg(long, default_value_t = 2)]
        clicks: u32,
        /// 每步等待内容稳定的毫秒数
        #[arg(long, default_value_t = 200)]
        pause_ms: u64,
        /// 输出文件路径（PNG）；缺省 = 拼接后打开预览窗口（保存/复制/贴图）
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// 无界面截屏：立即截取并保存/复制
    Shot {
        /// 截取区域，格式 X,Y,W,H（物理像素）；缺省为整个主屏
        #[arg(long, value_name = "X,Y,W,H", allow_hyphen_values = true)]
        region: Option<String>,
        /// 截取当前最前（活跃）窗口
        #[arg(long, conflicts_with_all = ["region", "window_at"])]
        window: bool,
        /// 截取虚拟桌面坐标 (X,Y) 处最上面的窗口
        #[arg(
            long,
            value_name = "X,Y",
            allow_hyphen_values = true,
            conflicts_with = "region"
        )]
        window_at: Option<String>,
        /// 输出文件路径（PNG）；缺省输出到 ~/Pictures
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// 同时复制到剪贴板
        #[arg(short, long)]
        clipboard: bool,
    },
    /// 打开配置面板（保存后托盘进程自动热加载）
    Config,
    /// 打开截图历史面板（最近截图/贴图/录屏，缩略图网格）
    History,
    /// 标注预览：把图片开进带完整工具栏的标注窗口（内部命令，滚动截图
    /// 拼接结果的预览走它，--help 不显示）
    #[command(hide = true)]
    Annotate {
        /// 图片文件路径（PNG）；缺省从 stdin 读 PNG
        #[arg(short, long)]
        input: Option<PathBuf>,
    },
}

/// 是否附上了父控制台（从 cmd 启动）。决定致命错误走 stderr 还是弹窗。
#[cfg(windows)]
static HAS_CONSOLE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(windows)]
fn win_message_box(msg: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
    let wide = |s: &str| -> Vec<u16> { s.encode_utf16().chain(std::iter::once(0)).collect() };
    let (text, title) = (wide(msg), wide("LaterScreen 启动失败"));
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONERROR,
        );
    }
}

/// 致命错误上报。GUI 子系统下 stderr 没有接收方——双击启动失败的表现就是
/// 「转圈一下什么都没发生」，用户和开发者都无从下手（v0.5.1 的实际反馈）。
/// 有控制台（cmd 启动）时只走 stderr，保持 CLI 的管道与重定向语义；
/// 没有控制台时弹系统对话框，保证失败原因一定能被看到。
fn report_fatal(msg: &str) {
    eprintln!("{msg}");
    #[cfg(windows)]
    if !HAS_CONSOLE.load(std::sync::atomic::Ordering::Relaxed) {
        win_message_box(msg);
    }
}

/// 统一跑 eframe 窗口，把 GL 环境类失败翻译成用户可自救的提示。
///
/// eframe 的 `OpenGL` / `NoGlutinConfigs` / `Glutin` 三个变体都指向同一件事：
/// 这台机器没有可用的 OpenGL 上下文（虚拟机、精简系统、只装了显卡的
/// 「显示驱动」而无 OpenGL 部分）。原始文案是 `egui_glow requires opengl
/// 2.0+.`——对开发者够用，对用户等于没说，所以这里补上出路。
///
/// 不在此处静默回退到软件渲染：glutin 的 `prefer_hardware_accelerated`
/// 默认就是「不表态」，已经允许挑软件实现；走到这个错误说明连软件 GL 都
/// 没有，切 `HardwareAcceleration::Off` 也救不回来。
fn run_eframe(
    title: &str,
    options: eframe::NativeOptions,
    app: eframe::AppCreator<'static>,
) -> Result<(), String> {
    eframe::run_native(title, options, app).map_err(|e| match e {
        eframe::Error::OpenGL(_)
        | eframe::Error::NoGlutinConfigs(..)
        | eframe::Error::Glutin(_) => {
            format!("{e}\n\n{GL_HINT}")
        }
        other => other.to_string(),
    })
}

/// GL 不可用时的自救指引。Windows 单列：这是实际反馈来源，且 ANGLE 那条
/// 路子只在 Windows 存在（Qt 同款做法，放同目录即可被 EGL 探测到）。
#[cfg(windows)]
const GL_HINT: &str = "这台电脑没有可用的 OpenGL 驱动，可任选一条解决：\n\
     1. 安装/更新显卡驱动（推荐，去显卡厂商官网，Windows 更新给的版本常缺 OpenGL）\n\
     2. 虚拟机里请在设置中开启 3D 加速并装好增强工具（VMware Tools / VBox Guest Additions）\n\
     3. 放一份软件渲染的 opengl32sw.dll 到 lscreen.exe 同目录（可从 Qt 发行包取得）";

#[cfg(not(windows))]
const GL_HINT: &str = "当前环境没有可用的 OpenGL 驱动：\n\
     - 桌面会话请确认已安装显卡驱动与 mesa（Debian 系：libgl1-mesa-dri）\n\
     - 远程/无显卡环境可用软件渲染兜底：LIBGL_ALWAYS_SOFTWARE=1 lscreen";

fn main() {
    // GUI 子系统下从终端启动时附回父控制台：--help/qr/ocr 输出与报错仍可见。
    // 双击/快捷方式启动没有父控制台，此时致命错误改走 report_fatal 的弹窗。
    // （已知取舍：cmd 不等待 GUI 进程，输出会与下一个提示符交错）
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
        let attached = AttachConsole(ATTACH_PARENT_PROCESS) != 0;
        HAS_CONSOLE.store(attached, std::sync::atomic::Ordering::Relaxed);
    }

    // panic = "abort" 下 panic 依然先跑 hook。没有它，启动期 panic（字体解析、
    // 窗口/GL 上下文创建等）在 GUI 子系统下是彻底静默的死亡。
    #[cfg(windows)]
    std::panic::set_hook(Box::new(|info| {
        report_fatal(&format!("内部错误：{info}"));
    }));

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
        // 无参数 = 托盘静默驻留（用户明确要求的默认形态）
        None => run_tray(false),
        Some(Cmd::Tray { foreground }) => run_tray(foreground),
        Some(Cmd::Gui) => run_gui(ui::Mode::Snip),
        Some(Cmd::Pick) => run_gui(ui::Mode::Pick),
        Some(Cmd::Qr { region, input }) => run_qr(region, input),
        Some(Cmd::Ocr {
            region,
            input,
            languages,
        }) => run_ocr(region, input, languages),
        Some(Cmd::Record {
            region,
            select,
            mp4,
            duration,
            fps,
            quality,
            output,
        }) => {
            // --select 优先：先框选再录；取消框选则静默退出（非错误）
            if select {
                match pick_region_interactive() {
                    Ok(Some(r)) => {
                        // 框选覆盖层已消耗本进程唯一的事件循环额度：macOS 的
                        // winit/NSApplication 同进程跑不了第二个 eframe 循环，
                        // 状态窗会直接拉不起来（表现=选完区一切消失）。带显式
                        // --region 转交新进程执行，保证每进程恰好一个循环
                        //（与托盘 spawn 子进程同款模式）。
                        let mut extra: Vec<String> = Vec::new();
                        if mp4 {
                            extra.push("--mp4".into());
                        }
                        extra.extend([
                            "--duration".to_string(),
                            duration.to_string(),
                            "--fps".to_string(),
                            fps.to_string(),
                        ]);
                        if let Some(q) = quality {
                            extra.extend(["--quality".to_string(), q.to_string()]);
                        }
                        if let Some(o) = output {
                            extra.extend(["--output".to_string(), o.display().to_string()]);
                        }
                        reexec_after_pick("record", &r, &extra)
                    }
                    Ok(None) => Ok(()),
                    Err(e) => {
                        eprintln!("lscreen: {e}");
                        std::process::exit(1);
                    }
                }
            } else {
                run_record(region, mp4, duration, fps, quality, output)
            }
        }
        Some(Cmd::Shot {
            region,
            window,
            window_at,
            output,
            clipboard,
        }) => run_shot(region, window, window_at, output, clipboard),
        Some(Cmd::Scroll {
            region,
            steps,
            clicks,
            pause_ms,
            output,
        }) => {
            // 交互框选优先（与 record --select 同体验）；取消则静默退出
            match region {
                Some(r) => run_scroll(Some(r), steps, clicks, pause_ms, output),
                None => match pick_region_interactive() {
                    Ok(Some(r)) => {
                        // 同 record --select：框选覆盖层已消耗本进程的事件循环
                        // 额度，拼接/状态窗转交新进程（详见那边的注释）
                        let mut extra: Vec<String> = vec![
                            "--steps".to_string(),
                            steps.to_string(),
                            "--clicks".to_string(),
                            clicks.to_string(),
                            "--pause-ms".to_string(),
                            pause_ms.to_string(),
                        ];
                        if let Some(o) = output {
                            extra.extend(["--output".to_string(), o.display().to_string()]);
                        }
                        reexec_after_pick("scroll", &r, &extra)
                    }
                    Ok(None) => Ok(()),
                    Err(e) => {
                        eprintln!("lscreen: {e}");
                        std::process::exit(1);
                    }
                },
            }
        }
        Some(Cmd::Pin { input, pos, scale }) => run_pin(input, pos, scale),
        Some(Cmd::Config) => run_settings(),
        Some(Cmd::History) => run_history(),
        Some(Cmd::Annotate { input }) => run_annotate(input),
    };
    if let Err(e) = result {
        report_fatal(&format!("lscreen: {e}"));
        std::process::exit(1);
    }
}

/// 托盘入口：默认分离到后台（静默驻留），--foreground 保持前台便于调试。
/// 分离方式 = 再拉起一个自身子进程（stdio 全断开 + 环境变量防递归），
/// 父进程随即退出，终端立即返回；子进程被 init 收养，无僵尸问题。
fn run_tray(foreground: bool) -> Result<(), String> {
    use std::process::Stdio;
    const CHILD_ENV: &str = "LSCREEN_TRAY_CHILD";
    if !foreground && std::env::var_os(CHILD_ENV).is_none() {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        std::process::Command::new(exe)
            .args(["tray", "--foreground"])
            .env(CHILD_ENV, "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("启动托盘进程失败: {e}"))?;
        eprintln!("LaterScreen 已驻留后台：托盘菜单或全局热键唤起（lscreen tray --foreground 可前台运行）");
        return Ok(());
    }
    tray::run()
}

/// 配置面板窗口。
fn run_settings() -> Result<(), String> {
    let viewport = eframe::egui::ViewportBuilder::default()
        .with_app_id("lscreen")
        .with_inner_size([480.0, 640.0])
        .with_min_inner_size([420.0, 520.0])
        .with_resizable(true);
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    run_eframe(
        "lscreen 配置",
        options,
        Box::new(|cc| Ok(Box::new(settings_ui::SettingsApp::new(cc)))),
    )
}

/// 截图历史面板（M11）：无边框置顶浮窗，缩略图网格展示最近截图/贴图/录屏。
/// 单击按类型分动作（截图/贴图=复制，录屏=打开目录并选中）；右键贴图/打开/删除。
/// 摆放靠近托盘：主屏右下角、桌面底部上方几个像素（Deepin 托盘在右下）。
fn run_history() -> Result<(), String> {
    // 单例：快捷键/菜单连按会不断 spawn 新历史进程，这里先抢锁，已有活着的
    // 历史窗口则本进程直接退出，不再弹第二个面板。
    if !history::acquire_single_instance() {
        return Ok(());
    }
    let mut viewport = eframe::egui::ViewportBuilder::default()
        .with_app_id("lscreen")
        .with_inner_size([280.0, 420.0])
        .with_min_inner_size([240.0, 200.0])
        .with_resizable(true)
        .with_decorations(false)
        .with_always_on_top();
    // 贴托盘摆放：Linux（Deepin dock 在右下）贴主屏右下角；macOS 状态栏在
    // 顶部，贴主屏右上角、菜单栏下方（菜单栏高 24-37pt，取 32 安全）。
    // 多屏时贴主屏而非虚拟桌面边界——monitor_bounds 是全部显示器并集，
    // 后者会落在最右那块屏上。
    if let Some((dx, dy, dw, dh)) = lscreen_capture::primary_monitor_bounds() {
        let (pw, ph) = (280.0, 420.0);
        let x = (dx as f32 + dw as f32 - pw - 8.0).max(dx as f32);
        let y = if cfg!(target_os = "macos") {
            // 菜单栏高 24pt（标准）/37pt（刘海屏），取 36 保证不压
            dy as f32 + 36.0
        } else {
            (dy as f32 + dh as f32 - ph - 8.0).max(dy as f32)
        };
        viewport = viewport.with_position(eframe::egui::Pos2::new(x, y));
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    run_eframe(
        "lscreen 历史",
        options,
        Box::new(|cc| Ok(Box::new(history::HistoryApp::new(cc)))),
    )
}

/// 框选完成后的转交：带显式 `--region` 重启自身执行剩余工作（状态窗/
/// 录制/拼接）。原因：框选覆盖层已跑过一个 eframe 事件循环，macOS 的
/// winit（NSApplication）同进程无法再跑第二个——状态窗直接拉不起来，
/// 表现就是「选完区一切消失」。转交后每进程恰好一个事件循环，与托盘
/// spawn 子进程的既有模式一致。参数原样经 argv 转发，语义不变。
fn reexec_after_pick(sub: &str, region: &str, extra: &[String]) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg(sub).arg("--region").arg(region);
    for a in extra {
        cmd.arg(a);
    }
    match cmd.spawn() {
        Ok(mut child) => {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            Ok(())
        }
        Err(e) => Err(format!("转交子进程失败（{sub}）: {e}")),
    }
}

/// 交互框选录制区域：复用截图覆盖层（Mode::Record），框完即关窗；
/// 返回绝对物理坐标的 "X,Y,W,H"；用户 Esc 取消返回 None。
fn pick_region_interactive() -> Result<Option<String>, String> {
    let shot = match lscreen_capture::cursor_position() {
        Some((x, y)) => {
            lscreen_capture::capture_at(x, y).or_else(|_| lscreen_capture::capture_primary())
        }
        None => lscreen_capture::capture_primary(),
    }
    .map_err(|e| e.to_string())?;
    let (ox, oy) = shot.origin;
    let font = font::load_system_font();
    let config = config::Config::load();
    let (windows, initial) = overlay_window_list(&shot, ui::Mode::Record, &config);

    let scale = if shot.scale > 0.0 { shot.scale } else { 1.0 };
    let pos = eframe::egui::Pos2::new(ox as f32 / scale, oy as f32 / scale);
    let size = eframe::egui::Vec2::new(shot.width as f32 / scale, shot.height as f32 / scale);
    let viewport = eframe::egui::ViewportBuilder::default()
        .with_app_id("lscreen")
        .with_position(pos)
        .with_inner_size(size)
        .with_fullscreen(true)
        .with_decorations(false)
        .with_always_on_top();
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    let shared: SharedRegion = std::sync::Arc::new(std::sync::Mutex::new(None));
    let out = shared.clone();
    run_eframe(
        "lscreen-record",
        options,
        Box::new(move |cc| {
            Ok(Box::new(ui::SnipApp::new(
                cc,
                ui::OverlayInit {
                    shot,
                    font,
                    mode: ui::Mode::Record,
                    config,
                    record_region: Some(shared),
                    windows,
                    initial_region: initial,
                    preview: false,
                },
            )))
        }),
    )?;
    let region = (*out.lock().unwrap()).map(|r| {
        format!(
            "{},{},{},{}",
            (ox as f32 + r.min.x) as i32,
            (oy as f32 + r.min.y) as i32,
            r.width() as u32,
            r.height() as u32
        )
    });
    Ok(region)
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
    let config = config::Config::load();
    // 窗口列表同样必须先于覆盖层建窗采集（否则最前窗口就是自己）；
    // 失败（Wayland 等）返回空列表，覆盖层自动降级为纯手动框选
    let (windows, initial) = overlay_window_list(&shot, mode, &config);

    // origin 为物理像素；X11 下 winit 逻辑坐标与物理一致（scale=1），
    // 其余平台按截屏缩放比换算
    let scale = if shot.scale > 0.0 { shot.scale } else { 1.0 };
    let pos = eframe::egui::Pos2::new(shot.origin.0 as f32 / scale, shot.origin.1 as f32 / scale);
    let size = eframe::egui::Vec2::new(shot.width as f32 / scale, shot.height as f32 / scale);
    let viewport = eframe::egui::ViewportBuilder::default()
        .with_app_id("lscreen")
        .with_position(pos)
        .with_inner_size(size)
        .with_fullscreen(true)
        .with_decorations(false)
        .with_always_on_top();
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    run_eframe(
        "lscreen",
        options,
        Box::new(move |cc| {
            Ok(Box::new(ui::SnipApp::new(
                cc,
                ui::OverlayInit {
                    shot,
                    font,
                    mode,
                    config,
                    record_region: None,
                    windows,
                    initial_region: initial,
                    preview: false,
                },
            )))
        }),
    )
}

/// 覆盖层的窗口吸附列表（M9）：窗口矩形换算到图像像素坐标（含与屏幕求交），
/// 以及按配置计算的初始预选矩形（仅 Window 模式需要；全屏/无在 SnipApp 内合成）。
/// 返回空列表 = 平台不支持或失败，覆盖层退化为纯手动框选。
fn overlay_window_list(
    shot: &lscreen_capture::Screenshot,
    mode: ui::Mode,
    config: &config::Config,
) -> (Vec<ui::WinRect>, Option<ui::WinRect>) {
    if mode == ui::Mode::Pick {
        return (Vec::new(), None);
    }
    let rect_of = |x: f32, y: f32, w: f32, h: f32| {
        lscreen_core::RectF::from_points(
            lscreen_core::P2::new(x, y),
            lscreen_core::P2::new(x + w, y + h),
        )
    };
    let windows: Vec<ui::WinRect> = lscreen_capture::list_windows()
        .iter()
        .filter_map(|w| {
            lscreen_capture::window_rect_in_image(w, shot).map(|(x, y, w_, h_)| ui::WinRect {
                title: w.title.clone(),
                rect: rect_of(x, y, w_, h_),
            })
        })
        .collect();
    let initial = if config.initial_selection() == config::InitialSelection::Window {
        // 最前窗口（活跃窗口优先）；其与当前屏无交或枚举失败时退回 Z 序最顶
        lscreen_capture::frontmost_window()
            .and_then(|fw| {
                lscreen_capture::window_rect_in_image(&fw, shot).map(|(x, y, w_, h_)| ui::WinRect {
                    title: fw.title.clone(),
                    rect: rect_of(x, y, w_, h_),
                })
            })
            .or_else(|| windows.first().cloned())
    } else {
        None
    };
    (windows, initial)
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
    window: bool,
    window_at: Option<String>,
    output: Option<PathBuf>,
    clipboard: bool,
) -> Result<(), String> {
    let renderer = lscreen_core::render::Renderer::new(None);
    let (rgba, w, h) = if window || window_at.is_some() {
        // 窗口模式：直接按窗口矩形抓虚拟桌面（capture_region 内部会钳到屏幕
        // 并集），天然支持跨显示器窗口；比「截主屏再裁剪」多屏更正确
        let win = if window {
            lscreen_capture::frontmost_window()
        } else {
            let (x, y) = parse_xy(window_at.as_deref().unwrap_or_default())?;
            lscreen_capture::window_at(x, y)
        }
        .ok_or("未找到可用窗口（可能无 EWMH 窗口管理器或会话不支持）")?;
        let shot = lscreen_capture::capture_region(win.x, win.y, win.width, win.height)
            .map_err(|e| e.to_string())?;
        export::compose(
            &renderer,
            &shot.rgba,
            shot.width,
            shot.height,
            &[],
            lscreen_core::RectF::from_points(
                lscreen_core::P2::new(0.0, 0.0),
                lscreen_core::P2::new(shot.width as f32, shot.height as f32),
            ),
        )
        .ok_or_else(|| "选区为空（窗口与屏幕无交集）".to_string())?
    } else {
        let shot = lscreen_capture::capture_primary().map_err(|e| e.to_string())?;
        let region = match region {
            Some(s) => parse_region(&s)?,
            None => lscreen_core::RectF::from_points(
                lscreen_core::P2::new(0.0, 0.0),
                lscreen_core::P2::new(shot.width as f32, shot.height as f32),
            ),
        };
        export::compose(&renderer, &shot.rgba, shot.width, shot.height, &[], region)
            .ok_or_else(|| "选区为空（region 与屏幕无交集）".to_string())?
    };

    if clipboard {
        export::copy_to_clipboard(&rgba, w, h)?;
    }
    if output.is_some() || !clipboard {
        let path = output.unwrap_or_else(|| export::default_save_path("png"));
        let saved = export::save_png(&rgba, w, h, &path)?;
        history::record_file(&saved, history::Kind::Shot, Some(&saved));
        println!("{}", saved.display());
    }
    Ok(())
}

/// 为录制状态窗找一个不与选区重叠的虚拟桌面角落（右下优先，逆时针）。
/// 四角都放不下（如全屏录制）时取右下并返回 overlap=true——上层提示用户
/// 状态窗已入镜。窗口尺寸不含 WM 装饰，误差几个像素可接受。
fn status_window_pos(
    sel: (f32, f32, f32, f32),
    desk: (f32, f32, f32, f32),
    win: (f32, f32),
) -> ((f32, f32), bool) {
    const MARGIN: f32 = 12.0;
    let (sx, sy, sw, sh) = sel;
    let (dx, dy, dw, dh) = desk;
    let (ww, wh) = win;
    // 状态窗可能比角落空间大（小屏）：max 保证贴边而不越出桌面
    let right = dx + dw - ww - MARGIN;
    let bottom = dy + dh - wh - MARGIN;
    let left = dx + MARGIN;
    let top = dy + MARGIN;
    let candidates = [(right, bottom), (right, top), (left, bottom), (left, top)];
    for &(cx, cy) in &candidates {
        let overlap = cx < sx + sw && cx + ww > sx && cy < sy + sh && cy + wh > sy;
        if !overlap {
            return ((cx.max(dx), cy.max(dy)), false);
        }
    }
    ((right.max(dx), bottom.max(dy)), true)
}

fn run_record(
    region: Option<String>,
    mp4_out: bool,
    duration: f32,
    fps: u32,
    quality: Option<u64>,
    output: Option<PathBuf>,
) -> Result<(), String> {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

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
    // 预估格式（仅用于区域取整与快速失败）：最终格式在 armed 确认后重读
    // 配置定案（状态窗齿轮可在 armed 阶段改格式/目录，对本次录制生效）
    let mp4_guess = mp4_out || config::Config::load().record_mp4();
    if let Some(q) = quality {
        let ok = if mp4_guess {
            (200..=50_000).contains(&q)
        } else {
            (1..=100).contains(&q)
        };
        if !ok {
            return Err("无效的 --quality（GIF 应为 1-100；MP4 为目标码率 kbps 200-50000）".into());
        }
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
    // MP4/H.264 宏块要求偶数尺寸：区域在此取整（右/下各裁 1px），
    // 边框标出的就是实际录制的区域。record 层对帧还有一道防御性裁偶
    if mp4_guess && (w < 2 || h < 2) {
        return Err("MP4 录制区域过小（至少 2×2 像素）".into());
    }
    let (x, y, w, h) = if mp4_guess {
        (x, y, w & !1, h & !1)
    } else {
        (x, y, w, h)
    };

    // M10 录制边框 + 闪烁：armed 阶段静态红边标出选区；开始录制后红/蓝交替
    // （每 400ms 换色）提示"正在录制"。guard move 进闪烁线程，线程退出（stop）
    // 时销毁窗口——绝不留残影。平台不支持时 None，录制行为不变。
    let started = Arc::new(AtomicBool::new(false));
    let blink_stop = Arc::new(AtomicBool::new(false));
    let blink_handle = lscreen_capture::record_border(x, y, w, h).map(|border| {
        let (bs, started_c) = (blink_stop.clone(), started.clone());
        std::thread::spawn(move || {
            const RED: u32 = 0xE5_39_35;
            const BLUE: u32 = 0x21_96_F3;
            let mut red = true;
            while !bs.load(Ordering::Relaxed) {
                std::thread::sleep(std::time::Duration::from_millis(400));
                if started_c.load(Ordering::Relaxed) && !bs.load(Ordering::Relaxed) {
                    red = !red;
                    border.set_color(if red { RED } else { BLUE });
                }
            }
        })
    });

    let stop = Arc::new(AtomicBool::new(false));
    // ctrlc：交互终端里 Ctrl+C 与状态窗口 Esc 等效；非终端（托盘 spawn）下
    // 收不到 SIGINT，靠状态窗口的停止按钮/Esc 结束
    {
        let stop = stop.clone();
        let _ = ctrlc::set_handler(move || stop.store(true, Ordering::Relaxed));
    }

    let status = Arc::new(Mutex::new(record_ui::RecordStatus::default()));

    // 录制线程：独立跑采帧+编码（X11 每次截屏新建连接，跨线程安全），
    // 主线程跑状态窗口事件循环，两者互不阻塞。grab_frame 每采一帧就把
    // 时长/帧数写进共享状态，状态窗口 0.5s 刷一次即读到实时值。
    let (th_stop, th_status) = (stop.clone(), status.clone());
    let th_duration = std::time::Duration::from_secs_f32(secs);
    let th_started = started.clone();
    // 首帧 poster（M11 历史缩略图）：GIF/MP4 都无法事后解码，录制时留首帧
    let poster = Arc::new(Mutex::new(None::<(Vec<u8>, u32, u32)>));
    let th_poster = poster.clone();
    let recorder = std::thread::spawn(move || {
        // armed：等待用户点「开始」/按 Enter；stop 先到（Esc/关窗/Ctrl+C）= 取消
        while !th_started.load(Ordering::Relaxed) && !th_stop.load(Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        if !th_started.load(Ordering::Relaxed) {
            th_status.lock().unwrap().done = true;
            return (
                0.0,
                Err(lscreen_record::RecordError("已取消".into())),
                PathBuf::new(),
                false,
            );
        }
        // 格式/路径在 armed 确认后才定案：状态窗的齿轮可打开配置面板，
        // armed 阶段改的录制格式/保存目录对本次录制生效（CLI --mp4 仍优先）
        let cfg = config::Config::load();
        let mp4_final = mp4_out || cfg.record_mp4();
        let quality = quality.unwrap_or(if mp4_final { 4000 } else { 90 });
        let bad_quality = if mp4_final {
            !(200..=50_000).contains(&quality)
        } else {
            !(1..=100).contains(&quality)
        };
        if bad_quality {
            th_status.lock().unwrap().done = true;
            return (
                0.0,
                Err(lscreen_record::RecordError(
                    "无效的 --quality（GIF 应为 1-100；MP4 为目标码率 kbps 200-50000）".into(),
                )),
                PathBuf::new(),
                false,
            );
        }
        let ext = if mp4_final { "mp4" } else { "gif" };
        let path = output.unwrap_or_else(|| export::default_save_path(ext));
        if mp4_final {
            // 扩展名提醒（不强制改写：用户可能故意用别的名字）
            if path.extension().is_some_and(|e| e != "mp4") {
                eprintln!("提示: --mp4 输出建议使用 .mp4 扩展名");
            }
        } else if path.extension().is_some_and(|e| e != "gif") {
            eprintln!("提示: GIF 输出建议使用 .gif 扩展名");
        }
        let start = std::time::Instant::now();
        let mut frames = 0usize;
        // 内层 grab 闭包 move 捕获，这里单独 clone 一份，外层结尾仍要用 th_status
        let status_inner = th_status.clone();
        let poster_inner = th_poster.clone();
        let result = if mp4_final {
            lscreen_record::record_mp4(
                move || {
                    let shot = lscreen_capture::capture_region(x, y, w, h)
                        .map(|s| (s.rgba, s.width, s.height))
                        .map_err(|e| lscreen_record::RecordError(e.to_string()))?;
                    frames += 1;
                    if frames == 1 {
                        *poster_inner.lock().unwrap() = Some((shot.0.clone(), shot.1, shot.2));
                    }
                    let mut st = status_inner.lock().unwrap();
                    st.elapsed = start.elapsed().as_secs_f32();
                    st.frames = frames;
                    Ok(shot)
                },
                &lscreen_record::Mp4Options {
                    fps,
                    bitrate_kbps: quality as u32,
                },
                th_duration,
                &th_stop,
                &path,
            )
        } else {
            lscreen_record::record_gif(
                move || {
                    let shot = lscreen_capture::capture_region(x, y, w, h)
                        .map(|s| (s.rgba, s.width, s.height))
                        .map_err(|e| lscreen_record::RecordError(e.to_string()))?;
                    frames += 1;
                    if frames == 1 {
                        *poster_inner.lock().unwrap() = Some((shot.0.clone(), shot.1, shot.2));
                    }
                    let mut st = status_inner.lock().unwrap();
                    st.elapsed = start.elapsed().as_secs_f32();
                    st.frames = frames;
                    Ok(shot)
                },
                &lscreen_record::GifOptions {
                    fps,
                    quality: quality as u8,
                },
                th_duration,
                &th_stop,
                &path,
            )
        };
        // 无论成败，都让状态窗口知道录制已结束（自然到时/出错/被停止）
        th_status.lock().unwrap().done = true;
        (
            start.elapsed().as_secs_f32(),
            result,
            path,
            cfg.open_dir_after_save,
        )
    });

    // 状态窗口：停止按钮/Esc 置 stop；窗口被关（含 done 自动关）后收尾。
    // 无边框 + 暗色自绘面板（record_ui）：无系统标题栏的一块小黑板，
    // 头部区域可拖动移动；位置避让选区（状态窗若压在选区上会被录进成品，M10）
    let mut viewport = eframe::egui::ViewportBuilder::default()
        .with_app_id("lscreen")
        .with_inner_size([320.0, 150.0])
        .with_resizable(false)
        .with_decorations(false)
        .with_transparent(false)
        .with_always_on_top()
        .with_title("lscreen 录制");
    let mut overlap_hint = false;
    // 仅 Linux X11 用显式坐标摆放：egui/winit 的窗口位置是逻辑点，X11 下
    // 恒等于根窗口物理像素（本仓库 Screenshot.scale 亦按 1.0 处理）；
    // Win/mac 多屏 DPI 换算不可靠，维持 WM 默认摆放
    if cfg!(target_os = "linux") {
        if let Some((dx, dy, dw, dh)) = lscreen_capture::monitor_bounds() {
            let ((px, py), overlap) = status_window_pos(
                (x as f32, y as f32, w as f32, h as f32),
                (dx as f32, dy as f32, dw as f32, dh as f32),
                (320.0, 150.0),
            );
            viewport = viewport.with_position(eframe::egui::Pos2::new(px, py));
            overlap_hint = overlap;
        }
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    let app_stop = stop.clone();
    let app_start = started.clone();
    let app_status = status.clone();
    run_eframe(
        "lscreen-record-status",
        options,
        Box::new(move |cc| {
            Ok(Box::new(record_ui::RecordApp::new(
                cc,
                app_stop,
                app_start,
                app_status,
                secs,
                false,
                overlap_hint,
            )))
        }),
    )?;

    // 事件循环退出（用户停止/窗口关闭/录制完成自动关）：确保录制线程收尾
    stop.store(true, Ordering::Relaxed);
    let cancelled = !started.load(Ordering::Relaxed);
    let (_elapsed, result, path, open_dir) = recorder
        .join()
        .map_err(|_| "录制线程异常退出".to_string())?;
    // 边框闪烁线程收尾（join 保证边条窗口在进程退出前销毁）
    blink_stop.store(true, Ordering::Relaxed);
    if let Some(h) = blink_handle {
        let _ = h.join();
    }
    let frames = match result {
        Ok(f) => f,
        // armed 阶段取消（Esc/关窗/Ctrl+C）：静默退出，不是错误
        Err(_) if cancelled => return Ok(()),
        Err(e) => return Err(e.to_string()),
    };
    eprintln!("已录制 {frames} 帧");
    println!("{}", path.display());
    // 保存后自动打开所在目录（配置项，截图 GUI 保存路径同款行为）。
    // open_dir 取自 armed 后的配置快照，与本次产物目录一致
    if open_dir {
        if let Some(dir) = path.parent() {
            export::open_in_file_manager(dir);
        }
    }
    // 录屏入历史（M11）：把首帧 poster 另存 PNG 并记一条，source 指向实际
    // GIF/MP4 文件供「打开目录并选中」定位
    if let Some((rgba, w, h)) = poster.lock().unwrap().take() {
        let poster_path = poster_path(&path);
        if let Ok(saved) = export::save_png(&rgba, w, h, &poster_path) {
            history::record_file(&saved, history::Kind::Record, Some(&path));
        }
    }
    Ok(())
}

/// 录屏首帧 poster 路径：与产物同目录、同名、`_poster.png` 后缀。
fn poster_path(video: &std::path::Path) -> PathBuf {
    let stem = video
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "lscreen_poster".to_string());
    video
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join(format!("{stem}_poster.png"))
}

/// 滚动长截图（M4）：框选区域 → 自动滚动 → 帧间拼接 → 长图 PNG。
/// 指针被移到区域中心驱动窗口滚动（XTest 滚轮事件落在指针下的窗口），
/// 结束后恢复原位置。停止条件：连续两帧无变化（滚到底）/内容匹配失败
/// （悬浮头等）/步数用尽/用户停止。
fn run_scroll(
    region: Option<String>,
    steps: u32,
    clicks: u32,
    pause_ms: u64,
    output: Option<PathBuf>,
) -> Result<(), String> {
    if !(1..=1000).contains(&steps) {
        return Err("无效的 --steps（应为 1-1000）".into());
    }
    if !(1..=9).contains(&clicks) {
        return Err("无效的 --clicks（应为 1-9）".into());
    }
    if !(50..=2000).contains(&pause_ms) {
        return Err("无效的 --pause-ms（应为 50-2000）".into());
    }
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
            // 无区域时已被上游交互框选填充；防御性兜底：主屏
            let shot = lscreen_capture::capture_primary().map_err(|e| e.to_string())?;
            (shot.origin.0, shot.origin.1, shot.width, shot.height)
        }
    };

    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = stop.clone();
        let _ = ctrlc::set_handler(move || stop.store(true, Ordering::Relaxed));
    }
    let status = Arc::new(Mutex::new(record_ui::RecordStatus::default()));
    let explicit_out = output.is_some();
    let path = output.unwrap_or_else(|| export::default_save_path("png"));

    // 拼接线程：滚动 + 采帧 + 匹配；状态窗口显示进度
    let (th_stop, th_status, th_path) = (stop.clone(), status.clone(), path.clone());
    let pause = std::time::Duration::from_millis(pause_ms);
    let stitcher = std::thread::spawn(
        move || -> std::result::Result<lscreen_record::scroll::ScrollStitcher, String> {
            // 指针移到区域中心驱动滚动，结束恢复原位（尽量不打扰用户）。
            // 主体收进闭包：任何错误路径都必须走到收尾（恢复指针 + 置 done
            // 让状态窗口自动关），提前 return 会让窗口挂死等用户手关
            let orig_pos = lscreen_capture::cursor_position();
            let result = (|| {
                let first =
                    lscreen_capture::capture_region(x, y, w, h).map_err(|e| e.to_string())?;
                let mut st = lscreen_record::scroll::ScrollStitcher::new(
                    &first.rgba,
                    first.width,
                    first.height,
                )
                .map_err(|e| e.to_string())?;
                {
                    let mut s = th_status.lock().unwrap();
                    s.height = st.height();
                    s.frames = 0;
                }
                let _ = lscreen_capture::warp_pointer(x + w as i32 / 2, y + h as i32 / 2);
                // 给 WM 一点时间完成指针移动与焦点切换
                std::thread::sleep(std::time::Duration::from_millis(150));

                let mut no_change = 0u32;
                let mut scrolled = false;
                for step in 1..=steps {
                    if th_stop.load(Ordering::Relaxed) {
                        break;
                    }
                    lscreen_capture::scroll_wheel(-(clicks as i32)).map_err(|e| e.to_string())?;
                    std::thread::sleep(pause);
                    let frame =
                        lscreen_capture::capture_region(x, y, w, h).map_err(|e| e.to_string())?;
                    let outcome = st
                        .push(&frame.rgba, frame.width, frame.height)
                        .map_err(|e| e.to_string())?;
                    {
                        let mut s = th_status.lock().unwrap();
                        s.frames = step as usize;
                        s.height = st.height();
                    }
                    match outcome {
                        lscreen_record::scroll::ScrollOutcome::Appended(_) => {
                            no_change = 0;
                            scrolled = true;
                        }
                        lscreen_record::scroll::ScrollOutcome::NoChange => {
                            no_change += 1;
                            // 连续两帧无新增 = 已滚到底（或窗口不滚动）
                            if no_change >= 2 {
                                break;
                            }
                        }
                        // 内容突变（悬浮表头/动画/弹窗）：保留已有结果停止
                        lscreen_record::scroll::ScrollOutcome::Mismatch => break,
                    }
                }
                if !scrolled {
                    return Err("未检测到滚动（窗口可能不支持滚轮或已在底部）".into());
                }
                Ok(st)
            })();
            if let Some((px, py)) = orig_pos {
                let _ = lscreen_capture::warp_pointer(px, py);
            }
            // 无论成败都让状态窗口自动关（对齐 run_record 的收尾语义）
            th_status.lock().unwrap().done = true;
            result
        },
    );

    // 状态窗口：停止按钮/Esc/关窗置 stop；拼接线程 done 后自动关
    let viewport = eframe::egui::ViewportBuilder::default()
        .with_app_id("lscreen")
        .with_inner_size([320.0, 150.0])
        .with_resizable(false)
        .with_always_on_top()
        .with_title("lscreen 滚动截图");
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    let app_stop = stop.clone();
    let app_status = status.clone();
    run_eframe(
        "lscreen-scroll-status",
        options,
        Box::new(move |cc| {
            Ok(Box::new(record_ui::RecordApp::new(
                cc,
                app_stop,
                Arc::new(AtomicBool::new(true)), // 滚动模式无 armed 阶段
                app_status,
                steps as f32,
                true,
                false,
            )))
        }),
    )?;

    // 事件循环退出（用户停止/窗口关闭）：收尾拼接线程
    stop.store(true, Ordering::Relaxed);
    let result = stitcher
        .join()
        .map_err(|_| "拼接线程异常退出".to_string())?;
    let st = result?;
    let (mut img, mut ih) = (st.image().to_vec(), st.height());
    let iw = st.width();
    // GPU 单纹理高度上限普遍 8192/16384：超限截断保预览可用（纹理被驱动
    // 静默裁剪更糟），不静默——stderr 明示
    const MAX_H: u32 = 8192;
    if ih > MAX_H {
        img.truncate((iw * MAX_H) as usize * 4);
        ih = MAX_H;
        eprintln!("lscreen: 长图超过 {MAX_H}px，已截断（建议分多次滚动截图）");
    }

    match explicit_out {
        // 显式 -o：脚本用法，直接落盘（保持旧行为）
        true => {
            let saved = export::save_png(&img, iw, ih, &th_path)?;
            eprintln!("已拼接长图 {iw}×{ih}");
            println!("{}", saved.display());
            history::record_file(&saved, history::Kind::Shot, Some(&saved));
            // 保存后自动打开所在目录（与截图/录屏保存同款配置行为）
            if config::Config::load().open_dir_after_save {
                if let Some(dir) = saved.parent() {
                    export::open_in_file_manager(dir);
                }
            }
            Ok(())
        }
        // 默认：打开标注预览窗口（复用截图标注会话：全工具标注 +
        // 保存/复制/贴图/OCR/二维码，Esc 退出）。预览放独立进程：本进程
        // 已跑过状态窗的事件循环，macOS 同进程跑不了第二个循环（同
        // pick_region_interactive 的转交理由）；PNG 经 stdin 传递，不落
        // 临时文件（与托盘贴图同款通道）
        false => {
            let full = image::RgbaImage::from_raw(iw, ih, img)
                .ok_or_else(|| "拼接结果尺寸不一致".to_string())?;
            let mut png = Vec::new();
            full.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
                .map_err(|e| format!("编码预览 PNG 失败: {e}"))?;
            spawn_annotate_stdin(&png)
        }
    }
}

/// 把拼接结果 PNG 经 stdin 交给独立的标注预览进程（见 run_annotate）。
/// 子进程先读完 stdin 再开窗，write_all 阻塞到对端读走为止，无死锁。
fn spawn_annotate_stdin(png: &[u8]) -> Result<(), String> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut child = Command::new(exe)
        .arg("annotate")
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| format!("启动预览进程失败: {e}"))?;
    child
        .stdin
        .take()
        .ok_or_else(|| "无法写入预览进程".to_string())?
        .write_all(png)
        .map_err(|e| format!("写入预览进程失败: {e}"))?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// 标注预览窗口：把已有图片开进带完整工具栏的标注窗（保存/复制/贴图/
/// OCR/二维码）。滚动截图拼接结果的内部预览入口——独立进程跑，因为拼接
/// 进程已消耗过自己的事件循环额度（macOS 限制），见 run_scroll。
fn run_annotate(input: Option<PathBuf>) -> Result<(), String> {
    let img = match input {
        Some(path) => image::open(&path)
            .map_err(|e| format!("无法读取 {}: {e}", path.display()))?
            .into_rgba8(),
        None => {
            let mut buf = Vec::new();
            std::io::Read::read_to_end(&mut std::io::stdin(), &mut buf)
                .map_err(|e| format!("读取 stdin 失败: {e}"))?;
            image::load_from_memory(&buf)
                .map_err(|e| format!("stdin 不是有效图片: {e}"))?
                .into_rgba8()
        }
    };
    let (w, h) = (img.width(), img.height());
    if w == 0 || h == 0 {
        return Err("空图片".into());
    }
    let rgba = img.into_raw();
    // 窗口尺寸与原先进程内预览同款：缩到 60%/50% 并夹到可用范围；宽度
    // 下限要容得下底部标注工具栏（预览模式约 560 逻辑点）
    let (vw, vh) = (
        (w as f32 * 0.6).clamp(640.0, 960.0),
        (h as f32 * 0.5).clamp(420.0, 720.0),
    );
    let viewport = eframe::egui::ViewportBuilder::default()
        .with_app_id("lscreen")
        .with_inner_size([vw, vh])
        .with_min_inner_size([640.0, 360.0])
        .with_title("lscreen 滚动截图 - 标注");
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    let config = config::Config::load();
    run_eframe(
        "lscreen-annotate",
        options,
        Box::new(move |cc| {
            let shot = lscreen_capture::Screenshot {
                rgba,
                width: w,
                height: h,
                origin: (0, 0),
                scale: 1.0,
                is_primary: true,
            };
            Ok(Box::new(ui::SnipApp::new(
                cc,
                ui::OverlayInit {
                    shot,
                    font: font::load_system_font(),
                    mode: ui::Mode::Snip,
                    config,
                    record_region: None,
                    windows: Vec::new(),
                    initial_region: None,
                    preview: true,
                },
            )))
        }),
    )
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
    // 源文件路径（-i 传入时）：保存时记历史 source，供「打开目录并选中」
    let source = input.clone();
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
        .with_app_id("lscreen")
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
    run_eframe(
        "lscreen-pin",
        options,
        Box::new(move |cc| {
            let font = font::load_system_font();
            Ok(Box::new(pin::PinApp::new(
                cc, rgba, w, h, scale, font, source,
            )))
        }),
    )
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

/// 解析 "X,Y" 整数坐标（虚拟桌面物理像素，`shot --window-at` 用）。
fn parse_xy(s: &str) -> Result<(i32, i32), String> {
    let parts: Vec<i32> = s
        .split(',')
        .map(|v| v.trim().parse::<i32>())
        .collect::<Result<_, _>>()
        .map_err(|_| format!("无效的坐标: {s}（应为 X,Y）"))?;
    if parts.len() != 2 {
        return Err(format!("无效的坐标: {s}（应为 X,Y）"));
    }
    Ok((parts[0], parts[1]))
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
    use super::{parse_pos, parse_xy, status_window_pos};

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

    #[test]
    fn xy_parsing() {
        assert_eq!(parse_xy("100,200").unwrap(), (100, 200));
        assert_eq!(parse_xy(" -1920 , 0 ").unwrap(), (-1920, 0));
        assert!(parse_xy("100").is_err());
        assert!(parse_xy("1,2,3").is_err());
        assert!(parse_xy("a,b").is_err());
    }

    #[test]
    fn status_pos_avoids_selection() {
        let desk = (0.0, 0.0, 1920.0, 1080.0);
        let win = (320.0, 150.0);
        // 选区在屏幕中央：右下角可用
        let ((x, y), overlap) = status_window_pos((760.0, 440.0, 400.0, 200.0), desk, win);
        assert!(!overlap);
        assert_eq!((x, y), (1920.0 - 320.0 - 12.0, 1080.0 - 150.0 - 12.0));
        // 右下被选区占住（选区贴右下角）：退到右上
        let ((x, y), overlap) = status_window_pos((1600.0, 900.0, 320.0, 180.0), desk, win);
        assert!(!overlap);
        assert_eq!((x, y), (1920.0 - 320.0 - 12.0, 12.0));
        // 全屏录制：四角都重叠，取右下并标记
        let ((x, y), overlap) = status_window_pos((0.0, 0.0, 1920.0, 1080.0), desk, win);
        assert!(overlap);
        assert_eq!((x, y), (1920.0 - 320.0 - 12.0, 1080.0 - 150.0 - 12.0));
    }

    #[test]
    fn status_pos_negative_desktop() {
        // 显示器在主屏左侧（虚拟桌面原点为负）
        let desk = (-1920.0, 0.0, 3840.0, 1080.0);
        let win = (320.0, 150.0);
        // 全屏主屏录制（1920,0 起）：右侧副屏角落可用
        let ((x, y), overlap) = status_window_pos((1920.0, 0.0, 1920.0, 1080.0), desk, win);
        assert!(!overlap);
        assert_eq!(
            (x, y),
            (-1920.0 + 3840.0 - 320.0 - 12.0, 1080.0 - 150.0 - 12.0)
        );
        // 两屏全被选区盖住：右下 + overlap
        let (_, overlap) = status_window_pos((-1920.0, 0.0, 3840.0, 1080.0), desk, win);
        assert!(overlap);
    }
}
