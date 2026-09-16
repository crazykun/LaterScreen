//! 录制点击高亮（M14）：鼠标按下处的扩散圆环。
//!
//! 纯函数 + 就地混合进 RGBA 帧——由录制采帧闭包在编码管线之前叠加，
//! GIF/MP4 编码层无感知。时间一律由调用方传入的相对毫秒驱动，core 不持
//! 时钟，几何/透明度插值可单测。

/// 圆环寿命（ms）：从按下出现到完全淡出
pub const RING_LIFETIME_MS: f32 = 300.0;
/// 圆环最大外扩半径（帧内像素）
pub const RING_MAX_RADIUS: f32 = 28.0;
/// 描边宽度（px）
const STROKE: f32 = 3.0;
/// 起始不透明度（随进度衰减到 0）
const ALPHA0: f32 = 0.55;
/// 与录制边框/默认标注色同源的警示红
const COLOR: [u8; 3] = [0xE5, 0x39, 0x35];

/// 经过 t 毫秒后圆环的 `(半径, 不透明度)`；t ∈ [0, 300) 有效，过期/非法
/// （负数、NaN）一律 None（不可见）。半径按 sqrt 缓动扩散——快速出现后
/// 减速，接近常见点击涟漪的手感；不透明度二次衰减，尾部淡出更快。
pub fn ring_at(t_ms: f32) -> Option<(f32, f32)> {
    if !(0.0..RING_LIFETIME_MS).contains(&t_ms) {
        return None;
    }
    let p = t_ms / RING_LIFETIME_MS;
    Some((RING_MAX_RADIUS * p.sqrt(), ALPHA0 * (1.0 - p) * (1.0 - p)))
}

/// 活动点击涟漪集合：事件时刻由调用方时钟提供（相对 ms），draw 时清理
/// 过期项。连续快速点击自然并存（300ms 窗口内人工点不出需要设上限的数量）。
#[derive(Default)]
pub struct ClickRipples {
    events: Vec<(f64, i32, i32)>,
}

impl ClickRipples {
    pub fn new() -> Self {
        Self::default()
    }

    /// 记录一次按下；(x, y) 为帧内像素坐标，允许落在帧外（画不出交集即不可见
    /// ——点击录制边框/状态窗的高亮天然被排除在成品外）
    pub fn push(&mut self, t_ms: f64, x: i32, y: i32) {
        self.events.push((t_ms, x, y));
    }

    /// 仍在队列中的涟漪数（诊断/测试用；过期项在 draw 时清理）
    pub fn pending(&self) -> usize {
        self.events.len()
    }

    /// 把 now_ms 时刻仍可见的圆环叠加进帧（source-over 混合，就地修改）。
    pub fn draw(&mut self, now_ms: f64, rgba: &mut [u8], w: u32, h: u32) {
        self.events
            .retain(|(t0, _, _)| now_ms - t0 < RING_LIFETIME_MS as f64);
        for &(t0, x, y) in self.events.iter() {
            if let Some((r, a)) = ring_at((now_ms - t0) as f32) {
                draw_ring(rgba, w, h, x, y, r, a);
            }
        }
    }
}

/// 抗锯齿圆环描边：只遍历圆环包围盒与帧的交集，每像素按「到圆心距离与
/// 环带中心的偏差」求 1px 边缘覆盖度
fn draw_ring(rgba: &mut [u8], w: u32, h: u32, cx: i32, cy: i32, r: f32, alpha: f32) {
    if r <= 0.0 || alpha <= 0.0 || w == 0 || h == 0 || rgba.len() < (w * h * 4) as usize {
        return;
    }
    let (cxf, cyf) = (cx as f32, cy as f32);
    let half = STROKE * 0.5;
    // 包围盒 = 圆环外沿 + 1px 抗锯齿余量，钳到帧内（整环在帧外时为空区间）
    let pad = r + half + 1.0;
    let x0 = (cxf - pad).ceil().max(0.0) as i32;
    let x1 = (cxf + pad).floor().min(w as f32 - 1.0) as i32;
    let y0 = (cyf - pad).ceil().max(0.0) as i32;
    let y1 = (cyf + pad).floor().min(h as f32 - 1.0) as i32;
    for py in y0..=y1 {
        for px in x0..=x1 {
            let dx = px as f32 + 0.5 - cxf;
            let dy = py as f32 + 0.5 - cyf;
            let d = ((dx * dx + dy * dy).sqrt() - r).abs();
            // 距环带中心 half 以内全覆盖，向外 1px 线性过渡到 0
            let cov = (1.0 - (d - half).max(0.0)).clamp(0.0, 1.0);
            if cov > 0.0 {
                blend(rgba, w, px, py, cov * alpha);
            }
        }
    }
}

/// source-over 混合。截屏帧 alpha 恒不透明，alpha 通道保持原值不动
fn blend(rgba: &mut [u8], w: u32, x: i32, y: i32, a: f32) {
    let idx = (y as usize * w as usize + x as usize) * 4;
    let dst = &mut rgba[idx..idx + 4];
    let a = a.clamp(0.0, 1.0);
    for i in 0..3 {
        dst[i] = (COLOR[i] as f32 * a + dst[i] as f32 * (1.0 - a) + 0.5) as u8;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gray_frame(w: u32, h: u32) -> Vec<u8> {
        vec![0x80; (w * h * 4) as usize]
    }

    /// 像素 (x, y) 的 RGBA 切片
    fn px(f: &[u8], w: u32, x: u32, y: u32) -> &[u8] {
        let i = ((y * w + x) * 4) as usize;
        &f[i..i + 4]
    }

    #[test]
    fn ring_progress_bounds_and_monotonic() {
        assert_eq!(ring_at(0.0), Some((0.0, ALPHA0)));
        // 有效域半开：300ms 整已过期；负数/NaN 不可见
        assert!(ring_at(300.0).is_none());
        assert!(ring_at(-1.0).is_none());
        assert!(ring_at(f32::NAN).is_none());

        let (mut last_r, mut last_a) = (-1.0_f32, ALPHA0 + 1.0);
        for t in 0..300 {
            let Some((r, a)) = ring_at(t as f32) else {
                panic!("t={t} 应有效");
            };
            assert!(r >= last_r, "半径应随时间递增");
            assert!(a <= last_a, "不透明度应随时间递减");
            assert!((0.0..=RING_MAX_RADIUS).contains(&r));
            assert!((0.0..=ALPHA0).contains(&a));
            last_r = r;
            last_a = a;
        }
    }

    #[test]
    fn draw_paints_ring_not_center_or_outside() {
        let (w, h) = (64u32, 64u32);
        let mut f = gray_frame(w, h);
        let mut r = ClickRipples::new();
        r.push(0.0, 32, 32);
        // t=150：半径 ≈ 19.8，(51,33) 距圆心 ≈ 19.56 落在环带内
        r.draw(150.0, &mut f, w, h);

        let ring = px(&f, w, 51, 33);
        assert!(ring[0] > 0x80, "环带像素 R 应被染红: {:?}", ring);
        assert!(ring[1] < 0x80, "环带像素 G 应被压低: {:?}", ring);
        // 圆心与远处不受影响
        assert_eq!(px(&f, w, 32, 32), &[0x80, 0x80, 0x80, 0x80]);
        assert_eq!(px(&f, w, 63, 63), &[0x80, 0x80, 0x80, 0x80]);
    }

    #[test]
    fn draw_expires_and_prunes() {
        let (w, h) = (64, 64);
        let mut f = gray_frame(w, h);
        let mut r = ClickRipples::new();
        r.push(0.0, 32, 32);
        r.draw(400.0, &mut f, w, h);
        assert_eq!(f, gray_frame(w, h), "过期涟漪不画");
        assert_eq!(r.pending(), 0, "draw 应清理过期事件");
    }

    #[test]
    fn draw_outside_frame_is_noop() {
        let (w, h) = (64, 64);
        let mut f = gray_frame(w, h);
        let mut r = ClickRipples::new();
        r.push(0.0, -100, -100);
        r.push(0.0, 1000, 1000);
        r.draw(100.0, &mut f, w, h);
        assert_eq!(f, gray_frame(w, h), "帧外涟漪只画交集，应无变化");
    }

    #[test]
    fn multiple_ripples_coexist() {
        let (w, h) = (128u32, 64u32);
        let mut f = gray_frame(w, h);
        let mut r = ClickRipples::new();
        r.push(0.0, 32, 32);
        r.push(100.0, 96, 32);
        r.draw(150.0, &mut f, w, h);
        // 第一个环 t=150 半径 ≈19.8；第二个环 t=50 半径 ≈11.4
        assert!(px(&f, w, 51, 32)[0] > 0x80, "第一个圆环可见");
        assert!(px(&f, w, 107, 32)[0] > 0x80, "第二个圆环可见");
        assert_eq!(r.pending(), 2);
    }

    #[test]
    fn degenerate_frame_is_noop() {
        let mut r = ClickRipples::new();
        r.push(0.0, 0, 0);
        let mut f: Vec<u8> = vec![];
        r.draw(100.0, &mut f, 0, 0); // 不 panic 即可
        r.draw(100.0, &mut f, 0, 8);
    }
}
