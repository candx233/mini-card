//! 绘制层：与业务无关的可复用件 —— 文本排版、玻璃切片、颜色（oklch）、图元。
//!
//! B 段在这里补场景树与布局（`layout`），M0 验证过的三件（text/glass/color）先搬成 crate。

pub mod color;
pub mod glass;
pub mod layout;
pub mod path;
pub mod paint;
pub mod text;
