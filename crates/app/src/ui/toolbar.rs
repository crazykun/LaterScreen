//! 工具栏：悬浮在选区下方（放不下则上方）。
//!
//! 按钮全部为 Painter 手绘矢量图标（12px 线稿）+ hover 文案：
//! 不依赖任何字体图标/emoji 覆盖，任何系统上渲染一致；文字仅用于
//! 标号「1」与 OCR「A」两个拉丁字形（egui 内置字体必有）。

use eframe::egui;
use egui::{Color32, Pos2, Rect, Response, Sense, Shape, Stroke, StrokeKind, Vec2};
use lscreen_core::{ElementKind, Rgba, Tool, P2};

use super::{egui_color, SnipApp, View};
use crate::theme::{ACCENT_FILL, SUCCESS_FILL, SUCCESS_ON_TEXT};

/// 图标按钮边长（逻辑点）
const BTN: f32 = 24.0;
/// 宽屏工具栏的目标宽度，用于位置夹取与单/双行布局判定；实际宽度仍受窗口
/// 可用宽度限制。24px 图标 + 2px 间距下单行实际需求约 610pt（预览模式，无
/// 尺寸编辑）/ 740pt（覆盖层模式带尺寸编辑），900 是 32px 按钮时代的旧值，
/// 会让 900pt 级的预览窗差几个点就折行（用户反馈「工具栏换行了」）
const BAR_W: f32 = 760.0;

/// 根据窗口可用宽度决定工具栏布局行数：
/// 宽屏（≥ [`BAR_W`]）单行放下全部控件；窄屏分两行——第一行编辑控件
/// 自然折行，第二行固定放交付动作（保存/上传/贴图/识别/生成/退出/复制），
/// 再窄也只增加行数，**任何动作都不收进二级菜单**（2026-09-29 用户反馈：
/// 文字菜单与图标工具栏割裂、竖排展开遮挡操作）。
fn single_row(available_width: f32) -> bool {
    available_width >= BAR_W
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
    let bar_width = BAR_W.min(available_width);
    let view = View {
        origin: screen.min,
        scale: app.shot.width as f32 / screen.width().max(1.0),
    };
    let region_pt = view.rect_pt(app.region);

    // 窄屏分两行，行数还会随折行增加；提前为工具栏预留高度，避免贴屏底被裁。
    // Record（录屏/滚动截图框选）的调整阶段恒为单行精简条，与宽屏同高
    let record_adjust = app.mode == super::Mode::Record;
    let bar_height = if record_adjust || single_row(available_width) {
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
                if record_adjust {
                    record_bar_contents(app, ui, ctx);
                } else {
                    ui.set_max_width(bar_width);
                    bar_contents(app, ui, ctx);
                }
            });
        });
}

/// Record（录屏/滚动截图框选）调整阶段的精简条：操作提示 + 取消/确认。
/// 与完整标注工具栏分流——此阶段只做选区几何调整（拖内部移动、拖角/边
/// 改大小），标注动作全部不适用。
fn record_bar_contents(app: &mut SnipApp, ui: &mut egui::Ui, ctx: &egui::Context) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.label(
            egui::RichText::new("拖动移动 · 拖角/边调整 · Enter 确认 · Esc 取消")
                .size(12.0)
                .color(ui.visuals().weak_text_color()),
        );
        ui.separator();
        // 取消：与关闭按钮同款悬停品牌红；不写共享通道直接关窗 = 取消
        let (rect, resp) = ui.allocate_exact_size(Vec2::splat(BTN), Sense::click());
        if resp.hovered() {
            ui.painter()
                .rect_filled(rect, 4.0, ACCENT_FILL.gamma_multiply(0.18));
        }
        let color = if resp.hovered() {
            ACCENT_FILL
        } else {
            ui.style().interact(&resp).fg_stroke.color
        };
        draw_close(ui.painter(), rect.shrink(6.0), color);
        if resp.on_hover_text("取消选区 (Esc)").clicked() {
            app.request_close(ctx);
        }
        // 确认：绿实心对号（复制按钮同款视觉锚点），交付选区给录屏/拼接进程
        let (rect, resp) = ui.allocate_exact_size(Vec2::splat(BTN), Sense::click());
        let fill = if resp.hovered() {
            SUCCESS_FILL.gamma_multiply(0.8)
        } else {
            SUCCESS_FILL
        };
        ui.painter().rect_filled(rect, 4.0, fill);
        draw_check(ui.painter(), rect.shrink(6.0), SUCCESS_ON_TEXT);
        if resp
            .on_hover_text("确认选区，开始录制/滚动 (Enter)")
            .clicked()
        {
            app.deliver_region(ctx);
        }
    });
}

/// 排列编辑控件与交付动作。宽屏单行；窄屏两行——编辑控件自然折行，
/// 交付动作固定一行（动作全为图标，折行时仍可全部展示）。
fn bar_contents(app: &mut SnipApp, ui: &mut egui::Ui, ctx: &egui::Context) {
    ui.spacing_mut().item_spacing.x = 2.0;
    if single_row(ui.available_width()) {
        ui.horizontal(|ui| {
            editing_controls(app, ui);
            ui.separator();
            delivery_actions(app, ui, ctx);
        });
    } else {
        ui.vertical(|ui| {
            // 窄窗口先让标注控件自然折行，第二行固定放全部交付图标，
            // 再窄的窗口也不会把完成或退出入口挤到屏幕外。
            ui.horizontal_wrapped(|ui| editing_controls(app, ui));
            ui.separator();
            ui.horizontal(|ui| delivery_actions(app, ui, ctx));
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

/// 绘制全部交付与退出动作（全图标，任何窗口宽度都直接展示）。
/// 保存、上传、贴图和识别会触发各自业务流程；生成二维码弹出输入浮层。
fn delivery_actions(app: &mut SnipApp, ui: &mut egui::Ui, ctx: &egui::Context) {
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
    qr_generate_button(app, ui, ctx);

    // 右端收尾一对「退出」语义：关闭 = 不交付直接退出（悬停品牌红提示），
    // 紧贴绿色复制按钮——「要走」的两个出口挨在一起，与前面的交付动作分组
    ui.add_space(6.0);

    if close_button(ui) {
        // 关闭只发出窗口退出请求，不保存、不复制，避免与主操作混淆。
        app.request_close(ctx);
    }
    if primary_copy_button(ui) {
        // 主操作会合成当前选区、写入剪贴板并关闭截图窗口。
        app.copy_and_exit(ctx);
    }
}

/// 二维码生成入口：图标按钮 + 弹出文本输入浮层（生成结果插入标注层）。
/// 图标 = 识别二维码 + 右上角「+」，与识别动作在视觉上成对。
fn qr_generate_button(app: &mut SnipApp, ui: &mut egui::Ui, ctx: &egui::Context) {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(BTN), Sense::click());
    let vis = ui.style().interact(&response);
    if response.hovered() {
        ui.painter().rect_filled(rect, 4.0, vis.bg_fill);
    }
    draw_qr_gen(ui.painter(), rect.shrink(6.0), vis.fg_stroke.color);
    let response = response.on_hover_text("生成二维码：输入文本插入标注层（可拖动/四角等比缩放）");
    egui::Popup::menu(&response)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            ui.set_min_width(230.0);
            let mut buffer = app.qr_input.clone().unwrap_or_default();
            ui.label("二维码内容");
            let edit = egui::TextEdit::singleline(&mut buffer).desired_width(f32::INFINITY);
            let response = ui.add(edit);
            let mut insert =
                response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
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
/// 悬停使用品牌强调色提示这是退出动作；紧贴复制按钮组成最右的出口对，
/// 尺寸与其余图标一致（v0.11.5 曾是 32×32，用户反馈统一为 BTN）。
fn close_button(ui: &mut egui::Ui) -> bool {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(BTN), Sense::click());
    if response.hovered() {
        ui.painter()
            .rect_filled(rect, 4.0, ACCENT_FILL.gamma_multiply(0.18));
    }
    let color = if response.hovered() {
        ACCENT_FILL
    } else {
        ui.style().interact(&response).fg_stroke.color
    };
    draw_close(ui.painter(), rect.shrink(6.0), color);
    response.on_hover_text("退出 (Esc)").clicked()
}

/// 交付/完成动作：绿色实心底 + 白色对号图标——「拿到了，收工」，与其左侧的
/// 一组灰色线性图标（保存/贴图/OCR…）在权重上一眼分层。
///
/// **配色走令牌**（见 [[later-screen-ui-color-tokens]]）：这里的 `SUCCESS_FILL`
/// 是 #2e7d32（Material Green 800），不是调色板里的绿 #43a047——后者配白字只有
/// 3.30:1，不到 AA 的 4.5:1（与品牌红同因，中间调色承载白字都要压深一档）。
/// 达标值由 `theme::success_fill_carries_white_text` 单测锁住。
///
/// **为什么没有文字**：v0.11.5 去掉右侧「复制」二字（与全图标工具栏一致，
/// 语义靠 hover 文案承载）。尺寸与其余图标统一为 BTN：v0.11.5 曾放大到
/// 32×32 补足失去文字的视觉权重，用户反馈反而突兀——绿色实心底本身就是
/// 最强的视觉锚点，不再需要额外放大小怪。
fn primary_copy_button(ui: &mut egui::Ui) -> bool {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(BTN), Sense::click());
    // 悬停压深一档而非提亮：绿色不比红色对「提亮->对比度跌破 AA」免疫
    // （同色系 Material Green 900 方向，白字对比度 5.13 → 7.17）。压深幅度
    // 取 0.8：v0.11.5 的 0.92 只暗 8%，实心块小到 hover 无感（用户反馈），
    // 20% 的明度差才与关闭按钮的红色悬停一个量级
    let fill = if response.hovered() {
        SUCCESS_FILL.gamma_multiply(0.8)
    } else {
        SUCCESS_FILL
    };
    ui.painter().rect_filled(rect, 4.0, fill);
    draw_check(ui.painter(), rect.shrink(6.0), SUCCESS_ON_TEXT);
    response
        .on_hover_text("复制到剪贴板并退出 (双击画布 / Ctrl+C / Enter)")
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

/// 对号图标：颜色跟随调用方（`c`）——贴图条把它画在弹层底色上（普通前景
/// 色），截图覆盖层的主复制按钮把它画在交付绿实心底上（传 `SUCCESS_ON_TEXT`
/// 白；v0.11.5 起底是绿不是红，白勾 5.13:1）。此前绿色对号画在品牌红底上只有
/// 1.79:1（v0.11.4 修复），红绿色弱也不可辨。
pub(crate) fn draw_check(p: &egui::Painter, r: Rect, c: Color32) {
    let s = Stroke::new(2.0, c);
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

/// 生成二维码图标（M13）：识别图标 + 右上角「+」——生成语义与识别区分。
fn draw_qr_gen(p: &egui::Painter, r: Rect, c: Color32) {
    draw_qr(p, r, c);
    let (cx, cy) = (r.max.x, r.min.y);
    let s = 2.5;
    let plus = Stroke::new(1.2, c);
    p.line_segment([Pos2::new(cx - s, cy), Pos2::new(cx + s, cy)], plus);
    p.line_segment([Pos2::new(cx, cy - s), Pos2::new(cx, cy + s)], plus);
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
    use super::single_row;

    /// 宽屏单行、窄屏两行折行；任何宽度下都没有动作被收进二级菜单
    /// （布局函数已是「全图标常驻」，这里锁住行数切换阈值不被误改）。
    #[test]
    fn row_layout_switches_at_bar_width() {
        assert!(single_row(1200.0), "宽屏应单行");
        assert!(!single_row(620.0), "中窄窗口应折行两行");
        assert!(!single_row(260.0), "极窄窗口应折行两行");
    }
}
