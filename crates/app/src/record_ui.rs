//! 录屏状态窗口（M8）：录屏期间的小常驻窗口，显示已录时长/帧数，提供停止按钮。
//!
//! 背景：录屏在独立线程跑 `record_gif`（主线程跑 eframe 事件循环，两者不阻塞）。
//! 状态窗口每帧从共享的录制状态（Arc<Mutex<RecordStatus>>）取已录时长/帧数刷新，
//! 停止按钮置 stop 标志位让采帧循环收尾。窗口关闭 = 停止录屏。
//!
//! 只在 `lscreen record` 命令进入（托盘菜单动作 spawn 的是 `record --select`，
//! 同样走到这里），不参与托盘进程本身。

use eframe::egui;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// 录制实时状态：主线程 UI 每帧读，录屏线程每次采帧更新。
#[derive(Clone, Copy, Default)]
pub struct RecordStatus {
    /// 已录时长（秒）
    pub elapsed: f32,
    /// 已采集帧数
    pub frames: usize,
    /// 滚动截图模式：已拼接图像高度（px）；GIF 模式恒 0
    pub height: u32,
    /// 录制已结束（自然到达时长 / 出错 / 被停止），UI 见到即自动关窗
    pub done: bool,
}

pub struct RecordApp {
    /// 停止标志：置 true 后录屏线程在下一帧采集中退出
    pub stop: Arc<AtomicBool>,
    /// 共享状态：录屏线程写入，UI 读取
    pub status: Arc<Mutex<RecordStatus>>,
    /// 显示时长（与 --duration 一致，用于进度条）；滚动模式传最大步数
    pub max_duration: f32,
    /// 滚动截图模式：文案与进度条语义不同（高度/步数 vs 秒/时长）
    pub scroll_mode: bool,
}

impl RecordApp {
    /// 独立窗口必须自己挂中文字体（否则中文显示为方块乱码——内置字体无 CJK
    /// 字形）；先用 core Renderer 验证可解析（epaint 对坏字体是 panic 而非 Err）。
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        stop: Arc<AtomicBool>,
        status: Arc<Mutex<RecordStatus>>,
        max_duration: f32,
        scroll_mode: bool,
    ) -> Self {
        crate::apply_window_class(cc);
        let font = crate::font::load_system_font();
        if let Some(bytes) = font {
            if lscreen_core::render::Renderer::new(Some(bytes.clone())).has_font() {
                crate::font::setup_egui_fonts(&cc.egui_ctx, bytes);
            }
        }
        Self {
            stop,
            status,
            max_duration,
            scroll_mode,
        }
    }

    /// 停止录屏（按钮点击或窗口关闭都走这里）。
    fn request_stop(&self, ctx: &egui::Context) {
        self.stop.store(true, Ordering::Relaxed);
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
}

impl eframe::App for RecordApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let status = *self.status.lock().unwrap();

        // 录制线程收尾后自动关窗（自然到时 / 出错 / 已停止）
        if status.done {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }

        egui::CentralPanel::default().show(ui, |ui| {
            ui.add_space(10.0);
            if self.scroll_mode {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("● 滚动拼接中")
                            .color(egui::Color32::from_rgb(0x21, 0x96, 0xf3))
                            .strong(),
                    );
                    ui.add_space(6.0);
                    ui.label(format!("高度 {} px / {} 步", status.height, status.frames));
                });
            } else {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("● 录制中")
                            .color(egui::Color32::RED)
                            .strong(),
                    );
                    ui.add_space(6.0);
                    ui.label(format!("{:.0} 秒 / {} 帧", status.elapsed, status.frames));
                });
            }
            ui.add_space(6.0);

            // 进度条：录制 = 已录时长/最大时长；滚动 = 已滚步数/最大步数
            let frac = if self.max_duration > 0.0 {
                let cur = if self.scroll_mode {
                    status.frames as f32
                } else {
                    status.elapsed
                };
                (cur / self.max_duration).clamp(0.0, 1.0)
            } else {
                0.0
            };
            ui.add(egui::ProgressBar::new(frac).animate(false));

            ui.add_space(8.0);
            if ui.button("停止 (Esc)").clicked() {
                self.request_stop(&ctx);
            }
            ui.add_space(6.0);
            ui.small(if self.scroll_mode {
                "停止后长图会保存到 Pictures 目录；也可按 Esc 结束。请保持窗口不再操作"
            } else {
                "停止后录屏文件会保存到 Pictures 目录；也可按 Esc 结束"
            });
        });

        // 每秒刷新一次（帧数/时长不是逐帧变化，避免空转烧 CPU）
        ctx.request_repaint_after(std::time::Duration::from_millis(500));

        // Esc 停止
        let esc = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
        if esc {
            self.request_stop(&ctx);
        }
    }
}
