//! 图元模型：所有标注元素的数据结构、命中检测、拖拽与控制点编辑。
//!
//! 设计要点：
//! - 图元只存几何与样式，渲染由交互层（egui）与导出层（tiny-skia）各自实现，
//!   两边共享这里的几何描述，保证所见即所得。
//! - 文本尺寸依赖字体测量，由 UI 层测量后写回 `Text::size`，命中检测直接用。

use crate::geom::{dist_to_polyline, RectF, P2};
use crate::history::History;

/// 文字背景矩形外扩（物理像素）与圆角半径（物理像素）。
/// 两条渲染路径共用，勿单侧改动。
pub const TEXT_BG_PAD: f32 = 2.0;
pub const TEXT_BG_RADIUS: f32 = 2.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rgba(pub [u8; 4]);

impl Rgba {
    pub const RED: Rgba = Rgba([0xe5, 0x39, 0x35, 0xff]);

    pub const fn r(&self) -> u8 {
        self.0[0]
    }
    pub const fn g(&self) -> u8 {
        self.0[1]
    }
    pub const fn b(&self) -> u8 {
        self.0[2]
    }
    pub const fn a(&self) -> u8 {
        self.0[3]
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Style {
    pub color: Rgba,
    /// 线宽（像素）。马赛克用它推导格子大小，橡皮擦用它做笔刷半径。
    pub width: f32,
    pub font_size: f32,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            color: Rgba::RED,
            width: 3.0,
            font_size: 24.0,
        }
    }
}

/// 工具栏上的当前工具。Select 负责悬停/拖拽已有图元。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tool {
    Select,
    Rect,
    Ellipse,
    Arrow,
    Line,
    Curve,
    Marker,
    Text,
    Mosaic,
    Eraser,
}

#[derive(Clone, PartialEq, Debug)]
pub enum ElementKind {
    Rect {
        rect: RectF,
    },
    Ellipse {
        rect: RectF,
    },
    Arrow {
        from: P2,
        to: P2,
    },
    Line {
        from: P2,
        to: P2,
    },
    Curve {
        points: Vec<P2>,
    },
    /// 自增序号标注（圆形背景 + 数字）。
    Marker {
        center: P2,
        number: u32,
    },
    /// size 为 UI 层测量后的包围盒尺寸，用于命中检测与导出布局。
    /// bg = 文字底色（M13 文字背景）：None 透明；Some 时文字颜色在创建时
    /// 已按底色亮度取对比色（见 color::contrast_text_color），两条渲染
    /// 路径都只管「先画 bg 圆角矩形、再按 style.color 画字」。
    Text {
        pos: P2,
        content: String,
        size: P2,
        bg: Option<Rgba>,
    },
    Mosaic {
        points: Vec<P2>,
    },
    /// 橡皮擦：渲染时将原图对应区域贴回，擦掉此前绘制的标注。
    Eraser {
        points: Vec<P2>,
    },
    /// 位图图元（M13 二维码生成）：小位图贴入标注层，与截图一起导出。
    /// rgba 装箱放 Arc——撤销快照只克隆指针，延续「图片本体不进快照」
    /// 原则（快照仍是全量 `Vec<Element>`，无需句柄表）。
    /// **恒等比缩放**（set_control_point 保证 rect 宽高比 = 位图宽高比）：
    /// 码图拉伸畸变会影响回扫识别。
    Image {
        rect: RectF,
        rgba: std::sync::Arc<image::RgbaImage>,
    },
}

#[derive(Clone, PartialEq, Debug)]
pub struct Element {
    pub id: u64,
    pub kind: ElementKind,
    pub style: Style,
}

impl Element {
    /// 命中检测：tol 为容差（像素）。描边类图元命中的是「边」而非内部，
    /// 与 Snipaste 交互一致（矩形中空处不挡住下层元素）。
    pub fn hit_test(&self, p: P2, tol: f32) -> bool {
        let t = tol + self.style.width / 2.0;
        match &self.kind {
            ElementKind::Rect { rect } => {
                rect.expand(t).contains(p) && !rect.expand(-t).contains(p)
            }
            ElementKind::Ellipse { rect } => {
                let (a, b) = (rect.width() / 2.0, rect.height() / 2.0);
                if a < 1.0 || b < 1.0 {
                    return rect.expand(t).contains(p);
                }
                let c = rect.center();
                // 归一化半径 r=1 为椭圆边界；用短半轴把偏差换算回像素近似距离
                let r = ((p.x - c.x) / a).powi(2) + ((p.y - c.y) / b).powi(2);
                (r.sqrt() - 1.0).abs() * a.min(b) <= t
            }
            ElementKind::Arrow { from, to } | ElementKind::Line { from, to } => {
                crate::geom::dist_to_segment(p, *from, *to) <= t
            }
            ElementKind::Curve { points } => dist_to_polyline(p, points) <= t,
            ElementKind::Marker { center, .. } => p.dist(*center) <= self.marker_radius() + tol,
            ElementKind::Text { pos, size, .. } => {
                RectF::from_points(*pos, pos.offset(size.x, size.y))
                    .expand(tol)
                    .contains(p)
            }
            // 可见范围 = 笔刷半径 + 被标记格子中心的外扩半格（见
            // render::mosaic_cells）+ 格子矩形自身的外半格。
            // 命中阈值必须覆盖完整可见区，否则最外一圈「画得到、选不中」。
            ElementKind::Mosaic { points } => {
                dist_to_polyline(p, points) <= tol + self.mosaic_brush() + self.mosaic_cell()
            }
            ElementKind::Eraser { points } => dist_to_polyline(p, points) <= self.eraser_brush(),
            // 实心命中（与 Marker 同语义）：码图任意点可选中
            ElementKind::Image { rect, .. } => rect.expand(tol).contains(p),
        }
    }

    pub fn translate(&mut self, dx: f32, dy: f32) {
        match &mut self.kind {
            ElementKind::Rect { rect } | ElementKind::Ellipse { rect } => {
                *rect = rect.translate(dx, dy)
            }
            ElementKind::Arrow { from, to } | ElementKind::Line { from, to } => {
                *from = from.offset(dx, dy);
                *to = to.offset(dx, dy);
            }
            ElementKind::Curve { points }
            | ElementKind::Mosaic { points }
            | ElementKind::Eraser { points } => {
                for p in points {
                    *p = p.offset(dx, dy);
                }
            }
            ElementKind::Marker { center, .. } => *center = center.offset(dx, dy),
            ElementKind::Text { pos, .. } => *pos = pos.offset(dx, dy),
            ElementKind::Image { rect, .. } => *rect = rect.translate(dx, dy),
        }
    }

    /// 可拖拽的控制点（矩形/椭圆四角、线段两端、位图四角）。其余图元返回空，只支持整体移动。
    pub fn control_points(&self) -> Vec<P2> {
        match &self.kind {
            ElementKind::Rect { rect } | ElementKind::Ellipse { rect } => rect.corners().to_vec(),
            ElementKind::Image { rect, .. } => rect.corners().to_vec(),
            ElementKind::Arrow { from, to } | ElementKind::Line { from, to } => vec![*from, *to],
            _ => Vec::new(),
        }
    }

    /// 拖拽第 idx 个控制点到 p。矩形/椭圆按「对角固定」语义调整；
    /// 位图同语义但**恒等比**（宽高比锁定为位图宽高比，码图拉伸
    /// 畸变会影响回扫识别）。
    pub fn set_control_point(&mut self, idx: usize, p: P2) {
        match &mut self.kind {
            ElementKind::Rect { rect } | ElementKind::Ellipse { rect } => {
                let corners = rect.corners();
                if idx < 4 {
                    let opposite = corners[(idx + 2) % 4];
                    *rect = RectF::from_points(opposite, p);
                }
            }
            ElementKind::Image { rect, rgba } => {
                let corners = rect.corners();
                if idx < 4 {
                    let opposite = corners[(idx + 2) % 4];
                    let aspect = rgba.width() as f32 / rgba.height() as f32;
                    let (dx, dy) = (p.x - opposite.x, p.y - opposite.y);
                    // 取两轴换算后的较大边长，保证拖拽方向不被截断
                    let width = dx.abs().max(dy.abs() * aspect).max(1.0);
                    let height = width / aspect;
                    let corner = P2::new(
                        opposite.x + width * dx.signum(),
                        opposite.y + height * dy.signum(),
                    );
                    *rect = RectF::from_points(opposite, corner);
                }
            }
            ElementKind::Arrow { from, to } | ElementKind::Line { from, to } => match idx {
                0 => *from = p,
                1 => *to = p,
                _ => {}
            },
            _ => {}
        }
    }

    /// 包围盒（未含线宽外扩），用于局部重绘与选中框显示。
    pub fn bounds(&self) -> RectF {
        let of_points = |pts: &[P2]| {
            pts.iter().fold(
                RectF {
                    min: P2::new(f32::INFINITY, f32::INFINITY),
                    max: P2::new(f32::NEG_INFINITY, f32::NEG_INFINITY),
                },
                |acc, p| acc.union(&RectF { min: *p, max: *p }),
            )
        };
        match &self.kind {
            ElementKind::Rect { rect } | ElementKind::Ellipse { rect } => *rect,
            ElementKind::Arrow { from, to } | ElementKind::Line { from, to } => {
                RectF::from_points(*from, *to)
            }
            ElementKind::Curve { points }
            | ElementKind::Mosaic { points }
            | ElementKind::Eraser { points } => of_points(points),
            ElementKind::Marker { center, .. } => {
                let r = self.marker_radius();
                RectF::from_points(center.offset(-r, -r), center.offset(r, r))
            }
            ElementKind::Text { pos, size, .. } => {
                RectF::from_points(*pos, pos.offset(size.x, size.y))
            }
            ElementKind::Image { rect, .. } => *rect,
        }
    }

    pub fn marker_radius(&self) -> f32 {
        self.style.font_size * 0.75
    }

    /// 文字背景（M13）：底色矩形（图像像素坐标）。两边渲染路径共用，
    /// 保证交互层与导出层几何一致；pad/radius 为常量（物理像素）。
    pub fn text_bg_rect(&self) -> RectF {
        let ElementKind::Text { pos, size, .. } = &self.kind else {
            return RectF::default();
        };
        RectF {
            min: P2::new(pos.x - TEXT_BG_PAD, pos.y - TEXT_BG_PAD),
            max: P2::new(pos.x + size.x + TEXT_BG_PAD, pos.y + size.y + TEXT_BG_PAD),
        }
    }

    pub fn mosaic_brush(&self) -> f32 {
        (self.style.width * 6.0).max(12.0)
    }

    pub fn eraser_brush(&self) -> f32 {
        (self.style.width * 5.0).max(10.0)
    }

    /// 马赛克格子边长。
    pub fn mosaic_cell(&self) -> f32 {
        (self.style.width * 3.0).max(8.0)
    }
}

/// 标注文档：图元列表 + 撤销/重做。
#[derive(Default)]
pub struct Document {
    pub elements: Vec<Element>,
    next_id: u64,
    history: History,
}

impl Document {
    /// 任何修改前调用一次，记录撤销快照并清空重做栈。
    /// 一次完整的用户手势（如拖拽全程）只调用一次。
    pub fn begin_change(&mut self) {
        self.history.push(&self.elements);
    }

    pub fn add(&mut self, kind: ElementKind, style: Style) -> u64 {
        self.next_id += 1;
        self.elements.push(Element {
            id: self.next_id,
            kind,
            style,
        });
        self.next_id
    }

    pub fn get_mut(&mut self, id: u64) -> Option<&mut Element> {
        self.elements.iter_mut().find(|e| e.id == id)
    }

    pub fn get(&self, id: u64) -> Option<&Element> {
        self.elements.iter().find(|e| e.id == id)
    }

    pub fn remove(&mut self, id: u64) {
        self.elements.retain(|e| e.id != id);
    }

    /// 命中最上层（最后绘制）的图元。
    pub fn hit_top(&self, p: P2, tol: f32) -> Option<u64> {
        self.elements
            .iter()
            .rev()
            .find(|e| e.hit_test(p, tol))
            .map(|e| e.id)
    }

    /// 下一个标号数字：由现存标号推导，撤销后自动回退。
    pub fn next_marker_number(&self) -> u32 {
        self.elements
            .iter()
            .filter_map(|e| match e.kind {
                ElementKind::Marker { number, .. } => Some(number),
                _ => None,
            })
            .max()
            .map_or(1, |n| n + 1)
    }

    pub fn undo(&mut self) {
        self.history.undo(&mut self.elements);
    }

    /// 回滚最近一次 begin_change 以来的修改，不产生重做项。
    pub fn cancel_change(&mut self) {
        self.history.cancel(&mut self.elements);
    }

    /// 结束一次「可能落空」的编辑手势（点选/控制点拖拽）：没有任何实际
    /// 修改时弹出该手势压入的空快照，避免产生无效撤销步。
    pub fn end_change_if_noop(&mut self) {
        self.history.drop_noop(&self.elements);
    }

    pub fn redo(&mut self) {
        self.history.redo(&mut self.elements);
    }

    pub fn can_undo(&self) -> bool {
        self.history.can_undo()
    }

    pub fn can_redo(&self) -> bool {
        self.history.can_redo()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect_elem() -> (Document, u64) {
        let mut doc = Document::default();
        doc.begin_change();
        let id = doc.add(
            ElementKind::Rect {
                rect: RectF::from_points(P2::new(10.0, 10.0), P2::new(100.0, 60.0)),
            },
            Style::default(),
        );
        (doc, id)
    }

    #[test]
    fn hit_border_not_inside() {
        let (doc, id) = rect_elem();
        let e = doc.get(id).unwrap();
        assert!(e.hit_test(P2::new(10.0, 30.0), 4.0)); // 左边缘
        assert!(!e.hit_test(P2::new(55.0, 35.0), 4.0)); // 中空处
    }

    /// 位图图元（M13）：四角拖拽恒等比（宽高比锁定位图宽高比）、实心命中。
    #[test]
    fn image_element_aspect_lock_and_hit() {
        let img = image::RgbaImage::from_pixel(100, 50, image::Rgba([255, 0, 0, 255]));
        let mut doc = Document::default();
        doc.begin_change();
        let id = doc.add(
            ElementKind::Image {
                rect: RectF::from_points(P2::new(0.0, 0.0), P2::new(100.0, 50.0)),
                rgba: std::sync::Arc::new(img),
            },
            Style::default(),
        );
        let e = doc.get_mut(id).unwrap();
        // 只往右拖 60px（y 不动），等比语义下高度同步增长（50 * 1.6 = 80）
        e.set_control_point(2, P2::new(160.0, 0.0));
        let ElementKind::Image { rect, .. } = &doc.get(id).unwrap().kind else {
            unreachable!()
        };
        assert!((rect.width() - 160.0).abs() < 0.01);
        assert!((rect.height() - 80.0).abs() < 0.01, "高应随宽等比增长");
        // 实心命中（内部可选，与 Rect 的中空语义相反）
        assert!(doc.get(id).unwrap().hit_test(rect.center(), 2.0));
    }

    #[test]
    fn undo_redo_roundtrip() {
        let (mut doc, _) = rect_elem();
        assert_eq!(doc.elements.len(), 1);
        doc.undo();
        assert_eq!(doc.elements.len(), 0);
        doc.redo();
        assert_eq!(doc.elements.len(), 1);
    }

    #[test]
    fn marker_number_recovers_after_undo() {
        let mut doc = Document::default();
        doc.begin_change();
        doc.add(
            ElementKind::Marker {
                center: P2::new(0.0, 0.0),
                number: doc.next_marker_number(),
            },
            Style::default(),
        );
        assert_eq!(doc.next_marker_number(), 2);
        doc.undo();
        assert_eq!(doc.next_marker_number(), 1);
    }

    #[test]
    fn mosaic_hit_covers_outer_cell_band() {
        // 回归：命中阈值原先只有笔刷半径，笔迹外沿约一个格子画得到却选不中
        let mut doc = Document::default();
        doc.begin_change();
        let id = doc.add(
            ElementKind::Mosaic {
                points: vec![P2::new(0.0, 0.0), P2::new(100.0, 0.0)],
            },
            Style::default(),
        );
        let e = doc.get(id).unwrap();
        let (brush, cell) = (e.mosaic_brush(), e.mosaic_cell());
        let tol = 4.0;
        for extra in [cell * 0.5, cell * 0.75, cell] {
            assert!(
                e.hit_test(P2::new(50.0, brush + extra), tol),
                "可见外沿 brush + {extra} 应可命中"
            );
        }
        assert!(e.hit_test(P2::new(0.0, brush + cell), tol));
        assert!(!e.hit_test(P2::new(50.0, brush + cell + tol + 0.1), tol));
    }

    #[test]
    fn control_point_drag_keeps_opposite_corner() {
        let (mut doc, id) = rect_elem();
        let e = doc.get_mut(id).unwrap();
        e.set_control_point(0, P2::new(0.0, 0.0)); // 拖左上角
        assert_eq!(e.bounds().max, P2::new(100.0, 60.0)); // 右下角不动
    }

    #[test]
    fn noop_gesture_leaves_no_undo_step() {
        let (mut doc, id) = rect_elem(); // undo 栈：[空文档]
                                         // 点选手势：push 快照但从未移动 → 松手弹出空快照
        doc.begin_change();
        doc.end_change_if_noop();
        doc.undo(); // 应直接回到空文档，中间没有「无反应」的一步
        assert_eq!(doc.elements.len(), 0);

        // 拖动手势：有实际修改，快照保留
        doc.redo();
        doc.begin_change();
        doc.get_mut(id).unwrap().translate(5.0, 0.0);
        doc.end_change_if_noop();
        assert!(doc.can_undo());
        doc.undo();
        assert_eq!(doc.elements[0].bounds().min.x, 10.0);
    }
}
