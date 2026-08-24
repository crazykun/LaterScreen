//! UI 层：全屏覆盖层应用。状态机：Selecting（框选区域）→ Editing（标注）。

mod canvas;
pub(crate) mod toolbar;

use std::collections::HashMap;

use eframe::egui;
use egui::{Color32, Pos2, TextureHandle, Vec2};
use lscreen_capture::Screenshot;
use lscreen_core::render::Renderer;
use lscreen_core::{Document, ElementKind, RectF, Rgba, Style, Tool, P2};

use crate::config::Config;
use crate::export;
use crate::history;
use std::sync::{Arc, Mutex};

/// 交互框选出的区域（`record --select` 用）：覆盖层写入，主流程读取。
pub type SharedRegion = Arc<Mutex<Option<RectF>>>;

/// 图像物理像素 <-> egui 逻辑点 的换算。
#[derive(Clone, Copy)]
pub struct View {
    pub origin: Pos2,
    /// 每逻辑点对应的物理像素数
    pub scale: f32,
}

impl View {
    pub fn to_px(self, p: Pos2) -> P2 {
        P2::new(
            (p.x - self.origin.x) * self.scale,
            (p.y - self.origin.y) * self.scale,
        )
    }

    pub fn to_pt(self, p: P2) -> Pos2 {
        Pos2::new(
            self.origin.x + p.x / self.scale,
            self.origin.y + p.y / self.scale,
        )
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

/// 启动模式：常规截图 / 纯取色器（`lscreen pick`）/ 录屏框选（`record --select`）。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Snip,
    Pick,
    /// 框选完成后不进标注，而是把区域写入 record_region 并关窗
    Record,
}

/// 一次按下-拖拽-释放手势正在进行的操作。
pub enum DragOp {
    /// 框选选区（Selecting 阶段）
    SelectRegion {
        start: P2,
    },
    /// 正在绘制的新图元
    Draw {
        id: u64,
        start: P2,
    },
    MoveElem {
        id: u64,
        last: P2,
        /// 是否已压撤销快照。首次真实位移才 begin_change：
        /// 点选（press 即 release）不应清空重做栈
        began: bool,
    },
    ControlPoint {
        id: u64,
        idx: usize,
        /// 同 MoveElem::began
        began: bool,
    },
    MoveRegion {
        last: P2,
    },
    /// 拖拽选区角点，anchor 为固定的对角
    ResizeRegion {
        anchor: P2,
    },
    /// 拖拽选区单条边（0=左 1=上 2=右 3=下）
    ResizeEdge {
        edge: usize,
    },
}

pub struct TextEditState {
    pub id: u64,
    pub buffer: String,
    /// 新建的文本（取消时整体撤销）；false 表示编辑既有文本
    pub is_new: bool,
}

/// 一个可吸附选区的窗口矩形（图像像素坐标，已与屏幕求交）。
#[derive(Clone)]
pub struct WinRect {
    pub title: String,
    pub rect: RectF,
}

/// 覆盖层启动参数（M9 收敛 new 的参数个数）。
pub struct OverlayInit {
    pub shot: Screenshot,
    pub font: Option<Vec<u8>>,
    pub mode: Mode,
    pub config: Config,
    /// Mode::Record 专用：框选完成后的输出通道
    pub record_region: Option<SharedRegion>,
    /// 窗口吸附列表（Z 序自顶向下，图像像素坐标；空 = 降级纯手动框选）
    pub windows: Vec<WinRect>,
    /// 初始预选窗口（仅配置为「最前窗口」时由调用方算好传入）
    pub initial_region: Option<WinRect>,
    /// 标注预览模式（滚动长截图收尾）：普通窗口 + 可滚动画布 + 底部
    /// 标注工具栏，直接进 Editing、选区 = 整图
    pub preview: bool,
}

pub struct SnipApp {
    pub shot: Screenshot,
    texture: Option<TextureHandle>,
    pub doc: Document,
    pub renderer: Renderer,
    pub mode: Mode,
    pub stage: Stage,
    pub region: RectF,
    /// Selecting 阶段的候选选区（M9）：初始 = 最前窗口/全屏（按配置），
    /// 悬停单击窗口时更新；确认后进 Editing
    pub sel_window: Option<WinRect>,
    /// 窗口矩形列表（Z 序自顶向下，图像像素坐标）。开窗前采集，
    /// 之后冻结——覆盖层自身与贴图窗口已被 capture 层排除
    pub windows: Vec<WinRect>,
    pub tool: Tool,
    pub style: Style,
    pub hover: Option<u64>,
    pub selected: Option<u64>,
    pub drag: Option<DragOp>,
    pub text_edit: Option<TextEditState>,
    /// 马赛克预览缓存：图元 id → (几何指纹, 色块列表)。指纹见 `canvas::mosaic_key`。
    pub mosaic_cache: MosaicCache,
    /// 自定义取色器上次变更时刻：拖动取色时节流撤销快照（同滑杆）
    pub color_drag_at: f64,
    /// 指针当前所在的图像像素坐标（取色用）
    pub cursor_px: Option<P2>,
    /// 工具栏尺寸输入的未提交值：编辑中（聚焦/拖拽）暂存，结束时一次性应用
    pub size_edit_buf: Option<(f32, f32)>,
    /// 结果面板（二维码/OCR 共用）：(标题, 文本条目)
    pub results_panel: Option<(String, Vec<String>)>,
    /// 后台识别任务（OCR/QR 在线程中跑，避免 UI 假死）
    scan_job: Option<std::sync::mpsc::Receiver<ScanOutcome>>,
    toast: Option<(String, f64)>,
    /// 复制/保存出错时置 false 阻止退出
    close_requested: bool,
    /// 用户配置（默认工具/颜色/线宽、复制后是否自动退出、保存后是否开目录）
    pub config: Config,
    /// Mode::Record 专用：框选完成后的输出通道
    pub record_region: Option<SharedRegion>,
    /// 标注预览模式（滚动长截图）：窗口非全屏、画布可滚动、工具栏锚窗口底部
    pub preview: bool,
    /// 预览画布缩放；0.0 = 首帧按窗口宽自适应（≤100%）
    pub preview_zoom: f32,
    /// 最近一帧画布的视图映射（文本编辑器定位用——预览模式下
    /// ctx.content_rect 与画布 rect 不再相等）
    pub last_view: View,
}

impl SnipApp {
    pub fn new(cc: &eframe::CreationContext<'_>, init: OverlayInit) -> Self {
        crate::apply_window_class(cc);
        let OverlayInit {
            shot,
            font,
            mode,
            config,
            record_region,
            windows,
            initial_region,
            preview,
        } = init;
        // 先用 Renderer 解析一遍：epaint 内部同样用 ab_glyph，且解析失败是 panic!
        // 而非 Err。这里只把已确认可解析的字节交给 egui，坏字体退回内置字体。
        let renderer = Renderer::new(font.clone());
        if renderer.has_font() {
            if let Some(bytes) = font {
                crate::font::setup_egui_fonts(&cc.egui_ctx, bytes);
            }
        }
        // 默认工具与样式来自配置（M8）
        let tool = config.tool();
        let style = config.style();
        // 初始选区（M9）：Pick 模式不预选；Snip/Record 按配置（窗口/全屏/无）
        let sel_window = if mode == Mode::Pick {
            None
        } else {
            match config.initial_selection() {
                crate::config::InitialSelection::Window => initial_region,
                crate::config::InitialSelection::Fullscreen => Some(WinRect {
                    title: "全屏".to_string(),
                    rect: RectF::from_points(
                        P2::new(0.0, 0.0),
                        P2::new(shot.width as f32, shot.height as f32),
                    ),
                }),
                crate::config::InitialSelection::None => None,
            }
        };
        // 预览模式直接进标注：选区 = 整图（保存/复制/OCR 都作用于全图）
        let (stage, region) = if preview {
            (
                Stage::Editing,
                RectF::from_points(
                    P2::new(0.0, 0.0),
                    P2::new(shot.width as f32, shot.height as f32),
                ),
            )
        } else {
            (Stage::Selecting, RectF::default())
        };
        Self {
            shot,
            texture: None,
            doc: Document::default(),
            renderer,
            mode,
            stage,
            region,
            sel_window,
            windows,
            tool,
            style,
            hover: None,
            selected: None,
            drag: None,
            text_edit: None,
            mosaic_cache: HashMap::new(),
            color_drag_at: f64::NEG_INFINITY,
            cursor_px: None,
            size_edit_buf: None,
            results_panel: None,
            scan_job: None,
            toast: None,
            close_requested: false,
            config,
            record_region,
            preview,
            preview_zoom: 0.0,
            last_view: View {
                origin: Pos2::ZERO,
                scale: 1.0,
            },
        }
    }

    fn texture(&mut self, ctx: &egui::Context) -> TextureHandle {
        if self.texture.is_none() {
            // GPU 单纹理有尺寸上限（常见 8192/16384），滚动长截图可能超高——
            // 超限纹理会上传失败或被驱动静默裁剪。超限时显示用整数因子降采样
            // 兜底（画布把纹理拉伸到目标 rect，坐标映射不受影响）；
            // 保存/复制/OCR 仍走全分辨率 shot.rgba
            const MAX_TEX: usize = 8192;
            let (w, h) = (self.shot.width as usize, self.shot.height as usize);
            let factor = w.max(h).div_ceil(MAX_TEX).max(1);
            let img = if factor == 1 {
                egui::ColorImage::from_rgba_unmultiplied([w, h], &self.shot.rgba)
            } else {
                let (dw, dh) = (w.div_ceil(factor), h.div_ceil(factor));
                let mut pixels = Vec::with_capacity(dw * dh);
                for y in (0..h).step_by(factor) {
                    for x in (0..w).step_by(factor) {
                        let i = (y * w + x) * 4;
                        let p = &self.shot.rgba[i..i + 4];
                        pixels.push(egui::Color32::from_rgba_unmultiplied(
                            p[0], p[1], p[2], p[3],
                        ));
                    }
                }
                egui::ColorImage::new([dw, dh], pixels)
            };
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

    /// 设定选区尺寸（物理像素），左上角保持不动；右/下越界时整体回挪，
    /// 保证请求的尺寸不被裁剪——固定尺寸截图的关键语义。
    pub fn set_region_size(&mut self, w: f32, h: f32) {
        let (sw, sh) = (self.shot.width as f32, self.shot.height as f32);
        let w = w.clamp(1.0, sw);
        let h = h.clamp(1.0, sh);
        self.region.min.x = self.region.min.x.min(sw - w).max(0.0);
        self.region.min.y = self.region.min.y.min(sh - h).max(0.0);
        self.region.max.x = self.region.min.x + w;
        self.region.max.y = self.region.min.y + h;
    }

    /// 选区确认：Snip 进标注阶段；Record 交付区域并关窗。
    pub fn confirm_region(&mut self, ctx: &egui::Context) {
        self.sel_window = None;
        if self.mode == Mode::Record {
            if let Some(shared) = &self.record_region {
                *shared.lock().unwrap() = Some(self.region);
            }
            self.request_close(ctx);
        } else {
            self.stage = Stage::Editing;
        }
    }

    fn compose(&self) -> Option<(Vec<u8>, u32, u32)> {
        export::compose(
            &self.renderer,
            &self.shot.rgba,
            self.shot.width,
            self.shot.height,
            &self.doc.elements,
            self.region,
        )
    }

    fn compose_for_export(&mut self) -> Option<(Vec<u8>, u32, u32)> {
        // 工具栏按钮可在 TextEdit 失焦回调前触发；导出边界必须主动提交，
        // 否则 compose 只会看到图元里的旧内容，静默丢掉当前输入缓冲。
        self.commit_text_edit();
        self.compose()
    }

    pub fn copy_and_exit(&mut self, ctx: &egui::Context) {
        let Some((rgba, w, h)) = self.compose_for_export() else {
            self.toast(ctx, "选区为空");
            return;
        };
        match export::copy_to_clipboard(&rgba, w, h) {
            Ok(()) => {
                if self.config.copy_auto_exit {
                    self.request_close(ctx);
                } else {
                    self.toast(ctx, "已复制到剪贴板");
                }
            }
            Err(e) => self.toast(ctx, format!("复制失败: {e}")),
        }
    }

    pub fn save_and_exit(&mut self, ctx: &egui::Context) {
        let Some((rgba, w, h)) = self.compose_for_export() else {
            self.toast(ctx, "选区为空");
            return;
        };
        let path = export::save_path(&self.config, "png");
        match export::save_png(&rgba, w, h, &path) {
            Ok(saved) => {
                println!("{}", saved.display());
                history::record_file(&saved, history::Kind::Shot, Some(&saved));
                if self.config.open_dir_after_save {
                    if let Some(dir) = saved.parent() {
                        export::open_in_file_manager(dir);
                    }
                }
                self.request_close(ctx);
            }
            Err(e) => self.toast(ctx, format!("保存失败: {e}")),
        }
    }

    /// 贴图：把当前选区钉在屏幕上（独立进程），本窗口随即退出。
    /// 图片经 stdin 以 PNG 传入子进程，免临时文件与清理问题；
    /// 写完再退出，保证子进程读到完整数据。
    pub fn pin_and_exit(&mut self, ctx: &egui::Context) {
        let Some((rgba, w, h)) = self.compose_for_export() else {
            self.toast(ctx, "选区为空");
            return;
        };
        // 贴图自动入历史（M11 用户定稿）：贴图本来不落盘，这条是「贴图历史」
        // 的唯一来源，在 spawn 贴图进程之前记录（即便贴图失败也保留历史）
        history::record_rgba(&rgba, w, h, history::Kind::Pin, None);
        let img = image::RgbaImage::from_raw(w, h, rgba)
            .ok_or_else(|| "invalid image buffer".to_string());
        let mut png = Vec::new();
        let enc = img.and_then(|img| {
            img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
                .map_err(|e| e.to_string())
        });
        if let Err(e) = enc {
            self.toast(ctx, format!("编码失败: {e}"));
            return;
        }
        let scale = if self.shot.scale > 0.0 {
            self.shot.scale
        } else {
            1.0
        };
        // 屏幕坐标 = 显示器原点 + 选区内偏移（region 是图像像素坐标）
        let (ox, oy) = self.shot.origin;
        let (x, y) = (
            (ox as f32 + self.region.min.x) / scale,
            (oy as f32 + self.region.min.y) / scale,
        );
        match spawn_pin(&png, x, y, scale) {
            Ok(()) => self.request_close(ctx),
            Err(e) => self.toast(ctx, format!("贴图失败: {e}")),
        }
    }

    pub fn request_close(&mut self, ctx: &egui::Context) {
        self.close_requested = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    /// 提交文本编辑：写回内容与包围盒；空内容按新建/既有分别撤销或删除。
    /// 编辑器内「确定」与画布点击外部共用此出口。
    pub fn commit_text_edit(&mut self) {
        let Some(edit) = self.text_edit.take() else {
            return;
        };
        let content = edit.buffer.trim_end().to_string();
        let font_size = self.doc.get(edit.id).map(|e| e.style.font_size);
        if content.is_empty() {
            if edit.is_new {
                self.doc.cancel_change();
            } else {
                // 既有文本清空 = 删除
                self.doc.remove(edit.id);
            }
            return;
        }
        let Some(font_size) = font_size else { return };
        let (w, h) = self.renderer.measure_text(&content, font_size);
        if let Some(e) = self.doc.get_mut(edit.id) {
            if let ElementKind::Text {
                content: c, size, ..
            } = &mut e.kind
            {
                *c = content;
                *size = P2::new(w.max(10.0), h.max(font_size));
            }
        }
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
    /// 结果面板打开时只保留 Esc（关闭面板），防止阅读识别结果时
    /// 误触 Ctrl+C/Enter 把整个截图窗口关掉。
    fn handle_keys(&mut self, ctx: &egui::Context) {
        use egui::{Key, Modifiers};
        if self.text_edit.is_some() {
            return;
        }
        if self.results_panel.is_some() {
            let esc = ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape));
            if esc {
                self.results_panel = None;
            }
            return;
        }
        let wants_keyboard = ctx.egui_wants_keyboard_input();
        let (undo, redo_y, redo_sz, save, copy, pin, del, enter, esc, rgb, hex, cmyk) = ctx
            .input_mut(|i| {
                (
                    i.consume_key(Modifiers::COMMAND, Key::Z),
                    i.consume_key(Modifiers::COMMAND, Key::Y),
                    i.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z),
                    i.consume_key(Modifiers::COMMAND, Key::S),
                    i.consume_key(Modifiers::COMMAND, Key::C),
                    i.consume_key(Modifiers::COMMAND, Key::P),
                    !wants_keyboard
                        && (i.consume_key(Modifiers::NONE, Key::Delete)
                            || i.consume_key(Modifiers::NONE, Key::Backspace)),
                    !wants_keyboard && i.consume_key(Modifiers::NONE, Key::Enter),
                    !wants_keyboard && i.consume_key(Modifiers::NONE, Key::Escape),
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
            if pin {
                self.pin_and_exit(ctx);
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
        } else if matches!(self.stage, Stage::Selecting) && (enter || copy) {
            // 初始选区已就位：Enter/Ctrl+C 一步出图——Snip 复制退出，Record 交付区域
            if let Some(sel) = self.sel_window.clone() {
                self.region = sel.rect;
                if self.mode == Mode::Record {
                    if let Some(shared) = &self.record_region {
                        *shared.lock().unwrap() = Some(self.region);
                    }
                    self.request_close(ctx);
                } else {
                    self.copy_and_exit(ctx);
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

    /// 扫描当前选区内的二维码（后台线程）。
    pub fn scan_qr(&mut self, ctx: &egui::Context) {
        if self.scan_job.is_some() {
            self.toast(ctx, "识别进行中…");
            return;
        }
        let Some((rgba, w, h)) = export::crop_rgba(
            &self.shot.rgba,
            self.shot.width,
            self.shot.height,
            self.region,
        ) else {
            self.toast(ctx, "选区为空");
            return;
        };
        self.spawn_scan(ctx, move || {
            let found = lscreen_core::qr::detect(&rgba, w, h);
            if found.is_empty() {
                ScanOutcome::Empty("选区内未识别到二维码".into())
            } else {
                ScanOutcome::Ok(
                    "二维码识别结果".into(),
                    found.into_iter().map(|r| r.content).collect(),
                )
            }
        });
    }

    /// 识别当前选区内的文字（后台线程，tesseract 大图可达秒级）。
    pub fn scan_ocr(&mut self, ctx: &egui::Context) {
        if self.scan_job.is_some() {
            self.toast(ctx, "识别进行中…");
            return;
        }
        let engine = lscreen_ocr::default_engine(&[]);
        if !engine.available() {
            self.toast(ctx, engine.describe());
            return;
        }
        let Some((rgba, w, h)) = export::crop_rgba(
            &self.shot.rgba,
            self.shot.width,
            self.shot.height,
            self.region,
        ) else {
            self.toast(ctx, "选区为空");
            return;
        };
        self.spawn_scan(ctx, move || match engine.recognize(&rgba, w, h) {
            Ok(out) if !out.is_empty() => {
                ScanOutcome::Ok("文字识别结果".into(), vec![out.plain_text()])
            }
            Ok(_) => ScanOutcome::Empty("选区内未识别到文字".into()),
            Err(e) => ScanOutcome::Empty(format!("识别失败: {e}")),
        });
    }

    fn spawn_scan(
        &mut self,
        ctx: &egui::Context,
        job: impl FnOnce() -> ScanOutcome + Send + 'static,
    ) {
        let (tx, rx) = std::sync::mpsc::channel();
        self.scan_job = Some(rx);
        self.toast(ctx, "识别中…");
        let repaint = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(job());
            repaint.request_repaint();
        });
    }

    /// 每帧轮询后台识别结果。
    fn poll_scan(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.scan_job else { return };
        match rx.try_recv() {
            Ok(ScanOutcome::Ok(title, items)) => {
                self.scan_job = None;
                self.toast = None;
                self.results_panel = Some((title, items));
            }
            Ok(ScanOutcome::Empty(msg)) => {
                self.scan_job = None;
                self.toast(ctx, msg);
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.scan_job = None;
                self.toast(ctx, "识别线程异常退出");
            }
        }
    }

    fn show_results(&mut self, ctx: &egui::Context) {
        /// 结果面板右下角悬浮复制按钮的尺寸（固定，因需先算位置再放控件）
        const FLOAT_BTN: egui::Vec2 = egui::vec2(88.0, 26.0);

        let Some((title, items)) = self.results_panel.clone() else {
            return;
        };
        let mut open = true;
        // 可拖动 + 可缩放：结果框默认落在选区中央，挡住的正是刚识别的那段原文。
        // 首帧居中用 default_pos + pivot 而非 anchor——anchor 会每帧把位置写回，
        // 拖拽位移当帧即被覆盖（等于拖不动）；constrain 防止拖出屏幕外再抓不回来。
        egui::Window::new(title)
            .collapsible(false)
            .resizable(true)
            .constrain(true)
            .default_pos(ctx.input(|i| i.viewport_rect()).center())
            .pivot(egui::Align2::CENTER_CENTER)
            .default_width(460.0)
            .min_width(260.0)
            .default_height(340.0)
            .open(&mut open)
            .show(ctx, |ui| {
                let multi = items.len() > 1;
                // 撑满窗口而非收缩到内容：高度随用户拉伸走（原先固定 320 上限，
                // 长文本 OCR 结果拉大窗口也看不到更多）
                let scroll = egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for (i, content) in items.iter().enumerate() {
                            if i > 0 {
                                ui.separator();
                            }
                            // 多条（QR 可一次识出多个码）才需要逐条复制，按钮放各条
                            // 文本上方；单条由右下角悬浮按钮负责，不再重复
                            if multi {
                                ui.horizontal(|ui| {
                                    ui.label(format!("#{}", i + 1));
                                    if ui.button("复制").clicked() {
                                        match export::copy_text_to_clipboard(content) {
                                            Ok(()) => self.toast(ctx, "已复制"),
                                            Err(e) => self.toast(ctx, format!("复制失败: {e}")),
                                        }
                                    }
                                });
                            }
                            // 上限放宽到 4000 字：窗口可拉伸后 600 字反倒成了瓶颈
                            // （egui 会为不可见文本也做布局，故仍留上限防大段文本卡顿）。
                            // 复制走的是原始 content，不受此显示上限影响
                            ui.label(egui::RichText::new(truncate(content, 4000)).monospace());
                            // 末条留出悬浮按钮的高度，避免尾行被按钮压住
                            if i + 1 == items.len() {
                                ui.add_space(FLOAT_BTN.y + 6.0);
                            }
                        }
                    });
                // 复制按钮悬浮在文本区右下角：跟在文本后面时会被长文本推到滚动区
                // 最底部（要滚到底才点得到），钉顶栏又太抢眼。这里按帧取滚动区实际
                // 矩形定位，故窗口拖动/缩放都跟着走；画在滚动内容之后 → 叠在其上，
                // 且同层后绘者优先命中，不会被滚动区的拖拽滚动抢掉点击。
                let anchor = scroll.inner_rect.max - FLOAT_BTN - egui::vec2(8.0, 8.0);
                let label = if multi {
                    "复制全部"
                } else {
                    "复制内容"
                };
                if ui
                    .put(
                        egui::Rect::from_min_size(anchor, FLOAT_BTN),
                        egui::Button::new(label),
                    )
                    .clicked()
                {
                    // 多条结果按换行拼接，直接粘贴即为逐行
                    let all = items.join("\n");
                    match export::copy_text_to_clipboard(&all) {
                        Ok(()) => self.toast(ctx, "已复制"),
                        Err(e) => self.toast(ctx, format!("复制失败: {e}")),
                    }
                }
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
        self.poll_scan(&ctx);

        let texture = self.texture(&ctx);
        if self.preview {
            canvas::show_scrolled(self, ui, &texture);
        } else {
            canvas::show(self, ui, &texture);
        }

        if self.mode == Mode::Snip && matches!(self.stage, Stage::Editing) {
            toolbar::show(self, &ctx);
        }
        canvas::show_text_editor(self, &ctx);
        self.show_results(&ctx);
        self.show_toast(&ctx);
    }
}

/// 后台识别任务的结果。
enum ScanOutcome {
    /// (面板标题, 条目)
    Ok(String, Vec<String>),
    /// 无结果或失败，toast 提示
    Empty(String),
}

/// 取色输出格式。
#[derive(Clone, Copy)]
pub enum ColorFormat {
    Rgb,
    Hex,
    Cmyk,
}

/// 一个马赛克预览色块：(x, y, 边长, 均值色)。
pub type MosaicCell = (f32, f32, f32, Rgba);
/// 马赛克预览缓存：图元 id → (几何指纹, 色块列表)。指纹见 `canvas::mosaic_key`。
pub type MosaicCache = HashMap<u64, (u64, Vec<MosaicCell>)>;

/// 拉起独立的贴图进程：stdin 写入 PNG 后即返回（不 wait——
/// 贴图进程独立存活，父进程退出后被 init 收养）。
pub(crate) fn spawn_pin(png: &[u8], x: f32, y: f32, scale: f32) -> Result<(), String> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut child = Command::new(exe)
        .args([
            "pin",
            "--pos",
            &format!("{x},{y}"),
            "--scale",
            &scale.to_string(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("启动贴图进程失败: {e}"))?;
    // 写完再退出：子进程读 stdin 到 EOF 才建窗，这里阻塞写不会死锁
    // （管道 64KB，子进程在读；PNG 编码在内存中已完成）
    child
        .stdin
        .take()
        .ok_or_else(|| "无法写入贴图进程".to_string())?
        .write_all(png)
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max).collect();
        format!("{cut}…")
    }
}

pub fn egui_color(c: Rgba) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), c.a())
}
