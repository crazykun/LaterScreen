//! 录制点击高亮（M14）：采帧闭包内轮询全局指针与主键状态，检测按压沿，
//! 在按下位置叠加扩散圆环（几何与透明度插值见 core::highlight 纯函数）。
//!
//! 为什么轮询而非全局事件钩子：录制期间焦点不在任何本进程窗口（状态窗
//! 不在选区内），事件订阅需要 X11 XRecord / Win 全局钩子 / mac CGEventTap
//! 三套实现且 mac 要辅助功能权限；按帧轮询 `pointer_state` 零权限零新依赖，
//! 10-30fps 粒度对 300ms 生命周期的圆环足够。指针不可查询（Wayland/无
//! 设备）时整体静默关闭，不再逐帧重试。

use lscreen_core::highlight::ClickRipples;
use std::time::Instant;

pub struct ClickHighlight {
    enabled: bool,
    /// 录制区域原点（虚拟桌面坐标）：指针绝对坐标 → 帧内坐标换算。
    /// 换算后允许落在帧外——点击录制边框/状态窗的涟漪画不出交集，天然
    /// 不进成品
    origin: (i32, i32),
    /// 涟漪时间轴（相对构造时刻，ms）
    clock: Instant,
    ripples: ClickRipples,
    last_down: Option<bool>,
}

impl ClickHighlight {
    pub fn new(enabled: bool, origin: (i32, i32)) -> Self {
        Self {
            enabled,
            origin,
            clock: Instant::now(),
            ripples: ClickRipples::new(),
            last_down: None,
        }
    }

    /// 每采一帧调用：轮询指针、检测主键按压沿、把活动圆环就地叠加进帧。
    /// 在首帧 poster 留档之后调用，poster 恒为纯净帧。
    pub fn on_frame(&mut self, rgba: &mut [u8], w: u32, h: u32) {
        if !self.enabled {
            return;
        }
        let Some((mx, my, down)) = lscreen_capture::pointer_state() else {
            // 平台不支持全局指针查询：永久关闭（Wayland 录制本就不可用，
            // 此处是防御性路径）
            self.enabled = false;
            return;
        };
        let t = self.clock.elapsed().as_secs_f64() * 1000.0;
        // 按压沿 = 上一帧未按、本帧按住。首个样本只建基线：录制开始的瞬间
        // 可能正按住鼠标（armed 阶段点「开始」按钮的残留按住），不算点击
        if self.last_down == Some(false) && down {
            self.ripples.push(t, mx - self.origin.0, my - self.origin.1);
        }
        self.last_down = Some(down);
        self.ripples.draw(t, rgba, w, h);
    }
}
