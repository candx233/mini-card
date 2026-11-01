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
use minicard_draw::layout::{layout, Dim, Dir, Justify, Node, Placed, Rect, TextSpec};
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
    // ① 场景树 → 布局（几何由 B1 算，不再手写 row_geom 那套公式）
    let tree = tree(t, rows);
    let mut m: &TextEngine = te; // `impl Measurer for &TextEngine`：本地绑定要可变
    let placed = layout(&tree, (t.w, t.h), &mut m);

    unsafe {
        g.begin()?;
        g.push_clip()?;

        // ② 背景：磨砂层 / 白纱 / 描边（是「卡的皮」，不是内容，不进树）
        if let Some((bmp, rect)) = glass {
            g.d2d.DrawBitmap(bmp, Some(&rect), 1.0, D2D1_INTERPOLATION_MODE_LINEAR, None, None);
        }
        let full = D2D_RECT_F { left: 0.0, top: 0.0, right: t.w, bottom: t.h };
        set_color(g, pal.veil)?;
        fill_round(g, full, t.radius)?;
        set_color(g, if g.hover.is_some() { pal.fg30 } else { pal.border })?;
        stroke_round(
            g,
            D2D_RECT_F { left: 0.5, top: 0.5, right: t.w - 0.5, bottom: t.h - 0.5 },
            (t.radius - 0.5).max(0.0),
            1.0,
        )?;

        // ③ 内容：照树画
        paint_tree(g, te, pal, rows, &placed, placed.rect, snap_text)?;

        if pal.debug_alpha {
            set_color(g, D2D1_COLOR_F { r: 1.0, g: 0.0, b: 0.0, a: 0.5 })?;
            fill_round(g, full, t.radius)?;
        }

        g.pop_clip();
        g.end()?;
        Ok(())
    }
}

/// 按卡尺寸建场景树。id 约定：进度条 = 1000 + 行号。
fn tree(t: &Tokens, rows: &[Row]) -> Node {
    let row_nodes = rows.iter().enumerate().map(|(i, r)| {
        let lab = Node::text(text_spec(&r.label, t.lab, t.line_h, 0));
        let pct = Node::text(text_spec(&r.text, t.lab, t.line_h, 1))
            .tabular(true)
            .style(|s| s.grow(1.0));
        let line = Node::new(Dir::Row).child(lab).child(pct);
        let bar = Node::new(Dir::Row)
            .id(1000 + i as u32)
            .style(|s| s.size(Dim::Pct(1.0), Dim::Px(t.bar_h)));
        Node::new(Dir::Column)
            .style(|s| s.grow(1.0).basis(0.0).justify(Justify::Center).gap(t.gap))
            .child(line)
            .child(bar)
    });
    Node::new(Dir::Column)
        .style(|s| {
            s.size(Dim::Px(t.w), Dim::Px(t.h))
                .pad([t.pad_tb, t.pad_lr, t.pad_tb, t.pad_lr])
        })
        .child(
            Node::new(Dir::Column)
                .style(|s| s.grow(1.0).basis(0.0).pad([0.0, 0.0, t.rows_pb, 0.0]))
                .children_of(row_nodes),
        )
}

fn text_spec(s: &str, size: f32, line_h: f32, align: u8) -> TextSpec {
    TextSpec {
        s: s.to_string(),
        size,
        line_h,
        weight: 400,
        wrap: false,
        align,
        tabular: false,
        spacing_em: 0.0,
    }
}

/// 画一棵排好版的树。
///
/// 文字的右边界取**父盒右缘**：CSS 里 `text-align` 相对包含块生效，`.pct` 的右对齐
/// 就是靠这个对上进度条右端的（照自己的盒宽算会短一截 —— 两段文字共享一行时尤其明显）。
fn paint_tree(
    g: &Gfx,
    te: &TextEngine,
    pal: &Palette,
    rows: &[Row],
    p: &Placed,
    parent: Rect,
    snap_text: bool,
) -> Result<()> {
    if let Some(spec) = p.text() {
        let right = parent.right().max(p.rect.right());
        let box_w = (right - p.rect.x).max(0.0);
        let mut y = p.rect.y;
        if snap_text {
            y = y.round();
        }
        let fmt = te.format_cached(spec.size, spec.line_h, spec.align)?;
        let lay = te.layout_cached(spec.align, &fmt, &spec.s, box_w, spec.line_h + 8.0, spec.tabular)?;
        set_color(g, pal.shadow)?;
        unsafe {
            g.d2d.DrawTextLayout(
                Vector2 { X: p.rect.x, Y: y + 0.833 },
                &lay,
                &g.brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
            );
            set_color(g, pal.fg)?;
            g.d2d.DrawTextLayout(
                Vector2 { X: p.rect.x, Y: y },
                &lay,
                &g.brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
            );
        }
    } else if p.id >= 1000 {
        let i = (p.id - 1000) as usize;
        let r = p.rect.h / 2.0;
        set_color(g, pal.track)?;
        fill_round(g, to_d2d(p.rect), r)?;
        if let Some(pct) = rows.get(i).and_then(|row| row.pct) {
            let w = p.rect.w * (pct.clamp(0.0, 100.0) / 100.0);
            if w > 0.0 {
                set_color(g, pal.fill)?;
                fill_round(g, to_d2d(Rect { w, ..p.rect }), r)?;
            }
        }
    }
    for c in &p.children {
        paint_tree(g, te, pal, rows, c, p.rect, snap_text)?;
    }
    Ok(())
}

fn to_d2d(r: Rect) -> D2D_RECT_F {
    D2D_RECT_F { left: r.x, top: r.y, right: r.right(), bottom: r.bottom() }
}
