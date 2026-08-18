//! lscreen-core: 图元模型、撤销栈、命中检测、导出渲染。
//! 本 crate 不依赖任何 UI 框架，供 GUI 层与 CLI 无头模式共用。

pub mod color;
pub mod geom;
pub mod history;
pub mod model;
pub mod qr;
pub mod render;

pub use geom::{RectF, P2};
pub use model::{Document, Element, ElementKind, Rgba, Style, Tool};
