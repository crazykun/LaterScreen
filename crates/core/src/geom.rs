//! 基础几何类型。图元坐标一律使用「截图图像像素坐标系」，
//! 与屏幕逻辑坐标的换算（DPI 缩放）由 UI 层负责。

#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct P2 {
    pub x: f32,
    pub y: f32,
}

impl P2 {
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }

    pub fn dist(self, other: P2) -> f32 {
        ((self.x - other.x).powi(2) + (self.y - other.y).powi(2)).sqrt()
    }

    pub fn offset(self, dx: f32, dy: f32) -> P2 {
        P2::new(self.x + dx, self.y + dy)
    }
}

#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct RectF {
    pub min: P2,
    pub max: P2,
}

impl RectF {
    /// 由任意两个对角点构造，自动归一化 min/max。
    pub fn from_points(a: P2, b: P2) -> Self {
        Self {
            min: P2::new(a.x.min(b.x), a.y.min(b.y)),
            max: P2::new(a.x.max(b.x), a.y.max(b.y)),
        }
    }

    pub fn width(&self) -> f32 {
        self.max.x - self.min.x
    }

    pub fn height(&self) -> f32 {
        self.max.y - self.min.y
    }

    pub fn center(&self) -> P2 {
        P2::new((self.min.x + self.max.x) / 2.0, (self.min.y + self.max.y) / 2.0)
    }

    pub fn contains(&self, p: P2) -> bool {
        p.x >= self.min.x && p.x <= self.max.x && p.y >= self.min.y && p.y <= self.max.y
    }

    pub fn expand(&self, d: f32) -> RectF {
        RectF {
            min: self.min.offset(-d, -d),
            max: self.max.offset(d, d),
        }
    }

    pub fn translate(&self, dx: f32, dy: f32) -> RectF {
        RectF {
            min: self.min.offset(dx, dy),
            max: self.max.offset(dx, dy),
        }
    }

    /// 四角：左上、右上、右下、左下。
    pub fn corners(&self) -> [P2; 4] {
        [
            self.min,
            P2::new(self.max.x, self.min.y),
            self.max,
            P2::new(self.min.x, self.max.y),
        ]
    }

    pub fn union(&self, other: &RectF) -> RectF {
        RectF {
            min: P2::new(self.min.x.min(other.min.x), self.min.y.min(other.min.y)),
            max: P2::new(self.max.x.max(other.max.x), self.max.y.max(other.max.y)),
        }
    }
}

/// 点 p 到线段 ab 的距离。
pub fn dist_to_segment(p: P2, a: P2, b: P2) -> f32 {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len2 = dx * dx + dy * dy;
    if len2 <= f32::EPSILON {
        return p.dist(a);
    }
    let t = (((p.x - a.x) * dx + (p.y - a.y) * dy) / len2).clamp(0.0, 1.0);
    p.dist(P2::new(a.x + t * dx, a.y + t * dy))
}

/// 点到折线的最小距离；折线不足两点时退化为点距。
pub fn dist_to_polyline(p: P2, pts: &[P2]) -> f32 {
    match pts {
        [] => f32::INFINITY,
        [a] => p.dist(*a),
        _ => pts
            .windows(2)
            .map(|w| dist_to_segment(p, w[0], w[1]))
            .fold(f32::INFINITY, f32::min),
    }
}
