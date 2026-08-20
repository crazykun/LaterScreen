//! 托盘常驻进程（M8）：一个空闲内存很小的常驻体，负责托盘菜单、全局热键
//! 与配置热加载；截图/取色/贴图/录屏全部按需 spawn 独立子进程，用完即退，
//! 托盘进程自身不持有任何截图缓冲（「小而美常驻」）。
//!
//! 平台后端：
//! - Linux：ksni（纯 Rust 的 StatusNotifierItem/D-Bus 直连，无动态库依赖；
//!   这是弃用 tray-icon Linux 后端的原因——它要链接 gtk/libappindicator）
//! - Win/mac：tray-icon（系统原生托盘 API），事件泵复用 eframe 已链接的
//!   winit（macOS 要求托盘在主线程已运行的事件循环上创建）
//!
//! 全局热键：global-hotkey（Win 原生 / mac CGEventTap / Linux X11 XGrabKey）。
//! Wayland 会话无 X11 时热键整体降级为不可用（仅告警），托盘与菜单仍可用。

use crate::config::{self, Config};
use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};

pub fn run() -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        linux_impl::run_linux()
    }
    #[cfg(not(target_os = "linux"))]
    {
        native_impl::run_native()
    }
}

// ---------------------------------------------------------------- 动作

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    Screenshot,
    Picker,
    Pin,
    Record,
    Config,
    Quit,
}

const MENU_ACTIONS: &[(Action, &str)] = &[
    (Action::Screenshot, "截图"),
    (Action::Picker, "取色"),
    (Action::Pin, "贴图（剪贴板）"),
    (Action::Record, "录屏（框选区域）"),
    (Action::Config, "配置"),
    (Action::Quit, "退出"),
];

/// 执行动作：除退出外都是拉起独立子进程（detached + 分离线程收割，防僵尸）。
/// 注意子命令名不能省：裸启动 = 托盘模式，会递归再驻留一个托盘。
fn dispatch(a: Action) -> bool {
    // 返回 false 表示应当退出托盘进程
    match a {
        Action::Screenshot => spawn_detached(&["gui"]),
        Action::Picker => spawn_detached(&["pick"]),
        Action::Record => spawn_detached(&["record", "--select"]),
        Action::Config => spawn_detached(&["config"]),
        Action::Pin => {
            if let Err(e) = pin_from_clipboard() {
                eprintln!("lscreen tray: {e}");
            }
        }
        Action::Quit => return false,
    }
    true
}

fn spawn_detached(args: &[&str]) {
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("lscreen tray: 无法定位自身可执行文件: {e}");
            return;
        }
    };
    match std::process::Command::new(exe).args(args).spawn() {
        Ok(mut child) => {
            // 常驻进程必须回收退出的子进程，否则累积僵尸
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
        Err(e) => {
            eprintln!(
                "lscreen tray: 启动子进程失败（{}）: {e}",
                args.first().unwrap_or(&"gui")
            )
        }
    }
}

/// 贴图动作：读剪贴板图片 → 独立贴图进程（PNG 经 stdin 传入）。
fn pin_from_clipboard() -> Result<(), String> {
    let mut cb = arboard::Clipboard::new().map_err(|e| format!("打开剪贴板失败: {e}"))?;
    let img = cb
        .get_image()
        .map_err(|_| "剪贴板中没有图片（先截图复制或复制一张图）".to_string())?;
    let bytes = img.bytes.into_owned();
    let img = image::RgbaImage::from_raw(img.width as u32, img.height as u32, bytes)
        .ok_or_else(|| "剪贴板图像数据无效".to_string())?;
    let (w, h) = (img.width(), img.height());
    if w == 0 || h == 0 {
        return Err("剪贴板图片为空".into());
    }
    let mut png = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|e| format!("编码 PNG 失败: {e}"))?;

    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    use std::io::Write;
    use std::process::{Command, Stdio};
    let mut child = Command::new(exe)
        .args(["pin", "--pos", "80,80", "--scale", "1.0"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("启动贴图进程失败: {e}"))?;
    child
        .stdin
        .take()
        .ok_or_else(|| "无法写入贴图进程".to_string())?
        .write_all(&png)
        .map_err(|e| format!("写入贴图进程失败: {e}"))?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

// ---------------------------------------------------------------- 热键

/// 热键字符串解析："Ctrl+Alt+A" → HotKey。
/// 修饰键：Ctrl/Control、Shift、Alt/Option、Super/Win、Meta/Cmd/Command
/// （macOS 的 Command 键对应 Meta）。
/// 裸键（无修饰键）仅允许 PrintScreen 与 F1-F12——裸字母/数字会注册成
/// 全局抢占，打字将无法输入该字符。
pub fn parse_hotkey(s: &str) -> Result<HotKey, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("为空".into());
    }
    let mut mods = Modifiers::empty();
    let mut key: Option<Code> = None;
    for part in s.split('+') {
        let p = part.trim();
        if p.is_empty() {
            return Err("存在空的组合段".into());
        }
        let lower = p.to_ascii_lowercase();
        match lower.as_str() {
            "ctrl" | "control" => mods |= Modifiers::CONTROL,
            "shift" => mods |= Modifiers::SHIFT,
            "alt" | "option" => mods |= Modifiers::ALT,
            "super" | "win" | "windows" => mods |= Modifiers::SUPER,
            "meta" | "cmd" | "command" => mods |= Modifiers::META,
            _ => {
                if key.is_some() {
                    return Err(format!("多个主键「{p}」"));
                }
                match parse_key(&lower) {
                    Some(c) => key = Some(c),
                    None => return Err(format!("无法识别「{p}」")),
                }
            }
        }
    }
    let Some(key) = key else {
        return Err("缺少主键（只有修饰键）".into());
    };
    if mods.is_empty() && !bare_key_allowed(key) {
        return Err("字母/数字等主键必须搭配修饰键（如 Ctrl+Alt+A）".into());
    }
    Ok(HotKey::new(Some(mods), key))
}

/// 允许裸注册的键：PrintScreen 与 F1-F12（不干扰打字）。
fn bare_key_allowed(key: Code) -> bool {
    matches!(
        key,
        Code::PrintScreen
            | Code::F1
            | Code::F2
            | Code::F3
            | Code::F4
            | Code::F5
            | Code::F6
            | Code::F7
            | Code::F8
            | Code::F9
            | Code::F10
            | Code::F11
            | Code::F12
    )
}

const LETTER_CODES: [Code; 26] = [
    Code::KeyA,
    Code::KeyB,
    Code::KeyC,
    Code::KeyD,
    Code::KeyE,
    Code::KeyF,
    Code::KeyG,
    Code::KeyH,
    Code::KeyI,
    Code::KeyJ,
    Code::KeyK,
    Code::KeyL,
    Code::KeyM,
    Code::KeyN,
    Code::KeyO,
    Code::KeyP,
    Code::KeyQ,
    Code::KeyR,
    Code::KeyS,
    Code::KeyT,
    Code::KeyU,
    Code::KeyV,
    Code::KeyW,
    Code::KeyX,
    Code::KeyY,
    Code::KeyZ,
];

const DIGIT_CODES: [Code; 10] = [
    Code::Digit0,
    Code::Digit1,
    Code::Digit2,
    Code::Digit3,
    Code::Digit4,
    Code::Digit5,
    Code::Digit6,
    Code::Digit7,
    Code::Digit8,
    Code::Digit9,
];

fn parse_key(lower: &str) -> Option<Code> {
    if lower.len() == 1 {
        let b = lower.as_bytes()[0];
        if b.is_ascii_lowercase() {
            return Some(LETTER_CODES[(b - b'a') as usize]);
        }
        if b.is_ascii_digit() {
            return Some(DIGIT_CODES[(b - b'0') as usize]);
        }
        return None;
    }
    Some(match lower {
        "f1" => Code::F1,
        "f2" => Code::F2,
        "f3" => Code::F3,
        "f4" => Code::F4,
        "f5" => Code::F5,
        "f6" => Code::F6,
        "f7" => Code::F7,
        "f8" => Code::F8,
        "f9" => Code::F9,
        "f10" => Code::F10,
        "f11" => Code::F11,
        "f12" => Code::F12,
        "printscreen" | "prtsc" | "print" => Code::PrintScreen,
        "space" => Code::Space,
        "tab" => Code::Tab,
        "enter" | "return" => Code::Enter,
        "escape" | "esc" => Code::Escape,
        "backspace" => Code::Backspace,
        "delete" | "del" => Code::Delete,
        "insert" => Code::Insert,
        "home" => Code::Home,
        "end" => Code::End,
        "pageup" => Code::PageUp,
        "pagedown" => Code::PageDown,
        "up" | "arrowup" => Code::ArrowUp,
        "down" | "arrowdown" => Code::ArrowDown,
        "left" | "arrowleft" => Code::ArrowLeft,
        "right" | "arrowright" => Code::ArrowRight,
        "comma" | "," => Code::Comma,
        "period" | "dot" | "." => Code::Period,
        "slash" | "/" => Code::Slash,
        "semicolon" | ";" => Code::Semicolon,
        "quote" | "'" => Code::Quote,
        "backquote" | "grave" | "`" => Code::Backquote,
        "minus" | "-" => Code::Minus,
        "equal" | "=" => Code::Equal,
        "bracketleft" | "[" => Code::BracketLeft,
        "bracketright" | "]" => Code::BracketRight,
        "backslash" | "\\" => Code::Backslash,
        _ => return None,
    })
}

/// 已注册热键集合：热键 → 动作。配置热加载时重建。
/// manager 为 Option：Wayland 等无 X11 会话创建失败时热键整体降级，
/// 托盘本体（D-Bus / 系统 API）不受影响。
pub struct Hotkeys {
    manager: Option<GlobalHotKeyManager>,
    entries: Vec<(HotKey, Action)>,
}

impl Hotkeys {
    fn new() -> Self {
        match GlobalHotKeyManager::new() {
            Ok(m) => Self {
                manager: Some(m),
                entries: Vec::new(),
            },
            Err(e) => {
                eprintln!("lscreen tray: 全局热键不可用（Wayland 会话无 X11？菜单仍可用）: {e}");
                Self {
                    manager: None,
                    entries: Vec::new(),
                }
            }
        }
    }

    fn action_for_id(&self, id: u32) -> Option<Action> {
        self.entries
            .iter()
            .find(|(hk, _)| hk.id() == id)
            .map(|(_, a)| *a)
    }

    /// 按配置注册全部热键。失败的逐条告警（热键被占用是常见场景，
    /// 不应让托盘起不来——§5 风险对策：保留菜单手动入口）。
    fn apply(&mut self, cfg: &Config) {
        let Some(manager) = &self.manager else { return };
        for (hk, _) in self.entries.drain(..) {
            let _ = manager.unregister(hk);
        }
        for (field, raw, action) in [
            (
                "hotkey_screenshot",
                &cfg.hotkey_screenshot,
                Action::Screenshot,
            ),
            ("hotkey_picker", &cfg.hotkey_picker, Action::Picker),
            ("hotkey_pin", &cfg.hotkey_pin, Action::Pin),
        ] {
            let _ = field;
            if raw.trim().is_empty() {
                continue;
            }
            match parse_hotkey(raw) {
                Ok(hk) => match manager.register(hk) {
                    Ok(()) => self.entries.push((hk, action)),
                    Err(e) => {
                        eprintln!("lscreen tray: 热键注册失败（可能被占用）「{raw}」: {e}")
                    }
                },
                Err(e) => eprintln!("lscreen tray: 忽略无效热键「{raw}」: {e}"),
            }
        }
    }
}

/// 菜单文案：带热键后缀（如「截图  Ctrl+Alt+A」）。
fn menu_label(cfg: &Config, a: Action) -> String {
    let base = MENU_ACTIONS
        .iter()
        .find(|(act, _)| *act == a)
        .map(|(_, label)| *label)
        .unwrap_or("");
    let hk = match a {
        Action::Screenshot => &cfg.hotkey_screenshot,
        Action::Picker => &cfg.hotkey_picker,
        Action::Pin => &cfg.hotkey_pin,
        _ => "",
    };
    if hk.trim().is_empty() {
        base.to_string()
    } else {
        format!("{base}  {hk}")
    }
}

fn base_tooltip() -> String {
    format!("LaterScreen {}", env!("CARGO_PKG_VERSION"))
}

// ---------------------------------------------------------------- 图标

const ICON_PNG: &[u8] = include_bytes!("../../../packaging/icon.png");

fn icon_rgba(size: u32) -> Option<image::RgbaImage> {
    let src = image::load_from_memory(ICON_PNG).ok()?.into_rgba8();
    if src.width() == size {
        return Some(src);
    }
    Some(image::imageops::resize(
        &src,
        size,
        size,
        image::imageops::FilterType::Lanczos3,
    ))
}

// ---------------------------------------------------------------- Linux：ksni

#[cfg(target_os = "linux")]
mod linux_impl {
    use super::*;

    use ksni::blocking::TrayMethods;
    use ksni::menu::StandardItem;
    use ksni::{Icon, Tray};
    use std::sync::mpsc::Sender;

    pub struct LscreenTray {
        pub cfg: Config,
        pub tx: Sender<Action>,
    }

    impl Tray for LscreenTray {
        /// 左键单击直接弹菜单（等价右键），降低发现成本；
        /// activate() 仍是宿主不支持时的兜底（直接截图）
        const MENU_ON_ACTIVATE: bool = true;

        fn id(&self) -> String {
            "lscreen".into()
        }

        fn title(&self) -> String {
            "LaterScreen".into()
        }

        fn tool_tip(&self) -> ksni::ToolTip {
            ksni::ToolTip {
                title: "LaterScreen".into(),
                description: base_tooltip(),
                ..Default::default()
            }
        }

        fn icon_pixmap(&self) -> Vec<Icon> {
            // SNI 的 pixmap 是网络字节序 ARGB32
            [32, 64]
                .iter()
                .filter_map(|&s| icon_rgba(s))
                .map(|img| Icon {
                    width: img.width() as i32,
                    height: img.height() as i32,
                    data: rgba_to_argb(&img),
                })
                .collect()
        }

        fn activate(&mut self, _x: i32, _y: i32) {
            let _ = self.tx.send(Action::Screenshot);
        }

        fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
            let items: Vec<_> = MENU_ACTIONS
                .iter()
                .map(|&(a, _)| menu_item(&menu_label(&self.cfg, a), &self.tx, a))
                .collect();
            // 退出与常规动作之间加分隔线
            let mut all = items;
            all.insert(all.len() - 1, ksni::MenuItem::Separator);
            all
        }
    }

    fn menu_item(label: &str, tx: &Sender<Action>, act: Action) -> ksni::MenuItem<LscreenTray> {
        let tx = tx.clone();
        StandardItem {
            label: label.to_string(),
            activate: Box::new(move |_t: &mut LscreenTray| {
                let _ = tx.send(act);
            }),
            ..Default::default()
        }
        .into()
    }

    fn rgba_to_argb(img: &image::RgbaImage) -> Vec<u8> {
        let mut out = Vec::with_capacity(img.len());
        for px in img.pixels() {
            out.extend_from_slice(&[px[3], px[0], px[1], px[2]]);
        }
        out
    }

    pub fn run_linux() -> Result<(), String> {
        let cfg = Config::load();
        let mut hotkeys = Hotkeys::new();
        hotkeys.apply(&cfg);

        let (tx, rx) = std::sync::mpsc::channel::<Action>();
        let handle = LscreenTray { cfg, tx }
            .spawn()
            .map_err(|e| format!("托盘启动失败（D-Bus / StatusNotifierWatcher 不可用）: {e}"))?;

        let mut last_mtime = config_mtime();
        let mut last_poll = std::time::Instant::now();

        'main: loop {
            // 菜单/图标动作
            while let Ok(a) = rx.try_recv() {
                if !dispatch(a) {
                    break 'main;
                }
            }
            // 全局热键（只响应按下）
            while let Ok(ev) = GlobalHotKeyEvent::receiver().try_recv() {
                if ev.state() == HotKeyState::Pressed {
                    if let Some(a) = hotkeys.action_for_id(ev.id()) {
                        if !dispatch(a) {
                            break 'main;
                        }
                    }
                }
            }
            // 配置热加载（面板保存会改 mtime；轮询间隔 1s 足够）
            if last_poll.elapsed() >= std::time::Duration::from_secs(1) {
                last_poll = std::time::Instant::now();
                let mtime = config_mtime();
                if mtime != last_mtime {
                    last_mtime = mtime;
                    let cfg = Config::load();
                    hotkeys.apply(&cfg);
                    handle.update(move |t: &mut LscreenTray| t.cfg = cfg);
                }
            }
            std::thread::park_timeout(std::time::Duration::from_millis(150));
        }
        Ok(())
    }

    fn config_mtime() -> Option<std::time::SystemTime> {
        config::config_path()
            .and_then(|p| p.metadata().ok())
            .and_then(|m| m.modified().ok())
    }
}

// ---------------------------------------------------------------- Win/mac：tray-icon + winit

#[cfg(not(target_os = "linux"))]
mod native_impl {
    use super::*;

    use std::collections::HashMap;
    use tray_icon::menu::{Menu, MenuEvent, MenuItem};
    use tray_icon::{MouseButton, TrayIcon, TrayIconBuilder, TrayIconEvent};
    use winit::application::ApplicationHandler;
    use winit::event::WindowEvent;
    use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
    use winit::window::WindowId;

    enum UserEvent {
        Menu(String),
        TrayClick,
        Hotkey(u32),
    }

    struct NativeApp {
        cfg: Config,
        tray: Option<TrayIcon>,
        menu_items: HashMap<String, MenuItem>,
        hotkeys: Hotkeys,
        last_mtime: Option<std::time::SystemTime>,
        mtime_initialized: bool,
    }

    impl NativeApp {
        /// 托盘必须在事件循环已运行后创建（macOS 全屏应用兼容性要求，
        /// 见 tray-icon 文档），因此放在 resumed 而非 run 之前。
        fn setup(&mut self) {
            let menu = Menu::new();
            let mut items = HashMap::new();
            for &(a, _) in MENU_ACTIONS {
                let id = action_id(a);
                let item = MenuItem::with_id(id.clone(), menu_label(&self.cfg, a), true, None);
                let _ = menu.append(&item);
                items.insert(id, item);
            }
            let icon = icon_rgba(32)
                .and_then(|img| tray_icon::Icon::from_rgba(img.into_raw(), 32, 32).ok())
                .expect("托盘图标解码失败");
            let tray = TrayIconBuilder::new()
                .with_menu(Box::new(menu))
                .with_tooltip(base_tooltip())
                .with_icon(icon)
                .build()
                .expect("创建托盘失败");
            self.hotkeys = Hotkeys::new();
            self.hotkeys.apply(&self.cfg);
            self.tray = Some(tray);
            self.menu_items = items;
        }

        fn handle(&mut self, ev: UserEvent, loop_target: &ActiveEventLoop) {
            let action = match ev {
                // 菜单项 id 与动作的稳定映射
                UserEvent::Menu(id) => match id.as_str() {
                    "shot" => Some(Action::Screenshot),
                    "pick" => Some(Action::Picker),
                    "pin" => Some(Action::Pin),
                    "record" => Some(Action::Record),
                    "config" => Some(Action::Config),
                    "quit" => Some(Action::Quit),
                    _ => None,
                },
                // 左键/双击托盘 = 快速截图（完整菜单在右键）
                UserEvent::TrayClick => Some(Action::Screenshot),
                UserEvent::Hotkey(id) => self.hotkeys.action_for_id(id),
            };
            if let Some(a) = action {
                if !dispatch(a) {
                    loop_target.exit();
                }
            }
        }

        fn tick(&mut self) {
            // 配置热加载
            let mtime = config_mtime();
            let changed = self.mtime_initialized && mtime != self.last_mtime;
            self.mtime_initialized = true;
            self.last_mtime = mtime;
            if changed {
                let cfg = Config::load();
                self.hotkeys.apply(&cfg);
                for &(a, _) in MENU_ACTIONS {
                    if let Some(item) = self.menu_items.get(&action_id(a)) {
                        item.set_text(menu_label(&cfg, a));
                    }
                }
                self.cfg = cfg;
            }
        }
    }

    fn config_mtime() -> Option<std::time::SystemTime> {
        config::config_path()
            .and_then(|p| p.metadata().ok())
            .and_then(|m| m.modified().ok())
    }

    fn action_id(a: Action) -> String {
        match a {
            Action::Screenshot => "shot",
            Action::Picker => "pick",
            Action::Pin => "pin",
            Action::Record => "record",
            Action::Config => "config",
            Action::Quit => "quit",
        }
        .to_string()
    }

    impl ApplicationHandler<UserEvent> for NativeApp {
        fn resumed(&mut self, _event_loop: &ActiveEventLoop) {
            if self.tray.is_none() {
                self.setup();
            }
        }

        fn user_event(&mut self, event_loop: &ActiveEventLoop, ev: UserEvent) {
            self.handle(ev, event_loop);
        }

        fn window_event(
            &mut self,
            _event_loop: &ActiveEventLoop,
            _window_id: WindowId,
            _event: WindowEvent,
        ) {
        }

        fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
            self.tick();
            // 周期醒来驱动 tick（配置热加载）；无事件时随 Wait 休眠，零 CPU
            event_loop.set_control_flow(ControlFlow::WaitUntil(
                std::time::Instant::now() + std::time::Duration::from_secs(1),
            ));
        }
    }

    pub fn run_native() -> Result<(), String> {
        let cfg = Config::load();
        let event_loop = EventLoop::<UserEvent>::with_user_event()
            .build()
            .map_err(|e| format!("创建事件循环失败: {e}"))?;

        // 三个事件源都经 EventLoopProxy 转发进事件循环（tray-icon README
        // 推荐做法），事件到达即唤醒循环，无事件时完全休眠
        let proxy = event_loop.create_proxy();
        {
            let p = proxy.clone();
            MenuEvent::set_event_handler(Some(move |ev: MenuEvent| {
                let _ = p.send_event(UserEvent::Menu(ev.id.0.clone()));
            }));
        }
        {
            let p = proxy.clone();
            TrayIconEvent::set_event_handler(Some(move |ev: TrayIconEvent| {
                let left = matches!(
                    &ev,
                    TrayIconEvent::Click { button, .. } if *button == MouseButton::Left
                ) || matches!(
                    &ev,
                    TrayIconEvent::DoubleClick { button, .. } if *button == MouseButton::Left
                );
                if left {
                    let _ = p.send_event(UserEvent::TrayClick);
                }
            }));
        }
        {
            let p = proxy;
            GlobalHotKeyEvent::set_event_handler(Some(move |ev: GlobalHotKeyEvent| {
                if ev.state() == HotKeyState::Pressed {
                    let _ = p.send_event(UserEvent::Hotkey(ev.id()));
                }
            }));
        }

        let mut app = NativeApp {
            cfg,
            tray: None,
            menu_items: HashMap::new(),
            hotkeys: Hotkeys::new(),
            last_mtime: None,
            mtime_initialized: false,
        };
        event_loop
            .run_app(&mut app)
            .map_err(|e| format!("事件循环异常退出: {e}"))
    }
}

// ---------------------------------------------------------------- 测试

#[cfg(test)]
mod tests {
    use super::parse_hotkey;

    #[test]
    fn hotkey_parse_ok() {
        assert!(parse_hotkey("Ctrl+Alt+A").is_ok());
        assert!(parse_hotkey(" ctrl + a ").is_ok());
        assert!(parse_hotkey("Shift+PrintScreen").is_ok());
        assert!(parse_hotkey("PrintScreen").is_ok()); // 裸 PrintScreen 允许
        assert!(parse_hotkey("F10").is_ok()); // 裸功能键允许
        assert!(parse_hotkey("Cmd+Shift+4").is_ok());
        assert!(parse_hotkey("Super+P").is_ok());
        assert!(parse_hotkey("Ctrl+Comma").is_ok());
        assert!(parse_hotkey("Ctrl+F5").is_ok());
        assert!(parse_hotkey("Alt+0").is_ok());
        assert!(parse_hotkey("Ctrl+ArrowUp").is_ok());
    }

    #[test]
    fn hotkey_parse_reject() {
        assert!(parse_hotkey("").is_err());
        assert!(parse_hotkey("Ctrl").is_err()); // 只有修饰键
        assert!(parse_hotkey("Ctrl++").is_err());
        assert!(parse_hotkey("A").is_err()); // 裸字母（会抢占打字）
        assert!(parse_hotkey("5").is_err()); // 裸数字
        assert!(parse_hotkey("Ctrl+Q+Q").is_err()); // 多主键
        assert!(parse_hotkey("Ctrl+HyperSpace").is_err()); // 未知键
        assert!(parse_hotkey("Ctrl+Alt").is_err());
    }
}
