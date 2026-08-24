//! 录屏状态窗口（M8）：录屏期间的小常驻窗口，显示已录时长/帧数，提供停止按钮。
//!
//! 背景：录屏在独立线程跑 `record_gif`（主线程跑 eframe 事件循环，两者不阻塞）。
//! 状态窗口每帧从共享的录制状态（Arc<Mutex<RecordStatus>>）取已录时长/帧数刷新，
//! 停止按钮置 stop 标志位让采帧循环收尾。窗口关闭 = 停止录屏。
//!
//! 只在 `lscreen record` 命令进入（托盘菜单动作 spawn 的是 `record --select`，
//! 同样走到这里），不参与托盘进程本身。
//!
//! 视觉：无边框暗色自绘面板（与主截图工具栏同一套色板），不走 egui 默认控件
//! 样式——头部呼吸灯 + 计数 + 齿轮一行，居中主按钮，底部灰色提示。头部可拖动
//! 整窗（ViewportCommand::StartDrag）。

use eframe::egui;
use egui::{Color32, Pos2, Rect, Stroke, Vec2};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

// 与主截图覆盖层/配置面板同一套暗色色板
const BG: Color32 = Color32::from_rgb(0x21, 0x21, 0x27);
const HOVER_BG: Color32 = Color32::from_rgb(0x33, 0x33, 0x3d);
const TRACK: Color32 = Color32::from_rgb(0x3a, 0x3a, 0x44);
const TEXT: Color32 = Color32::from_rgb(0xec, 0xec, 0xf0);
const MUTED: Color32 = Color32::from_rgb(0x9e, 0x9e, 0xaa);
const RED: Color32 = Color32::from_rgb(0xe5, 0x39, 0x35);
const RED_HOVER: Color32 = Color32::from_rgb(0xef, 0x5a, 0x50);
const BLUE: Color32 = Color32::from_rgb(0x42, 0xa5, 0xf5);
const BLUE_HOVER: Color32 = Color32::from_rgb(0x64, 0xb5, 0xf6);
const ORANGE: Color32 = Color32::from_rgb(0xf5, 0x7c, 0x00);

/// 录制实时状态：主线程 UI 每帧读，录屏线程每次采帧更新。
#[derive(Clone, Copy, Default)]
pub struct RecordStatus {
    /// 已开始录制（armed→recording）；armed 期间 UI 显示开始按钮
    pub started: bool,
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
    /// 开始标志：armed 状态点「开始」/按 Enter 置 true，录制线程见到才开录
    pub start: Arc<AtomicBool>,
    /// 共享状态：录屏线程写入，UI 读取
    pub status: Arc<Mutex<RecordStatus>>,
    /// 显示时长（与 --duration 一致，用于进度条）；滚动模式传最大步数
    pub max_duration: f32,
    /// 滚动截图模式：文案与进度条语义不同（高度/步数 vs 秒/时长）
    pub scroll_mode: bool,
    /// 状态窗与录制选区重叠（四面角落都避不开，如全屏录制）：显示入镜提示
    pub overlap_hint: bool,
}

impl RecordApp {
    /// 独立窗口必须自己挂中文字体（否则中文显示为方块乱码——内置字体无 CJK
    /// 字形）；先用 core Renderer 验证可解析（epaint 对坏字体是 panic 而非 Err）。
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        stop: Arc<AtomicBool>,
        start: Arc<AtomicBool>,
        status: Arc<Mutex<RecordStatus>>,
        max_duration: f32,
        scroll_mode: bool,
        overlap_hint: bool,
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
            start,
            status,
            max_duration,
            scroll_mode,
            overlap_hint,
        }
    }

    /// 停止录屏（按钮点击或窗口关闭都走这里）。
    fn request_stop(&self, ctx: &egui::Context) {
        self.stop.store(true, Ordering::Relaxed);
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    /// 开始录屏（armed 状态点「开始」或按 Enter）。
    fn request_start(&self, ctx: &egui::Context) {
        self.start.store(true, Ordering::Relaxed);
        self.status.lock().unwrap().started = true;
        ctx.request_repaint();
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

        // armed：待用户确认开始（仅录屏模式；滚动截图一进就滚）
        let armed = !self.scroll_mode && !status.started;
        let t = ui.input(|i| i.time);

        egui::CentralPanel::default()
            .frame(
                egui::Frame::NONE
                    .fill(BG)
                    .inner_margin(egui::Margin::same(12)),
            )
            .show(ui, |ui| {
                let avail = ui.available_width();

                // ---- 头部：呼吸灯 + 状态词（左） · 计数（右） · 齿轮（最右） ----
                let (hdr, hdr_resp) =
                    ui.allocate_exact_size(Vec2::new(avail, 26.0), egui::Sense::click_and_drag());
                let p = ui.painter_at(hdr);
                let running = !armed;
                // 呼吸灯：运行中亮度随时间正弦起伏（0.45..1.0），待开始常亮
                let pulse = if running {
                    0.45 + 0.55 * (0.5 + 0.5 * (t * 3.2).sin())
                } else {
                    1.0
                };
                let (dot_rgb, label, counter) = if armed {
                    (BLUE, "待开始", format!("最长 {:.0} 秒", self.max_duration))
                } else if self.scroll_mode {
                    (
                        BLUE,
                        "滚动拼接中",
                        format!("{} px · {} 步", status.height, status.frames),
                    )
                } else {
                    (
                        RED,
                        "录制中",
                        format!("{:.0} 秒 · {} 帧", status.elapsed, status.frames),
                    )
                };
                p.circle_filled(
                    Pos2::new(hdr.left() + 6.0, hdr.center().y),
                    5.0,
                    dot_rgb.gamma_multiply(pulse as f32),
                );
                p.text(
                    Pos2::new(hdr.left() + 18.0, hdr.center().y),
                    egui::Align2::LEFT_CENTER,
                    label,
                    egui::FontId::proportional(14.0),
                    TEXT,
                );

                // 齿轮：独立交互区，悬停有圆形底；点击打开配置面板
                let gear = Rect::from_min_size(hdr.right_top(), Vec2::splat(26.0));
                let gear_resp = ui.interact(gear, egui::Id::new("rec-gear"), egui::Sense::click());
                let gear_c = gear_resp.hovered() || gear_resp.is_pointer_button_down_on();
                if gear_c {
                    p.circle_filled(gear.center(), 15.0, HOVER_BG);
                }
                draw_gear(&p, gear.shrink(6.0), if gear_c { TEXT } else { MUTED });
                if gear_resp.clicked() {
                    open_settings();
                }
                // 计数右对齐到齿轮左侧
                p.text(
                    Pos2::new(gear.left() - 8.0, hdr.center().y),
                    egui::Align2::RIGHT_CENTER,
                    counter,
                    egui::FontId::proportional(12.0),
                    MUTED,
                );
                // 头部其余区域拖动 = 移动窗口（_NET_WM_MOVERESIZE；无装饰窗口
                // 的标准交互，本窗建窗即有焦点，无贴图窗口的首次拖不动问题）。
                // 点在齿轮上时不触发（点击与拖动互不影响，齿轮自身吃掉 click）
                if hdr_resp.dragged_by(egui::PointerButton::Primary) {
                    ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                }
                gear_resp
                    .on_hover_text("配置（保存目录 / 录制格式）；armed 阶段保存后对本次录制生效");

                // ---- 进度条：自绘圆角细条 ----
                ui.add_space(8.0);
                let frac = if armed {
                    0.0
                } else if self.max_duration > 0.0 {
                    let cur = if self.scroll_mode {
                        status.frames as f32
                    } else {
                        status.elapsed
                    };
                    (cur / self.max_duration).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                let (bar, _) = ui.allocate_exact_size(Vec2::new(avail, 6.0), egui::Sense::hover());
                let p2 = ui.painter_at(bar);
                p2.rect_filled(bar, 3.0, TRACK);
                if frac > 0.0 {
                    let mut fill = bar;
                    fill.set_width(bar.width() * frac);
                    p2.rect_filled(fill, 3.0, if armed { BLUE } else { RED });
                }

                // ---- 主按钮：居中，armed=蓝色开始 / 录制中=红色停止 ----
                ui.add_space(14.0);
                let btn = Rect::from_center_size(
                    Pos2::new(ui.max_rect().center().x, ui.cursor().min.y + 17.0),
                    Vec2::new(210.0, 34.0),
                );
                let btn_resp = ui.interact(btn, egui::Id::new("rec-btn"), egui::Sense::click());
                let p3 = ui.painter_at(btn);
                let (base, hi, label) = if armed {
                    (BLUE, BLUE_HOVER, "开始录制  Enter")
                } else {
                    (RED, RED_HOVER, "停止录制  Esc")
                };
                let fill = if btn_resp.hovered() || btn_resp.is_pointer_button_down_on() {
                    hi
                } else {
                    base
                };
                p3.rect_filled(btn, 7.0, fill);
                p3.text(
                    btn.center(),
                    egui::Align2::CENTER_CENTER,
                    label,
                    egui::FontId::proportional(14.5),
                    Color32::WHITE,
                );
                if btn_resp.clicked() {
                    if armed {
                        self.request_start(&ctx);
                    } else {
                        self.request_stop(&ctx);
                    }
                }
                ui.allocate_rect(
                    Rect::from_min_size(ui.cursor().min, Vec2::new(avail, 34.0)),
                    egui::Sense::hover(),
                );

                // ---- 提示 ----
                ui.add_space(9.0);
                let hint = if armed {
                    "Enter 开始 · Esc 取消 · 齿轮改目录与格式"
                } else if self.scroll_mode {
                    "Esc 结束；保存到 Pictures 目录。请保持窗口不再操作"
                } else {
                    "Esc 停止；文件保存到 Pictures 目录"
                };
                let p4 = ui.painter_at(ui.max_rect());
                let hint_y = ui.cursor().min.y;
                p4.text(
                    Pos2::new(ui.max_rect().center().x, hint_y),
                    egui::Align2::CENTER_CENTER,
                    hint,
                    egui::FontId::proportional(10.5),
                    MUTED,
                );
                if self.overlap_hint {
                    p4.text(
                        Pos2::new(ui.max_rect().center().x, hint_y + 14.0),
                        egui::Align2::CENTER_CENTER,
                        "注意：本窗与选区重叠，会被录入成品",
                        egui::FontId::proportional(10.5),
                        ORANGE,
                    );
                }
            });

        // 运行中呼吸灯需要连续重绘（80ms 足够平滑）；静止时半秒一刷省 CPU
        ctx.request_repaint_after(std::time::Duration::from_millis(if armed {
            500
        } else {
            80
        }));

        // armed：Enter 开始，Esc 取消；录制中：Esc 停止
        if armed {
            let enter = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
            if enter {
                self.request_start(&ctx);
            }
        }
        let esc = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
        if esc {
            self.request_stop(&ctx);
        }
    }
}

/// 齿轮图标：双圈 + 8 齿。与工具栏同约定——纯 painter 绘制，
/// 不依赖字体图标/emoji 覆盖，任何系统渲染一致
fn draw_gear(p: &egui::Painter, r: Rect, c: Color32) {
    let ctr = r.center();
    let rad = r.width() * 0.5;
    p.circle_stroke(ctr, rad * 0.70, Stroke::new(1.5, c));
    p.circle_stroke(ctr, rad * 0.26, Stroke::new(1.5, c));
    for i in 0..8 {
        let a = i as f32 * std::f32::consts::TAU / 8.0;
        let dir = Vec2::new(a.cos(), a.sin());
        p.line_segment(
            [ctr + dir * (rad * 0.60), ctr + dir * (rad * 0.94)],
            Stroke::new(1.5, c),
        );
    }
}

/// 打开配置面板：spawn 独立 `lscreen config` 子进程（与本状态窗互不干扰）。
/// 本进程可能先退出，子进程被 init 收养，无需 wait
fn open_settings() {
    match std::env::current_exe() {
        Ok(exe) => {
            if let Err(e) = std::process::Command::new(exe).arg("config").spawn() {
                eprintln!("lscreen: 打开配置面板失败: {e}");
            }
        }
        Err(e) => eprintln!("lscreen: 无法定位自身可执行文件: {e}"),
    }
}
