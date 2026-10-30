//! 图元：在 `Gfx` 上画一个形状的最小 API。
//!
//! 用自由函数而不是 `Gfx` 的方法，是为了让 `minicard-shell` 只负责「设备与呈像」，
//! 画笔语义（圆角矩形 / 描边 / 圆）留在绘制层。B 段补路径、圆弧、渐变时也落这里。

use windows::core::Result;
use windows::Win32::Graphics::Direct2D::Common::{D2D_RECT_F, D2D1_COLOR_F};
use windows::Win32::Graphics::Direct2D::D2D1_ROUNDED_RECT;

use minicard_shell::gfx::Gfx;

pub fn set_color(g: &Gfx, c: D2D1_COLOR_F) -> Result<()> {
    // ID2D1SolidColorBrush::SetColor 返回 void（不是 HRESULT）
    unsafe { g.brush.SetColor(&c) };
    Ok(())
}

pub fn fill_round(g: &Gfx, rect: D2D_RECT_F, radius: f32) -> Result<()> {
    unsafe {
        g.d2d
            .FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect, radiusX: radius, radiusY: radius }, &g.brush)
    };
    Ok(())
}

pub fn stroke_round(g: &Gfx, rect: D2D_RECT_F, radius: f32, width: f32) -> Result<()> {
    unsafe {
        g.d2d.DrawRoundedRectangle(
            &D2D1_ROUNDED_RECT { rect, radiusX: radius, radiusY: radius },
            &g.brush,
            width,
            None,
        )
    };
    Ok(())
}

pub fn fill_rect(g: &Gfx, rect: D2D_RECT_F) -> Result<()> {
    unsafe { g.d2d.FillRectangle(&rect, &g.brush) };
    Ok(())
}
