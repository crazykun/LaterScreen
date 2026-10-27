//! 合成器交互式移动（`ViewportCommand::StartDrag`）的统一发送入口。
//!
//! 各无边框窗（标注窗标题栏、贴图窗、录窗、历史窗）的拖动条都应经由
//! [`start_drag`] 发起移动，不要自行只判 `drag_started`。

use eframe::egui::{self, Context, PointerButton, Response};

/// 判定本帧是否应发起拖动并发送 StartDrag。
///
/// 常规路径：本帧刚判定为左键拖拽（`drag_started_by`）。兜底路径：Wayland
/// 下合成器接管移动（interactive move）后，结束移动的松开事件会被吞（mutter
/// 实测；niri 会正常回传所以此问题不显），egui 误认为按键仍按下，下一次
/// `drag_started` 不再触发，表现为「拖一次有效、下一次无效」交替。此处检测
/// 「本帧收到落在拖动区内的新左键按下且仍处于拖拽态」直接补发——新按下的
/// serial 有效，合成器可正常接管。
///
/// 两条边界都不可破：
/// - 事件只在到达的那一帧存在，天然不会逐帧重发（逐帧重发会让交互式移动
///   反复重启、窗口永远追着鼠标，见 record_ui 同款坑）；
/// - 补发要求按下位置落在拖动区内，避免 stale 拖拽态下点击窗口其他按钮
///   被误判成移动。
pub fn start_drag(ctx: &Context, resp: &Response) {
    let fresh_press_in_zone = ctx.input(|i| {
        i.events.iter().any(|e| {
            matches!(
                e,
                egui::Event::PointerButton {
                    button: PointerButton::Primary,
                    pressed: true,
                    pos,
                    ..
                } if resp.rect.contains(*pos)
            )
        })
    });
    if resp.drag_started_by(PointerButton::Primary) || (fresh_press_in_zone && resp.dragged()) {
        ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
    }
}
