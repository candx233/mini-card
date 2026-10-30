//! 性能卡视图：把 `web/card.html` 的 CSS 数值逐条搬成 D2D 绘制（rem → 设计 px → k 缩放）。
//!
//! 映射关系（card.html 原文 → 这里）：
//!   .card-perf { padding: .4375rem 1rem }        → pad_tb / pad_lr
//!   .perf-rows { flex:1; justify-content:center } → rows_top / rows_h（等分 3 行，行内居中）
//!   .perf-rows:not(.dim){ padding-bottom:.2rem } → rows_pb（光学补偿，旧值一律重标）
//!   .perf-row { gap:.25rem; justify-content:center } → gap
//!   .perf-rows .lab/.pct { font-size:1.3125rem; line-height:1.15 }
//!   .perf-rows .bar { height:.9375rem; border-radius:999px }
//!   .bar 底 = fg 25% / 填充 = fg 90%（SVG 药丸，亚像素连续）
//!   .card { border-radius:1rem; text-shadow:0 .833px 1.667px #000/45% }

use windows::core::Result;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows_numerics::Vector2;

use minicard_draw::color::Palette;
use minicard_draw::paint::{fill_round, set_color, stroke_round};
use minicard_draw::text::TextEngine;
use minicard_shell::gfx::Gfx;

pub struct Row {
    pub label: String,
    pub text: String,
    /// None = 没有进度条（GPU「未检测到」那一档）
    pub pct: Option<f32>,
}

/// 卡片几何：全部由「窗口尺寸 + 该卡 BASE」推出来，与 card.html 的 applyScale 同源。
pub struct Tokens {
    pub w: f32,
    pub h: f32,
    #[allow(dead_code)]
    pub k: f32,      // 缩放比
    #[allow(dead_code)]
    pub r: f32,      // 根字号 = 16k（CSS 里 1rem）
    pub radius: f32, // border-radius: 1rem
    pub pad_lr: f32,
    pub pad_tb: f32,
    pub rows_pb: f32,
    pub gap: f32,
    pub lab: f32,
    pub line_h: f32,
    pub bar_h: f32,
}

impl Tokens {
    /// 性能卡是 HEIGHT_ONLY：k = h / 192（BASE = 320×192），宽度差全部由进度条吃掉。
    pub fn perf(w: f32, h: f32) -> Self {
        let k = (h / 192.0).clamp(0.4, 2.5);
        let r = 16.0 * k;
        Self {
            w,
            h,
            k,
            r,
            radius: 1.0 * r,
            pad_lr: 1.0 * r,
            pad_tb: 0.4375 * r,
            rows_pb: 0.2 * r,
            gap: 0.25 * r,
            lab: 1.3125 * r,
            line_h: 1.3125 * r * 1.15,
            bar_h: 0.9375 * r,
        }
    }

    pub fn bar_w(&self) -> f32 {
        self.w - 2.0 * self.pad_lr
    }

    /// 第 i 行（共 n 行）的几何 → (行盒顶 y, 条顶 y)
    pub fn row_geom(&self, n: usize, i: usize) -> (f32, f32) {
        let rows_h = self.h - 2.0 * self.pad_tb - self.rows_pb;
        let row_h = rows_h / n as f32;
        let content_h = self.line_h + self.gap + self.bar_h;
        let y0 = self.pad_tb + row_h * i as f32 + (row_h - content_h) / 2.0;
        (y0, y0 + self.line_h + self.gap)
    }
}

#[allow(clippy::too_many_arguments)]
pub fn paint(
    g: &Gfx,
    te: &TextEngine,
    pal: &Palette,
    t: &Tokens,
    rows: &[Row],
    glass: Option<(&ID2D1Bitmap1, D2D_RECT_F)>,
    snap_text: bool,
) -> Result<()> {
    unsafe {
        g.begin()?;
        g.push_clip()?;

        // ① 磨砂层（壁纸切片，已模糊）
        if let Some((bmp, rect)) = glass {
            g.d2d.DrawBitmap(bmp, Some(&rect), 1.0, D2D1_INTERPOLATION_MODE_LINEAR, None, None);
        }

        // ② 白纱 + 描边（.veil）
        let full = D2D_RECT_F { left: 0.0, top: 0.0, right: t.w, bottom: t.h };
        set_color(g, pal.veil)?;
        fill_round(g, full, t.radius)?;
        set_color(g, pal.border)?;
        stroke_round(g, 
            D2D_RECT_F { left: 0.5, top: 0.5, right: t.w - 0.5, bottom: t.h - 0.5 },
            (t.radius - 0.5).max(0.0),
            1.0,
        )?;

        // ③ 三行
        let fmt_lab = te.format_cached(t.lab, t.line_h, 0)?;
        let fmt_pct = te.format_cached(t.lab, t.line_h, 1)?;
        let bw = t.bar_w();
        for (i, row) in rows.iter().enumerate() {
            let (mut line_top, bar_top) = t.row_geom(rows.len(), i);
            if snap_text {
                line_top = line_top.round();
            }

            // 文字阴影：CSS text-shadow 0 .833px 1.667px #000/45%（偏移照搬；模糊省到 M1）
            let lay = te.layout_cached(0, &fmt_lab, &row.label, bw, t.line_h + 8.0, false)?;
            set_color(g, pal.shadow)?;
            g.d2d.DrawTextLayout(Vector2 { X: t.pad_lr, Y: line_top + 0.833 }, &lay, &g.brush, D2D1_DRAW_TEXT_OPTIONS_NONE);

            let layp = te.layout_cached(1, &fmt_pct, &row.text, bw, t.line_h + 8.0, true)?;
            g.d2d.DrawTextLayout(Vector2 { X: t.pad_lr, Y: line_top + 0.833 }, &layp, &g.brush, D2D1_DRAW_TEXT_OPTIONS_NONE);

            set_color(g, pal.fg)?;
            g.d2d.DrawTextLayout(Vector2 { X: t.pad_lr, Y: line_top }, &lay, &g.brush, D2D1_DRAW_TEXT_OPTIONS_NONE);
            g.d2d.DrawTextLayout(Vector2 { X: t.pad_lr, Y: line_top }, &layp, &g.brush, D2D1_DRAW_TEXT_OPTIONS_NONE);

            // 进度条：底轨（fg 25%）+ 填充（fg 90%），药丸
            let r = t.bar_h / 2.0;
            set_color(g, pal.track)?;
            fill_round(g, D2D_RECT_F { left: t.pad_lr, top: bar_top, right: t.pad_lr + bw, bottom: bar_top + t.bar_h }, r)?;
            if let Some(p) = row.pct {
                let fw = bw * (p.clamp(0.0, 100.0) / 100.0);
                if fw > 0.0 {
                    set_color(g, pal.fill)?;
                    fill_round(g, 
                        D2D_RECT_F { left: t.pad_lr, top: bar_top, right: t.pad_lr + fw, bottom: bar_top + t.bar_h },
                        r,
                    )?;
                }
            }
        }

        // 诊断：半透明红块压满卡面。用于判定 DComp 合成是否真的走逐像素 alpha
        if pal.debug_alpha {
            set_color(g, D2D1_COLOR_F { r: 1.0, g: 0.0, b: 0.0, a: 0.5 })?;
            fill_round(g, full, t.radius)?;
        }

        g.pop_clip();
        g.end()?;
        Ok(())
    }
}
