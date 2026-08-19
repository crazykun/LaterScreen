//! 贴图：置顶无边框窗口显示一张图片（Snipaste 招牌能力）。
//!
//! 独立进程形态（`lscreen pin`）：由覆盖层 spawn，选区合成 PNG 经 stdin
//! 传入，本进程读完即建窗。每个贴图一个轻量进程，关闭即释放全部内存。
//!
//! 交互：拖拽移动窗口（手动定位，见 PinApp::drag）、滚轮缩放 25%–400%
//! （光标下的图像点锚定不动）、双击复制、图像下方条带工具条、Esc/Delete 关闭。

use eframe::egui;
use egui::{Color32, Pos2, Rect, Stroke, Vec2, ViewportCommand};

use crate::export;

const MIN_ZOOM: f32 = 0.25;
const MAX_ZOOM: f32 = 4.0;
/// 缩放指示条/工具条的显示时长（秒）
const TOAST_SECS: f64 = 1.6;
/// 底部工具条条带高度：窗口高 = 图像显示高 + BAR_H，按钮在图像外侧，
/// 不遮挡贴图内容（与截图覆盖层「工具栏在选区下方」同布局）
pub const BAR_H: f32 = 34.0;

/// 贴图窗口初始逻辑尺寸：图像 + 底部工具条条带。
pub fn window_size(w: u32, h: u32, scale: f32) -> Vec2 {
    Vec2::new(w as f32 / scale, h as f32 / scale + BAR_H)
}

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
    /// 是否置顶（工具条可切换；建窗时 with_always_on_top，初始为 true）
    topmost: bool,
    /// 缩放手势锚点。一次滚动手势内定格：逐帧用「窗口位置 + 窗口内指针
    /// 坐标」重算会踩「窗口移动后静止指针局部坐标陈旧」的同一坑
    /// （见 drag 注释），表现为缩放时窗口位置抖动
    zoom_anchor: Option<ZoomAnchor>,
}

struct ZoomAnchor {
    /// 指针屏幕坐标（逻辑点，取锚时定格）
    screen: Pos2,
    /// 锚点在图像内的比例位置（0–1，取锚时定格）
    frac: Vec2,
    /// 上次滚动时刻：间隔超过阈值视为新手势，重新取锚
    at: f64,
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
            topmost: true,
            zoom_anchor: None,
        }
    }

    fn do_copy(&mut self, ctx: &egui::Context) {
        match export::copy_to_clipboard(&self.rgba, self.w, self.h) {
            Ok(()) => self.toast(ctx, "已复制"),
            Err(e) => self.toast(ctx, format!("复制失败: {e}")),
        }
    }

    fn do_save(&mut self, ctx: &egui::Context) {
        let path = export::default_save_path("png");
        match export::save_png(&self.rgba, self.w, self.h, &path) {
            Ok(p) => self.toast(ctx, format!("已保存 {}", p.display())),
            Err(e) => self.toast(ctx, format!("保存失败: {e}")),
        }
    }

    fn do_close(&mut self, ctx: &egui::Context) {
        ctx.send_viewport_cmd(ViewportCommand::Close);
    }

    fn toggle_topmost(&mut self, ctx: &egui::Context) {
        self.topmost = !self.topmost;
        let level = if self.topmost {
            egui::viewport::WindowLevel::AlwaysOnTop
        } else {
            egui::viewport::WindowLevel::Normal
        };
        ctx.send_viewport_cmd(ViewportCommand::WindowLevel(level));
        self.toast(
            ctx,
            if self.topmost {
                "已置顶"
            } else {
                "已取消置顶"
            },
        );
    }

    fn toast(&mut self, ctx: &egui::Context, msg: impl Into<String>) {
        self.toast = Some((msg.into(), ctx.input(|i| i.time) + TOAST_SECS));
    }

    /// 滚轮缩放：光标下的图像点锚定不动（窗口尺寸与位置同步换算）。
    /// 锚点在手势开始时定格（见 zoom_anchor 注释），手势内所有几何量
    /// 恒定，无逐帧重算的反馈抖动。
    fn handle_zoom(&mut self, ctx: &egui::Context, image_rect: Rect) {
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

        // 手势间隔 > 0.4s 重新取锚；取锚用 QueryPointer 绝对坐标
        // （退回 outer_rect + 局部坐标，此刻窗口静止、坐标可信）
        let now = ctx.input(|i| i.time);
        if self.zoom_anchor.as_ref().is_none_or(|a| now - a.at > 0.4) {
            let outer_min = ctx.input(|i| i.viewport().outer_rect).map(|r| r.min);
            let screen = lscreen_capture::cursor_position()
                .map(|(x, y)| Pos2::new(x as f32 / self.scale, y as f32 / self.scale))
                .or_else(|| {
                    let local = ctx.input(|i| i.pointer.latest_pos())?;
                    Some(outer_min? + local.to_vec2())
                });
            self.zoom_anchor = screen.zip(outer_min).map(|(screen, omin)| {
                let local = screen - omin;
                ZoomAnchor {
                    screen,
                    frac: Vec2::new(
                        (local.x / image_rect.width().max(1.0)).clamp(0.0, 1.0),
                        (local.y / image_rect.height().max(1.0)).clamp(0.0, 1.0),
                    ),
                    at: now,
                }
            });
        }

        let new_size = self.base * new_zoom;
        // 窗口 = 图像 + 底部条带
        ctx.send_viewport_cmd(ViewportCommand::InnerSize(new_size + Vec2::new(0.0, BAR_H)));
        if let Some(a) = &mut self.zoom_anchor {
            a.at = now;
            let new_min = a.screen - Vec2::new(new_size.x * a.frac.x, new_size.y * a.frac.y);
            ctx.send_viewport_cmd(ViewportCommand::OuterPosition(new_min));
        }

        let pct = (new_zoom * 100.0).round() as i32;
        let dir = if new_zoom > old_zoom { "+" } else { "" };
        self.toast(ctx, format!("{dir}{pct}%"));
    }

    /// 底部条带工具条：按钮在图像外侧的专属条带里（与截图覆盖层
    /// 「工具栏在选区下方」同布局），不遮挡贴图内容。
    /// 条带宽度放不下全部按钮时按优先级裁减：复制并关闭 > 关闭 > 置顶 > 保存。
    fn show_toolbar(&mut self, ctx: &egui::Context, bar: Rect) {
        use crate::ui::toolbar::{action_button, draw_check, draw_close, draw_save, icon_button};
        // 单按钮 24pt + 间距 2pt，左右各留 4pt
        let fit = (((bar.width() - 8.0) / 26.0) as usize).min(4);
        if fit == 0 {
            return;
        }
        let (show_close, show_top, show_save) = (fit >= 2, fit >= 3, fit >= 4);
        let need = fit as f32 * 26.0 - 2.0;
        let pos = Pos2::new(bar.center().x - need / 2.0, bar.center().y - 12.0);
        let mut action: Option<u8> = None;
        let topmost = self.topmost;
        egui::Area::new(egui::Id::new("pin-bar"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .interactable(true)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing = Vec2::splat(2.0);
                    if show_top
                        && icon_button(ui, topmost, "置顶：保持在最上层（点击切换）", draw_topmost)
                            .clicked()
                    {
                        action = Some(4);
                    }
                    if show_save && action_button(ui, true, "保存为 PNG (Ctrl+S)", draw_save) {
                        action = Some(1);
                    }
                    if show_close && action_button(ui, true, "关闭贴图 (Esc / Delete)", draw_close)
                    {
                        action = Some(2);
                    }
                    // 与覆盖层一致：最常用动作放最右，绿色对号
                    if action_button(ui, true, "复制并关闭 (双击/Ctrl+C 仅复制)", draw_check)
                    {
                        action = Some(3);
                    }
                });
            });
        match action {
            Some(1) => self.do_save(ctx),
            Some(2) => self.do_close(ctx),
            Some(3) => {
                // 对号 = 拿到图并结束：复制后直接关闭（仅复制走双击/Ctrl+C）
                self.do_copy(ctx);
                self.do_close(ctx);
            }
            Some(4) => self.toggle_topmost(ctx),
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
            .anchor(egui::Align2::CENTER_BOTTOM, Vec2::new(0.0, -(BAR_H + 10.0)))
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style())
                    .fill(Color32::from_black_alpha(200))
                    .show(ui, |ui| {
                        // 不换行：窄贴图里 "+150%" 会被折成两行
                        ui.add(
                            egui::Label::new(egui::RichText::new(msg).color(Color32::WHITE))
                                .wrap_mode(egui::TextWrapMode::Extend),
                        );
                    });
            });
        ctx.request_repaint();
    }
}

impl eframe::App for PinApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let full = ui.max_rect();
        // 窗口纵向切成两段：上方图像区 + 下方工具条条带
        let split_y = (full.max.y - BAR_H).max(full.min.y);
        let image_rect = Rect::from_min_max(full.min, Pos2::new(full.max.x, split_y));
        let bar_rect = Rect::from_min_max(Pos2::new(full.min.x, split_y), full.max);

        // 滚轮缩放（先于绘制：InnerSize 下一帧生效）
        self.handle_zoom(&ctx, image_rect);

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

        let response = ui.allocate_rect(full, egui::Sense::click_and_drag());
        if let Some(tex) = &self.texture {
            ui.painter().image(
                tex.id(),
                image_rect,
                egui::Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
        }
        // 图像区 1px 中性细线：与相近背景略作区隔。底部条带已承担
        // 贴图辨识职责，醒目蓝框叠在图像内容上反而突兀，弃用
        ui.painter().rect_stroke(
            image_rect,
            0.0,
            Stroke::new(1.0, Color32::from_black_alpha(90)),
            egui::StrokeKind::Inside,
        );
        // 底部条带背景 + 分隔线（按钮由 show_toolbar 画）
        ui.painter()
            .rect_filled(bar_rect, 0.0, ui.visuals().panel_fill);
        ui.painter().line_segment(
            [bar_rect.left_top(), bar_rect.right_top()],
            Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color),
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

        self.show_toolbar(&ctx, bar_rect);
        self.show_toast(&ctx);

        if copy_k {
            self.do_copy(&ctx);
        }
        if save_k {
            self.do_save(&ctx);
        }
    }
}

/// 置顶图标：顶部横线 + 向上箭头（推到最上层）。激活态由 icon_button 高亮。
fn draw_topmost(p: &egui::Painter, r: Rect, c: Color32) {
    let s = Stroke::new(1.4, c);
    let w = r.width();
    p.line_segment([r.left_top(), r.right_top()], s);
    let cx = r.center().x;
    let tip = Pos2::new(cx, r.min.y + w * 0.22);
    p.line_segment([Pos2::new(cx, r.max.y), tip], s);
    p.line_segment([tip, Pos2::new(cx - w * 0.28, r.min.y + w * 0.52)], s);
    p.line_segment([tip, Pos2::new(cx + w * 0.28, r.min.y + w * 0.52)], s);
}
