//! UI 层：全屏覆盖层应用。状态机：Selecting（框选区域）→ Editing（标注）。

mod canvas;
mod toolbar;

use std::collections::HashMap;

use eframe::egui;
use egui::{Color32, Pos2, TextureHandle, Vec2};
use lscreen_capture::Screenshot;
use lscreen_core::render::Renderer;
use lscreen_core::{Document, P2, RectF, Rgba, Style, Tool};

use crate::export;

/// 图像物理像素 <-> egui 逻辑点 的换算。
#[derive(Clone, Copy)]
pub struct View {
    pub origin: Pos2,
    /// 每逻辑点对应的物理像素数
    pub scale: f32,
}

impl View {
    pub fn to_px(&self, p: Pos2) -> P2 {
        P2::new((p.x - self.origin.x) * self.scale, (p.y - self.origin.y) * self.scale)
    }

    pub fn to_pt(&self, p: P2) -> Pos2 {
        Pos2::new(self.origin.x + p.x / self.scale, self.origin.y + p.y / self.scale)
    }

    pub fn len_pt(&self, px: f32) -> f32 {
        px / self.scale
    }

    pub fn rect_pt(&self, r: RectF) -> egui::Rect {
        egui::Rect::from_min_max(self.to_pt(r.min), self.to_pt(r.max))
    }
}

pub enum Stage {
    Selecting,
    Editing,
}

/// 启动模式：常规截图 / 纯取色器（`lscreen pick`）。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Snip,
    Pick,
}

/// 一次按下-拖拽-释放手势正在进行的操作。
pub enum DragOp {
    /// 框选选区（Selecting 阶段）
    SelectRegion { start: P2 },
    /// 正在绘制的新图元
    Draw { id: u64, start: P2 },
    MoveElem { id: u64, last: P2 },
    ControlPoint { id: u64, idx: usize },
    MoveRegion { last: P2 },
    /// 拖拽选区角点，anchor 为固定的对角
    ResizeRegion { anchor: P2 },
}

pub struct TextEditState {
    pub id: u64,
    pub buffer: String,
    /// 新建的文本（取消时整体撤销）；false 表示编辑既有文本
    pub is_new: bool,
}

pub struct SnipApp {
    pub shot: Screenshot,
    texture: Option<TextureHandle>,
    pub doc: Document,
    pub renderer: Renderer,
    pub mode: Mode,
    pub stage: Stage,
    pub region: RectF,
    pub tool: Tool,
    pub style: Style,
    pub hover: Option<u64>,
    pub selected: Option<u64>,
    pub drag: Option<DragOp>,
    pub text_edit: Option<TextEditState>,
    /// 马赛克预览缓存：id -> (采样点数, 色块)
    pub mosaic_cache: HashMap<u64, (usize, Vec<(f32, f32, f32, Rgba)>)>,
    /// 指针当前所在的图像像素坐标（取色用）
    pub cursor_px: Option<P2>,
    /// 结果面板（二维码/OCR 共用）：(标题, 文本条目)
    pub results_panel: Option<(String, Vec<String>)>,
    toast: Option<(String, f64)>,
    /// 复制/保存出错时置 false 阻止退出
    close_requested: bool,
}

impl SnipApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        shot: Screenshot,
        font: Option<Vec<u8>>,
        mode: Mode,
    ) -> Self {
        if let Some(bytes) = font.clone() {
            setup_fonts(&cc.egui_ctx, bytes);
        }
        Self {
            shot,
            texture: None,
            doc: Document::default(),
            renderer: Renderer::new(font),
            mode,
            stage: Stage::Selecting,
            region: RectF::default(),
            tool: Tool::Select,
            style: Style::default(),
            hover: None,
            selected: None,
            drag: None,
            text_edit: None,
            mosaic_cache: HashMap::new(),
            cursor_px: None,
            results_panel: None,
            toast: None,
            close_requested: false,
        }
    }

    fn texture(&mut self, ctx: &egui::Context) -> TextureHandle {
        if self.texture.is_none() {
            let img = egui::ColorImage::from_rgba_unmultiplied(
                [self.shot.width as usize, self.shot.height as usize],
                &self.shot.rgba,
            );
            // 放大用最近邻（放大镜像素格清晰），常规显示用线性
            let options = egui::TextureOptions {
                magnification: egui::TextureFilter::Nearest,
                minification: egui::TextureFilter::Linear,
                ..Default::default()
            };
            self.texture = Some(ctx.load_texture("screenshot", img, options));
        }
        self.texture.clone().unwrap()
    }

    pub fn toast(&mut self, ctx: &egui::Context, msg: impl Into<String>) {
        self.toast = Some((msg.into(), ctx.input(|i| i.time) + 2.5));
    }

    /// 选区限制在图像范围内。
    pub fn clamp_region(&mut self) {
        let (w, h) = (self.shot.width as f32, self.shot.height as f32);
        self.region.min.x = self.region.min.x.clamp(0.0, w);
        self.region.min.y = self.region.min.y.clamp(0.0, h);
        self.region.max.x = self.region.max.x.clamp(0.0, w);
        self.region.max.y = self.region.max.y.clamp(0.0, h);
    }

    fn compose(&self) -> (Vec<u8>, u32, u32) {
        export::compose(
            &self.renderer,
            &self.shot.rgba,
            self.shot.width,
            self.shot.height,
            &self.doc.elements,
            self.region,
        )
    }

    pub fn copy_and_exit(&mut self, ctx: &egui::Context) {
        let (rgba, w, h) = self.compose();
        match export::copy_to_clipboard(&rgba, w, h) {
            Ok(()) => self.request_close(ctx),
            Err(e) => self.toast(ctx, format!("复制失败: {e}")),
        }
    }

    pub fn save_and_exit(&mut self, ctx: &egui::Context) {
        let (rgba, w, h) = self.compose();
        let path = export::default_save_path();
        match export::save_png(&rgba, w, h, &path) {
            Ok(()) => {
                println!("{}", path.display());
                self.request_close(ctx);
            }
            Err(e) => self.toast(ctx, format!("保存失败: {e}")),
        }
    }

    pub fn request_close(&mut self, ctx: &egui::Context) {
        self.close_requested = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    /// 取色：把指针处颜色以指定格式写入剪贴板。
    /// Pick 模式下复制即完成使命，直接退出。
    pub fn copy_color(&mut self, ctx: &egui::Context, format: ColorFormat) {
        let Some(p) = self.cursor_px else { return };
        let Some(px) = self.shot.pixel(p.x as u32, p.y as u32) else {
            return;
        };
        let c = Rgba(px);
        let (label, text) = match format {
            ColorFormat::Rgb => ("RGB", lscreen_core::color::to_rgb_str(c)),
            ColorFormat::Hex => ("HEX", lscreen_core::color::to_hex(c)),
            ColorFormat::Cmyk => ("CMYK", lscreen_core::color::to_cmyk_str(c)),
        };
        match export::copy_text_to_clipboard(&text) {
            Ok(()) => {
                if self.mode == Mode::Pick {
                    self.request_close(ctx);
                } else {
                    self.toast(ctx, format!("已复制 {label}: {text}"));
                }
            }
            Err(e) => self.toast(ctx, format!("复制失败: {e}")),
        }
    }

    /// 全局快捷键。文本编辑中不响应（把键留给输入框）。
    fn handle_keys(&mut self, ctx: &egui::Context) {
        use egui::{Key, Modifiers};
        if self.text_edit.is_some() {
            return;
        }
        let (undo, redo_y, redo_sz, save, copy, del, enter, esc, rgb, hex, cmyk) =
            ctx.input_mut(|i| {
                (
                    i.consume_key(Modifiers::COMMAND, Key::Z),
                    i.consume_key(Modifiers::COMMAND, Key::Y),
                    i.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z),
                    i.consume_key(Modifiers::COMMAND, Key::S),
                    i.consume_key(Modifiers::COMMAND, Key::C),
                    i.consume_key(Modifiers::NONE, Key::Delete)
                        || i.consume_key(Modifiers::NONE, Key::Backspace),
                    i.consume_key(Modifiers::NONE, Key::Enter),
                    i.consume_key(Modifiers::NONE, Key::Escape),
                    i.consume_key(Modifiers::COMMAND, Key::R),
                    i.consume_key(Modifiers::COMMAND, Key::H),
                    i.consume_key(Modifiers::COMMAND, Key::K),
                )
            });

        if rgb {
            self.copy_color(ctx, ColorFormat::Rgb);
        }
        if hex {
            self.copy_color(ctx, ColorFormat::Hex);
        }
        if cmyk {
            self.copy_color(ctx, ColorFormat::Cmyk);
        }

        if undo {
            self.selected = None;
            self.doc.undo();
        }
        if redo_y || redo_sz {
            self.selected = None;
            self.doc.redo();
        }
        if matches!(self.stage, Stage::Editing) {
            if save {
                self.save_and_exit(ctx);
            }
            if copy || enter {
                self.copy_and_exit(ctx);
            }
            if del {
                if let Some(id) = self.selected.take() {
                    self.doc.begin_change();
                    self.doc.remove(id);
                }
            }
        }
        if esc {
            if self.results_panel.is_some() {
                self.results_panel = None;
            } else if self.selected.is_some() {
                self.selected = None;
            } else {
                self.request_close(ctx);
            }
        }
    }

    /// 扫描当前选区内的二维码，结果放入面板。
    pub fn scan_qr(&mut self, ctx: &egui::Context) {
        let (rgba, w, h) = export::crop_rgba(
            &self.shot.rgba,
            self.shot.width,
            self.shot.height,
            self.region,
        );
        let found = lscreen_core::qr::detect(&rgba, w, h);
        if found.is_empty() {
            self.toast(ctx, "选区内未识别到二维码");
        } else {
            self.results_panel = Some((
                "二维码识别结果".into(),
                found.into_iter().map(|r| r.content).collect(),
            ));
        }
    }

    /// 识别当前选区内的文字。同步调用，大图会短暂卡顿（异步化排在 M5）。
    pub fn scan_ocr(&mut self, ctx: &egui::Context) {
        let engine = lscreen_ocr::default_engine(&[]);
        if !engine.available() {
            self.toast(ctx, engine.describe());
            return;
        }
        let (rgba, w, h) = export::crop_rgba(
            &self.shot.rgba,
            self.shot.width,
            self.shot.height,
            self.region,
        );
        match engine.recognize(&rgba, w, h) {
            Ok(out) if !out.is_empty() => {
                self.results_panel = Some(("文字识别结果".into(), vec![out.plain_text()]));
            }
            Ok(_) => self.toast(ctx, "选区内未识别到文字"),
            Err(e) => self.toast(ctx, format!("识别失败: {e}")),
        }
    }

    fn show_results(&mut self, ctx: &egui::Context) {
        let Some((title, items)) = self.results_panel.clone() else {
            return;
        };
        let mut open = true;
        egui::Window::new(title)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.set_max_width(460.0);
                egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                    for (i, content) in items.iter().enumerate() {
                        if i > 0 {
                            ui.separator();
                        }
                        ui.label(egui::RichText::new(truncate(content, 600)).monospace());
                        if ui.button("复制内容").clicked() {
                            match export::copy_text_to_clipboard(content) {
                                Ok(()) => self.toast(ctx, "已复制"),
                                Err(e) => self.toast(ctx, format!("复制失败: {e}")),
                            }
                        }
                    }
                });
            });
        if !open {
            self.results_panel = None;
        }
    }

    fn show_toast(&mut self, ctx: &egui::Context) {
        let Some((msg, until)) = &self.toast else {
            return;
        };
        if ctx.input(|i| i.time) > *until {
            self.toast = None;
            return;
        }
        let msg = msg.clone();
        egui::Area::new(egui::Id::new("toast"))
            .anchor(egui::Align2::CENTER_BOTTOM, Vec2::new(0.0, -48.0))
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style())
                    .fill(Color32::from_black_alpha(200))
                    .show(ui, |ui| {
                        ui.colored_label(Color32::WHITE, msg);
                    });
            });
        ctx.request_repaint();
    }
}

impl eframe::App for SnipApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if self.close_requested {
            return;
        }
        self.handle_keys(&ctx);

        let texture = self.texture(&ctx);
        canvas::show(self, ui, &texture);

        if self.mode == Mode::Snip && matches!(self.stage, Stage::Editing) {
            toolbar::show(self, &ctx);
        }
        canvas::show_text_editor(self, &ctx);
        self.show_results(&ctx);
        self.show_toast(&ctx);
    }
}

/// 取色输出格式。
#[derive(Clone, Copy)]
pub enum ColorFormat {
    Rgb,
    Hex,
    Cmyk,
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max).collect();
        format!("{cut}…")
    }
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

pub fn egui_color(c: Rgba) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), c.a())
}
