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
    if uninstall {
        250.0
    } else {
        340.0
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

        egui::CentralPanel::default()
            .frame(egui::Frame::new().inner_margin(egui::Margin::same(24)))
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
            ui.add_space(12.0);
            match bottom_buttons(ui, &[("卸载", true), ("取消", false)]) {
                Some(0) => self.start(),
                Some(_) => close(ui),
                None => {}
            }
            return;
        }

        if !embed_ok() {
            ui.label(
                RichText::new("本安装器未内嵌主程序（开发构建）。请从项目发布页下载完整安装包。")
                    .color(Color32::from_rgb(0xe5, 0x39, 0x35)),
            );
            if bottom_buttons(ui, &[("退出", true)]).is_some() {
                close(ui);
            }
            return;
        }

        ui.label("安装位置：");
        ui.horizontal(|ui| {
            let edit = egui::TextEdit::singleline(&mut self.dir).desired_width(f32::INFINITY);
            ui.add(edit);
            if ui.button("浏览").clicked() {
                if let Some(dir) = rfd::FileDialog::new()
                    .set_title("选择安装位置")
                    .pick_folder()
                {
                    self.dir = dir.display().to_string();
                }
            }
        });
        ui.checkbox(&mut self.desktop_shortcut, "创建桌面快捷方式");
        ui.checkbox(&mut self.start_menu, "创建开始菜单快捷方式");
        ui.label(
            RichText::new("可在 Windows「设置 - 应用」中卸载；重复安装将覆盖更新")
                .weak()
                .size(12.0),
        );

        match bottom_buttons(ui, &[("安装", true), ("退出", false)]) {
            Some(0) => self.start(),
            Some(_) => close(ui),
            None => {}
        }
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
                    .weak()
                    .size(12.0),
            );
        }
        let buttons: &[(&str, bool)] = if self.uninstall {
            &[("关闭", true)]
        } else {
            &[("运行 LaterScreen", true), ("完成", false)]
        };
        match bottom_buttons(ui, buttons) {
            Some(0) => {
                if !self.uninstall {
                    win::launch(&PathBuf::from(&self.dir));
                }
                close(ui);
            }
            Some(_) => close(ui),
            None => {}
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
            .color(Color32::from_rgb(0xe5, 0x39, 0x35)),
        );
        ui.label(RichText::new(self.err.clone()).size(13.0));
        match bottom_buttons(ui, &[("返回重试", true), ("退出", false)]) {
            Some(0) => self.page = Page::Options,
            Some(_) => close(ui),
            None => {}
        }
    }
}

fn close(ui: &egui::Ui) {
    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
}

/// 底部右侧按钮组。`(文案, 是否主要动作)`；返回被点按钮的序号。
/// 主要动作按钮加最小宽度，视觉权重高于次要动作。
fn bottom_buttons(ui: &mut egui::Ui, buttons: &[(&str, bool)]) -> Option<usize> {
    let mut clicked = None;
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        for (tag, (label, primary)) in buttons.iter().enumerate() {
            let min_w = if *primary { 130.0 } else { 84.0 };
            if ui
                .add_sized([min_w, 30.0], egui::Button::new(*label))
                .clicked()
            {
                clicked = Some(tag);
            }
        }
    });
    clicked
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
