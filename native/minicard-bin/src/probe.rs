//! 形状探针：把 B3 的图元各画一个，落成帧缓冲供**逐像素断言**（不靠肉眼）。
//!
//! 版面固定（240×240，等价测试图）：
//! ① 环：圆心 (60,60) r=40、线宽 6、扫 75%（12 点起顺时针）→ 3/4 圈，左上方缺口
//! ② 勾：起点 (120,140)、缩放 4、画到 60%（round cap）
//! ③ 实心圆角矩形：(20,140)-(100,200)、r=12
//! ④ 裁剪：右缘 100 的裁剪盒里画一条超宽文字，右侧越界部分必须没画出来

use windows::Win32::Graphics::Direct2D::Common::{D2D_RECT_F, D2D1_COLOR_F};
use windows::core::Result;

use minicard_draw::paint::{
    fill_round, pop_clip, push_clip, set_color, stroke_path,
};
use minicard_draw::path::Path;
use minicard_draw::text::TextEngine;
use minicard_shell::gfx::Gfx;

const WHITE: D2D1_COLOR_F = D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a: 1.0 };

pub fn shapes(g: &Gfx, te: &TextEngine, w: f32, h: f32) -> Result<()> {
    unsafe {
        g.begin()?;
        g.push_clip()?;

        set_color(g, WHITE)?;

        // ① 环：12 点起顺时针 270°
        let ring = Path::ring_arc(60.0, 60.0, 40.0, 0.75);
        stroke_path(g, &ring, 6.0, true)?;

        // ② 待办勾：画到 60%（dashoffset 等价）
        let check = Path::check_mark(120.0, 140.0, 4.0, 0.6);
        stroke_path(g, &check, 4.0, true)?;

        // ③ 实心圆角矩形
        fill_round(
            g,
            D2D_RECT_F { left: 20.0, top: 140.0, right: 100.0, bottom: 200.0 },
            12.0,
        )?;

        // ④ 裁剪：盒右缘 100，文字自然宽远超它
        let _ = h;
        let clip = D2D_RECT_F { left: 0.0, top: 210.0, right: 100.0, bottom: 240.0 };
        push_clip(g, clip);
        set_color(g, WHITE)?;
        let fmt = te.format_cached(20.0, 24.0, 0)?;
        let lay = te.layout_cached(0, &fmt, "ABCDEFGHIJKLMNOP", 400.0, 24.0, false)?;
        g.d2d.DrawTextLayout(
            windows_numerics::Vector2 { X: 0.0, Y: 212.0 },
            &lay,
            &g.brush,
            windows::Win32::Graphics::Direct2D::D2D1_DRAW_TEXT_OPTIONS_NONE,
        );
        pop_clip(g);

        let _ = w;
        g.pop_clip();
        g.end()?;
    }
    Ok(())
}
