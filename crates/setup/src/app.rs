//! 安装器 UI：egui 自绘，与主程序同一渲染栈与视觉语言。
//! 单屏安装（无向导翻页）：选项 → 进度 → 完成。

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use eframe::egui::{self, Align, Color32, Layout, RichText};

use crate::win::{self, InstallPlan};
use crate::{embed_ok, BIN};

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// 后台线程 → UI 的进度通道
struct Progress {
    frac: f32,
    msg: String,
    result: Option<Result<(), String>>,
}
type Shared = Arc<Mutex<Progress>>;

#[derive(PartialEq, Clone, Copy)]
enum Page {
    /// 安装：选项；卸载：确认
    Options,
    Working,
    Done,
    Failed,
}

pub struct SetupApp {
    uninstall: bool,
    page: Page,
    dir: String,
    desktop_shortcut: bool,
    start_menu: bool,
    prog: Shared,
    /// Working 期间缓存的进度（避免持锁绘制）
    frac: f32,
    msg: String,
    err: String,
    logo: Option<egui::TextureHandle>,
}

/// 标题旁 logo 与窗口图标共用同一份 PNG（packaging/icon.png，
/// 与 Linux 桌面包/嵌入 exe 的 ICO 同源）。
const LOGO: &[u8] = include_bytes!("../../../packaging/icon.png");

// ---------------------------------------------------------------- 设计令牌
// 与配置面板（app/src/settings_ui.rs）同一套色板，保证两个窗口视觉一致。

/// 品牌强调色
const ACCENT: Color32 = Color32::from_rgb(0xe5, 0x39, 0x35);
/// 窗口底色
const BG: Color32 = Color32::from_rgb(0x14, 0x14, 0x18);
/// 控件描边
const CARD_STROKE: Color32 = Color32::from_rgb(0x2b, 0x2b, 0x35);
/// 次级文字
const MUTED: Color32 = Color32::from_rgb(0x9a, 0x9a, 0xa5);
/// 主文字
const TEXT: Color32 = Color32::from_rgb(0xec, 0xec, 0xf1);
/// 底部按钮条底色
const FOOTER: Color32 = Color32::from_rgb(0x11, 0x11, 0x15);
/// 输入框/次级控件底色
const FIELD: Color32 = Color32::from_rgb(0x16, 0x16, 0x1b);

/// 底部按钮条高度（固定，保证按钮永不被内容挤出窗口）
const FOOTER_H: f32 = 64.0;
/// 按钮统一尺寸：主次同宽同高，只用颜色区分层级
const BTN: egui::Vec2 = egui::vec2(112.0, 34.0);

/// 强制暗色并套用自定义视觉。
///
/// 必须显式定主题：默认 `ThemePreference::System` 下，系统浅色主题会解析出
/// 浅色控件，而 `clear_color` 默认是近黑透明色 —— 那正是"暗底白按钮"的来源。
fn apply_theme(ctx: &egui::Context) {
    let mut style = egui::Style {
        visuals: egui::Visuals::dark(),
        ..Default::default()
    };
    let v = &mut style.visuals;
    v.panel_fill = BG;
    v.selection.bg_fill = ACCENT;
    v.hyperlink_color = ACCENT;
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = egui::CornerRadius::same(6);
        w.fg_stroke.color = TEXT;
    }
    for w in [&mut v.widgets.noninteractive, &mut v.widgets.inactive] {
        w.weak_bg_fill = FIELD;
        w.bg_stroke.color = CARD_STROKE;
    }
    style.spacing.button_padding = egui::vec2(12.0, 6.0);
    style.spacing.interact_size.y = 30.0;
    ctx.set_style_of(egui::Theme::Dark, std::sync::Arc::new(style));
    ctx.set_theme(egui::ThemePreference::Dark);
}

fn logo_rgba() -> Option<(Vec<u8>, u32, u32)> {
    let img = image::load_from_memory(LOGO).ok()?.into_rgba8();
    let (w, h) = (img.width(), img.height());
    Some((img.into_raw(), w, h))
}

pub fn run(uninstall: bool) -> eframe::Result<()> {
    let title = if uninstall {
        "LaterScreen 卸载"
    } else {
        "LaterScreen 安装"
    };
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([470.0, uninstall_wanted(uninstall)])
        .with_min_inner_size([470.0, 280.0])
        .with_resizable(false)
        .with_title(title);
    if let Some((rgba, w, h)) = logo_rgba() {
        viewport = viewport.with_icon(egui::IconData {
            rgba,
            width: w,
            height: h,
        });
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "lscreen-setup",
        options,
        Box::new(move |cc| {
            apply_theme(&cc.egui_ctx);
            // 安装器也是独立进程：UI 中文需要自己挂系统字体
            // （先用 core Renderer 验证字节可解析，epaint 对坏字体是 panic）
            if let Some(bytes) = load_font() {
                setup_fonts(&cc.egui_ctx, bytes);
            }
            Ok(Box::new(SetupApp::new(cc, uninstall)))
        }),
    )
}

fn uninstall_wanted(uninstall: bool) -> f32 {
    // 含 64px 固定底部按钮条
    if uninstall {
        280.0
    } else {
        380.0
    }
}

impl SetupApp {
    fn new(cc: &eframe::CreationContext<'_>, uninstall: bool) -> Self {
        let logo = logo_rgba().map(|(rgba, w, h)| {
            let img = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
            cc.egui_ctx
                .load_texture("logo", img, egui::TextureOptions::default())
        });
        Self {
            uninstall,
            page: Page::Options,
            dir: win::default_dir().display().to_string(),
            desktop_shortcut: true,
            start_menu: true,
            prog: empty_prog(),
            frac: 0.0,
            msg: String::new(),
            err: String::new(),
            logo,
        }
    }

    fn start(&mut self) {
        self.page = Page::Working;
        self.prog = empty_prog();
        let prog = self.prog.clone();
        if self.uninstall {
            std::thread::spawn(move || {
                let r = win::uninstall(|f, m| set_prog(&prog, f, m, None));
                set_prog(&prog, 1.0, "", Some(r));
            });
        } else {
            let plan = InstallPlan {
                dir: PathBuf::from(&self.dir),
                desktop_shortcut: self.desktop_shortcut,
                start_menu: self.start_menu,
            };
            std::thread::spawn(move || {
                let r = win::install(BIN, &plan, VERSION, |f, m| set_prog(&prog, f, m, None));
                set_prog(&prog, 1.0, "", Some(r));
            });
        }
    }

    /// Working 期间轮询后台进度；结果到达时转页
    fn poll(&mut self, ctx: &egui::Context) {
        if self.page != Page::Working {
            return;
        }
        let (frac, msg, result) = {
            let p = self.prog.lock().unwrap();
            (p.frac, p.msg.clone(), p.result.clone())
        };
        self.frac = frac;
        self.msg.clone_from(&msg);
        match result {
            Some(Ok(())) => self.page = Page::Done,
            Some(Err(e)) => {
                self.err = e;
                self.page = Page::Failed;
            }
            None => {
                // 线程仍在跑：保持 ~30fps 刷新进度条
                ctx.request_repaint_after(Duration::from_millis(33));
            }
        }
    }
}

fn empty_prog() -> Shared {
    Arc::new(Mutex::new(Progress {
        frac: 0.0,
        msg: "准备".into(),
        result: None,
    }))
}

fn set_prog(prog: &Shared, frac: f32, msg: &str, result: Option<Result<(), String>>) {
    let mut p = prog.lock().unwrap();
    p.frac = frac;
    if !msg.is_empty() {
        p.msg = msg.to_string();
    }
    if result.is_some() {
        p.result = result;
    }
}

impl eframe::App for SetupApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.poll(ui.ctx());
        if self.page != Page::Working && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }

        // 按钮固定在底部条内，内容再多也不会把按钮挤出窗口
        if let Some(idx) = self.footer(ui) {
            self.on_button(ui, idx);
        }
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(BG)
                    .inner_margin(egui::Margin::symmetric(24, 20)),
            )
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 10.0;
                self.header(ui);
                ui.separator();
                match self.page {
                    Page::Options => self.options_page(ui),
                    Page::Working => self.working_page(ui),
                    Page::Done => self.done_page(ui),
                    Page::Failed => self.failed_page(ui),
                }
            });
    }
}

impl SetupApp {
    /// 当前页的底部按钮组。`(文案, 是否主要动作)`，序号即 on_button 的入参。
    fn buttons(&self) -> &'static [(&'static str, bool)] {
        match self.page {
            Page::Options if self.uninstall => &[("卸载", true), ("取消", false)],
            Page::Options if !embed_ok() => &[("退出", true)],
            Page::Options => &[("安装", true), ("退出", false)],
            Page::Working => &[],
            Page::Done if self.uninstall => &[("关闭", true)],
            Page::Done => &[("运行 LaterScreen", true), ("完成", false)],
            Page::Failed => &[("返回重试", true), ("退出", false)],
        }
    }

    /// 底部固定按钮条；返回被点按钮的序号。
    fn footer(&mut self, ui: &mut egui::Ui) -> Option<usize> {
        let buttons = self.buttons();
        let mut clicked = None;
        egui::Panel::bottom(egui::Id::new("setup-footer"))
            .exact_size(FOOTER_H)
            .frame(
                egui::Frame::new()
                    .fill(FOOTER)
                    .stroke(egui::Stroke::new(1.0, CARD_STROKE))
                    .inner_margin(egui::Margin::symmetric(16, 0)),
            )
            .show(ui, |ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    // right_to_left：主按钮排最右
                    for (tag, (label, primary)) in buttons.iter().enumerate() {
                        let btn = if *primary {
                            egui::Button::new(RichText::new(*label).color(Color32::WHITE).strong())
                                .fill(ACCENT)
                        } else {
                            egui::Button::new(RichText::new(*label).color(MUTED))
                                .stroke(egui::Stroke::new(1.0, CARD_STROKE))
                        };
                        // min_size（而非 add_sized）：长文案自动加宽，不截字
                        if ui.add(btn.min_size(BTN).corner_radius(8)).clicked() {
                            clicked = Some(tag);
                        }
                        ui.add_space(8.0);
                    }
                });
            });
        clicked
    }

    /// 底部按钮点击分发（序号与 buttons() 一一对应）。
    fn on_button(&mut self, ui: &egui::Ui, idx: usize) {
        match self.page {
            Page::Options if self.uninstall => match idx {
                0 => self.start(),
                _ => close(ui),
            },
            Page::Options if !embed_ok() => close(ui),
            Page::Options => match idx {
                0 => self.start(),
                _ => close(ui),
            },
            Page::Working => {}
            Page::Done => {
                if idx == 0 && !self.uninstall {
                    win::launch(&PathBuf::from(&self.dir));
                }
                close(ui);
            }
            Page::Failed => match idx {
                0 => self.page = Page::Options,
                _ => close(ui),
            },
        }
    }
}

impl SetupApp {
    fn header(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if let Some(tex) = &self.logo {
                ui.image((tex.id(), egui::Vec2::splat(40.0)));
            }
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("LaterScreen").size(24.0).strong());
                    ui.label(RichText::new(format!("v{VERSION}")).weak().size(13.0));
                });
                ui.label(
                    RichText::new(if self.uninstall {
                        "卸载将删除程序文件、快捷方式与卸载注册信息"
                    } else {
                        "轻量跨平台截图标注工具（单用户安装，无需管理员权限）"
                    })
                    .weak()
                    .size(12.0),
                );
            });
        });
    }

    fn options_page(&mut self, ui: &mut egui::Ui) {
        if self.uninstall {
            ui.label(format!("安装目录：{}", self.dir));
            return;
        }

        if !embed_ok() {
            ui.label(
                RichText::new("本安装器未内嵌主程序（开发构建）。请从项目发布页下载完整安装包。")
                    .color(ACCENT),
            );
            return;
        }

        ui.label("安装位置：");
        // right_to_left 先放「浏览」按钮，输入框吃剩余宽度 —— 顺排会把按钮挤出窗口
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui
                .add(egui::Button::new("浏览").min_size(egui::vec2(64.0, 30.0)))
                .clicked()
            {
                if let Some(dir) = rfd::FileDialog::new()
                    .set_title("选择安装位置")
                    .pick_folder()
                {
                    self.dir = dir.display().to_string();
                }
            }
            ui.add_space(4.0);
            ui.add(egui::TextEdit::singleline(&mut self.dir).desired_width(f32::INFINITY));
        });
        ui.checkbox(&mut self.desktop_shortcut, "创建桌面快捷方式");
        ui.checkbox(&mut self.start_menu, "创建开始菜单快捷方式");
        ui.label(
            RichText::new("可在 Windows「设置 - 应用」中卸载；重复安装将覆盖更新")
                .color(MUTED)
                .size(12.0),
        );
    }

    fn working_page(&mut self, ui: &mut egui::Ui) {
        ui.add_space(24.0);
        ui.horizontal(|ui| {
            ui.add(egui::Spinner::new());
            ui.label(if self.uninstall {
                "正在卸载…"
            } else {
                "正在安装…"
            });
        });
        ui.add(
            egui::ProgressBar::new(self.frac)
                .text(self.msg.clone())
                .desired_height(20.0),
        );
    }

    fn done_page(&mut self, ui: &mut egui::Ui) {
        ui.add_space(12.0);
        ui.label(
            RichText::new(if self.uninstall {
                "卸载完成"
            } else {
                "安装完成"
            })
            .size(18.0)
            .strong(),
        );
        if !self.uninstall {
            ui.label(
                RichText::new(format!("已安装到 {}", self.dir))
                    .color(MUTED)
                    .size(12.0),
            );
        }
    }

    fn failed_page(&mut self, ui: &mut egui::Ui) {
        ui.add_space(8.0);
        ui.label(
            RichText::new(if self.uninstall {
                "卸载失败"
            } else {
                "安装失败"
            })
            .size(18.0)
            .strong()
            .color(ACCENT),
        );
        ui.label(RichText::new(self.err.clone()).size(13.0));
    }
}

fn close(ui: &egui::Ui) {
    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
}

fn load_font() -> Option<Vec<u8>> {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    for f in ["msyh.ttc", "msyh.ttf", "simhei.ttf", "simsun.ttc"] {
        let p = std::path::Path::new(&root).join("Fonts").join(f);
        if let Ok(bytes) = std::fs::read(&p) {
            if lscreen_core::render::Renderer::new(Some(bytes.clone())).has_font() {
                return Some(bytes);
            }
        }
    }
    None
}

fn setup_fonts(ctx: &egui::Context, bytes: Vec<u8>) {
    let mut fonts = egui::FontDefinitions::default();
    fonts
        .font_data
        .insert("system".into(), egui::FontData::from_owned(bytes).into());
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts
            .families
            .entry(family)
            .or_default()
            .push("system".into());
    }
    ctx.set_fonts(fonts);
}
