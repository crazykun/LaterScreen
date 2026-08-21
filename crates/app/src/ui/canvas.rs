//! 画布：背景绘制、选区框选、图元绘制与全部鼠标交互。
//!
//! 交互层图元绘制是 core 导出渲染（tiny-skia）的镜像实现，
//! 几何参数（箭头头长、马赛克格子、笔刷半径）全部取自 Element 方法，两边不漂移。

use eframe::egui;
use egui::{
    Align2, Color32, CursorIcon, FontId, Pos2, Rect, Response, Sense, Shape, Stroke, StrokeKind,
    TextureHandle, Vec2,
};
use lscreen_core::render::mosaic_cells;
use lscreen_core::{Element, ElementKind, RectF, Tool, P2};

use super::{egui_color, DragOp, MosaicCache, SnipApp, Stage, TextEditState, View};

/// 马赛克色块缓存的几何指纹：点数 + 全部坐标 + 笔刷/格子尺寸。
/// f32 按位取整参与哈希，坐标变化（拖动、撤销）必然改变指纹。
pub fn mosaic_key(points: &[P2], brush: f32, cell: f32) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    points.len().hash(&mut h);
    for p in points {
        p.x.to_bits().hash(&mut h);
        p.y.to_bits().hash(&mut h);
    }
    brush.to_bits().hash(&mut h);
    cell.to_bits().hash(&mut h);
    h.finish()
}

const DIM: Color32 = Color32::from_black_alpha(120);
const ACCENT: Color32 = Color32::from_rgb(0x21, 0x96, 0xf3);
const UV_FULL: Rect = Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1.0, 1.0));
/// 命中容差（逻辑点）
const HIT_TOL_PT: f32 = 4.0;
/// 选区角点手柄半径（逻辑点）
const HANDLE_PT: f32 = 5.0;
/// 选区角点抓取半径（逻辑点）：比手柄可视尺寸大，框内外一圈都可命中
const CORNER_GRAB_PT: f32 = 12.0;
/// 选区边抓取容差（逻辑点）
const EDGE_GRAB_PT: f32 = 8.0;

pub fn show(app: &mut SnipApp, ui: &mut egui::Ui, texture: &TextureHandle) {
    show_in(app, ui, ui.max_rect(), texture);
}

/// 预览模式（滚动长截图标注）：可滚动画布。
/// ScrollArea 内容坐标 == 屏幕坐标系（egui 每帧按滚动偏移重排内容），
/// 因此 show_in 的 View 映射、命中检测、绘制无需任何平移换算。
pub fn show_scrolled(app: &mut SnipApp, ui: &mut egui::Ui, texture: &TextureHandle) {
    // 首帧按窗口宽度自适应缩放（长图主形态；不放大超过 100%）
    if app.preview_zoom <= 0.0 {
        let avail = (ui.available_width() - 12.0).max(50.0);
        app.preview_zoom = (avail / app.shot.width as f32).clamp(0.05, 1.0);
    }
    let size = Vec2::new(app.shot.width as f32, app.shot.height as f32) * app.preview_zoom;
    egui::ScrollArea::both()
        .id_salt("preview-canvas")
        .auto_shrink([false, false])
        .show_viewport(ui, |ui, _viewport| {
            // 内容比视口窄时水平居中
            let cursor = ui.cursor().min;
            let extra = ((ui.available_width() - size.x) / 2.0).max(0.0);
            let rect = Rect::from_min_size(Pos2::new(cursor.x + extra, cursor.y), size);
            ui.allocate_rect(rect, Sense::click_and_drag());
            show_in(app, ui, rect, texture);
        });
}

fn show_in(app: &mut SnipApp, ui: &mut egui::Ui, rect: Rect, texture: &TextureHandle) {
    let view = View {
        origin: rect.min,
        scale: app.shot.width as f32 / rect.width().max(1.0),
    };
    app.last_view = view;
    let response = ui.allocate_rect(rect, Sense::click_and_drag());
    let painter = ui.painter().clone();

    painter.image(texture.id(), rect, UV_FULL, Color32::WHITE);

    // 记录指针的图像像素坐标（取色快捷键用）
    app.cursor_px = response
        .hover_pos()
        .or_else(|| response.interact_pointer_pos())
        .map(|p| view.to_px(p));

    if app.mode == super::Mode::Pick {
        picking(app, ui, &response, &painter, view, rect, texture);
        return;
    }

    match app.stage {
        Stage::Selecting => selecting(app, ui, &response, &painter, view, rect),
        Stage::Editing => editing(app, ui, &response, &painter, view, rect, texture),
    }

    // 框选阶段显示取景放大镜（Editing 阶段指针要服务绘图工具，不显示）
    if matches!(app.stage, Stage::Selecting) {
        if let Some(p) = app.cursor_px {
            draw_magnifier(app, &painter, view, rect, p, texture);
        }
    }
}

/// Pick 模式：全屏取色器。单击复制 HEX 并退出。
fn picking(
    app: &mut SnipApp,
    ui: &egui::Ui,
    response: &Response,
    painter: &egui::Painter,
    view: View,
    screen: Rect,
    texture: &TextureHandle,
) {
    ui.ctx()
        .output_mut(|o| o.cursor_icon = CursorIcon::Crosshair);
    if let Some(p) = app.cursor_px {
        draw_magnifier(app, painter, view, screen, p, texture);
    }
    if response.clicked() {
        app.copy_color(ui.ctx(), super::ColorFormat::Hex);
    }
}

/// 取景放大镜：像素网格 + 十字线 + 颜色值 + 快捷键提示。
fn draw_magnifier(
    app: &SnipApp,
    painter: &egui::Painter,
    view: View,
    screen: Rect,
    p: P2,
    texture: &TextureHandle,
) {
    let Some(px) = app.shot.pixel(p.x as u32, p.y as u32) else {
        return;
    };
    let color = lscreen_core::Rgba(px);

    // 源区域：以指针为中心的 13×13 像素，放大到 156pt
    const SRC: f32 = 13.0;
    const ZOOM_SIZE: f32 = 156.0;
    let (w, h) = (app.shot.width as f32, app.shot.height as f32);
    let half = SRC / 2.0;
    let (cx, cy) = (p.x.floor() + 0.5, p.y.floor() + 0.5);
    let uv = Rect::from_min_max(
        Pos2::new((cx - half) / w, (cy - half) / h),
        Pos2::new((cx + half) / w, (cy + half) / h),
    );

    // 面板位置：跟随指针右下，越界翻转
    let cursor_pt = view.to_pt(p);
    let info_h = 64.0;
    let panel = Vec2::new(ZOOM_SIZE, ZOOM_SIZE + info_h);
    let mut anchor = cursor_pt + Vec2::new(24.0, 24.0);
    if anchor.x + panel.x > screen.max.x - 8.0 {
        anchor.x = cursor_pt.x - 24.0 - panel.x;
    }
    if anchor.y + panel.y > screen.max.y - 8.0 {
        anchor.y = cursor_pt.y - 24.0 - panel.y;
    }
    let zoom_rect = Rect::from_min_size(anchor, Vec2::splat(ZOOM_SIZE));

    // 放大图（纹理放大过滤为最近邻，像素格清晰）
    painter.rect_filled(
        Rect::from_min_size(anchor, panel).expand(1.0),
        3.0,
        Color32::from_black_alpha(230),
    );
    painter.image(texture.id(), zoom_rect, uv, Color32::WHITE);
    // 中心十字线
    let cell = ZOOM_SIZE / SRC;
    let c = zoom_rect.center();
    let cross = Stroke::new(1.0, Color32::from_rgba_unmultiplied(0x21, 0x96, 0xf3, 180));
    painter.line_segment(
        [
            Pos2::new(zoom_rect.min.x, c.y),
            Pos2::new(zoom_rect.max.x, c.y),
        ],
        cross,
    );
    painter.line_segment(
        [
            Pos2::new(c.x, zoom_rect.min.y),
            Pos2::new(c.x, zoom_rect.max.y),
        ],
        cross,
    );
    painter.rect_stroke(
        Rect::from_center_size(c, Vec2::splat(cell)),
        0.0,
        Stroke::new(1.0, Color32::WHITE),
        StrokeKind::Outside,
    );

    // 信息区：色块 + 坐标 + RGB/HEX
    let pad = 6.0;
    let info_top = zoom_rect.max.y + pad;
    painter.rect_filled(
        Rect::from_min_size(Pos2::new(anchor.x + pad, info_top), Vec2::splat(14.0)),
        2.0,
        egui_color(color),
    );
    let text_x = anchor.x + pad + 20.0;
    painter.text(
        Pos2::new(text_x, info_top),
        Align2::LEFT_TOP,
        format!(
            "{}  ({}, {})",
            lscreen_core::color::to_hex(color),
            p.x as u32,
            p.y as u32
        ),
        FontId::monospace(12.0),
        Color32::WHITE,
    );
    painter.text(
        Pos2::new(anchor.x + pad, info_top + 18.0),
        Align2::LEFT_TOP,
        format!("RGB {}", lscreen_core::color::to_rgb_str(color)),
        FontId::monospace(12.0),
        Color32::from_white_alpha(220),
    );
    painter.text(
        Pos2::new(anchor.x + pad, info_top + 36.0),
        Align2::LEFT_TOP,
        "Ctrl+R/H/K 复制 RGB/HEX/CMYK",
        FontId::proportional(11.0),
        Color32::from_white_alpha(150),
    );
}

// ---------------------------------------------------------------- Selecting

/// 悬停窗口命中：Z 序自顶向下第一个含点的窗口（拖拽中不做窗口吸附）。
fn hovered_window(app: &SnipApp) -> Option<usize> {
    if app.drag.is_some() {
        return None;
    }
    let p = app.cursor_px?;
    app.windows.iter().position(|w| w.rect.contains(p))
}

fn selecting(
    app: &mut SnipApp,
    ui: &egui::Ui,
    response: &Response,
    painter: &egui::Painter,
    view: View,
    screen: Rect,
) {
    ui.ctx()
        .output_mut(|o| o.cursor_icon = CursorIcon::Crosshair);

    if response.drag_started() {
        // 用按下原点而非当前位置：drag_started 触发时指针已越过拖动阈值
        // （约 6px），偏移后的框选起点会和实际按下的位置差一截
        let origin = ui.input(|i| i.pointer.press_origin());
        if let Some(pos) = origin.or_else(|| response.interact_pointer_pos()) {
            app.drag = Some(DragOp::SelectRegion {
                start: view.to_px(pos),
            });
        }
    }

    let mut current: Option<RectF> = None;
    if let (Some(DragOp::SelectRegion { start }), Some(pos)) =
        (&app.drag, response.interact_pointer_pos())
    {
        current = Some(RectF::from_points(*start, view.to_px(pos)));
    }

    // 悬停窗口（自由框选拖拽期间关闭）
    let hover = hovered_window(app);
    // 悬停窗口是否与当前候选选区同位（同位不再重复高亮）
    let hover_same = hover.is_some_and(|i| {
        app.sel_window
            .as_ref()
            .is_some_and(|s| s.rect == app.windows[i].rect)
    });

    match current {
        Some(r) => {
            dim_outside(painter, view, screen, r);
            let rp = view.rect_pt(r);
            painter.rect_stroke(rp, 0.0, Stroke::new(1.5, ACCENT), StrokeKind::Outside);
            size_label(painter, rp, r);
        }
        None => match app.sel_window.as_ref() {
            Some(sel) => {
                // 初始/已选窗口：暗化四周 + 边框 + 标签
                dim_outside(painter, view, screen, sel.rect);
                let rp = view.rect_pt(sel.rect);
                painter.rect_stroke(rp, 0.0, Stroke::new(1.5, ACCENT), StrokeKind::Outside);
                size_label(painter, rp, sel.rect);
                win_label(painter, rp, screen, &sel.title);
            }
            None => {
                painter.rect_filled(screen, 0.0, DIM);
                painter.text(
                    screen.center(),
                    Align2::CENTER_CENTER,
                    "拖拽选择区域 · 移动鼠标选窗口 · 单击全屏 · Esc 退出",
                    FontId::proportional(16.0),
                    Color32::from_white_alpha(200),
                );
            }
        },
    }

    // 悬停窗口高亮（拖拽中/与候选选区同位时不画）
    if let Some(i) = hover.filter(|_| current.is_none()) {
        if !hover_same {
            let w = &app.windows[i];
            let rp = view.rect_pt(w.rect);
            painter.rect_stroke(
                rp,
                0.0,
                Stroke::new(1.5, Color32::from_rgba_unmultiplied(0x21, 0x96, 0xf3, 200)),
                StrokeKind::Outside,
            );
            win_label(painter, rp, screen, &w.title);
        }
    }

    if response.drag_stopped() {
        if let Some(r) = current {
            if r.width() >= 4.0 && r.height() >= 4.0 {
                app.region = r;
                app.clamp_region();
                app.confirm_region(ui.ctx());
                app.drag = None;
                return;
            }
        }
        app.drag = None;
    }
    if response.clicked() {
        // 单击（未拖拽）：窗口上 = 选中该窗口；空白处 = 全屏（旧行为）
        if let Some(i) = hover {
            app.region = app.windows[i].rect;
        } else {
            app.region = RectF::from_points(
                P2::new(0.0, 0.0),
                P2::new(app.shot.width as f32, app.shot.height as f32),
            );
        }
        app.clamp_region();
        app.confirm_region(ui.ctx());
    }
}

/// 窗口标签：标题（截断），画在矩形左上角上方。
fn win_label(painter: &egui::Painter, region_pt: Rect, _screen: Rect, title: &str) {
    let label = if title.is_empty() {
        "窗口".to_string()
    } else {
        truncate_str(title, 40)
    };
    let galley = painter.layout_no_wrap(label, FontId::proportional(12.0), Color32::WHITE);
    let bg = Rect::from_min_size(
        Pos2::new(region_pt.min.x, (region_pt.min.y - 20.0).max(4.0)),
        galley.size() + Vec2::new(10.0, 5.0),
    );
    painter.rect_filled(bg, 3.0, Color32::from_black_alpha(180));
    painter.galley(
        Pos2::new(bg.min.x + 5.0, bg.min.y + 2.0),
        galley,
        Color32::WHITE,
    );
}

fn truncate_str(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max).collect();
        format!("{cut}…")
    }
}

// ------------------------------------------------------------------ Editing

/// 点击型工具（单击即完成一次完整操作）：Marker 连点、Text 开编辑器。
/// 这类工具的双击是「快速连续两次操作」，不能被「双击=复制」吞掉。
fn is_click_tool(t: Tool) -> bool {
    matches!(t, Tool::Marker | Tool::Text)
}

fn editing(
    app: &mut SnipApp,
    ui: &egui::Ui,
    response: &Response,
    painter: &egui::Painter,
    view: View,
    screen: Rect,
    texture: &TextureHandle,
) {
    let shift = ui.input(|i| i.modifiers.shift);
    let pointer_px = response
        .hover_pos()
        .or_else(|| response.interact_pointer_pos())
        .map(|p| view.to_px(p));

    // ---- 输入 ----
    // 文本编辑是模态的：点击画布任意处=提交当前输入，其余指针交互让位
    // （否则再点一次 Text 会在编辑器底下遗留一个空文本图元）
    if app.text_edit.is_some() {
        if response.clicked() {
            app.commit_text_edit();
        }
    } else {
        if response.drag_started() {
            // 命中测试用按下原点：drag_started 触发时指针已越过拖动阈值（约 6px），
            // 在角点/边上按下后快速拖动，指针可能已滑出容差圈，
            // 用当前位置测会出现「光标显示可拖、实际拖不动」
            let origin = ui.input(|i| i.pointer.press_origin());
            if let Some(pos) = origin.or_else(|| response.interact_pointer_pos()) {
                on_press(app, view, view.to_px(pos), pos, shift);
            }
        } else if response.dragged() {
            if let Some(pos) = response.interact_pointer_pos() {
                on_drag(app, view.to_px(pos), shift);
            }
        }
        if response.drag_stopped() {
            on_release(app);
        }
        // 原地单击：egui 的 drag_started 需要越过拖动阈值（约 6px），静止点击不会触发。
        // 点击型工具（文本/标号）和 Select 的点选必须单独走一遍 press/release。
        // 双击的第二击让给 on_double_click（复制/编辑文本），避免先误放一个图元。
        let swallow_second_click = response.double_clicked() && !is_click_tool(app.tool);
        if response.clicked() && !swallow_second_click {
            if let Some(pos) = response.interact_pointer_pos() {
                on_press(app, view, view.to_px(pos), pos, shift);
                on_release(app);
            }
        }
        if response.double_clicked() {
            on_double_click(app, ui.ctx(), pointer_px);
        }

        // ---- 悬停状态（仅 Select 工具） ----
        app.hover = None;
        if app.tool == Tool::Select && app.drag.is_none() {
            if let Some(p) = pointer_px {
                app.hover = app.doc.hit_top(p, HIT_TOL_PT * view.scale);
            }
        }
    }
    update_cursor(app, ui, view, pointer_px);

    // ---- 绘制 ----
    dim_outside(painter, view, screen, app.region);
    let region_pt = view.rect_pt(app.region);
    let clipped = painter.with_clip_rect(region_pt.expand(1.0));
    // 拆字段借用：遍历 elements（不可变）同时更新 mosaic_cache（可变），
    // 免去每帧整体 clone
    let editing_text_id = app.text_edit.as_ref().map(|t| t.id);
    for e in &app.doc.elements {
        draw_element(
            &mut app.mosaic_cache,
            &app.shot,
            editing_text_id,
            &clipped,
            view,
            e,
            texture,
        );
    }
    // 回收已删除/已撤销图元的马赛克缓存，长会话不泄漏
    if !app.mosaic_cache.is_empty() {
        app.mosaic_cache
            .retain(|id, _| app.doc.elements.iter().any(|e| e.id == *id));
    }
    draw_highlights(app, painter, view);

    // 预览模式选区固定整图：画一圈淡边即可，不画 8 个拖拽手柄
    if app.preview {
        painter.rect_stroke(
            region_pt,
            0.0,
            Stroke::new(1.0, Color32::from_black_alpha(90)),
            StrokeKind::Inside,
        );
        return;
    }
    painter.rect_stroke(
        region_pt,
        0.0,
        Stroke::new(1.5, ACCENT),
        StrokeKind::Outside,
    );
    // 8 个手柄：4 角 + 4 边中点（边中点提示整条边可拖）
    let r = app.region;
    let (cx, cy) = ((r.min.x + r.max.x) * 0.5, (r.min.y + r.max.y) * 0.5);
    let mids = [
        P2::new(r.min.x, cy),
        P2::new(cx, r.min.y),
        P2::new(r.max.x, cy),
        P2::new(cx, r.max.y),
    ];
    for c in app.region.corners().iter().chain(mids.iter()) {
        let p = view.to_pt(*c);
        painter.rect_filled(
            Rect::from_center_size(p, Vec2::splat(HANDLE_PT * 2.0)),
            1.0,
            Color32::WHITE,
        );
        painter.rect_stroke(
            Rect::from_center_size(p, Vec2::splat(HANDLE_PT * 2.0)),
            1.0,
            Stroke::new(1.0, ACCENT),
            StrokeKind::Outside,
        );
    }
}

/// 命中选区角点：返回 (角点下标, 对角点)。预览模式选区固定整图，不可调。
fn hit_corner(app: &SnipApp, view: View, pos_pt: Pos2) -> Option<(usize, P2)> {
    if app.preview {
        return None;
    }
    let corners = app.region.corners();
    for (i, c) in corners.iter().enumerate() {
        if view.to_pt(*c).distance(pos_pt) <= CORNER_GRAB_PT {
            return Some((i, corners[(i + 2) % 4]));
        }
    }
    None
}

/// 命中选区边（整条边可拖）：返回边下标（0=左 1=上 2=右 3=下）。
/// 角点优先级更高，调用方需先测角点。预览模式不可调。
fn hit_edge(app: &SnipApp, view: View, pos_pt: Pos2) -> Option<usize> {
    if app.preview {
        return None;
    }
    let r = view.rect_pt(app.region);
    let tol = EDGE_GRAB_PT;
    let in_x = pos_pt.x >= r.min.x - tol && pos_pt.x <= r.max.x + tol;
    let in_y = pos_pt.y >= r.min.y - tol && pos_pt.y <= r.max.y + tol;
    if in_y && (pos_pt.x - r.min.x).abs() <= tol {
        return Some(0);
    }
    if in_x && (pos_pt.y - r.min.y).abs() <= tol {
        return Some(1);
    }
    if in_y && (pos_pt.x - r.max.x).abs() <= tol {
        return Some(2);
    }
    if in_x && (pos_pt.y - r.max.y).abs() <= tol {
        return Some(3);
    }
    None
}

fn on_press(app: &mut SnipApp, view: View, p: P2, pos_pt: Pos2, shift: bool) {
    // 1. 选区角点优先，其次选区边
    if let Some((_, anchor)) = hit_corner(app, view, pos_pt) {
        app.drag = Some(DragOp::ResizeRegion { anchor });
        return;
    }
    if let Some(edge) = hit_edge(app, view, pos_pt) {
        app.drag = Some(DragOp::ResizeEdge { edge });
        return;
    }
    let inside = app.region.contains(p);

    if app.tool == Tool::Select {
        // 2. 已选中图元的控制点
        if let Some(id) = app.selected {
            if let Some(e) = app.doc.get(id) {
                for (idx, cp) in e.control_points().iter().enumerate() {
                    if view.to_pt(*cp).distance(pos_pt) <= HANDLE_PT * 1.8 {
                        // 快照推迟到首次真实位移（on_drag）：按下即松手的
                        // 点选不应触发 begin_change 清空重做栈
                        app.drag = Some(DragOp::ControlPoint {
                            id,
                            idx,
                            began: false,
                        });
                        return;
                    }
                }
            }
        }
        // 3. 命中图元 → 选中并准备移动（快照同样推迟到首次位移）
        if let Some(id) = app.doc.hit_top(p, HIT_TOL_PT * view.scale) {
            app.selected = Some(id);
            app.drag = Some(DragOp::MoveElem {
                id,
                last: p,
                began: false,
            });
            return;
        }
        app.selected = None;
        // 4. 选区内空白 → 移动选区
        if inside {
            app.drag = Some(DragOp::MoveRegion { last: p });
        }
        return;
    }

    if !inside {
        return;
    }
    // 5. 绘图工具：新建图元
    let style = app.style;
    app.doc.begin_change();
    let kind = match app.tool {
        Tool::Rect => ElementKind::Rect {
            rect: RectF::from_points(p, p),
        },
        Tool::Ellipse => ElementKind::Ellipse {
            rect: RectF::from_points(p, p),
        },
        Tool::Arrow => ElementKind::Arrow { from: p, to: p },
        Tool::Line => ElementKind::Line { from: p, to: p },
        // 曲线工具 + Shift = 直线
        Tool::Curve => {
            if shift {
                ElementKind::Line { from: p, to: p }
            } else {
                ElementKind::Curve { points: vec![p] }
            }
        }
        Tool::Marker => ElementKind::Marker {
            center: p,
            number: app.doc.next_marker_number(),
        },
        Tool::Mosaic => ElementKind::Mosaic { points: vec![p] },
        Tool::Eraser => ElementKind::Eraser { points: vec![p] },
        Tool::Text => {
            let id = app.doc.add(
                ElementKind::Text {
                    pos: p,
                    content: String::new(),
                    size: P2::new(0.0, 0.0),
                },
                style,
            );
            app.text_edit = Some(TextEditState {
                id,
                buffer: String::new(),
                is_new: true,
            });
            return;
        }
        Tool::Select => unreachable!(),
    };
    let id = app.doc.add(kind, style);
    app.drag = Some(DragOp::Draw { id, start: p });
}

fn on_drag(app: &mut SnipApp, p: P2, shift: bool) {
    let Some(op) = &mut app.drag else { return };
    match op {
        DragOp::Draw { id, start, .. } => {
            let (id, start) = (*id, *start);
            let Some(e) = app.doc.get_mut(id) else { return };
            match &mut e.kind {
                ElementKind::Rect { rect } | ElementKind::Ellipse { rect } => {
                    // Shift：正方形 / 正圆
                    let p2 = if shift { constrain_square(start, p) } else { p };
                    *rect = RectF::from_points(start, p2);
                }
                ElementKind::Arrow { to, .. } | ElementKind::Line { to, .. } => *to = p,
                ElementKind::Curve { points }
                | ElementKind::Mosaic { points }
                | ElementKind::Eraser { points } => {
                    if points.last().is_none_or(|l| l.dist(p) > 1.5) {
                        points.push(p);
                    }
                }
                ElementKind::Marker { center, .. } => *center = p,
                ElementKind::Text { .. } => {}
            }
        }
        DragOp::MoveElem { id, last, began } => {
            let (dx, dy) = (p.x - last.x, p.y - last.y);
            *last = p;
            if dx == 0.0 && dy == 0.0 {
                return;
            }
            // 首次真实位移才压快照（begin_change 会清空重做栈）
            if !*began {
                *began = true;
                app.doc.begin_change();
            }
            let id = *id;
            if let Some(e) = app.doc.get_mut(id) {
                e.translate(dx, dy);
            }
        }
        DragOp::ControlPoint { id, idx, began } => {
            if !*began {
                *began = true;
                app.doc.begin_change();
            }
            let (id, idx) = (*id, *idx);
            if let Some(e) = app.doc.get_mut(id) {
                e.set_control_point(idx, p);
            }
        }
        DragOp::MoveRegion { last } => {
            // 先把位移夹到边界内再整体平移：直接 translate + clamp min/max
            // 会在拖出屏幕时压扁选区，且拖回来尺寸不恢复。
            let (dx, dy) = (p.x - last.x, p.y - last.y);
            *last = p;
            let (w, h) = (app.shot.width as f32, app.shot.height as f32);
            // 上界优先，避免 min > max 的 clamp panic（见 render.rs 箭头头长）
            let dx = dx.max(-app.region.min.x.max(0.0)).min(w - app.region.max.x);
            let dy = dy.max(-app.region.min.y.max(0.0)).min(h - app.region.max.y);
            app.region = app.region.translate(dx, dy);
            app.clamp_region();
        }
        DragOp::ResizeRegion { anchor } => {
            app.region = RectF::from_points(*anchor, p);
            app.clamp_region();
        }
        DragOp::ResizeEdge { edge } => {
            let r = &mut app.region;
            match edge {
                0 => r.min.x = p.x.min(r.max.x - 1.0),
                1 => r.min.y = p.y.min(r.max.y - 1.0),
                2 => r.max.x = p.x.max(r.min.x + 1.0),
                _ => r.max.y = p.y.max(r.min.y + 1.0),
            }
            app.clamp_region();
        }
        DragOp::SelectRegion { .. } => {}
    }
}

fn on_release(app: &mut SnipApp) {
    if let Some(op) = app.drag.take() {
        match op {
            DragOp::Draw { id, .. } => {
                // 拖拽出的退化图形（没有实际尺寸）直接回滚
                let degenerate = app.doc.get(id).is_some_and(|e| {
                    let b = e.bounds();
                    match &e.kind {
                        ElementKind::Rect { .. }
                        | ElementKind::Ellipse { .. }
                        | ElementKind::Arrow { .. }
                        | ElementKind::Line { .. } => b.width() + b.height() < 3.0,
                        // 自由笔迹是拖拽工具；静止单击（包括双击的第一击）不应留下
                        // 点状图元，也不应占用一个撤销步骤。
                        ElementKind::Curve { points }
                        | ElementKind::Mosaic { points }
                        | ElementKind::Eraser { points } => points.len() < 2,
                        _ => false,
                    }
                });
                if degenerate {
                    app.doc.cancel_change();
                }
            }
            // 快照在首次位移才压（见 on_drag），点选不再产生空撤销步；
            // 这里兜底「拖出去又拖回原位」的净零修改，弹出等同现状的快照
            DragOp::MoveElem { .. } | DragOp::ControlPoint { .. } => {
                app.doc.end_change_if_noop();
            }
            _ => {}
        }
    }
}

fn on_double_click(app: &mut SnipApp, ctx: &egui::Context, pointer_px: Option<P2>) {
    let Some(p) = pointer_px else { return };
    if app.text_edit.is_some() {
        return;
    }
    // 点击型工具的双击是连续放置（标号 1、2、3…），不触发复制退出
    if is_click_tool(app.tool) {
        return;
    }
    // Select 工具双击文本 → 进入编辑
    if app.tool == Tool::Select {
        if let Some(id) = app.hover.or(app.selected) {
            if let Some(Element {
                kind: ElementKind::Text { content, .. },
                ..
            }) = app.doc.get(id)
            {
                let buffer = content.clone();
                app.doc.begin_change();
                app.text_edit = Some(TextEditState {
                    id,
                    buffer,
                    is_new: false,
                });
                return;
            }
        }
    }
    // 其余情况：双击选区内 = 复制并退出
    if app.region.contains(p) {
        app.copy_and_exit(ctx);
    }
}

fn constrain_square(start: P2, p: P2) -> P2 {
    let (dx, dy) = (p.x - start.x, p.y - start.y);
    let side = dx.abs().max(dy.abs());
    P2::new(start.x + side * dx.signum(), start.y + side * dy.signum())
}

fn update_cursor(app: &SnipApp, ui: &egui::Ui, view: View, pointer_px: Option<P2>) {
    let Some(p) = pointer_px else { return };
    let pos_pt = view.to_pt(p);
    let icon = if let Some((i, _)) = hit_corner(app, view, pos_pt) {
        // 0=左上 1=右上 2=右下 3=左下：主对角线用 NwSe，副对角线用 NeSw
        if i % 2 == 0 {
            CursorIcon::ResizeNwSe
        } else {
            CursorIcon::ResizeNeSw
        }
    } else if let Some(edge) = hit_edge(app, view, pos_pt) {
        if edge % 2 == 0 {
            CursorIcon::ResizeHorizontal
        } else {
            CursorIcon::ResizeVertical
        }
    } else if app.tool == Tool::Select {
        if app.hover.is_some() {
            CursorIcon::Move
        } else if app.region.contains(p) {
            CursorIcon::Grab
        } else {
            CursorIcon::Default
        }
    } else if app.tool == Tool::Text {
        CursorIcon::Text
    } else if app.region.contains(p) {
        CursorIcon::Crosshair
    } else {
        CursorIcon::Default
    };
    ui.ctx().output_mut(|o| o.cursor_icon = icon);
}

// ------------------------------------------------------------------- 绘制

fn dim_outside(painter: &egui::Painter, view: View, screen: Rect, region: RectF) {
    let r = view.rect_pt(region);
    let top = Rect::from_min_max(screen.min, Pos2::new(screen.max.x, r.min.y));
    let bottom = Rect::from_min_max(Pos2::new(screen.min.x, r.max.y), screen.max);
    let left = Rect::from_min_max(
        Pos2::new(screen.min.x, r.min.y),
        Pos2::new(r.min.x, r.max.y),
    );
    let right = Rect::from_min_max(
        Pos2::new(r.max.x, r.min.y),
        Pos2::new(screen.max.x, r.max.y),
    );
    for part in [top, bottom, left, right] {
        if part.width() > 0.0 && part.height() > 0.0 {
            painter.rect_filled(part, 0.0, DIM);
        }
    }
}

/// 首次框选拖拽期间的只读尺寸提示（此时还没有稳定选区可编辑）。
fn size_label(painter: &egui::Painter, region_pt: Rect, region: RectF) {
    let text = format!("{} × {}", region.width().round(), region.height().round());
    let pos = Pos2::new(region_pt.min.x, (region_pt.min.y - 22.0).max(4.0));
    let galley = painter.layout_no_wrap(text, FontId::proportional(13.0), Color32::WHITE);
    let bg = Rect::from_min_size(pos, galley.size() + Vec2::new(10.0, 6.0));
    painter.rect_filled(bg, 3.0, Color32::from_black_alpha(180));
    painter.galley(pos + Vec2::new(5.0, 3.0), galley, Color32::WHITE);
}

/// 交互层图元绘制。参数为 SnipApp 的字段拆借：遍历 elements（不可变）
/// 的同时可更新 mosaic_cache（可变），见 editing() 调用点。
#[allow(clippy::too_many_arguments)]
fn draw_element(
    mosaic_cache: &mut MosaicCache,
    shot: &lscreen_capture::Screenshot,
    editing_text_id: Option<u64>,
    painter: &egui::Painter,
    view: View,
    e: &Element,
    texture: &TextureHandle,
) {
    let color = egui_color(e.style.color);
    let stroke = Stroke::new(view.len_pt(e.style.width), color);

    match &e.kind {
        ElementKind::Rect { rect } => {
            painter.rect_stroke(view.rect_pt(*rect), 0.0, stroke, StrokeKind::Middle);
        }
        ElementKind::Ellipse { rect } => {
            let r = view.rect_pt(*rect);
            painter.add(egui::epaint::EllipseShape {
                center: r.center(),
                radius: r.size() / 2.0,
                fill: Color32::TRANSPARENT,
                stroke,
                angle: 0.0,
            });
        }
        ElementKind::Line { from, to } => {
            painter.line_segment([view.to_pt(*from), view.to_pt(*to)], stroke);
        }
        ElementKind::Arrow { from, to } => {
            draw_arrow(painter, view, *from, *to, e.style.width, color, stroke);
        }
        ElementKind::Curve { points } => {
            let pts: Vec<Pos2> = points.iter().map(|p| view.to_pt(*p)).collect();
            if pts.len() == 1 {
                painter.circle_filled(pts[0], stroke.width / 2.0, color);
            } else {
                painter.add(Shape::line(pts, stroke));
            }
        }
        ElementKind::Marker { center, number } => {
            let c = view.to_pt(*center);
            painter.circle_filled(c, view.len_pt(e.marker_radius()), color);
            painter.text(
                c,
                Align2::CENTER_CENTER,
                number.to_string(),
                FontId::proportional(view.len_pt(e.style.font_size)),
                Color32::WHITE,
            );
        }
        ElementKind::Text { pos, content, .. } => {
            // 编辑中的文本由编辑窗口呈现，避免重影
            if editing_text_id == Some(e.id) {
                return;
            }
            painter.text(
                view.to_pt(*pos),
                Align2::LEFT_TOP,
                content,
                FontId::proportional(view.len_pt(e.style.font_size)),
                color,
            );
        }
        ElementKind::Mosaic { points } => {
            // 缓存指纹必须含几何：移动马赛克只改坐标不改点数，
            // 只比点数会让预览留在原位而导出用新坐标，形成「预览遮住了、导出没遮」。
            let key = mosaic_key(points, e.mosaic_brush(), e.mosaic_cell());
            let (n_cached, cells) = mosaic_cache.entry(e.id).or_insert_with(|| (0, Vec::new()));
            if *n_cached != key {
                *cells = mosaic_cells(
                    &shot.rgba,
                    shot.width,
                    shot.height,
                    points,
                    e.mosaic_brush(),
                    e.mosaic_cell(),
                );
                *n_cached = key;
            }
            for (x, y, size, c) in cells.iter() {
                let min = view.to_pt(P2::new(*x, *y));
                let cell = Rect::from_min_size(min, Vec2::splat(view.len_pt(*size)));
                painter.rect_filled(cell, 0.0, egui_color(*c));
            }
        }
        ElementKind::Eraser { points } => {
            // 用原图纹理沿笔迹盖章，近似导出层的「原图回贴」
            let brush_pt = view.len_pt(e.eraser_brush());
            let (w, h) = (shot.width as f32, shot.height as f32);
            let stamp = |p: P2| {
                let center = view.to_pt(p);
                let rect = Rect::from_center_size(center, Vec2::splat(brush_pt * 2.0));
                let uv = Rect::from_min_max(
                    Pos2::new((p.x - e.eraser_brush()) / w, (p.y - e.eraser_brush()) / h),
                    Pos2::new((p.x + e.eraser_brush()) / w, (p.y + e.eraser_brush()) / h),
                );
                painter.image(texture.id(), rect, uv, Color32::WHITE);
            };
            match points.as_slice() {
                [] => {}
                [p] => stamp(*p),
                pts => {
                    let step = e.eraser_brush() * 0.6;
                    for seg in pts.windows(2) {
                        let (a, b) = (seg[0], seg[1]);
                        let n = (a.dist(b) / step).ceil().max(1.0) as i32;
                        for i in 0..=n {
                            let t = i as f32 / n as f32;
                            stamp(P2::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t));
                        }
                    }
                }
            }
        }
    }
}

fn draw_arrow(
    painter: &egui::Painter,
    view: View,
    from: P2,
    to: P2,
    width_px: f32,
    color: Color32,
    stroke: Stroke,
) {
    let len = from.dist(to);
    if len < 1.0 {
        return;
    }
    // 与 core render.rs 保持一致：max/min 而非 clamp，短箭头时避免 min>max panic
    let head = (width_px * 4.5).max(10.0).min(len * 0.5);
    let (ux, uy) = ((to.x - from.x) / len, (to.y - from.y) / len);
    let base = P2::new(to.x - ux * head, to.y - uy * head);
    let (px, py) = (-uy, ux);
    let half = head * 0.5;
    painter.line_segment([view.to_pt(from), view.to_pt(base)], stroke);
    painter.add(Shape::convex_polygon(
        vec![
            view.to_pt(to),
            view.to_pt(P2::new(base.x + px * half, base.y + py * half)),
            view.to_pt(P2::new(base.x - px * half, base.y - py * half)),
        ],
        color,
        Stroke::NONE,
    ));
}

fn draw_highlights(app: &SnipApp, painter: &egui::Painter, view: View) {
    if app.tool != Tool::Select {
        return;
    }
    if let Some(id) = app.hover.filter(|h| Some(*h) != app.selected) {
        if let Some(e) = app.doc.get(id) {
            painter.rect_stroke(
                view.rect_pt(e.bounds()).expand(4.0),
                2.0,
                Stroke::new(1.0, Color32::from_rgba_unmultiplied(0x21, 0x96, 0xf3, 140)),
                StrokeKind::Outside,
            );
        }
    }
    if let Some(e) = app.selected.and_then(|id| app.doc.get(id)) {
        painter.rect_stroke(
            view.rect_pt(e.bounds()).expand(4.0),
            2.0,
            Stroke::new(1.5, ACCENT),
            StrokeKind::Outside,
        );
        for cp in e.control_points() {
            let p = view.to_pt(cp);
            painter.circle_filled(p, HANDLE_PT, Color32::WHITE);
            painter.circle_stroke(p, HANDLE_PT, Stroke::new(1.5, ACCENT));
        }
    }
}

// -------------------------------------------------------------- 文本编辑器

pub fn show_text_editor(app: &mut SnipApp, ctx: &egui::Context) {
    let Some(edit) = &mut app.text_edit else {
        return;
    };
    let Some(elem) = app.doc.get(edit.id) else {
        app.text_edit = None;
        return;
    };
    let (pos, style) = match &elem.kind {
        ElementKind::Text { pos, .. } => (*pos, elem.style),
        _ => {
            app.text_edit = None;
            return;
        }
    };
    // 预览模式画布 rect ≠ ctx.content_rect，用画布帧缓存的 last_view
    let view = app.last_view;
    let anchor = view.to_pt(pos);
    let font_pt = view.len_pt(style.font_size);

    let mut commit = false;
    let mut cancel = false;
    egui::Area::new(egui::Id::new("text-editor"))
        .fixed_pos(anchor)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                let te = egui::TextEdit::multiline(&mut edit.buffer)
                    .font(FontId::proportional(font_pt))
                    .text_color(egui_color(style.color))
                    .desired_rows(1)
                    .desired_width(320.0)
                    .hint_text("输入文本…");
                let r = ui.add(te);
                // 只在未持有焦点时请求：egui 的 request_focus 会无条件
                // interrupt_ime，每帧调用等于每帧销毁 IME 上下文，
                // 中文输入法（fcitx5 等）的组合窗口刚弹出就被杀，无法输入中文
                if !r.has_focus() {
                    r.request_focus();
                }
                ui.horizontal(|ui| {
                    if ui.button("确定 (Ctrl+Enter)").clicked()
                        || ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::Enter))
                    {
                        commit = true;
                    }
                    if ui.button("取消").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape))
                    {
                        cancel = true;
                    }
                });
            });
        });

    if commit {
        app.commit_text_edit();
    } else if cancel {
        app.text_edit = None;
        app.doc.cancel_change();
    }
}
