//! 贴图：置顶无边框窗口显示一张图片（Snipaste 招牌能力）。
//!
//! 独立进程形态（`lscreen pin`）：由覆盖层 spawn，选区合成 PNG 经 stdin
//! 传入，本进程读完即建窗。每个贴图一个轻量进程，关闭即释放全部内存。
//!
//! 交互：拖拽移动窗口（手动定位，见 PinApp::drag）、滚轮缩放 25%–400%
//! （光标下的图像点锚定不动）、双击复制、底部常驻图标工具条、Esc/Delete 关闭。

use eframe::egui;
use egui::{Color32, Pos2, Vec2, ViewportCommand};

use crate::export;

const MIN_ZOOM: f32 = 0.25;
const MAX_ZOOM: f32 = 4.0;
/// 缩放指示条/工具条的显示时长（秒）
const TOAST_SECS: f64 = 1.6;
/// 主题色：与覆盖层选区边框一致
const ACCENT: Color32 = Color32::from_rgb(0x21, 0x96, 0xf3);
/// 工具条需要的空间（3 个按钮 + 间距 + popup 边距的估计值）
const BAR_NEED: f32 = 104.0;

pub struct PinApp {
    rgba: Vec<u8>,
    w: u32,
    h: u32,
    /// zoom=1 时的窗口逻辑尺寸（物理像素 / 屏幕缩放比）
    base: Vec2,
    zoom: f32,
    texture: Option<egui::TextureHandle>,
    toast: Option<(String, f64)>,
    /// 手动拖拽状态。不用 ViewportCommand::StartDrag：它走 WM 交互式移动
    /// （_NET_WM_MOVERESIZE），spawn 后未激活的窗口第一次按下会被 WM 拿去做
    /// 焦点转移，请求被忽略，表现为「第一次总是拖不住」。
    ///
    /// 目标 = 指针屏幕坐标 − 抓取偏移（按下时定格）。指针屏幕坐标必须取
    /// 绝对来源（X11 QueryPointer）：窗口被程序移动时静止指针不产生
    /// MotionNotify，egui 的窗口内坐标是陈旧值，而 outer_rect 随
    /// ConfigureNotify 更新——用「新窗口位置 + 旧局部坐标」拼出的指针
    /// 位置比真实值多出刚移动的 Δ，会把自己的移动误判为指针移动再移一次，
    /// 逐帧自激成「窗口乱跑」。
    drag: Option<DragState>,
    /// 屏幕缩放比（物理像素/逻辑点），QueryPointer 物理坐标换算逻辑点用
    scale: f32,
}

struct DragState {
    /// 指针按下点相对窗口左上的偏移（窗口内坐标，拖动期间不变）
    grab_offset: Vec2,
    /// 上次发送的窗口目标位置（相等则不重发）
    sent: Pos2,
}

impl PinApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        rgba: Vec<u8>,
        w: u32,
        h: u32,
        scale: f32,
        font: Option<Vec<u8>>,
    ) -> Self {
        // 贴图是独立进程，必须自己挂中文字体，否则按钮/菜单/toast 的中文
        // 会因 egui 内置字体无 CJK 而显示为方框。先用 core Renderer 验证
        // 字节可解析（epaint 对坏字体是 panic 而非 Err）。
        if let Some(bytes) = font {
            if lscreen_core::render::Renderer::new(Some(bytes.clone())).has_font() {
                crate::font::setup_egui_fonts(&cc.egui_ctx, bytes);
            }
        }
        let img = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
        // 放大用最近邻（滚轮放大像素格清晰），缩小用线性
        let options = egui::TextureOptions {
            magnification: egui::TextureFilter::Nearest,
            minification: egui::TextureFilter::Linear,
            ..Default::default()
        };
        let texture = cc.egui_ctx.load_texture("pin", img, options);
        Self {
            rgba,
            w,
            h,
            base: Vec2::new(w as f32 / scale, h as f32 / scale),
            zoom: 1.0,
            texture: Some(texture),
            toast: None,
            drag: None,
            scale,
        }
    }

    fn do_copy(&mut self, ctx: &egui::Context) {
        match export::copy_to_clipboard(&self.rgba, self.w, self.h) {
            Ok(()) => self.toast(ctx, "已复制"),
            Err(e) => self.toast(ctx, format!("复制失败: {e}")),
        }
    }

    fn do_save(&mut self, ctx: &egui::Context) {
        let path = export::default_save_path();
        match export::save_png(&self.rgba, self.w, self.h, &path) {
            Ok(p) => self.toast(ctx, format!("已保存 {}", p.display())),
            Err(e) => self.toast(ctx, format!("保存失败: {e}")),
        }
    }

    fn do_close(&mut self, ctx: &egui::Context) {
        ctx.send_viewport_cmd(ViewportCommand::Close);
    }

    fn toast(&mut self, ctx: &egui::Context, msg: impl Into<String>) {
        self.toast = Some((msg.into(), ctx.input(|i| i.time) + TOAST_SECS));
    }

    /// 滚轮缩放：光标下的图像点锚定不动（窗口尺寸与位置同步换算）。
    fn handle_zoom(&mut self, ctx: &egui::Context, cur_size: Vec2) {
        let scroll = ctx.input(|i| i.smooth_scroll_delta.y);
        if scroll == 0.0 {
            return;
        }
        // 每 400pt 滚动量 ≈ e 倍缩放；向上滚为正 = 放大
        let factor = (scroll / 400.0).exp();
        let new_zoom = (self.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        if new_zoom == self.zoom {
            return;
        }
        let old_zoom = self.zoom;
        self.zoom = new_zoom;
        let new_size = self.base * new_zoom;
        ctx.send_viewport_cmd(ViewportCommand::InnerSize(new_size));

        // 锚定光标：cursor_screen = outer.min + cursor_local，
        // 新窗口位置 = cursor_screen - new_size * (cursor_local / cur_size)
        let anchor = ctx.input(|i| i.pointer.latest_pos()).map(|c| {
            Vec2::new(
                (c.x / cur_size.x.max(1.0)).clamp(0.0, 1.0),
                (c.y / cur_size.y.max(1.0)).clamp(0.0, 1.0),
            )
        });
        let outer = ctx.input(|i| i.viewport().outer_rect);
        if let (Some(cursor), Some(outer)) = (ctx.input(|i| i.pointer.latest_pos()), outer) {
            let screen_cursor = outer.min + cursor.to_vec2();
            let new_min = screen_cursor - new_size * anchor.unwrap_or(Vec2::splat(0.5));
            ctx.send_viewport_cmd(ViewportCommand::OuterPosition(new_min));
        }

        let pct = (new_zoom * 100.0).round() as i32;
        let dir = if new_zoom > old_zoom { "+" } else { "" };
        self.toast(ctx, format!("{dir}{pct}%"));
    }

    /// 常驻图标工具条：与截图覆盖层同一套手绘图标/按钮（ui::toolbar）。
    /// 前身是右键菜单：未激活窗口的右键常被 WM 拿去做焦点转移，菜单时常不弹，
    /// 且 hover 才显示的工具条不可发现，改为常驻。
    ///
    /// 窄窗兜底：横排放不下（窗宽 < BAR_NEED）改竖排贴右缘；
    /// 竖排也放不下的极小贴图直接不显示，交互走快捷键/双击。
    fn show_toolbar(&mut self, ctx: &egui::Context, window: egui::Rect) {
        use crate::ui::toolbar::{action_button, draw_check, draw_close, draw_save};
        let horizontal = window.width() >= BAR_NEED;
        if !horizontal && window.height() < BAR_NEED {
            return;
        }
        let mut action: Option<u8> = None;
        let mut buttons = |ui: &mut egui::Ui| {
            ui.spacing_mut().item_spacing = Vec2::splat(2.0);
            if action_button(ui, true, "保存为 PNG (Ctrl+S)", draw_save) {
                action = Some(1);
            }
            if action_button(ui, true, "关闭贴图 (Esc / Delete)", draw_close) {
                action = Some(2);
            }
            // 与覆盖层一致：最常用动作放最后（横排最右/竖排最下），绿色对号
            if action_button(ui, true, "复制到剪贴板 (Ctrl+C / 双击)", draw_check) {
                action = Some(3);
            }
        };
        let (align, offset) = if horizontal {
            (egui::Align2::CENTER_BOTTOM, Vec2::new(0.0, -8.0))
        } else {
            (egui::Align2::RIGHT_CENTER, Vec2::new(-8.0, 0.0))
        };
        egui::Area::new(egui::Id::new("pin-bar"))
            .order(egui::Order::Foreground)
            .anchor(align, offset)
            .interactable(true)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    if horizontal {
                        ui.horizontal(&mut buttons);
                    } else {
                        ui.vertical(&mut buttons);
                    }
                })
            });
        match action {
            Some(1) => self.do_save(ctx),
            Some(2) => self.do_close(ctx),
            Some(3) => self.do_copy(ctx),
            _ => {}
        }
    }

    fn show_toast(&mut self, ctx: &egui::Context) {
        let Some((msg, until)) = self.toast.clone() else {
            return;
        };
        if ctx.input(|i| i.time) > until {
            self.toast = None;
            return;
        }
        egui::Area::new(egui::Id::new("pin-toast"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::CENTER_BOTTOM, Vec2::new(0.0, -40.0))
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

impl eframe::App for PinApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let rect = ui.max_rect();

        // 滚轮缩放（先于绘制：InnerSize 下一帧生效）
        self.handle_zoom(&ctx, rect.size());

        // 键盘
        use egui::{Key, Modifiers};
        let (esc, del, copy_k, save_k) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::Escape),
                i.consume_key(Modifiers::NONE, Key::Delete),
                i.consume_key(Modifiers::COMMAND, Key::C),
                i.consume_key(Modifiers::COMMAND, Key::S),
            )
        });
        if esc || del {
            self.do_close(&ctx);
            return;
        }

        let response = ui.allocate_rect(rect, egui::Sense::click_and_drag());
        if let Some(tex) = &self.texture {
            ui.painter().image(
                tex.id(),
                rect,
                egui::Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
        }
        // 描边高亮：贴图常常正好盖在被截的原位上，与背景无缝、根本找不到。
        // 画不到窗口外侧（外发光/投影需要透明窗口 + 外扩边距，依赖合成器，
        // M7 刻意不用 with_transparent），用窗口内缘双线近似：
        // 最外 1px 暗线与相近色背景隔开，内侧 2px 主题蓝与选区边框同语言
        ui.painter().rect_stroke(
            rect,
            0.0,
            egui::Stroke::new(1.0, Color32::from_black_alpha(160)),
            egui::StrokeKind::Inside,
        );
        ui.painter().rect_stroke(
            rect.shrink(1.0),
            0.0,
            egui::Stroke::new(2.0, ACCENT),
            egui::StrokeKind::Inside,
        );

        if response.drag_started() {
            // 抓取偏移用按下原点（越过拖动阈值前窗口静止，局部坐标可信）
            if let (Some(outer), Some(origin)) = (
                ctx.input(|i| i.viewport().outer_rect),
                ctx.input(|i| i.pointer.press_origin()),
            ) {
                self.drag = Some(DragState {
                    grab_offset: origin.to_vec2(),
                    sent: outer.min,
                });
            }
        }
        let scale = self.scale;
        if let Some(d) = &mut self.drag {
            if response.dragged() {
                // 优先 X11 QueryPointer：绝对屏幕坐标，与窗口移动解耦，
                // 无自激回路（见 drag 字段注释）。Win/mac 拿不到时退回
                // egui 局部坐标换算，且仅在指针真实移动的帧重算，
                // 静止指针的陈旧局部坐标不参与计算
                let pointer_screen = lscreen_capture::cursor_position()
                    .map(|(x, y)| Pos2::new(x as f32 / scale, y as f32 / scale))
                    .or_else(|| {
                        ctx.input(|i| {
                            let fresh = i.pointer.delta() != Vec2::ZERO;
                            match (fresh, i.viewport().outer_rect, i.pointer.latest_pos()) {
                                (true, Some(o), Some(c)) => Some(o.min + c.to_vec2()),
                                _ => None,
                            }
                        })
                    });
                if let Some(p) = pointer_screen {
                    let target = p - d.grab_offset;
                    if target != d.sent {
                        ctx.send_viewport_cmd(ViewportCommand::OuterPosition(target));
                        d.sent = target;
                    }
                }
            }
            if response.drag_stopped() {
                self.drag = None;
            }
        }
        if response.double_clicked() {
            self.do_copy(&ctx);
        }

        self.show_toolbar(&ctx, rect);
        self.show_toast(&ctx);

        if copy_k {
            self.do_copy(&ctx);
        }
        if save_k {
            self.do_save(&ctx);
        }
    }
}
