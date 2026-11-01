//! Dock 自绘（D0）：玻璃条 + 真图标 + 分组线 + 运行点。
//!
//! 几何口径照抄 `docs/dock-plan.md` 与 `web/dock.html`：
//! * 玻璃条高 = 图标 + 上下各 8；条宽 = n×(图标+12) + 24 + 分组线额外宽度；
//! * 图标中心 x = 18 + 图标/2 + i×(图标+12) + 前面分组线数量×8；
//! * 圆角 20；条底贴窗口底边，窗口顶部 28 是放大余量（D1 起用）。
//! * 圆角由**窗口区域**负责（`Gfx::new(.., radius)`），所以这里不做路径裁剪。
//!
//! 交互（hover 余弦放大、点击、拖整条、气泡）还没接 —— 那是 D1/D2 的事。

use minicard_core::config::Dock;
use minicard_draw::color::Palette;
use minicard_draw::layout::{Rect, TextSpec};
use minicard_draw::paint::{fill_path, fill_round, set_color, stroke_path, stroke_round};
use minicard_draw::path::Path;
use minicard_draw::text::{Mode, TextEngine};
use minicard_shell::gfx::Gfx;
use windows::core::Result;
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::Direct2D::ID2D1Bitmap1;

/// 放大余量（逻辑点）：窗口比玻璃条高这么多，鼠标压上来时图标往外长
pub const HEADROOM: f32 = 28.0;
/// 圆角（dock-plan：比卡片 16 更圆）
pub const RADIUS: f32 = 20.0;

pub struct Tokens {
    pub w: f32,
    /// 窗口高 = 条高 + HEADROOM
    pub h: f32,
    /// 玻璃条高
    pub bar_h: f32,
    pub size: f32,
    pub radius: f32,
    pub pad: f32,
    pub gap: f32,
    /// 条顶 y（= HEADROOM）
    pub bar_top: f32,
}

impl Tokens {
    pub fn dock(d: &Dock) -> Self {
        let bar_h = d.bar_height();
        Self {
            w: d.bar_width(),
            h: bar_h + HEADROOM,
            bar_h,
            size: d.size as f32,
            radius: RADIUS,
            pad: 18.0,
            gap: 12.0,
            bar_top: HEADROOM,
        }
    }
}

/// 第 i 个图标在**条内**的中心 x（与 `Dock::item_center_x` 同口径）
pub fn icon_cx(d: &Dock, t: &Tokens, i: usize) -> f32 {
    t.pad
        + t.size / 2.0
        + i as f32 * (t.size + t.gap)
        + d.dividers_before(i) as f32 * Dock::DIVIDER_EXTRA
}

/// 画整条 Dock。`icons[i]` 为空 = 该条目没有图标（画首字母色块兜底）。
pub fn paint(
    g: &Gfx,
    te: &TextEngine,
    pal: &Palette,
    t: &Tokens,
    dock: &Dock,
    icons: &[Option<&ID2D1Bitmap1>],
    running: &[bool],
    glass: Option<(&ID2D1Bitmap1, D2D_RECT_F)>,
) -> Result<()> {
    let n = dock.items.len();
    let bar = D2D_RECT_F {
        left: 0.0,
        top: t.bar_top,
        right: t.w,
        bottom: t.h,
    };
    unsafe {
        g.begin()?;

        // 玻璃只铺在条上：上面 28px 是放大预留，必须留全透明
        // （否则整块 92px 都像玻璃，条就显得比 WebView 高一截、也更平）
        if let Some((bmp, rect)) = glass {
            g.d2d.PushAxisAlignedClip(
                &bar,
                windows::Win32::Graphics::Direct2D::D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
            );
            g.d2d.DrawBitmap(
                bmp,
                Some(&rect),
                1.0,
                windows::Win32::Graphics::Direct2D::D2D1_INTERPOLATION_MODE_LINEAR,
                None,
                None,
            );
            g.d2d.PopAxisAlignedClip();
        }
        set_color(g, pal.veil)?;
        fill_round(g, bar, t.radius)?;

        for i in 0..n {
            let cx = icon_cx(dock, t, i);
            let cy = t.bar_top + t.bar_h / 2.0;
            let half = t.size / 2.0;

            // 分组线：1px 竖线画在两图标之间。
            // 口径照 `dock.html`：`fg 26%`、高 = 图标×0.62、距条底 8 + 图标×0.19。
            // （早先用了 `pal.border`(10%) —— 在深色条上只有 1 级亮度差，等于看不见）
            if i > 0 && dock.items[i].divider_before {
                let x = cx - half - t.gap / 2.0 - Dock::DIVIDER_EXTRA / 2.0;
                let h = t.size * 0.62;
                let bottom = 8.0 + t.size * 0.19;
                let y2 = t.h - bottom;
                let y1 = y2 - h;
                set_color(g, windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F {
                    a: 0.26,
                    ..pal.fg
                })?;
                let line = Path::polyline(&[(x, y1), (x, y2)]);
                stroke_path(g, &line, 1.0, false)?;
            }

            match icons.get(i).and_then(|o| *o) {
                Some(bmp) => {
                    let (iw, ih) = {
                        let s = bmp.GetPixelSize();
                        (s.width as f32, s.height as f32)
                    };
                    // contain：保持比例塞进 size×size
                    let k = (t.size / iw).min(t.size / ih);
                    let (dw, dh) = (iw * k, ih * k);
                    let dst = D2D_RECT_F {
                        left: cx - dw / 2.0,
                        top: cy - dh / 2.0,
                        right: cx + dw / 2.0,
                        bottom: cy + dh / 2.0,
                    };
                    let src = D2D_RECT_F { left: 0.0, top: 0.0, right: iw, bottom: ih };
                    minicard_draw::image::draw(g, bmp, src, dst)?;
                }
                None => {
                    // 兜底：首字母 + 圆角色块
                    let r = Rect { x: cx - half, y: cy - half, w: t.size, h: t.size };
                    set_color(g, pal.badge_bg)?;
                    fill_round(
                        g,
                        D2D_RECT_F { left: r.x, top: r.y, right: r.x + r.w, bottom: r.y + r.h },
                        10.0,
                    )?;
                    if let Some(it) = dock.items.get(i) {
                        let ch = it.name.chars().next().unwrap_or('?').to_string();
                        let fs = t.size * 0.5;
                        let spec = TextSpec {
                            s: ch,
                            size: fs,
                            line_h: fs,
                            weight: 600,
                            wrap: false,
                            align: 1,
                            tabular: false,
                            spacing_em: 0.0,
                        };
                        let lay = te.layout_spec(&spec, t.size, fs + 8.0, Mode::Clip)?;
                        set_color(g, pal.fg)?;
                        g.d2d.DrawTextLayout(
                            windows_numerics::Vector2 { X: r.x, Y: cy - fs / 2.0 },
                            &lay,
                            &g.brush,
                            windows::Win32::Graphics::Direct2D::D2D1_DRAW_TEXT_OPTIONS_NONE,
                        );
                    }
                }
            }

            // 运行指示点：4×4 圆点，条底往上 5（CSS `bottom:-5px`）
            if running.get(i).copied().unwrap_or(false) {
                let dot = Path::circle(cx, t.h - 5.0, 2.0);
                set_color(g, pal.fg)?;
                fill_path(g, &dot)?;
            }
        }

        // 描边：1px 落整像素
        set_color(g, if g.hover.is_some() { pal.fg30 } else { pal.border })?;
        stroke_round(
            g,
            D2D_RECT_F {
                left: 0.5,
                top: t.bar_top + 0.5,
                right: t.w - 0.5,
                bottom: t.h - 0.5,
            },
            (t.radius - 0.5).max(0.0),
            1.0,
        )?;

        g.end()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use minicard_core::config::DockItem;

    fn dock_of(n: usize, divs: &[usize]) -> Dock {
        let mut d = Dock::default();
        d.size = 48;
        d.items = (0..n)
            .map(|i| DockItem {
                name: format!("应用{i}"),
                path: format!("C:/x/{i}.exe"),
                divider_before: divs.contains(&i),
                ..Default::default()
            })
            .collect();
        d
    }

    /// 绘制用的图标中心必须与 `Dock::item_center_x`（命中/排序同源）逐项相等 ——
    /// 一旦分叉，图标会被画在点击落点之外。
    #[test]
    fn 图标中心与命中口径一致() {
        let d = dock_of(16, &[0, 5, 10]);
        let t = Tokens::dock(&d);
        for i in 0..d.items.len() {
            assert_eq!(icon_cx(&d, &t, i), d.item_center_x(i), "第 {i} 个");
        }
    }

    /// 条宽 = 16×(48+12) + 24 + 2×8；条高 = 48+16；窗口高 = 条高 + HEADROOM
    #[test]
    fn 条尺寸与_dock_一致() {
        let d = dock_of(16, &[0, 5, 10]);
        let t = Tokens::dock(&d);
        assert_eq!(t.w, 1000.0);
        assert_eq!(t.bar_h, 64.0);
        assert_eq!(t.h, 64.0 + HEADROOM);
        assert_eq!(t.w, d.bar_width());
        assert_eq!(t.bar_h, d.bar_height());
    }

    /// 空 Dock 也不能炸：条照样按 24 宽画
    #[test]
    fn 空_dock_不炸() {
        let d = dock_of(0, &[]);
        let t = Tokens::dock(&d);
        assert_eq!(t.w, 24.0);
        // 条高不随条目数收缩（空态也是一条 64 高的空条），窗口高 = 条高 + 预留
        assert_eq!(t.h, d.bar_height() + HEADROOM);
    }
}
