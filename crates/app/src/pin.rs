//! 贴图：置顶无边框窗口显示一张图片（Snipaste 招牌能力）。
//!
//! 独立进程形态（`lscreen pin`）：由覆盖层 spawn，选区合成 PNG 经 stdin
//! 传入，本进程读完即建窗。每个贴图一个轻量进程，关闭即释放全部内存。
//!
//! 交互：拖拽移动窗口（手动定位，见 PinApp::drag）、滚轮缩放 25%–1600%
//! （光标下的图像点锚定不动）、**Shift+滚轮调不透明度**（20%–100%）、
//! **R/H/V 旋转 90°/水平/垂直翻转**（纯像素置换，复制/保存的就是变换后
//! 的画面）、高倍放大（实际显示 ≥8x）叠加像素网格、工具条**点击穿透**
//! （点击穿到下层窗口，经托盘菜单「退出贴图穿透」恢复——穿透后本窗口
//! 收不到任何事件，Esc 只在仍持键盘焦点时有效）、双击复制、Esc/Delete
//! 关闭。

use eframe::egui;
use egui::{Color32, Pos2, Rect, Stroke, Vec2, ViewportCommand};

use crate::export;
use crate::history;
use std::path::PathBuf;

const MIN_ZOOM: f32 = 0.25;
/// 放开到 16x：像素网格（≥8x 叠加）对查看小图标/像素画才有意义；大图
/// 受 WM 最大窗口尺寸约束自然到不了这么高（窗口 = 整图缩放，无画布平移）
const MAX_ZOOM: f32 = 16.0;
/// 不透明度下限：再低就基本看不见，且容易误以为贴图丢了
const MIN_OPACITY: f32 = 0.2;
/// 像素网格的触发阈值：每个图像像素在屏幕上 ≥8 逻辑像素时叠加网格
const PIXEL_GRID_MIN: f32 = 8.0;
/// 缩放指示条/工具条的显示时长（秒）
const TOAST_SECS: f64 = 1.6;
/// 底部工具条条带高度：窗口高 = 图像显示高 + BAR_H，按钮在图像外侧，
/// 不遮挡贴图内容（与截图覆盖层「工具栏在选区下方」同布局）
pub const BAR_H: f32 = 34.0;
/// 放大用最近邻（滚轮放大像素格清晰），缩小用线性
fn texture_options() -> egui::TextureOptions {
    egui::TextureOptions {
        magnification: egui::TextureFilter::Nearest,
        minification: egui::TextureFilter::Linear,
        ..Default::default()
    }
}

// ---------------- 托盘 → 贴图广播控制（穿透退出/全部关闭）

/// 广播控制文件（托盘写，各贴图进程轮询）。内容 `"cmd nonce"`，nonce 用
/// unix 微秒单调递增；**文件不删、后写覆盖**——多张贴图各自记住已应用
/// 的 nonce，无「一个进程消费掉别的进程就看不到」的竞态。
fn pins_ctl_path() -> Option<std::path::PathBuf> {
    crate::config::cache_dir().map(|d| d.join("pins.ctl"))
}

fn read_ctl() -> Option<(String, u64)> {
    let text = std::fs::read_to_string(pins_ctl_path()?).ok()?;
    let mut it = text.split_whitespace();
    Some((it.next()?.to_string(), it.next()?.parse().ok()?))
}

/// 托盘调用：向全部贴图进程广播命令（unthrough / close）。
pub(crate) fn write_ctl(cmd: &str) {
    let Some(p) = pins_ctl_path() else { return };
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let us = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .unwrap_or(0);
    let _ = std::fs::write(p, format!("{cmd} {us}\n"));
}

// ---------------- 像素级变换（旋转/翻转，纯置换无插值）

/// 顺时针旋转 90°：dst(x', y') = src(y, h-1-x')…按「新坐标反推旧坐标」
/// 实现：new(x', y') = old(h-1-y', x')，其中 new_w = h。
fn rotate_cw_rgba(rgba: &[u8], w: u32, h: u32) -> (Vec<u8>, u32, u32) {
    let (w, h) = (w as usize, h as usize);
    let mut out = vec![0u8; rgba.len()];
    for y in 0..h {
        for x in 0..w {
            let src = (y * w + x) * 4;
            let (dx, dy) = (h - 1 - y, x);
            let dst = (dy * h + dx) * 4;
            out[dst..dst + 4].copy_from_slice(&rgba[src..src + 4]);
        }
    }
    (out, h as u32, w as u32)
}

/// 就地翻转：horizontal = 左右（列镜像），否则上下（行镜像）。
fn flip_rgba(rgba: &mut [u8], w: u32, h: u32, horizontal: bool) {
    let (w, h) = (w as usize, h as usize);
    if horizontal {
        for y in 0..h {
            let row = y * w;
            for x in 0..w / 2 {
                let (a, b) = ((row + x) * 4, (row + (w - 1 - x)) * 4);
                for k in 0..4 {
                    rgba.swap(a + k, b + k);
                }
            }
        }
    } else {
        for y in 0..h / 2 {
            let (a, b) = (y * w * 4, (h - 1 - y) * w * 4);
            for k in 0..w * 4 {
                rgba.swap(a + k, b + k);
            }
        }
    }
}

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
    /// 原始图片文件路径（`-i` 传入时）；stdin 传入为 None。保存时据此记
    /// 历史源文件（M11），供「打开目录并选中」定位
    source: Option<PathBuf>,
    /// 原生窗口句柄：不透明度（Shift+滚轮）与点击穿透（工具条按钮）经此
    /// 下发。拿不到（非预期平台变体）时两功能整体降级为不可用并 toast
    native: Option<lscreen_capture::NativeWindow>,
    /// 当前整窗不透明度（1.0 = 不透明）。仅 UI 状态，实际生效与否取决于
    /// set_opacity 返回值（失败会回滚本值）
    opacity: f32,
    /// 点击穿透中：窗口不收任何指针事件，交互只剩键盘（若仍持焦点）与
    /// 托盘广播。开启期间靠 300ms 心跳轮询 pins.ctl
    through: bool,
    /// 已应用的 pins.ctl nonce：新 nonce = 新命令。启动时初始化为当前
    /// 文件里的 nonce（吞掉历史命令，新贴图不响应「上一轮」广播）
    ctl_seen: u64,
    last_ctl_check: f64,
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
        source: Option<PathBuf>,
    ) -> Self {
        crate::apply_window_class(cc);
        // 贴图是独立进程，必须自己挂中文字体，否则按钮/菜单/toast 的中文
        // 会因 egui 内置字体无 CJK 而显示为方框。先用 core Renderer 验证
        // 字节可解析（epaint 对坏字体是 panic 而非 Err）。
        if let Some(bytes) = font {
            if lscreen_core::render::Renderer::new(Some(bytes.clone())).has_font() {
                crate::font::setup_egui_fonts(&cc.egui_ctx, bytes);
            }
        }
        let img = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
        let texture = cc.egui_ctx.load_texture("pin", img, texture_options());
        let native = {
            use raw_window_handle::HasWindowHandle;
            cc.window_handle()
                .ok()
                .and_then(|h| lscreen_capture::NativeWindow::from_raw(h.as_raw()))
        };
        // 吞掉启动前残留的广播命令：新贴图不该响应「上一轮」穿透切换
        let ctl_seen = read_ctl().map(|(_, n)| n).unwrap_or(0);
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
            source,
            native,
            opacity: 1.0,
            through: false,
            ctl_seen,
            last_ctl_check: 0.0,
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
        // 自动命名走防覆盖（create_new 消除 TOCTOU 覆盖窗口）
        match export::save_png_unique(&self.rgba, self.w, self.h, &path) {
            Ok(p) => {
                history::record_file(&p, history::Kind::Pin, self.source.as_deref());
                self.toast(ctx, format!("已保存 {}", p.display()));
            }
            Err(e) => self.toast(ctx, format!("保存失败: {e}")),
        }
    }

    fn do_close(&mut self, ctx: &egui::Context) {
        ctx.send_viewport_cmd(ViewportCommand::Close);
    }

    /// 切换点击穿透。开启后本窗口不收任何指针事件——恢复路径：Esc
    /// （仅当仍持键盘焦点）或托盘菜单「退出贴图穿透」（广播 pins.ctl，
    /// 本进程 300ms 心跳轮询）。
    fn set_through(&mut self, ctx: &egui::Context, through: bool) {
        if self.through == through {
            return;
        }
        let Some(n) = &self.native else {
            self.toast(ctx, "当前环境不支持点击穿透");
            return;
        };
        if let Err(e) = n.set_click_through(through) {
            self.toast(ctx, format!("点击穿透设置失败: {e}"));
            return;
        }
        self.through = through;
        self.toast(
            ctx,
            if through {
                "穿透已开：Esc 或托盘「退出贴图穿透」恢复"
            } else {
                "已恢复交互"
            },
        );
    }

    /// 穿透期间的托盘广播轮询。窗口无输入事件后 eframe 不会再重绘，
    /// 必须主动心跳（与历史面板 raise 信号同一坑），否则轮询永不执行。
    fn poll_ctl(&mut self, ctx: &egui::Context) {
        ctx.request_repaint_after(std::time::Duration::from_millis(300));
        let now = ctx.input(|i| i.time);
        if now - self.last_ctl_check < 0.3 {
            return;
        }
        self.last_ctl_check = now;
        let Some((cmd, nonce)) = read_ctl() else {
            return;
        };
        if nonce == self.ctl_seen {
            return;
        }
        self.ctl_seen = nonce;
        match cmd.as_str() {
            "unthrough" => self.set_through(ctx, false),
            "close" => self.do_close(ctx),
            _ => {}
        }
    }

    /// Shift+滚轮调不透明度（20%–100%）。平台调用失败（拿不到句柄/系统
    /// 拒绝）回滚状态值，避免 UI 显示与实际不符。
    fn adjust_opacity(&mut self, ctx: &egui::Context, scroll: f32) {
        let old = self.opacity;
        // 一格标准滚轮 ≈ ±0.125：全量程 6–7 格，与 Snipaste 手感接近
        self.opacity = (self.opacity + scroll / 800.0).clamp(MIN_OPACITY, 1.0);
        if (self.opacity - old).abs() < f32::EPSILON {
            return;
        }
        let applied = self
            .native
            .as_ref()
            .map(|n| n.set_opacity(self.opacity).is_ok())
            .unwrap_or(false);
        if !applied {
            self.opacity = old;
            self.toast(ctx, "当前环境不支持窗口不透明度");
            return;
        }
        let pct = (self.opacity * 100.0).round() as i32;
        self.toast(ctx, format!("不透明度 {pct}%"));
    }

    /// 变换（旋转/翻转）后的公共收尾：重建纹理、按新宽高重算基准尺寸并
    /// 请求窗口尺寸更新。复制/保存直接用 self.rgba——所见即所得。
    fn after_transform(&mut self, ctx: &egui::Context, rgba: Vec<u8>, w: u32, h: u32) {
        self.rgba = rgba;
        self.w = w;
        self.h = h;
        self.base = Vec2::new(w as f32 / self.scale, h as f32 / self.scale);
        // TextureHandle::set 需 &mut：clone 句柄（内部 Arc，廉价）
        if let Some(mut t) = self.texture.clone() {
            let img =
                egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &self.rgba);
            t.set(img, texture_options());
        }
        // 锚点几何随尺寸失效，新手势重新取锚
        self.zoom_anchor = None;
        ctx.send_viewport_cmd(ViewportCommand::InnerSize(
            self.base * self.zoom + Vec2::new(0.0, BAR_H),
        ));
    }

    fn rotate_cw(&mut self, ctx: &egui::Context) {
        let (out, w, h) = rotate_cw_rgba(&self.rgba, self.w, self.h);
        self.after_transform(ctx, out, w, h);
        self.toast(ctx, "旋转 90°");
    }

    fn flip(&mut self, ctx: &egui::Context, horizontal: bool) {
        let (mut rgba, w, h) = (std::mem::take(&mut self.rgba), self.w, self.h);
        flip_rgba(&mut rgba, w, h, horizontal);
        self.after_transform(ctx, rgba, w, h);
        self.toast(
            ctx,
            if horizontal {
                "水平翻转"
            } else {
                "垂直翻转"
            },
        );
    }

    /// 设定缩放（工具条 −/+/百分比与键盘 +/−/0 用）。锚定窗口左上角不动
    /// （无指针参与，无锚点换算），档位对齐 STEPS 的干净值。
    fn set_zoom(&mut self, ctx: &egui::Context, zoom: f32) {
        let z = zoom.clamp(MIN_ZOOM, MAX_ZOOM);
        if (z - self.zoom).abs() < 1e-3 {
            return;
        }
        self.zoom = z;
        self.zoom_anchor = None;
        ctx.send_viewport_cmd(ViewportCommand::InnerSize(
            self.base * z + Vec2::new(0.0, BAR_H),
        ));
        let pct = (z * 100.0).round() as i32;
        self.toast(ctx, format!("{pct}%"));
    }

    /// 档位步进（−/+ 按钮）：像主流看图软件一样吸附到干净档位，
    /// 连点不累积浮点误差
    fn step_zoom(&mut self, ctx: &egui::Context, up: bool) {
        const STEPS: [f32; 13] = [
            0.25, 0.5, 0.75, 1.0, 1.25, 1.5, 2.0, 3.0, 4.0, 6.0, 8.0, 12.0, 16.0,
        ];
        let cur = self.zoom;
        let next = if up {
            STEPS.iter().copied().find(|s| *s > cur + 1e-3)
        } else {
            STEPS.iter().copied().rev().find(|s| *s < cur - 1e-3)
        };
        if let Some(n) = next {
            self.set_zoom(ctx, n);
        }
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
        // Shift+滚轮被 egui 默认归为「水平滚动」（horizontal_scroll_modifier
        // = SHIFT），值落在 delta.x——纵轴读取对 Shift 组合恒为 0，必须合成
        let (sy, sx) = ctx.input(|i| (i.smooth_scroll_delta.y, i.smooth_scroll_delta.x));
        // Shift+滚轮 = 不透明度（用户决策：独立于缩放的修饰键方案，不做
        // 可配置快捷键——贴图内交互本就无冲突面）
        let shifted = ctx.input(|i| i.modifiers.shift);
        let scroll = if shifted { sx + sy } else { sy };
        if scroll == 0.0 {
            return;
        }
        if shifted {
            self.adjust_opacity(ctx, scroll);
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
    ///
    /// 条带宽度放不下全部按钮时**按优先级贪心装入**（前缀和 ≤ 预算），
    /// 优先级：复制并关闭 > 关闭 > 缩放组(−/%/+) > 置顶 > 穿透 > 旋转 >
    /// 翻转 > 保存；视觉顺序固定：
    /// 置顶 | 穿透 | 旋转 | 翻转 | [− % +] | 保存 | 关闭 | 复制并关闭。
    fn show_toolbar(&mut self, ctx: &egui::Context, bar: Rect) {
        use crate::ui::toolbar::{action_button, draw_check, draw_close, draw_save, icon_button};
        // 单图标 24pt + 间距 2pt = 26pt；缩放组 = 24 + 40 + 24 + 双间距 = 92pt
        const WIDTHS: [f32; 8] = [26.0, 26.0, 92.0, 26.0, 26.0, 26.0, 26.0, 26.0];
        // 与 WIDTHS 同序的优先级 id（前缀和判定用）
        const ORDER: [&str; 8] = [
            "copy", "close", "zoomgrp", "top", "through", "rotate", "flip", "save",
        ];
        let budget = bar.width() - 8.0;
        let mut prefix = [0.0f32; 8];
        let mut acc = 0.0;
        for (i, w) in WIDTHS.iter().enumerate() {
            acc += w;
            prefix[i] = acc;
        }
        // id 显示条件：它及比它更高优先级的项都装得下
        let show = |id: &str| prefix[ORDER.iter().position(|x| *x == id).unwrap()] <= budget;
        // 实际装入的总宽（定位居中用）
        let mut shown = 0.0;
        for p in prefix {
            if p <= budget {
                shown = p;
            }
        }
        let pos = Pos2::new(bar.center().x - shown / 2.0 + 4.0, bar.center().y - 12.0);
        let mut action: Option<u8> = None;
        let (topmost, through, zoom) = (self.topmost, self.through, self.zoom);
        egui::Area::new(egui::Id::new("pin-bar"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .interactable(true)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing = Vec2::splat(2.0);
                    if show("top")
                        && icon_button(ui, topmost, "置顶：保持在最上层（点击切换）", draw_topmost)
                            .clicked()
                    {
                        action = Some(4);
                    }
                    if show("through")
                        && icon_button(
                            ui,
                            through,
                            "点击穿透：点击穿到下层窗口（Esc/托盘菜单恢复）",
                            draw_through,
                        )
                        .clicked()
                    {
                        action = Some(5);
                    }
                    if show("rotate") && action_button(ui, true, "旋转 90° (R)", draw_rotate) {
                        action = Some(6);
                    }
                    if show("flip")
                        && action_button(ui, true, "水平翻转 (H)，垂直翻转 (V)", draw_flip)
                    {
                        action = Some(7);
                    }
                    if show("zoomgrp") {
                        if action_button(ui, true, "缩小（键盘 -）", draw_minus) {
                            action = Some(8);
                        }
                        // 百分比 = 缩放预览，点击重置 100%
                        let pct = (zoom * 100.0).round() as i32;
                        let btn = egui::Button::new(
                            egui::RichText::new(format!("{pct}%")).strong().size(12.0),
                        )
                        .min_size(Vec2::new(40.0, 24.0));
                        if ui
                            .add(btn)
                            .on_hover_text("缩放预览，点击重置为 100%（键盘 0）")
                            .clicked()
                        {
                            action = Some(9);
                        }
                        if action_button(ui, true, "放大（键盘 +）", draw_plus) {
                            action = Some(10);
                        }
                    }
                    if show("save") && action_button(ui, true, "保存为 PNG (Ctrl+S)", draw_save)
                    {
                        action = Some(1);
                    }
                    if show("close")
                        && action_button(ui, true, "关闭贴图 (Esc / Delete)", draw_close)
                    {
                        action = Some(2);
                    }
                    // 与覆盖层一致：最常用动作放最右，绿色对号
                    if show("copy")
                        && action_button(ui, true, "复制并关闭 (双击/Ctrl+C 仅复制)", draw_check)
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
            Some(5) => self.set_through(ctx, !through),
            Some(6) => self.rotate_cw(ctx),
            Some(7) => self.flip(ctx, true),
            Some(8) => self.step_zoom(ctx, false),
            Some(9) => self.set_zoom(ctx, 1.0),
            Some(10) => self.step_zoom(ctx, true),
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
        let (esc, del, copy_k, save_k, rot_k, flip_h, flip_v) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::Escape),
                i.consume_key(Modifiers::NONE, Key::Delete),
                i.consume_key(Modifiers::COMMAND, Key::C),
                i.consume_key(Modifiers::COMMAND, Key::S),
                i.consume_key(Modifiers::NONE, Key::R),
                i.consume_key(Modifiers::NONE, Key::H),
                i.consume_key(Modifiers::NONE, Key::V),
            )
        });
        // 缩放键走裸事件扫描而非 consume_key：主键盘 '+' 是 Shift+= 组合
        // （US 布局），带修饰符会被 Modifiers::NONE 精确匹配拒掉；这里
        // 只要物理键按下就认，不关心修饰
        let key_hit = |k: Key| {
            ctx.input(|i| {
                i.events.iter().any(|e| match e {
                    egui::Event::Key {
                        key, pressed: true, ..
                    } => *key == k,
                    _ => false,
                })
            })
        };
        let (zin, zout, zreset) = (
            key_hit(Key::Plus) || key_hit(Key::Equals),
            key_hit(Key::Minus),
            key_hit(Key::Num0),
        );
        // 穿透中 Esc/Delete 先退出穿透（窗口若仍持键盘焦点；失焦后只能靠
        // 托盘广播），不直接关窗——用户按 Esc 的意图大概率是「拿回交互」
        if esc || del {
            if self.through {
                self.set_through(&ctx, false);
            } else {
                self.do_close(&ctx);
            }
            return;
        }

        // 穿透期间无输入事件，主动心跳轮询托盘广播（含 close 退出）
        if self.through {
            self.poll_ctl(&ctx);
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
        // 像素网格：实际显示比例 ≥8x（每个图像像素 ≥8 逻辑像素）时叠加。
        // 交互层装饰，不进复制/保存的位图（那些走 self.rgba）。只画可见
        // 部分（clip 交集），大图深放大时限流线条数量
        let px_per_img = image_rect.width() / self.w.max(1) as f32;
        if px_per_img >= PIXEL_GRID_MIN && self.w > 0 && self.h > 0 {
            let clip = ui.clip_rect().intersect(image_rect);
            let step = px_per_img;
            let grid = Stroke::new(1.0, Color32::from_rgba_unmultiplied(128, 128, 128, 110));
            let mut i = (((clip.min.x - image_rect.min.x) / step).ceil() as usize).max(1);
            while i < self.w as usize {
                let x = image_rect.min.x + i as f32 * step;
                if x > clip.max.x {
                    break;
                }
                ui.painter()
                    .line_segment([Pos2::new(x, clip.min.y), Pos2::new(x, clip.max.y)], grid);
                i += 1;
            }
            let mut j = (((clip.min.y - image_rect.min.y) / step).ceil() as usize).max(1);
            while j < self.h as usize {
                let y = image_rect.min.y + j as f32 * step;
                if y > clip.max.y {
                    break;
                }
                ui.painter()
                    .line_segment([Pos2::new(clip.min.x, y), Pos2::new(clip.max.x, y)], grid);
                j += 1;
            }
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
        if rot_k {
            self.rotate_cw(&ctx);
        }
        if flip_h {
            self.flip(&ctx, true);
        }
        if flip_v {
            self.flip(&ctx, false);
        }
        if zin {
            self.step_zoom(&ctx, true);
        }
        if zout {
            self.step_zoom(&ctx, false);
        }
        if zreset {
            self.set_zoom(&ctx, 1.0);
        }
    }
}

/// 图标统一线宽：与参考风格（细线圆头）一致，所有贴图条图标共用。
const ICON_W: f32 = 1.5;

/// 置顶图标：顶部横线 + 向上箭头（推到最上层）。激活态由 icon_button 高亮。
fn draw_topmost(p: &egui::Painter, r: Rect, c: Color32) {
    let s = Stroke::new(ICON_W, c);
    let w = r.width();
    p.line_segment([r.left_top(), r.right_top()], s);
    let cx = r.center().x;
    let tip = Pos2::new(cx, r.min.y + w * 0.22);
    p.line_segment([Pos2::new(cx, r.max.y), tip], s);
    p.line_segment([tip, Pos2::new(cx - w * 0.28, r.min.y + w * 0.52)], s);
    p.line_segment([tip, Pos2::new(cx + w * 0.28, r.min.y + w * 0.52)], s);
}

/// 穿透图标：窗口轮廓（左右留过口）+ 水平箭头穿堂而过——点击落到下层。
/// 激活态由 icon_button 高亮。
fn draw_through(p: &egui::Painter, r: Rect, c: Color32) {
    let s = Stroke::new(ICON_W, c);
    let w = r.width();
    let m = w * 0.15;
    let gap = w * 0.24; // 左右边中段的箭头过口
    let (top, bot) = (r.min.y + m, r.max.y - m);
    let (left, right) = (r.min.x + m, r.max.x - m);
    let cy = (top + bot) / 2.0;
    // 上下整边
    p.line_segment([Pos2::new(left, top), Pos2::new(right, top)], s);
    p.line_segment([Pos2::new(left, bot), Pos2::new(right, bot)], s);
    // 左右各两段，中段留过口
    for x in [left, right] {
        p.line_segment([Pos2::new(x, top), Pos2::new(x, cy - gap / 2.0)], s);
        p.line_segment([Pos2::new(x, cy + gap / 2.0), Pos2::new(x, bot)], s);
    }
    // 水平箭头：从左侧外穿到右侧外
    let (from, to) = (
        Pos2::new(left - w * 0.05, cy),
        Pos2::new(right + w * 0.05, cy),
    );
    p.line_segment([from, to], s);
    p.line_segment([to, Pos2::new(to.x - w * 0.22, cy - w * 0.14)], s);
    p.line_segment([to, Pos2::new(to.x - w * 0.22, cy + w * 0.14)], s);
}

/// 旋转图标：小方块画面 + 一道掠过右上角的贝塞尔弧，末端箭头指向
/// 顺时针切线方向（与 R 键一致）。
fn draw_rotate(p: &egui::Painter, r: Rect, c: Color32) {
    let s = Stroke::new(ICON_W, c);
    let w = r.width();
    let at = |fx: f32, fy: f32| Pos2::new(r.min.x + w * fx, r.min.y + w * fy);
    // 画面方块（左下，约占图标一半）
    p.rect_stroke(
        Rect::from_min_max(at(0.14, 0.38), at(0.62, 0.88)),
        1.0,
        s,
        egui::StrokeKind::Inside,
    );
    // 弧：从方块顶边中部上方掠过右上角，落到右侧
    let tip = at(0.86, 0.46);
    p.add(egui::Shape::QuadraticBezier(
        egui::epaint::QuadraticBezierShape::from_points_stroke(
            [at(0.34, 0.28), at(0.72, 0.08), tip],
            false,
            Color32::TRANSPARENT,
            s,
        ),
    ));
    // 箭头两翼：从终点沿切线反方向向后张开（顺时针 = 指向右下）
    p.line_segment([tip, tip + Vec2::new(-w * 0.16, -w * 0.10)], s);
    p.line_segment([tip, tip + Vec2::new(-w * 0.02, -w * 0.20)], s);
}

/// 翻转图标：圆角矩形 + 中央竖直虚线（镜像轴，两半互为镜像）。
fn draw_flip(p: &egui::Painter, r: Rect, c: Color32) {
    let s = Stroke::new(ICON_W, c);
    let w = r.width();
    p.rect_stroke(r.shrink(0.5), 2.0, s, egui::StrokeKind::Inside);
    let cx = r.center().x;
    for (a, b) in [(0.14, 0.38), (0.46, 0.54), (0.62, 0.86)] {
        p.line_segment(
            [
                Pos2::new(cx, r.min.y + w * a),
                Pos2::new(cx, r.min.y + w * b),
            ],
            s,
        );
    }
}

/// 缩小图标：放大镜 + 圆内减号。
fn draw_minus(p: &egui::Painter, r: Rect, c: Color32) {
    draw_glass(p, r, c, false);
}

/// 放大图标：放大镜 + 圆内加号。
fn draw_plus(p: &egui::Painter, r: Rect, c: Color32) {
    draw_glass(p, r, c, true);
}

/// 放大镜：圆在左上、柄伸向右下角，圆内嵌 ± 号。
fn draw_glass(p: &egui::Painter, r: Rect, c: Color32, plus: bool) {
    let s = Stroke::new(ICON_W, c);
    let w = r.width();
    let center = Pos2::new(r.min.x + w * 0.42, r.min.y + w * 0.42);
    let rad = w * 0.32;
    p.circle_stroke(center, rad, s);
    // 柄：从圆周 45° 到右下角
    let k = std::f32::consts::FRAC_1_SQRT_2;
    p.line_segment(
        [
            Pos2::new(center.x + rad * k, center.y + rad * k),
            Pos2::new(r.min.x + w * 0.96, r.min.y + w * 0.96),
        ],
        s,
    );
    let d = w * 0.16;
    p.line_segment(
        [
            Pos2::new(center.x - d, center.y),
            Pos2::new(center.x + d, center.y),
        ],
        s,
    );
    if plus {
        p.line_segment(
            [
                Pos2::new(center.x, center.y - d),
                Pos2::new(center.x, center.y + d),
            ],
            s,
        );
    }
}

// ---------------------------------------------------------------- 测试

#[cfg(test)]
mod tests {
    use super::*;

    /// 生成 w×h 的 RGBA：每像素 = (x, y, 0, 255)，便于断言坐标置换
    fn xy_image(w: u32, h: u32) -> Vec<u8> {
        let mut v = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                v[i] = x as u8;
                v[i + 1] = y as u8;
                v[i + 2] = 0;
                v[i + 3] = 255;
            }
        }
        v
    }

    #[test]
    fn rotate_cw_maps_coordinates() {
        // 3 宽 × 2 高。顺时针 90°：out(x', y') = old(y', h-1-x')
        // （物理验证：old 左上 → new 右上、old 左下 → new 左上）
        let src = xy_image(3, 2);
        let (out, nw, nh) = rotate_cw_rgba(&src, 3, 2);
        assert_eq!((nw, nh), (2, 3));
        let p = |x: usize, y: usize| {
            let i = (y * 2 + x) * 4;
            (out[i], out[i + 1])
        };
        assert_eq!(p(0, 0), (0, 1)); // new 左上 ← old 左下
        assert_eq!(p(1, 0), (0, 0)); // new 右上 ← old 左上
        assert_eq!(p(0, 2), (2, 1)); // new 左下 ← old 右下
        assert_eq!(p(1, 2), (2, 0)); // new 右下 ← old 右上
                                     // 旋转 4 次回到原样
        let (a, w, h) = rotate_cw_rgba(&src, 3, 2);
        let (b, w, h) = rotate_cw_rgba(&a, w, h);
        let (c, w, h) = rotate_cw_rgba(&b, w, h);
        let (d, w, h) = rotate_cw_rgba(&c, w, h);
        assert_eq!((w, h), (3, 2));
        assert_eq!(d, src);
    }

    #[test]
    fn flip_h_and_v_are_involutions() {
        let src = xy_image(3, 2);
        // 水平翻转：x 镜像
        let mut a = src.clone();
        flip_rgba(&mut a, 3, 2, true);
        let px = |v: &[u8], x: usize, y: usize| {
            let i = (y * 3 + x) * 4;
            (v[i], v[i + 1])
        };
        assert_eq!(px(&a, 0, 0), (2, 0));
        assert_eq!(px(&a, 2, 1), (0, 1));
        let mut b = a.clone();
        flip_rgba(&mut b, 3, 2, true);
        assert_eq!(b, src);
        // 垂直翻转：y 镜像
        let mut c = src.clone();
        flip_rgba(&mut c, 3, 2, false);
        assert_eq!(px(&c, 1, 0), (1, 1));
        let mut d = c.clone();
        flip_rgba(&mut d, 3, 2, false);
        assert_eq!(d, src);
    }

    /// 离屏渲染贴图条全部图标为一张 PNG，供人工核对造型（无窗口依赖，
    /// egui tessellate + 软件三角形光栅化）。CI 不跑：
    /// `cargo test -p lscreen icon_preview -- --ignored --nocapture`
    #[test]
    #[ignore = "生成 /tmp/lscreen_icon_preview.png 供人工核对图标造型"]
    fn icon_preview() {
        use crate::ui::toolbar::{action_button, draw_check, draw_close, draw_save, icon_button};
        use egui::epaint::{Primitive, WHITE_UV};

        let ctx = egui::Context::default();
        ctx.set_pixels_per_point(4.0); // 4x 渲染，缩略图也看得清线稿
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(300.0, 40.0))),
            ..Default::default()
        };
        let out = ctx.run_ui(input, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing = Vec2::splat(2.0);
                    icon_button(ui, false, "", draw_topmost);
                    icon_button(ui, true, "", draw_through);
                    action_button(ui, true, "", draw_rotate);
                    action_button(ui, true, "", draw_flip);
                    action_button(ui, true, "", draw_minus);
                    // 百分比按钮原样占位；其文字走字体图集纹理，下面的
                    // 光栅化只画无纹理几何，文字留空不影响核对
                    ui.add(
                        egui::Button::new(egui::RichText::new("100%").strong().size(12.0))
                            .min_size(Vec2::new(40.0, 24.0)),
                    );
                    action_button(ui, true, "", draw_plus);
                    action_button(ui, true, "", draw_save);
                    action_button(ui, true, "", draw_close);
                    action_button(ui, true, "", draw_check);
                });
            });
        });
        let prims = ctx.tessellate(out.shapes, out.pixels_per_point);

        // 软件光栅化：逐三角形重心覆盖，顶点 alpha 插值 + clamp 处理
        // 线段衔接处的少量重叠。线条/填充与文字字形共用同一 mesh
        // （texture 均为 Managed(0)），靠 uv 区分：字形 quad 的 uv 落在
        // 字体图集内，逐三角形过滤（uv==WHITE_UV 才是无纹理几何）
        let ppp = out.pixels_per_point;
        let w = (300.0 * ppp) as usize;
        let h = (40.0 * ppp) as usize;
        // 累积 alpha 与预乘 rgb，末端归一化后对深灰底做 src-over，
        // 保留 Frame 底色与线条白色的层次
        let mut acc_a = vec![0.0f32; w * h];
        let mut acc_c = vec![[0.0f32; 3]; w * h];
        for cp in &prims {
            let Primitive::Mesh(mesh) = &cp.primitive else {
                continue;
            };
            for tri in mesh.indices.chunks_exact(3) {
                let v = |i: u32| &mesh.vertices[i as usize];
                let (a, b, c) = (v(tri[0]), v(tri[1]), v(tri[2]));
                if [a, b, c].iter().any(|v| v.uv != WHITE_UV) {
                    continue;
                }
                let (ax, ay) = (a.pos.x * ppp, a.pos.y * ppp);
                let (bx, by) = (b.pos.x * ppp, b.pos.y * ppp);
                let (cx, cy) = (c.pos.x * ppp, c.pos.y * ppp);
                let area2 = (bx - ax) * (cy - ay) - (by - ay) * (cx - ax);
                if area2.abs() < 1e-9 {
                    continue;
                }
                let min_x = (ax.min(bx).min(cx).floor().max(0.0) as usize).min(w - 1);
                let min_y = (ay.min(by).min(cy).floor().max(0.0) as usize).min(h - 1);
                let max_x = (ax.max(bx).max(cx).ceil() as usize).min(w - 1);
                let max_y = (ay.max(by).max(cy).ceil() as usize).min(h - 1);
                for py in min_y..=max_y {
                    for px in min_x..=max_x {
                        let (sx, sy) = (px as f32 + 0.5, py as f32 + 0.5);
                        let wa = ((bx - sx) * (cy - sy) - (by - sy) * (cx - sx)) / area2;
                        let wb = ((cx - sx) * (ay - sy) - (cy - sy) * (ax - sx)) / area2;
                        let wc = 1.0 - wa - wb;
                        if wa >= -1e-6 && wb >= -1e-6 && wc >= -1e-6 {
                            let (wa, wb, wc) = (wa.max(0.0), wb.max(0.0), wc.max(0.0));
                            // Color32 为预乘 alpha：rgb 通道直接累积
                            let chans =
                                |v: &egui::epaint::Vertex| [v.color.r(), v.color.g(), v.color.b()];
                            let (ca, cb, cc) = (chans(a), chans(b), chans(c));
                            let i = py * w + px;
                            acc_a[i] += wa * f32::from(a.color.a())
                                + wb * f32::from(b.color.a())
                                + wc * f32::from(c.color.a());
                            for (k, slot) in acc_c[i].iter_mut().enumerate() {
                                *slot += wa * f32::from(ca[k])
                                    + wb * f32::from(cb[k])
                                    + wc * f32::from(cc[k]);
                            }
                        }
                    }
                }
            }
        }
        let mut img = image::RgbaImage::new(w as u32, h as u32);
        for (x, y, p) in img.enumerate_pixels_mut() {
            let i = y as usize * w + x as usize;
            let a = (acc_a[i] / 255.0).min(1.0);
            // 累积色去预乘还原原色（预乘 rgb = 原色×alpha/255），再 src-over
            let mut px = [40u8; 3];
            if acc_a[i] > 1e-3 {
                for (k, ch) in px.iter_mut().enumerate() {
                    let c = acc_c[i][k] * 255.0 / acc_a[i];
                    *ch = (c * a + 40.0 * (1.0 - a)).round() as u8;
                }
            }
            *p = image::Rgba([px[0], px[1], px[2], 255]);
        }
        img.save("/tmp/lscreen_icon_preview.png").unwrap();
        println!("已生成 /tmp/lscreen_icon_preview.png（{}x{}）", w, h);
    }
}
