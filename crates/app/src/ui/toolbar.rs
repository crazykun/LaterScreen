//! 工具栏：悬浮在选区下方（放不下则上方）。
//!
//! 按钮全部为 Painter 手绘矢量图标（12px 线稿）+ hover 文案：
//! 不依赖任何字体图标/emoji 覆盖，任何系统上渲染一致；文字仅用于
//! 标号「1」与 OCR「A」两个拉丁字形（egui 内置字体必有）。

use eframe::egui;
use egui::{Color32, Pos2, Rect, Response, Sense, Shape, Stroke, StrokeKind, Vec2};
use lscreen_core::{ElementKind, Rgba, Tool, P2};

use super::{egui_color, SnipApp, View};
use crate::theme::{ACCENT_FILL, ACCENT_ON_TEXT};

/// 图标按钮边长（逻辑点）
const BTN: f32 = 24.0;
/// 宽屏工具栏的目标宽度，用于位置夹取；实际宽度仍受窗口可用宽度限制。
const BAR_W: f32 = 900.0;

/// 工具栏在当前可用宽度下必须留在主栏的操作。
/// 输入来自当前窗口的逻辑像素宽度，结果供工具栏布局和回归测试共同使用。
#[derive(Clone, Copy)]
struct ToolbarPlan {
    copy_inline: bool,
    close_inline: bool,
    more_inline: bool,
    low_frequency_inline: bool,
}

/// 根据窗口可用宽度生成工具栏计划。
/// 复制、关闭与更多是任何尺寸下的逃生入口，不能因响应式收纳而隐藏。
fn toolbar_plan(available_width: f32) -> ToolbarPlan {
    ToolbarPlan {
        copy_inline: true,
        close_inline: true,
        more_inline: true,
        low_frequency_inline: available_width >= 900.0,
    }
}

const TOOLS: &[(Tool, &str)] = &[
    (Tool::Select, "选择：移动/编辑已绘制元素"),
    (Tool::Rect, "矩形：拖拽绘制，Shift 正方形"),
    (Tool::Ellipse, "椭圆：拖拽绘制，Shift 正圆"),
    (Tool::Arrow, "箭头：拖拽绘制"),
    (Tool::Curve, "画笔：自由曲线，Shift 直线"),
    (Tool::Marker, "标号：单击放置自增序号"),
    (Tool::Text, "文本：单击输入文字"),
    (Tool::Mosaic, "马赛克：涂抹打码"),
    (Tool::Eraser, "橡皮：擦除标注恢复原图"),
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

/// 在截图选区附近显示响应式工具栏。
/// 输入来自当前截图会话与 egui 窗口，工具栏动作直接回写会话并用于保存、复制或退出。
pub fn show(app: &mut SnipApp, ctx: &egui::Context) {
    let screen = ctx.content_rect();
    let available_width = (screen.width() - 8.0).max(140.0);
    let plan = toolbar_plan(available_width);
    let bar_width = BAR_W.min(available_width);
    let view = View {
        origin: screen.min,
        scale: app.shot.width as f32 / screen.width().max(1.0),
    };
    let region_pt = view.rect_pt(app.region);

    // 中窄布局会把编辑控件折成多行；提前为工具栏预留高度，避免贴屏底时被裁掉。
    let bar_height = if plan.low_frequency_inline {
        40.0
    } else if available_width < 320.0 {
        136.0
    } else {
        96.0
    };
    // 预览模式（滚动长截图标注）：工具栏常驻窗口底部居中；
    // 窗口比工具栏窄时靠左钳住，保证第一批按钮可达（右侧溢出可拉宽窗口）
    let (x, y) = if app.preview {
        (
            (screen.center().x - bar_width / 2.0).max(screen.min.x + 4.0),
            (screen.max.y - bar_height - 6.0).max(screen.min.y),
        )
    } else {
        // 默认放选区下方；空间不足放上方；再不足贴屏幕底部
        let y = if region_pt.max.y + bar_height + 12.0 < screen.max.y {
            region_pt.max.y + 8.0
        } else if region_pt.min.y - bar_height - 12.0 > screen.min.y {
            region_pt.min.y - bar_height - 8.0
        } else {
            (screen.max.y - bar_height - 8.0).max(screen.min.y)
        };
        let x = region_pt
            .min
            .x
            .max(screen.min.x + 4.0)
            .min((screen.max.x - bar_width).max(screen.min.x + 4.0));
        (x, y)
    };

    egui::Area::new(egui::Id::new("toolbar"))
        .fixed_pos(Pos2::new(x, y))
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_max_width(bar_width);
                bar_contents(app, ui, ctx, plan);
            });
        });
}

/// 按宽度计划排列编辑控件与交付动作。
/// 宽屏使用单行；中窄窗口把低频动作收进“更多”，并允许编辑控件换行。
fn bar_contents(
    app: &mut SnipApp,
    ui: &mut egui::Ui,
    ctx: &egui::Context,
    plan: ToolbarPlan,
) {
    ui.spacing_mut().item_spacing.x = 2.0;
    if plan.low_frequency_inline {
        ui.horizontal(|ui| {
            editing_controls(app, ui);
            ui.separator();
            delivery_actions(app, ui, ctx, plan);
        });
    } else {
        ui.vertical(|ui| {
            // 中窄窗口先让标注控件自然折行，第二行固定保留退出、更多与复制，
            // 这样再窄的窗口也不会把完成或退出入口挤到屏幕外。
            ui.horizontal_wrapped(|ui| editing_controls(app, ui));
            ui.separator();
            ui.horizontal(|ui| delivery_actions(app, ui, ctx, plan));
        });
    }
}

/// 绘制工具、颜色、粗细和撤销重做等编辑控件。
/// 控件只修改当前截图会话，不执行保存、上传或退出等外部副作用。
fn editing_controls(app: &mut SnipApp, ui: &mut egui::Ui) {
    // 预览模式选区固定整图，尺寸编辑不适用
    if !app.preview {
        size_edit(app, ui);
        ui.separator();
    }

    for (tool, tip) in TOOLS {
        let active = app.tool == *tool;
        let resp = icon_button(ui, active, tip, |p, r, c| draw_tool_icon(*tool, p, r, c));
        if resp.clicked() {
            app.tool = *tool;
            if *tool != Tool::Select {
                app.selected = None;
            }
        }
    }
    ui.separator();

    color_picker(app, ui);
    ui.separator();

    // 文字背景（M13）：仅文本工具显示，只影响之后新建的文字
    if app.tool == Tool::Text {
        let active = app.text_bg;
        let cur = egui_color(app.style.color);
        let resp = icon_button(
            ui,
            active,
            "文字背景：开 = 当前色圆角底 + 自动对比字色（仅对新建文字生效）",
            move |p, r, _c| draw_text_bg_icon(p, r, cur, active),
        );
        if resp.clicked() {
            app.text_bg = !app.text_bg;
        }
        ui.separator();
    }

    // 线宽 / 字号
    ui.spacing_mut().slider_width = 56.0;
    let mut width = app.style.width;
    let slider = ui
        .add(egui::Slider::new(&mut width, 1.0..=12.0).show_value(false))
        .on_hover_text("粗细：线宽与字号");
    if slider.changed() {
        app.style.width = width;
        app.style.font_size = 12.0 + width * 4.0;
        // 拖拽期间每帧都会 changed()，只在手势起点压一次快照，
        // 否则一次拖动几十个快照会把真实历史挤出撤销栈（上限 100）。
        let snapshot = !slider.dragged() || slider.drag_started();
        apply_style_to_selected(app, snapshot);
    }
    ui.separator();

    let can_undo = app.doc.can_undo();
    let can_redo = app.doc.can_redo();
    if action_button(ui, can_undo, "撤销 (Ctrl+Z)", draw_undo) {
        app.selected = None;
        app.doc.undo();
    }
    if action_button(ui, can_redo, "重做 (Ctrl+Y)", draw_redo) {
        app.selected = None;
        app.doc.redo();
    }
}

/// 绘制交付与退出动作，并按宽度计划决定低频动作是否内联。
/// 保存、上传、贴图和识别会触发各自业务流程；复制与关闭始终留在主栏。
fn delivery_actions(
    app: &mut SnipApp,
    ui: &mut egui::Ui,
    ctx: &egui::Context,
    plan: ToolbarPlan,
) {
    if plan.close_inline && close_button(ui) {
        // 关闭只发出窗口退出请求，不保存、不复制，避免与右侧主操作混淆。
        app.request_close(ctx);
    }
    ui.separator();

    if plan.low_frequency_inline {
        inline_low_frequency_actions(app, ui, ctx);
    }

    if plan.more_inline {
        more_menu(app, ui, ctx, !plan.low_frequency_inline);
    }
    ui.add_space(6.0);

    if plan.copy_inline && primary_copy_button(ui) {
        // 主操作会合成当前选区、写入剪贴板并关闭截图窗口。
        app.copy_and_exit(ctx);
    }
}

/// 在宽屏主栏展示保存、上传、贴图与识别动作。
/// 这些动作在中窄窗口会由“更多”菜单承接，避免挤掉复制和关闭。
fn inline_low_frequency_actions(app: &mut SnipApp, ui: &mut egui::Ui, ctx: &egui::Context) {
    if action_button(ui, true, "保存为 PNG (Ctrl+S)", draw_save) {
        app.save_and_exit(ctx);
    }
    // 上传（M15）：仅在配置了 [upload].command 时显示；上传中转禁用态防连点
    if app.config.upload_command().is_some() {
        let busy = app.upload_job.is_some();
        let tip = if busy {
            "上传中…"
        } else {
            "上传：保存并交给外部命令，链接自动复制"
        };
        if action_button(ui, !busy, tip, draw_upload) {
            app.upload_and_exit(ctx);
        }
    }
    if action_button(ui, true, "贴图：把选区钉在屏幕上 (Ctrl+P)", draw_pin) {
        app.pin_and_exit(ctx);
    }
    if action_button(ui, true, "识别选区内的二维码", draw_qr) {
        app.scan_qr(ctx);
    }
    if action_button(ui, true, "识别选区内的文字 (OCR)", draw_ocr) {
        app.scan_ocr(ctx);
    }
}

/// 显示“更多”菜单；中窄宽度收纳全部低频动作，宽屏仍保留二维码生成入口。
/// 菜单项接收当前选区业务数据，可能保存文件、调用上传命令、识别内容或创建贴图。
fn more_menu(app: &mut SnipApp, ui: &mut egui::Ui, ctx: &egui::Context, include_all: bool) {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(44.0, 32.0), Sense::click());
    let visuals = ui.style().interact(&response);
    if response.hovered() {
        ui.painter().rect_filled(rect, 4.0, visuals.bg_fill);
    }
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        "更多",
        egui::FontId::proportional(13.0),
        visuals.fg_stroke.color,
    );
    let response = response.on_hover_text("更多操作");
    egui::Popup::menu(&response)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            ui.set_min_width(190.0);
            if include_all {
                // 中窄窗口把会产生文件、外部命令或识别任务的低频动作统一收纳到此处。
                if ui.button("保存为 PNG (Ctrl+S)").clicked() {
                    // 合成当前选区并写入配置的保存目录，成功后退出截图窗口。
                    app.save_and_exit(ctx);
                    ui.close();
                }
                let upload_busy = app.upload_job.is_some();
                if app.config.upload_command().is_some()
                    && ui
                        .add_enabled(!upload_busy, egui::Button::new("上传并复制链接"))
                        .clicked()
                {
                    // 先保存选区，再把文件路径交给用户配置的上传命令，并复制返回链接。
                    app.upload_and_exit(ctx);
                    ui.close();
                }
                if ui.button("贴图 (Ctrl+P)").clicked() {
                    // 导出当前选区并创建独立贴图窗口，原截图窗口随即退出。
                    app.pin_and_exit(ctx);
                    ui.close();
                }
                if ui.button("识别二维码").clicked() {
                    // 异步扫描当前选区，结果稍后回到截图界面展示或复制。
                    app.scan_qr(ctx);
                    ui.close();
                }
                if ui.button("识别文字 (OCR)").clicked() {
                    // 异步识别当前选区文字，避免阻塞覆盖层绘制。
                    app.scan_ocr(ctx);
                    ui.close();
                }
                ui.separator();
            }
            qr_generate_menu(app, ui, ctx);
        });
}

/// 在“更多”中提供二维码文本输入并把生成结果插入标注层。
/// 输入来自用户文本，成功后生成位图图元供当前文档继续移动和缩放。
fn qr_generate_menu(app: &mut SnipApp, ui: &mut egui::Ui, ctx: &egui::Context) {
    ui.menu_button("生成二维码…", |ui| {
        ui.set_min_width(230.0);
        let mut buffer = app.qr_input.clone().unwrap_or_default();
        ui.label("二维码内容");
        let edit = egui::TextEdit::singleline(&mut buffer).desired_width(f32::INFINITY);
        let response = ui.add(edit);
        let mut insert = response.lost_focus()
            && ui.input(|input| input.key_pressed(egui::Key::Enter));
        app.qr_input = Some(buffer);
        ui.horizontal(|ui| {
            if ui.button("插入").clicked() {
                insert = true;
            }
            if ui.button("取消").clicked() {
                app.qr_input = None;
                ui.close();
            }
        });
        if insert {
            let text = app.qr_input.take().unwrap_or_default();
            let text = text.trim();
            if !text.is_empty() {
                // 生成二维码位图并写入当前文档，后续仍可移动、缩放和撤销。
                app.insert_qr(ctx, text);
            }
            ui.close();
        }
    });
}

/// 绘制独立的关闭按钮；返回是否请求退出，不直接修改截图会话。
/// 悬停使用品牌强调色提示这是退出动作，并与复制按钮保持分隔。
fn close_button(ui: &mut egui::Ui) -> bool {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(32.0), Sense::click());
    if response.hovered() {
        ui.painter()
            .rect_filled(rect, 4.0, ACCENT_FILL.gamma_multiply(0.18));
    }
    let color = if response.hovered() {
        ACCENT_FILL
    } else {
        ui.style().interact(&response).fg_stroke.color
    };
    draw_close(ui.painter(), rect.shrink(9.0), color);
    response.on_hover_text("退出 (Esc)").clicked()
}

/// 绘制带文字的主复制按钮；返回是否执行“复制并退出”。
/// 该按钮固定为 76×32 逻辑点，让完成动作比普通图标更醒目且含义更明确。
fn primary_copy_button(ui: &mut egui::Ui) -> bool {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(76.0, 32.0), Sense::click());
    let fill = if response.hovered() {
        ACCENT_FILL.gamma_multiply(1.08)
    } else {
        ACCENT_FILL
    };
    ui.painter().rect_filled(rect, 5.0, fill);
    let icon_rect = Rect::from_min_size(rect.min + Vec2::new(9.0, 9.0), Vec2::splat(14.0));
    draw_check(ui.painter(), icon_rect, ACCENT_ON_TEXT);
    ui.painter().text(
        Pos2::new(rect.min.x + 48.0, rect.center().y),
        egui::Align2::CENTER_CENTER,
        "复制",
        egui::FontId::proportional(14.0),
        ACCENT_ON_TEXT,
    );
    response
        .on_hover_text("复制到剪贴板并退出 (Ctrl+C / Enter)")
        .clicked()
}

/// 选区尺寸编辑：宽 × 高，可拖可双击输入。
/// 原先悬浮在选区左上角，会盖住选区内容，故并入工具栏最左侧。
///
/// 编辑期间（输入框聚焦或拖拽中）数值只进暂存 `size_edit_buf`，
/// 失焦/松手才一次性应用——键入 "1920" 逐字符生效会让选区先跳到
/// 1、19、192 各闪一帧。
fn size_edit(app: &mut SnipApp, ui: &mut egui::Ui) {
    let (sw, sh) = (app.shot.width as f32, app.shot.height as f32);
    let (mut w, mut h) = app
        .size_edit_buf
        .unwrap_or_else(|| (app.region.width().round(), app.region.height().round()));

    let mut editing = false;
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.x = 3.0;
        // 与图标按钮（BTN）同高：压小内边距，抬高交互高度，字号用正文而非 Small
        ui.spacing_mut().interact_size.y = BTN;
        ui.spacing_mut().button_padding = Vec2::new(5.0, 2.0);
        ui.style_mut().drag_value_text_style = egui::TextStyle::Body;
        let rw = ui
            .add(
                egui::DragValue::new(&mut w)
                    .speed(1.0)
                    .range(1.0..=sw as f64),
            )
            .on_hover_text("宽（像素）：拖动或双击输入，离开生效");
        ui.label("×");
        let rh = ui
            .add(
                egui::DragValue::new(&mut h)
                    .speed(1.0)
                    .range(1.0..=sh as f64),
            )
            .on_hover_text("高（像素）：拖动或双击输入，离开生效");
        editing = rw.has_focus() || rh.has_focus() || rw.dragged() || rh.dragged();
    });

    if editing {
        app.size_edit_buf = Some((w, h));
    } else if let Some((w, h)) = app.size_edit_buf.take() {
        app.set_region_size(w, h);
    }
}

/// 当前色按钮 + 弹出调色板：取代常驻 8 个色块。
fn color_picker(app: &mut SnipApp, ui: &mut egui::Ui) {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(BTN), Sense::click());
    let painter = ui.painter();
    let swatch = rect.shrink(5.0);
    painter.rect_filled(swatch, 3.0, egui_color(app.style.color));
    painter.rect_stroke(
        swatch,
        3.0,
        Stroke::new(1.0, ui.visuals().widgets.inactive.fg_stroke.color),
        StrokeKind::Outside,
    );
    let resp = resp.on_hover_text("颜色");

    // CloseOnClickOutside：内嵌的自定义取色器要能连续交互，
    // 预设色块点击后用 ui.close() 显式收起
    egui::Popup::menu(&resp)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            ui.set_max_width(220.0);
            // 取色器的色域方块/滑条宽度取自 slider_width（默认 100），
            // 不加宽会缩在弹层左半边，右侧留一大片空白
            ui.spacing_mut().slider_width = 205.0;
            ui.horizontal_wrapped(|ui| {
                for c in PALETTE {
                    let (rect, resp) =
                        ui.allocate_exact_size(Vec2::splat(BTN - 4.0), Sense::click());
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
                        apply_style_to_selected(app, true);
                        ui.close();
                    }
                }
            });
            ui.separator();
            // 内联完整取色器：不用 color_edit_button 的嵌套 popup——
            // 嵌套弹层会被外层 popup 的关闭逻辑连坐，点开即塌
            let mut c32 = egui_color(app.style.color);
            if egui::color_picker::color_picker_color32(
                ui,
                &mut c32,
                egui::color_picker::Alpha::Opaque,
            ) {
                app.style.color = Rgba([c32.r(), c32.g(), c32.b(), 0xff]);
                // 取色器拖动每帧触发变更，按时间窗节流快照：
                // 间隔 >0.6s 视为新一次调整，压一次撤销快照
                let now = ui.input(|i| i.time);
                let snapshot = now - app.color_drag_at > 0.6;
                app.color_drag_at = now;
                apply_style_to_selected(app, snapshot);
            }
        });
}

/// 可选中的图标按钮（工具）。
/// pub(crate)：贴图窗口（pin.rs）复用做置顶开关的选中态样式。
pub(crate) fn icon_button(
    ui: &mut egui::Ui,
    active: bool,
    tip: &str,
    draw: impl FnOnce(&egui::Painter, Rect, Color32),
) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(BTN), Sense::click());
    let vis = ui.style().interact_selectable(&resp, active);
    if active || resp.hovered() {
        ui.painter().rect_filled(rect, 4.0, vis.bg_fill);
    }
    draw(ui.painter(), rect.shrink(6.0), vis.fg_stroke.color);
    resp.on_hover_text(tip)
}

/// 动作图标按钮（可禁用），返回是否被点击。
/// pub(crate)：贴图窗口（pin.rs）复用同一套按钮与图标，风格一致。
pub(crate) fn action_button(
    ui: &mut egui::Ui,
    enabled: bool,
    tip: &str,
    draw: impl FnOnce(&egui::Painter, Rect, Color32),
) -> bool {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(BTN), Sense::click());
    let vis = if enabled {
        ui.style().interact(&resp)
    } else {
        &ui.style().visuals.widgets.noninteractive
    };
    if enabled && resp.hovered() {
        ui.painter().rect_filled(rect, 4.0, vis.bg_fill);
    }
    let color = if enabled {
        vis.fg_stroke.color
    } else {
        ui.visuals().weak_text_color()
    };
    draw(ui.painter(), rect.shrink(6.0), color);
    let resp = resp.on_hover_text(tip);
    enabled && resp.clicked()
}

fn draw_tool_icon(tool: Tool, p: &egui::Painter, r: Rect, c: Color32) {
    match tool {
        Tool::Select => draw_cursor(p, r, c),
        Tool::Rect => {
            p.rect_stroke(r.shrink(0.5), 1.0, Stroke::new(1.4, c), StrokeKind::Inside);
        }
        Tool::Ellipse => {
            p.circle_stroke(r.center(), r.width() * 0.48, Stroke::new(1.4, c));
        }
        Tool::Arrow => {
            p.arrow(
                r.left_bottom(),
                r.right_top() - r.left_bottom(),
                Stroke::new(1.4, c),
            );
        }
        Tool::Curve => {
            let w = r.width();
            let pts = vec![
                r.left_bottom(),
                Pos2::new(r.min.x + w * 0.35, r.min.y + w * 0.25),
                Pos2::new(r.min.x + w * 0.65, r.min.y + w * 0.75),
                r.right_top(),
            ];
            p.add(Shape::line(pts, Stroke::new(1.4, c)));
        }
        Tool::Marker => {
            p.circle_stroke(r.center(), r.width() * 0.48, Stroke::new(1.2, c));
            p.text(
                r.center(),
                egui::Align2::CENTER_CENTER,
                "1",
                egui::FontId::proportional(r.height() * 0.75),
                c,
            );
        }
        Tool::Text => {
            let s = Stroke::new(1.4, c);
            p.line_segment([r.left_top(), r.right_top()], s);
            p.line_segment([r.center_top(), r.center_bottom()], s);
        }
        Tool::Mosaic => {
            let cell = r.width() / 3.0;
            for gy in 0..3 {
                for gx in 0..3 {
                    if (gx + gy) % 2 == 0 {
                        let min = Pos2::new(r.min.x + gx as f32 * cell, r.min.y + gy as f32 * cell);
                        p.rect_filled(Rect::from_min_size(min, Vec2::splat(cell)), 0.0, c);
                    }
                }
            }
        }
        Tool::Eraser => {
            // 正立排刷：上柄 + 中箍 + 刷毛竖线（斜置版本会被误读成箭头）
            let w = r.width();
            let o = r.min;
            p.rect_filled(
                Rect::from_min_max(
                    o + Vec2::new(w * 0.38, 0.0),
                    o + Vec2::new(w * 0.62, w * 0.28),
                ),
                0.5,
                c,
            );
            p.rect_filled(
                Rect::from_min_max(
                    o + Vec2::new(w * 0.10, w * 0.30),
                    o + Vec2::new(w * 0.90, w * 0.56),
                ),
                0.5,
                c,
            );
            let s = Stroke::new(1.3, c);
            for i in 0..4 {
                let x = o.x + w * (0.18 + i as f32 * 0.213);
                p.line_segment(
                    [Pos2::new(x, o.y + w * 0.62), Pos2::new(x, o.y + w * 1.0)],
                    s,
                );
            }
        }
        Tool::Line => {
            p.line_segment([r.left_bottom(), r.right_top()], Stroke::new(1.4, c));
        }
    }
}

/// 鼠标指针箭头（选择工具）。
fn draw_cursor(p: &egui::Painter, r: Rect, c: Color32) {
    let w = r.width();
    let o = r.min;
    let pts = vec![
        o + Vec2::new(w * 0.15, 0.0),
        o + Vec2::new(w * 0.15, w * 0.85),
        o + Vec2::new(w * 0.38, w * 0.63),
        o + Vec2::new(w * 0.55, w * 1.0),
        o + Vec2::new(w * 0.70, w * 0.92),
        o + Vec2::new(w * 0.53, w * 0.56),
        o + Vec2::new(w * 0.85, w * 0.53),
    ];
    p.add(Shape::closed_line(pts, Stroke::new(1.2, c)));
}

fn draw_undo(p: &egui::Painter, r: Rect, c: Color32) {
    let s = Stroke::new(1.4, c);
    let w = r.width();
    // 底部弧线简化为折线 + 左向箭头
    p.line_segment(
        [
            Pos2::new(r.min.x, r.min.y + w * 0.35),
            Pos2::new(r.max.x - w * 0.2, r.min.y + w * 0.35),
        ],
        s,
    );
    p.line_segment(
        [
            Pos2::new(r.max.x - w * 0.2, r.min.y + w * 0.35),
            Pos2::new(r.max.x - w * 0.2, r.max.y - w * 0.1),
        ],
        s,
    );
    // 箭头头部（指向左）
    let tip = Pos2::new(r.min.x, r.min.y + w * 0.35);
    p.line_segment([tip, tip + Vec2::new(w * 0.28, -w * 0.25)], s);
    p.line_segment([tip, tip + Vec2::new(w * 0.28, w * 0.25)], s);
}

fn draw_redo(p: &egui::Painter, r: Rect, c: Color32) {
    let s = Stroke::new(1.4, c);
    let w = r.width();
    p.line_segment(
        [
            Pos2::new(r.max.x, r.min.y + w * 0.35),
            Pos2::new(r.min.x + w * 0.2, r.min.y + w * 0.35),
        ],
        s,
    );
    p.line_segment(
        [
            Pos2::new(r.min.x + w * 0.2, r.min.y + w * 0.35),
            Pos2::new(r.min.x + w * 0.2, r.max.y - w * 0.1),
        ],
        s,
    );
    let tip = Pos2::new(r.max.x, r.min.y + w * 0.35);
    p.line_segment([tip, tip + Vec2::new(-w * 0.28, -w * 0.25)], s);
    p.line_segment([tip, tip + Vec2::new(-w * 0.28, w * 0.25)], s);
}

/// 保存图标：下载样式——向下箭头 + 底部横线（比软盘更直白，主流画法）。
pub(crate) fn draw_save(p: &egui::Painter, r: Rect, c: Color32) {
    let s = Stroke::new(1.5, c);
    let w = r.width();
    let cx = r.center().x;
    // 竖干 + 两翼箭头
    p.line_segment(
        [
            Pos2::new(cx, r.min.y + w * 0.10),
            Pos2::new(cx, r.min.y + w * 0.58),
        ],
        s,
    );
    for dx in [-w * 0.22, w * 0.22] {
        p.line_segment(
            [
                Pos2::new(cx + dx, r.min.y + w * 0.36),
                Pos2::new(cx, r.min.y + w * 0.58),
            ],
            s,
        );
    }
    // 底部托盘横线
    p.line_segment(
        [
            Pos2::new(r.min.x + w * 0.10, r.min.y + w * 0.88),
            Pos2::new(r.min.x + w * 0.90, r.min.y + w * 0.88),
        ],
        s,
    );
}

/// 绿色对号：复制并退出（最常用动作，固定绿色突出）。
pub(crate) fn draw_check(p: &egui::Painter, r: Rect, _c: Color32) {
    let s = Stroke::new(2.0, Color32::from_rgb(0x4c, 0xaf, 0x50));
    let w = r.width();
    p.add(Shape::line(
        vec![
            Pos2::new(r.min.x, r.min.y + w * 0.55),
            Pos2::new(r.min.x + w * 0.38, r.max.y),
            Pos2::new(r.max.x, r.min.y + w * 0.08),
        ],
        s,
    ));
}

/// 图钉（地图钉样式）：圆头 + 两侧汇聚线到针尖。
pub(crate) fn draw_pin(p: &egui::Painter, r: Rect, c: Color32) {
    let s = Stroke::new(1.4, c);
    let w = r.width();
    let head = Pos2::new(r.center().x, r.min.y + w * 0.36);
    let rad = w * 0.32;
    p.circle_stroke(head, rad, s);
    let tip = Pos2::new(r.center().x, r.max.y);
    p.line_segment([Pos2::new(head.x - rad, head.y + rad * 0.45), tip], s);
    p.line_segment([Pos2::new(head.x + rad, head.y + rad * 0.45), tip], s);
}

/// 上传（M15）：云朵 + 上行箭头——「发出去」语义，与保存（下行入托盘）对仗。
/// 云底用三段折线拼碗形，避免圆弧 API 的跨版本差异。
pub(crate) fn draw_upload(p: &egui::Painter, r: Rect, c: Color32) {
    let s = Stroke::new(1.4, c);
    let w = r.width();
    let cx = r.center().x;
    // 上行箭头：竖干 + 两翼
    p.line_segment(
        [
            Pos2::new(cx, r.min.y + w * 0.16),
            Pos2::new(cx, r.min.y + w * 0.52),
        ],
        s,
    );
    for dx in [-w * 0.2, w * 0.2] {
        p.line_segment(
            [
                Pos2::new(cx + dx, r.min.y + w * 0.34),
                Pos2::new(cx, r.min.y + w * 0.52),
            ],
            s,
        );
    }
    // 云碗：左沿—左肩—中凹—右肩—右沿，五点折线。肩部起伏给足，
    // 缩到 16px 也保得住「云」的轮廓而不是一条平线
    let bowl = [
        Pos2::new(r.min.x + w * 0.08, r.min.y + w * 0.76),
        Pos2::new(r.min.x + w * 0.22, r.min.y + w * 0.58),
        Pos2::new(r.min.x + w * 0.44, r.min.y + w * 0.70),
        Pos2::new(r.min.x + w * 0.66, r.min.y + w * 0.54),
        Pos2::new(r.max.x - w * 0.08, r.min.y + w * 0.76),
    ];
    p.add(Shape::line(bowl.to_vec(), s));
}

fn draw_qr(p: &egui::Painter, r: Rect, c: Color32) {
    let w = r.width();
    let sq = w * 0.4;
    let corner = |min: Pos2| {
        p.rect_stroke(
            Rect::from_min_size(min, Vec2::splat(sq)),
            0.0,
            Stroke::new(1.2, c),
            StrokeKind::Inside,
        );
    };
    corner(r.min);
    corner(Pos2::new(r.max.x - sq, r.min.y));
    corner(Pos2::new(r.min.x, r.max.y - sq));
    p.rect_filled(
        Rect::from_min_size(r.max - Vec2::splat(sq * 0.9), Vec2::splat(sq * 0.6)),
        0.0,
        c,
    );
}

fn draw_ocr(p: &egui::Painter, r: Rect, c: Color32) {
    p.text(
        Pos2::new(r.center().x, r.min.y + r.height() * 0.38),
        egui::Align2::CENTER_CENTER,
        "A",
        egui::FontId::proportional(r.height() * 0.9),
        c,
    );
    p.line_segment(
        [
            Pos2::new(r.min.x, r.max.y - 1.0),
            Pos2::new(r.max.x, r.max.y - 1.0),
        ],
        Stroke::new(1.3, c),
    );
}

/// 文字背景图标（M13）：开 = 当前色填充块 + 对比色 A；关 = 空心块。
/// 预览语义与真实渲染一致（底色块上自动对比字色）。
fn draw_text_bg_icon(p: &egui::Painter, r: Rect, fill: Color32, active: bool) {
    if active {
        p.rect_filled(r, 2.0, fill);
    } else {
        p.rect_stroke(r, 2.0, Stroke::new(1.2, fill), StrokeKind::Inside);
    }
    let glyph = if active {
        let luma = 0.299 * fill.r() as f32 + 0.587 * fill.g() as f32 + 0.114 * fill.b() as f32;
        if luma < 128.0 {
            Color32::WHITE
        } else {
            Color32::from_rgb(0x11, 0x11, 0x11)
        }
    } else {
        fill
    };
    p.text(
        r.center(),
        egui::Align2::CENTER_CENTER,
        "A",
        egui::FontId::proportional(r.height() * 0.75),
        glyph,
    );
}

pub(crate) fn draw_close(p: &egui::Painter, r: Rect, c: Color32) {
    let s = Stroke::new(1.5, c);
    let r = r.shrink(1.0);
    p.line_segment([r.left_top(), r.right_bottom()], s);
    p.line_segment([r.right_top(), r.left_bottom()], s);
}

/// 修改样式时同步应用到当前选中的图元。
///
/// `snapshot=false` 用于连续手势（滑杆拖动）的中间帧：只改数据不压撤销快照。
fn apply_style_to_selected(app: &mut SnipApp, snapshot: bool) {
    let Some(id) = app.selected else { return };
    if snapshot {
        app.doc.begin_change();
    }
    let style = app.style;
    // font_size 随粗细变化，文本的包围盒必须跟着重测，
    // 否则命中检测与选中框停留在旧字号上。
    let resized = match app.doc.get(id).map(|e| &e.kind) {
        Some(ElementKind::Text { content, .. }) if !content.is_empty() => {
            let (w, h) = app.renderer.measure_text(content, style.font_size);
            Some(P2::new(w.max(10.0), h.max(style.font_size)))
        }
        _ => None,
    };
    if let Some(e) = app.doc.get_mut(id) {
        e.style = style;
        if let (ElementKind::Text { size, .. }, Some(new_size)) = (&mut e.kind, resized) {
            *size = new_size;
        }
    }
    app.mosaic_cache.remove(&id);
}

#[cfg(test)]
mod tests {
    use super::toolbar_plan;

    /// 不同宽度下，完成与退出入口都必须留在主工具栏。
    #[test]
    fn available_width_never_hides_copy_close_or_more() {
        for available_width in [1200.0, 620.0, 260.0] {
            let plan = toolbar_plan(available_width);
            assert!(plan.copy_inline, "{available_width}px 时复制必须留在主工具栏");
            assert!(plan.close_inline, "{available_width}px 时关闭必须留在主工具栏");
            assert!(plan.more_inline, "{available_width}px 时更多必须留在主工具栏");
        }
    }

    /// 中窄窗口必须收纳低频动作，宽屏则可直接展示。
    #[test]
    fn medium_and_narrow_widths_move_low_frequency_actions_into_more() {
        assert!(toolbar_plan(1200.0).low_frequency_inline);
        assert!(!toolbar_plan(620.0).low_frequency_inline);
        assert!(!toolbar_plan(260.0).low_frequency_inline);
    }
}
