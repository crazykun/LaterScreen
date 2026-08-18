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

use crate::geom::{P2, RectF};
use crate::model::{Element, ElementKind, Rgba};

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
        let mut paint = Paint::default();
        paint.anti_alias = true;
        let c = e.style.color;
        paint.set_color_rgba8(c.r(), c.g(), c.b(), c.a());

        let stroke = Stroke {
            width: e.style.width,
            line_cap: tiny_skia::LineCap::Round,
            line_join: tiny_skia::LineJoin::Round,
            ..Stroke::default()
        };
        let id = Transform::identity();

        match &e.kind {
            ElementKind::Rect { rect } => {
                if let Some(r) = sk_rect(rect) {
                    let path = PathBuilder::from_rect(r);
                    canvas.stroke_path(&path, &paint, &stroke, id, None);
                }
            }
            ElementKind::Ellipse { rect } => {
                if let Some(r) = sk_rect(rect) {
                    if let Some(path) = PathBuilder::from_oval(r) {
                        canvas.stroke_path(&path, &paint, &stroke, id, None);
                    }
                }
            }
            ElementKind::Line { from, to } => {
                if let Some(path) = polyline_path(&[*from, *to]) {
                    canvas.stroke_path(&path, &paint, &stroke, id, None);
                }
            }
            ElementKind::Arrow { from, to } => {
                self.draw_arrow(canvas, &paint, &stroke, *from, *to, e.style.width);
            }
            ElementKind::Curve { points } => {
                if let Some(path) = polyline_path(points) {
                    canvas.stroke_path(&path, &paint, &stroke, id, None);
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
            ElementKind::Text { pos, content, .. } => {
                self.draw_text(canvas, content, pos.x, pos.y, e.style.font_size, c);
            }
            ElementKind::Mosaic { points } => {
                draw_mosaic(canvas, original, points, e.mosaic_brush(), e.mosaic_cell());
            }
            ElementKind::Eraser { points } => {
                // 用原图作为 Pattern 沿笔迹描边，把原始像素贴回
                let mut pat = Paint::default();
                pat.shader = Pattern::new(
                    original.as_ref(),
                    SpreadMode::Pad,
                    FilterQuality::Nearest,
                    1.0,
                    id,
                );
                let brush = Stroke {
                    width: e.eraser_brush() * 2.0,
                    line_cap: tiny_skia::LineCap::Round,
                    line_join: tiny_skia::LineJoin::Round,
                    ..Stroke::default()
                };
                if let Some(path) = polyline_path(points) {
                    canvas.stroke_path(&path, &pat, &brush, id, None);
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
    let mut paint = Paint::default();
    paint.anti_alias = false;
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
            let Some(px) = rgba.get(i..i + 4) else { continue };
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
    for px in rgba.chunks_exact_mut(4) {
        px[3] = 255;
    }
}

fn sk_rect(r: &RectF) -> Option<SkRect> {
    SkRect::from_ltrb(r.min.x, r.min.y, r.max.x, r.max.y)
}

fn polyline_path(points: &[P2]) -> Option<tiny_skia::Path> {
    if points.is_empty() {
        return None;
    }
    let mut pb = PathBuilder::new();
    pb.move_to(points[0].x, points[0].y);
    if points.len() == 1 {
        // 单点：画一段极短线让 Round cap 呈现为圆点
        pb.line_to(points[0].x + 0.01, points[0].y);
    } else {
        for p in &points[1..] {
            pb.line_to(p.x, p.y);
        }
    }
    pb.finish()
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
