//! 导出渲染：用 tiny-skia 将图元合成到截图位图上，产出最终 RGBA。
//!
//! 与交互层（egui Painter）的对应关系：两边读取同一份 `Element` 几何数据，
//! 绘制参数（线宽、箭头头部比例、马赛克格子）全部来自 `Element`/`Style` 上的
//! 方法，避免两份实现漂移。
//!
//! 文本用 ab_glyph 光栅化。字体字节由调用方传入（App 层从系统字体目录定位，
//! 不在二进制里捆绑字体，保证体积）。画布始终不透明（截图打底），因此
//! 直通 alpha 与预乘 alpha 等价，文本混合按不透明背景做 lerp。

use ab_glyph::{Font, FontArc, ScaleFont};
use tiny_skia::{
    FillRule, FilterQuality, Paint, PathBuilder, Pattern, Pixmap, Rect as SkRect, SpreadMode,
    Stroke, Transform,
};

use crate::geom::{RectF, P2};
use crate::model::{Element, ElementKind, Rgba, TEXT_BG_RADIUS};

pub struct Renderer {
    font: Option<FontArc>,
}

impl Renderer {
    /// font_data: TTF/OTF 字节。None 时文本/标号数字跳过（仅圆底）。
    pub fn new(font_data: Option<Vec<u8>>) -> Self {
        let font = font_data.and_then(|d| FontArc::try_from_vec(d).ok());
        Self { font }
    }

    pub fn has_font(&self) -> bool {
        self.font.is_some()
    }

    /// 将 elements 按顺序合成到 source（RGBA8，尺寸 w×h）上，返回新的 RGBA 缓冲。
    pub fn render(&self, source: &[u8], w: u32, h: u32, elements: &[Element]) -> Vec<u8> {
        let mut canvas = match pixmap_from_rgba(source, w, h) {
            Some(p) => p,
            None => return source.to_vec(),
        };
        // 原图副本：橡皮擦贴回、马赛克取样都以原图为基准
        let original = canvas.clone();

        for e in elements {
            self.draw_element(&mut canvas, &original, e);
        }

        let mut out = canvas.take();
        force_opaque(&mut out);
        out
    }

    fn draw_element(&self, canvas: &mut Pixmap, original: &Pixmap, e: &Element) {
        let c = e.style.color;
        let mut paint = Paint {
            anti_alias: true,
            ..Paint::default()
        };
        paint.set_color_rgba8(c.r(), c.g(), c.b(), c.a());

        let stroke = Stroke {
            width: e.style.width,
            line_cap: tiny_skia::LineCap::Round,
            line_join: tiny_skia::LineJoin::Round,
            ..Stroke::default()
        };
        // egui 的 line_segment / Shape::line 使用平头端帽；箭头和椭圆继续
        // 使用圆头参数，避免共享 stroke 让导出端点额外外扩半个线宽。
        let flat_stroke = Stroke {
            line_cap: tiny_skia::LineCap::Butt,
            ..stroke.clone()
        };
        // 矩形四角用尖角连接：交互层 egui 矩形描边（Middle）是直角拐角，
        // Round join 会在导出层多出半径 w/2 的外圆角，两边不一致
        let miter_stroke = Stroke {
            line_join: tiny_skia::LineJoin::Miter,
            ..stroke.clone()
        };
        let id = Transform::identity();

        match &e.kind {
            ElementKind::Rect { rect } => {
                if let Some(r) = sk_rect(rect) {
                    let path = PathBuilder::from_rect(r);
                    canvas.stroke_path(&path, &paint, &miter_stroke, id, None);
                }
            }
            ElementKind::Ellipse { rect } => {
                // egui 的 EllipseShape 用 StrokeKind::Outside（描边整体落在椭圆边界外侧），
                // tiny-skia 是居中描边：把椭圆外扩半线宽再居中描边，内外边界都与交互层一致。
                if let Some(r) = sk_rect(rect) {
                    if let Some(oval) = r.outset(e.style.width * 0.5, e.style.width * 0.5) {
                        if let Some(path) = PathBuilder::from_oval(oval) {
                            canvas.stroke_path(&path, &paint, &stroke, id, None);
                        }
                    }
                }
            }
            ElementKind::Line { from, to } => {
                if let Some(path) = polyline_path(&[*from, *to]) {
                    canvas.stroke_path(&path, &paint, &flat_stroke, id, None);
                }
            }
            ElementKind::Arrow { from, to } => {
                // 箭头杆在交互层是 line_segment（平头端帽），与 Line/Curve 同规则
                self.draw_arrow(canvas, &paint, &flat_stroke, *from, *to, e.style.width);
            }
            ElementKind::Curve { points } => {
                if points.len() == 1 {
                    // 单点笔迹 = 圆点：与交互层 circle_filled(width/2) 像素一致。
                    // （旧实现画 0.01px 平头线段，导出后不可见，单击落点丢失）
                    let mut pb = PathBuilder::new();
                    pb.push_circle(points[0].x, points[0].y, e.style.width * 0.5);
                    if let Some(path) = pb.finish() {
                        canvas.fill_path(&path, &paint, FillRule::Winding, id, None);
                    }
                } else if let Some(path) = polyline_path(points) {
                    canvas.stroke_path(&path, &paint, &flat_stroke, id, None);
                }
            }
            ElementKind::Marker { center, number } => {
                let r = e.marker_radius();
                let mut pb = PathBuilder::new();
                pb.push_circle(center.x, center.y, r);
                if let Some(path) = pb.finish() {
                    canvas.fill_path(&path, &paint, FillRule::Winding, id, None);
                }
                let label = number.to_string();
                let size = e.style.font_size;
                let (tw, th) = self.measure_text(&label, size);
                self.draw_text(
                    canvas,
                    &label,
                    center.x - tw / 2.0,
                    center.y - th / 2.0,
                    size,
                    Rgba([255, 255, 255, 255]),
                );
            }
            ElementKind::Text {
                pos, content, bg, ..
            } => {
                // 文字背景（M13）：圆角矩形（立方角，kappa≈0.5523），
                // 半径/外扩常量与交互层共用（model::TEXT_BG_*）
                if let Some(bg) = bg {
                    let r = e.text_bg_rect();
                    let radius = TEXT_BG_RADIUS;
                    let mut pb = PathBuilder::new();
                    push_rounded_rect(&mut pb, r, radius);
                    if let Some(path) = pb.finish() {
                        let mut p = Paint {
                            anti_alias: true,
                            ..Paint::default()
                        };
                        p.set_color_rgba8(bg.r(), bg.g(), bg.b(), bg.a());
                        canvas.fill_path(&path, &p, FillRule::Winding, Transform::identity(), None);
                    }
                }
                self.draw_text(canvas, content, pos.x, pos.y, e.style.font_size, c);
            }
            ElementKind::Mosaic { points } => {
                draw_mosaic(canvas, original, points, e.mosaic_brush(), e.mosaic_cell());
            }
            ElementKind::Eraser { points } => {
                // 原图回贴：方形盖章（边长 2r、步进 0.6r），与交互层逐参数一致
                // （canvas.rs 的 UV 盖章）。原先这里是 Pattern 圆头描边（胶囊形），
                // 与交互层的方形预览形状不同，违反双路径像素一致约束。
                // Pattern 锚定在画布原点，每处盖章取回同一位置的原始像素。
                let pat = Paint {
                    shader: Pattern::new(
                        original.as_ref(),
                        SpreadMode::Pad,
                        FilterQuality::Nearest,
                        1.0,
                        id,
                    ),
                    ..Paint::default()
                };
                let r = e.eraser_brush();
                let stamp = |canvas: &mut Pixmap, p: P2| {
                    if let Some(rect) = SkRect::from_ltrb(p.x - r, p.y - r, p.x + r, p.y + r) {
                        canvas.fill_rect(rect, &pat, id, None);
                    }
                };
                match points.as_slice() {
                    [] => {}
                    [p] => stamp(canvas, *p),
                    pts => {
                        let step = r * 0.6;
                        for seg in pts.windows(2) {
                            let (a, b) = (seg[0], seg[1]);
                            let n = (a.dist(b) / step).ceil().max(1.0) as i32;
                            for i in 0..=n {
                                let t = i as f32 / n as f32;
                                stamp(
                                    canvas,
                                    P2::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t),
                                );
                            }
                        }
                    }
                }
            }
            ElementKind::Image { rect, rgba } => {
                // 位图图元（M13）：nearest 预缩放到 rect 的整数尺寸后整像素
                // 贴入——二维码要像素锐利（双线性会让码点糊边），且整像素
                // 对齐与交互层的 NEAREST 纹理采样一致
                let dw = rect.width().round().max(1.0) as u32;
                let dh = rect.height().round().max(1.0) as u32;
                if let Some(mut scaled) = Pixmap::new(dw, dh) {
                    nearest_scale_into(&mut scaled, rgba);
                    let pp = tiny_skia::PixmapPaint::default();
                    canvas.draw_pixmap(
                        rect.min.x.round() as i32,
                        rect.min.y.round() as i32,
                        scaled.as_ref(),
                        &pp,
                        Transform::identity(),
                        None,
                    );
                }
            }
        }
    }

    fn draw_arrow(
        &self,
        canvas: &mut Pixmap,
        paint: &Paint,
        stroke: &Stroke,
        from: P2,
        to: P2,
        width: f32,
    ) {
        let len = from.dist(to);
        if len < 1.0 {
            return;
        }
        // 期望 10px 起步、不超过线长一半；短箭头时 len*0.5 < 10，用 max/min 而非 clamp 避免 min>max panic
        let head = (width * 4.5).max(10.0).min(len * 0.5);
        let (ux, uy) = ((to.x - from.x) / len, (to.y - from.y) / len);
        // 线段止于箭头底部，避免线帽穿出三角
        let base = P2::new(to.x - ux * head, to.y - uy * head);
        let (px, py) = (-uy, ux); // 垂直方向
        let half = head * 0.5;

        if let Some(path) = polyline_path(&[from, base]) {
            canvas.stroke_path(&path, paint, stroke, Transform::identity(), None);
        }
        let mut pb = PathBuilder::new();
        pb.move_to(to.x, to.y);
        pb.line_to(base.x + px * half, base.y + py * half);
        pb.line_to(base.x - px * half, base.y - py * half);
        pb.close();
        if let Some(path) = pb.finish() {
            canvas.fill_path(&path, paint, FillRule::Winding, Transform::identity(), None);
        }
    }

    /// 文本测量：返回 (宽, 高)。UI 层与导出层都用它，保证命中框一致。
    pub fn measure_text(&self, text: &str, size: f32) -> (f32, f32) {
        let Some(font) = &self.font else {
            return (0.0, 0.0);
        };
        let scaled = font.as_scaled(size);
        let line_h = scaled.height() + scaled.line_gap();
        let mut max_w: f32 = 0.0;
        let mut lines = 0;
        for line in text.split('\n') {
            lines += 1;
            let mut w = 0.0;
            let mut prev = None;
            for ch in line.chars() {
                let gid = scaled.glyph_id(ch);
                if let Some(p) = prev {
                    w += scaled.kern(p, gid);
                }
                w += scaled.h_advance(gid);
                prev = Some(gid);
            }
            max_w = max_w.max(w);
        }
        (max_w, line_h * lines as f32)
    }

    /// 在 (x, y)（文本框左上角）绘制多行文本。
    fn draw_text(&self, canvas: &mut Pixmap, text: &str, x: f32, y: f32, size: f32, color: Rgba) {
        let Some(font) = &self.font else { return };
        let scaled = font.as_scaled(size);
        let line_h = scaled.height() + scaled.line_gap();
        let (cw, chh) = (canvas.width() as i32, canvas.height() as i32);

        for (i, line) in text.split('\n').enumerate() {
            let baseline = y + line_h * i as f32 + scaled.ascent();
            let mut pen_x = x;
            let mut prev = None;
            for ch in line.chars() {
                let gid = scaled.glyph_id(ch);
                if let Some(p) = prev {
                    pen_x += scaled.kern(p, gid);
                }
                let glyph = gid.with_scale_and_position(size, ab_glyph::point(pen_x, baseline));
                if let Some(outlined) = font.outline_glyph(glyph) {
                    let bb = outlined.px_bounds();
                    let data = canvas.data_mut();
                    outlined.draw(|gx, gy, cov| {
                        let px = bb.min.x as i32 + gx as i32;
                        let py = bb.min.y as i32 + gy as i32;
                        if px < 0 || py < 0 || px >= cw || py >= chh {
                            return;
                        }
                        let idx = ((py * cw + px) * 4) as usize;
                        // 不透明背景上的 lerp 混合
                        let a = (cov * color.a() as f32 / 255.0).clamp(0.0, 1.0);
                        data[idx] = lerp_u8(data[idx], color.r(), a);
                        data[idx + 1] = lerp_u8(data[idx + 1], color.g(), a);
                        data[idx + 2] = lerp_u8(data[idx + 2], color.b(), a);
                        data[idx + 3] = 255;
                    });
                }
                pen_x += scaled.h_advance(gid);
                prev = Some(gid);
            }
        }
    }
}

/// 马赛克：沿笔迹标记被覆盖的格子，每格填充原图均值色。
/// 交互层用同样的算法画色块，两边像素一致。
pub fn mosaic_cells(
    original_rgba: &[u8],
    w: u32,
    h: u32,
    points: &[P2],
    brush: f32,
    cell: f32,
) -> Vec<(f32, f32, f32, Rgba)> {
    let cell = cell.max(2.0);
    let (cols, rows) = (
        (w as f32 / cell).ceil() as i32,
        (h as f32 / cell).ceil() as i32,
    );
    let mut marked = std::collections::HashSet::new();
    // 沿折线按半个格距采样，标记笔刷半径内的格子
    let mut mark_around = |p: P2| {
        let r_cells = (brush / cell).ceil() as i32;
        let (cx, cy) = ((p.x / cell) as i32, (p.y / cell) as i32);
        for dy in -r_cells..=r_cells {
            for dx in -r_cells..=r_cells {
                let (gx, gy) = (cx + dx, cy + dy);
                if gx < 0 || gy < 0 || gx >= cols || gy >= rows {
                    continue;
                }
                let center = P2::new((gx as f32 + 0.5) * cell, (gy as f32 + 0.5) * cell);
                if center.dist(p) <= brush + cell * 0.5 {
                    marked.insert((gx, gy));
                }
            }
        }
    };
    match points {
        [] => {}
        [p] => mark_around(*p),
        _ => {
            for seg in points.windows(2) {
                let (a, b) = (seg[0], seg[1]);
                let n = (a.dist(b) / (cell * 0.5)).ceil().max(1.0) as i32;
                for i in 0..=n {
                    let t = i as f32 / n as f32;
                    mark_around(P2::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t));
                }
            }
        }
    }

    marked
        .into_iter()
        .map(|(gx, gy)| {
            let x0 = (gx as f32 * cell) as u32;
            let y0 = (gy as f32 * cell) as u32;
            let avg = average_color(original_rgba, w, h, x0, y0, cell as u32);
            (gx as f32 * cell, gy as f32 * cell, cell, avg)
        })
        .collect()
}

fn draw_mosaic(canvas: &mut Pixmap, original: &Pixmap, points: &[P2], brush: f32, cell: f32) {
    let (w, h) = (original.width(), original.height());
    let cells = mosaic_cells(original.data(), w, h, points, brush, cell);
    // 色块必须硬边（无 AA）：半格过渡会让交互层与导出层出现可辨差异
    let mut paint = Paint {
        anti_alias: false,
        ..Paint::default()
    };
    for (x, y, size, color) in cells {
        paint.set_color_rgba8(color.r(), color.g(), color.b(), 255);
        if let Some(r) = SkRect::from_xywh(x, y, size, size) {
            canvas.fill_rect(r, &paint, Transform::identity(), None);
        }
    }
}

fn average_color(rgba: &[u8], w: u32, h: u32, x0: u32, y0: u32, cell: u32) -> Rgba {
    let (mut r, mut g, mut b, mut n) = (0u64, 0u64, 0u64, 0u64);
    for y in y0..(y0 + cell).min(h) {
        for x in x0..(x0 + cell).min(w) {
            let i = (y as usize * w as usize + x as usize) * 4;
            let Some(px) = rgba.get(i..i + 4) else {
                continue;
            };
            r += px[0] as u64;
            g += px[1] as u64;
            b += px[2] as u64;
            n += 1;
        }
    }
    if n == 0 {
        return Rgba([0, 0, 0, 255]);
    }
    Rgba([(r / n) as u8, (g / n) as u8, (b / n) as u8, 255])
}

fn pixmap_from_rgba(source: &[u8], w: u32, h: u32) -> Option<Pixmap> {
    if source.len() != (w as usize) * (h as usize) * 4 || w == 0 || h == 0 {
        return None;
    }
    // 截图不透明，直通 alpha 与预乘等价；强制 alpha=255 规避个别平台返回 0 alpha
    let mut data = source.to_vec();
    force_opaque(&mut data);
    Pixmap::from_vec(data, tiny_skia::IntSize::from_wh(w, h)?)
}

fn lerp_u8(dst: u8, src: u8, a: f32) -> u8 {
    (dst as f32 * (1.0 - a) + src as f32 * a).round() as u8
}

fn force_opaque(rgba: &mut [u8]) {
    for px in rgba.as_chunks_mut::<4>().0 {
        px[3] = 255;
    }
}

fn sk_rect(r: &RectF) -> Option<SkRect> {
    SkRect::from_ltrb(r.min.x, r.min.y, r.max.x, r.max.y)
}

fn polyline_path(points: &[P2]) -> Option<tiny_skia::Path> {
    // 少于 2 点无可见线段（单点笔迹由调用方画圆点），空路径直接放弃
    if points.len() < 2 {
        return None;
    }
    let mut pb = PathBuilder::new();
    pb.move_to(points[0].x, points[0].y);
    for p in &points[1..] {
        pb.line_to(p.x, p.y);
    }
    pb.finish()
}

/// 圆角矩形路径（顺时针，立方角近似圆弧，kappa≈0.5523）。
/// 半径自动钳到短边一半，退化矩形（r≈0）为直角矩形。
fn push_rounded_rect(pb: &mut PathBuilder, r: RectF, radius: f32) {
    let (x0, y0, x1, y1) = (r.min.x, r.min.y, r.max.x, r.max.y);
    let rad = radius.min((x1 - x0).max(0.0).min(y1 - y0) * 0.5);
    if rad <= 0.0 {
        if let Some(rect) = SkRect::from_ltrb(x0, y0, x1, y1) {
            pb.push_rect(rect);
        }
        return;
    }
    let k = rad * 0.5523;
    pb.move_to(x0 + rad, y0);
    pb.line_to(x1 - rad, y0);
    pb.cubic_to(x1 - k, y0, x1, y0 + k, x1, y0 + rad);
    pb.line_to(x1, y1 - rad);
    pb.cubic_to(x1, y1 - k, x1 - k, y1, x1 - rad, y1);
    pb.line_to(x0 + rad, y1);
    pb.cubic_to(x0 + k, y1, x0, y1 - k, x0, y1 - rad);
    pb.line_to(x0, y0 + rad);
    pb.cubic_to(x0, y0 + k, x0 + k, y0, x0 + rad, y0);
    pb.close();
}

/// 位图 nearest 缩放（M13 Image 图元导出用）。目标尺寸由 rect 取整而来，
/// 源为不透明的 RGBA8；与 GPU 侧 NEAREST 采样同规则。
fn nearest_scale_into(dst: &mut Pixmap, src: &image::RgbaImage) {
    let (sw, sh) = (src.width() as usize, src.height() as usize);
    let (dw, dh) = (dst.width() as usize, dst.height() as usize);
    if sw == 0 || sh == 0 || dw == 0 || dh == 0 {
        return;
    }
    let s = src.as_raw();
    let d = dst.data_mut();
    for y in 0..dh {
        let sy = (y * sh / dh).min(sh - 1);
        for x in 0..dw {
            let sx = (x * sw / dw).min(sw - 1);
            let si = (sy * sw + sx) * 4;
            let di = (y * dw + x) * 4;
            d[di..di + 4].copy_from_slice(&s[si..si + 4]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ElementKind, Style};

    fn blank(w: u32, h: u32) -> Vec<u8> {
        vec![255u8; (w * h * 4) as usize]
    }

    #[test]
    fn render_short_arrow_no_panic() {
        // 回归：len < 20 时 head 上限 len*0.5 < 10，曾因 clamp(10.0, <10) panic
        let r = Renderer::new(None);
        let src = blank(64, 64);
        let elems = vec![Element {
            id: 1,
            kind: ElementKind::Arrow {
                from: P2::new(10.0, 10.0),
                to: P2::new(20.0, 20.0),
            },
            style: Style::default(),
        }];
        let _ = r.render(&src, 64, 64, &elems);
    }

    #[test]
    fn render_rect_changes_pixels() {
        let r = Renderer::new(None);
        let src = blank(64, 64);
        let elems = vec![Element {
            id: 1,
            kind: ElementKind::Rect {
                rect: RectF::from_points(P2::new(8.0, 8.0), P2::new(56.0, 56.0)),
            },
            style: Style::default(),
        }];
        let out = r.render(&src, 64, 64, &elems);
        assert_ne!(out, src);
        assert_eq!(out.len(), src.len());
    }

    #[test]
    fn image_element_renders_scaled() {
        // M13 位图图元：nearest 缩放贴入，源图的红色应出现在目标矩形中心
        let r = Renderer::new(None);
        let src = blank(64, 64);
        let img = image::RgbaImage::from_pixel(10, 10, image::Rgba([0xe5, 0x39, 0x35, 0xff]));
        let elems = vec![Element {
            id: 1,
            kind: ElementKind::Image {
                rect: RectF::from_points(P2::new(8.0, 8.0), P2::new(40.0, 40.0)),
                rgba: std::sync::Arc::new(img),
            },
            style: Style::default(),
        }];
        let out = r.render(&src, 64, 64, &elems);
        let px = |x: usize, y: usize| &out[(y * 64 + x) * 4..(y * 64 + x) * 4 + 3];
        assert_eq!(px(24, 24), &[0xe5, 0x39, 0x35], "矩形中心为源图红");
        assert_eq!(px(4, 24), &[255, 255, 255], "矩形外保持原样");
    }

    #[test]
    fn text_bg_fills_padding_and_uses_contrast() {
        // M13 文字背景：bg 矩形（pos-2 ~ pos+size+2）应被底色填充；
        // 文字色按创建约定已由 UI 层写成对比色，这里验证渲染层忠实画底
        let r = Renderer::new(None);
        let src = blank(64, 64);
        let bg = Rgba([0xe5, 0x39, 0x35, 0xff]); // 红
        let elems = vec![Element {
            id: 1,
            kind: ElementKind::Text {
                pos: P2::new(20.0, 20.0),
                content: String::new(),
                size: P2::new(24.0, 12.0),
                bg: Some(bg),
            },
            style: Style::default(),
        }];
        let out = r.render(&src, 64, 64, &elems);
        let px = |x: usize, y: usize| &out[(y * 64 + x) * 4..(y * 64 + x) * 4 + 3];
        // 背景矩形中心（字空着无字形遮挡）
        assert_eq!(px(32, 26), &[0xe5, 0x39, 0x35]);
        // 上边距带（pos.y-1，在 pad=2 内）
        assert_eq!(px(32, 19), &[0xe5, 0x39, 0x35]);
        // 矩形外 3px 保持原样
        assert_eq!(px(32, 15), &[255, 255, 255]);
        assert_eq!(px(10, 26), &[255, 255, 255]);
        // 对比色：红底 → 白字
        assert_eq!(
            crate::color::contrast_text_color(bg),
            Rgba([0xff, 0xff, 0xff, 0xff])
        );
        assert_eq!(
            crate::color::contrast_text_color(Rgba([0xfd, 0xd8, 0x35, 0xff])), // 黄底
            Rgba([0x11, 0x11, 0x11, 0xff])
        );
    }

    #[test]
    fn single_point_curve_renders_dot() {
        // 回归：单点笔迹在交互层是 circle_filled(width/2) 的圆点，导出层
        // 曾画 0.01px 平头线段（不可见，单击落点丢失）
        let r = Renderer::new(None);
        let src = blank(64, 64);
        let elems = vec![Element {
            id: 1,
            kind: ElementKind::Curve {
                points: vec![P2::new(32.0, 32.0)],
            },
            style: Style {
                width: 8.0,
                ..Style::default()
            },
        }];
        let out = r.render(&src, 64, 64, &elems);
        let center = ((32 * 64 + 32) * 4) as usize;
        assert_ne!(&out[center..center + 3], &[255, 255, 255], "圆心应被着色");
        // 圆点覆盖半径 width/2=4：距圆心 2px 处应着色、8px 外应保持原样
        let near = ((32 * 64 + 34) * 4) as usize;
        assert_ne!(&out[near..near + 3], &[255, 255, 255]);
        let far = ((32 * 64 + 42) * 4) as usize;
        assert_eq!(&out[far..far + 3], &[255, 255, 255]);
    }

    #[test]
    fn eraser_restores_original() {
        let r = Renderer::new(None);
        let src = blank(64, 64);
        // 先画满一个大矩形再整体擦除，中心像素应回到白色
        let style = Style {
            width: 30.0,
            ..Style::default()
        };
        let elems = vec![
            Element {
                id: 1,
                kind: ElementKind::Rect {
                    rect: RectF::from_points(P2::new(20.0, 20.0), P2::new(44.0, 44.0)),
                },
                style,
            },
            Element {
                id: 2,
                kind: ElementKind::Eraser {
                    points: vec![P2::new(0.0, 32.0), P2::new(64.0, 32.0)],
                },
                style: Style {
                    width: 20.0,
                    ..Style::default()
                },
            },
        ];
        let out = r.render(&src, 64, 64, &elems);
        let center = ((32 * 64 + 20) * 4) as usize; // 矩形左边缘上的点
        assert_eq!(&out[center..center + 3], &[255, 255, 255]);
    }
}
