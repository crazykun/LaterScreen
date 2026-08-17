//! 工具栏：悬浮在选区下方（放不下则上方）。

use eframe::egui;
use egui::{Align2, Color32, Pos2, Stroke, StrokeKind, Vec2};
use lscreen_core::{Rgba, Tool};

use super::{egui_color, SnipApp, View};

const TOOLS: &[(Tool, &str, &str)] = &[
    (Tool::Select, "选择", "选择/移动/编辑已绘制元素"),
    (Tool::Rect, "矩形", "拖拽绘制矩形，Shift 正方形"),
    (Tool::Ellipse, "椭圆", "拖拽绘制椭圆，Shift 正圆"),
    (Tool::Arrow, "箭头", "拖拽绘制箭头"),
    (Tool::Curve, "画笔", "自由曲线，Shift 直线"),
    (Tool::Marker, "标号", "单击放置自增序号"),
    (Tool::Text, "文本", "单击输入文字"),
    (Tool::Mosaic, "马赛克", "涂抹打码"),
    (Tool::Eraser, "橡皮", "擦除标注恢复原图"),
];

const PALETTE: &[Rgba] = &[
    Rgba([0xe5, 0x39, 0x35, 0xff]), // 红
    Rgba([0xfb, 0x8c, 0x00, 0xff]), // 橙
    Rgba([0xfd, 0xd8, 0x35, 0xff]), // 黄
    Rgba([0x43, 0xa0, 0x47, 0xff]), // 绿
    Rgba([0x1e, 0x88, 0xe5, 0xff]), // 蓝
    Rgba([0x8e, 0x24, 0xaa, 0xff]), // 紫
    Rgba([0x00, 0x00, 0x00, 0xff]), // 黑
    Rgba([0xff, 0xff, 0xff, 0xff]), // 白
];

pub fn show(app: &mut SnipApp, ctx: &egui::Context) {
    let screen = ctx.content_rect();
    let view = View {
        origin: screen.min,
        scale: app.shot.width as f32 / screen.width().max(1.0),
    };
    let region_pt = view.rect_pt(app.region);

    // 默认放选区下方；空间不足放上方；再不足贴屏幕底部
    const BAR_H: f32 = 40.0;
    let y = if region_pt.max.y + BAR_H + 12.0 < screen.max.y {
        region_pt.max.y + 8.0
    } else if region_pt.min.y - BAR_H - 12.0 > screen.min.y {
        region_pt.min.y - BAR_H - 8.0
    } else {
        screen.max.y - BAR_H - 8.0
    };
    let x = region_pt
        .min
        .x
        .clamp(screen.min.x + 4.0, (screen.max.x - 620.0).max(screen.min.x + 4.0));

    egui::Area::new(egui::Id::new("toolbar"))
        .fixed_pos(Pos2::new(x, y))
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.horizontal(|ui| {
                    bar_contents(app, ui, ctx);
                });
            });
        });
}

fn bar_contents(app: &mut SnipApp, ui: &mut egui::Ui, ctx: &egui::Context) {
    for (tool, label, tip) in TOOLS {
        let active = app.tool == *tool;
        if ui.selectable_label(active, *label).on_hover_text(*tip).clicked() {
            app.tool = *tool;
            if *tool != Tool::Select {
                app.selected = None;
            }
        }
    }
    ui.separator();

    // 颜色
    for c in PALETTE {
        let (rect, resp) =
            ui.allocate_exact_size(Vec2::splat(16.0), egui::Sense::click());
        let painter = ui.painter();
        painter.rect_filled(rect.shrink(1.0), 3.0, egui_color(*c));
        if app.style.color == *c {
            painter.rect_stroke(
                rect,
                3.0,
                Stroke::new(2.0, Color32::from_rgb(0x21, 0x96, 0xf3)),
                StrokeKind::Outside,
            );
        } else {
            painter.rect_stroke(
                rect.shrink(1.0),
                3.0,
                Stroke::new(1.0, Color32::from_gray(120)),
                StrokeKind::Inside,
            );
        }
        if resp.clicked() {
            app.style.color = *c;
            apply_style_to_selected(app);
        }
    }
    ui.separator();

    // 线宽 / 字号
    let mut width = app.style.width;
    ui.label("粗细");
    if ui
        .add(egui::Slider::new(&mut width, 1.0..=12.0).show_value(false))
        .changed()
    {
        app.style.width = width;
        app.style.font_size = 12.0 + width * 4.0;
        apply_style_to_selected(app);
    }
    ui.separator();

    let can_undo = app.doc.can_undo();
    let can_redo = app.doc.can_redo();
    if ui
        .add_enabled(can_undo, egui::Button::new("撤销"))
        .on_hover_text("Ctrl+Z")
        .clicked()
    {
        app.selected = None;
        app.doc.undo();
    }
    if ui
        .add_enabled(can_redo, egui::Button::new("重做"))
        .on_hover_text("Ctrl+Y")
        .clicked()
    {
        app.selected = None;
        app.doc.redo();
    }
    ui.separator();

    if ui.button("保存").on_hover_text("Ctrl+S 保存为 PNG").clicked() {
        app.save_and_exit(ctx);
    }
    if ui
        .button("复制")
        .on_hover_text("Ctrl+C / 双击 复制到剪贴板并退出")
        .clicked()
    {
        app.copy_and_exit(ctx);
    }
    if ui.button("✕").on_hover_text("Esc 退出").clicked() {
        app.request_close(ctx);
    }
    let _ = Align2::CENTER_CENTER; // 保留引用避免未使用告警（后续版本使用）
}

/// 修改样式时同步应用到当前选中的图元。
fn apply_style_to_selected(app: &mut SnipApp) {
    let Some(id) = app.selected else { return };
    app.doc.begin_change();
    if let Some(e) = app.doc.get_mut(id) {
        e.style = app.style;
    }
    app.mosaic_cache.remove(&id);
}
