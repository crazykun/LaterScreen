//! 延时截图倒计时窗（M13）：`gui/shot --delay` 与托盘「延时截图」（默认 3 秒）
//! 的前置提示窗。
//!
//! **时序是本模块存在的理由**（PLAN M13）：倒计时结束必须
//! **先关提示窗 → 等合成器重绘 → 再截屏 → 最后开覆盖层**。
//! 顺序反了提示窗会被截进图里（X11 关窗到合成器真正回收表面有
//! ~200ms 延迟，RecordBorder 实测数据），所以倒计时结束只负责关窗，
//! 截屏前的 300ms 等待由调用方（main.rs）执行——两个 eframe 循环
//! 不能在同一进程里先后跑（macOS 限制），等待动作必须在循环外。
//!
//! 交互：Esc / 点击「取消」/ WM 强关 = 取消（run 返回 false）；
//! 自然走完 = 关窗并返回 true。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui;
use egui::{Color32, Pos2};

use crate::run_eframe;

const BG: Color32 = Color32::from_rgb(0x21, 0x21, 0x27);
const TEXT: Color32 = Color32::from_rgb(0xec, 0xec, 0xf0);
const MUTED: Color32 = Color32::from_rgb(0x9e, 0x9e, 0xaa);
const ACCENT: Color32 = Color32::from_rgb(0xe5, 0x39, 0x35);

/// 跑一次倒计时窗口。返回 false = 用户取消（调用方应静默退出）。
/// eframe 启动失败（无 GL 等）按「不阻塞截图」处理：直接返回 true，
/// 让截图流程继续（延时退化为纯 sleep），错误交给后续可能的弹窗路径。
pub fn run(secs: f64) -> bool {
    let done = Arc::new(AtomicBool::new(false));
    let cancelled = Arc::new(AtomicBool::new(false));
    // 居中于主屏：显式 pos，避免 WM 把无边框小窗摆到角落。
    // 高度按内容实测预留：10 边距 ×2 + 34pt 大数字行高 + 11pt 说明行 +
    // 取消按钮 + 控件间距，96 高会把按钮裁出窗外
    let (vw, vh) = (180.0, 148.0);
    let pos = lscreen_capture::primary_monitor_bounds()
        .map(|(x, y, w, h)| {
            Pos2::new(
                x as f32 + (w as f32 - vw) / 2.0,
                y as f32 + (h as f32 - vh) / 2.0,
            )
        })
        .unwrap_or(Pos2::new(80.0, 80.0));
    let viewport = egui::ViewportBuilder::default()
        .with_app_id("lscreen")
        .with_position(pos)
        .with_inner_size([vw, vh])
        .with_decorations(false)
        .with_resizable(false)
        .with_always_on_top();
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    let d = done.clone();
    let c = cancelled.clone();
    let app = move |cc: &eframe::CreationContext<'_>| {
        crate::apply_window_class(cc);
        // 系统字体装进 egui（内置字体无 CJK，不装则中文显示为方框/乱码），
        // 与 record_ui/history 同款；加载失败退回内置字体（只剩数字可用）
        if let Some(bytes) = crate::font::load_system_font() {
            crate::font::setup_egui_fonts(&cc.egui_ctx, bytes);
        }
        Ok(Box::new(CountdownApp {
            deadline: Instant::now() + Duration::from_secs_f64(secs),
            done: d,
            cancelled: c,
        }) as Box<dyn eframe::App>)
    };
    if let Err(e) = run_eframe("lscreen-countdown", options, Box::new(app)) {
        eprintln!("lscreen: 倒计时窗不可用（{e}），改为纯延时");
        std::thread::sleep(Duration::from_secs_f64(secs));
        return true;
    }
    done.load(Ordering::Relaxed) && !cancelled.load(Ordering::Relaxed)
}

struct CountdownApp {
    deadline: Instant,
    done: Arc<AtomicBool>,
    cancelled: Arc<AtomicBool>,
}

impl eframe::App for CountdownApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let remain = self.deadline.saturating_duration_since(Instant::now());
        let esc = ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));

        if remain.is_zero() {
            self.done.store(true, Ordering::Relaxed);
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        if esc {
            self.cancelled.store(true, Ordering::Relaxed);
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        // 后台无输入事件也要走帧：按剩余时间的亚秒部分对齐下一帧
        ctx.request_repaint_after(Duration::from_millis(50));

        egui::CentralPanel::default()
            .frame(
                egui::Frame::NONE
                    .fill(BG)
                    .inner_margin(egui::Margin::same(12)),
            )
            .show(ui, |ui| {
                // 固定尺寸小窗里手工 add_space 配平易漂：改布局驱动——
                // 按钮钉在底部（bottom_up：先添加的在最下），数字区在
                // 剩余空间垂直居中，上下留白由布局自动均分
                ui.with_layout(egui::Layout::bottom_up(egui::Align::Center), |ui| {
                    let btn = egui::Button::new("取消 (Esc)")
                        .min_size(egui::vec2(ui.available_width(), 26.0))
                        .fill(ACCENT.gamma_multiply(0.15))
                        .stroke(egui::Stroke::new(1.0, ACCENT));
                    if ui.add(btn).clicked() {
                        self.cancelled.store(true, Ordering::Relaxed);
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    ui.add_space(10.0);
                    ui.with_layout(egui::Layout::top_down(egui::Align::Center), |ui| {
                        // 剩余整秒（向上取整：3.0 → 3，2.4 → 3 观感更自然）
                        let secs = remain.as_secs_f64().ceil();
                        // 两行内容高（34pt 数字行 + 11pt 说明行），剩余上下对半
                        let content_h = 66.0;
                        let pad = ((ui.available_height() - content_h) * 0.5).max(0.0);
                        ui.add_space(pad);
                        ui.label(
                            egui::RichText::new(format!("{secs:.0}"))
                                .size(34.0)
                                .color(TEXT)
                                .strong(),
                        );
                        ui.label(egui::RichText::new("秒后截图").size(11.0).color(MUTED));
                    });
                });
            });
    }
}
