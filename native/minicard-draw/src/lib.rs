//! 绘制层：与业务无关的可复用件 —— 文本排版、玻璃切片、颜色（oklch）、图元。
//!
//! B 段会在这里补上场景树与布局；现在先把 M0 里验证过的三件搬成 crate。

pub mod color;
pub mod glass;
pub mod paint;
pub mod text;
