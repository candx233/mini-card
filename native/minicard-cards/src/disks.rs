//! 磁盘卡：`web/card.html` 的 `.card-disk` / `.disk-*` 搬到 D2D。
//!
//! 缩放口径（与 `card.html::applyScale` 同源）：`FIXED_SCALE = 180` —— 磁盘卡高度随之变
//! （1 盘 60 / 2–3 盘 120 / 4–5 盘 180），但**字号钉在 180 档**（k = 180/192 = 0.9375，
//! 根字号 15px）。高度差全部由行距档吸收，否则 60 高时整卡会缩成四成。
//!
//! 行距三档（`renderDisks` 里的 tier）：3 盘 `d3`、4 盘 `mid`、≥5 盘 `d5`。

use windows::core::Result;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::{
    ID2D1Bitmap1, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_INTERPOLATION_MODE_LINEAR,
};
use windows_numerics::Vector2;

use minicard_draw::color::Palette;
use minicard_draw::layout::{layout, Align, Dim, Dir, Justify, Node, Placed, Rect, TextSpec};
use minicard_draw::paint::{fill_round, set_color, stroke_round};
use minicard_draw::text::TextEngine;
use minicard_shell::gfx::Gfx;

pub struct Disk {
    pub name: String,
    pub used: u64,
    pub total: u64,
}

/// 行距档：与 `renderDisks` 的 `tier` 判定同源。
#[derive(Clone, Copy, PartialEq)]
pub enum Tier {
    Base,
    D3,
    Mid,
    D5,
}

impl Tier {
    pub fn for_count(n: usize) -> Self {
        match n {
            3 => Tier::D3,
            4 => Tier::Mid,
            n if n >= 5 => Tier::D5,
            _ => Tier::Base,
        }
    }
}

/// 卡片几何：全部由「窗口尺寸 + 盘数」推出来。
pub struct Tokens {
    pub w: f32,
    pub h: f32,
    pub r: f32,
    pub radius: f32,
    pub pad_lr: f32,
    pub pad_tb: f32,
    pub rows_gap: f32,
    pub rows_pb: f32,
    pub row_gap: f32,
    pub row_pt: f32,
    pub badge_mt: f32,
    pub badge_max: f32,
    pub badge_radius: f32,
    pub dbody_gap: f32,
    pub fs: f32,
    pub line_h: f32,
    pub bar_h: f32,
    pub pct_fs: f32,
}

impl Tokens {
    pub fn disks(w: f32, h: f32, n: usize) -> Self {
        // FIXED_SCALE：k 钉在 180/192，不跟窗口高走
        let k = (180.0 / 192.0f32).clamp(0.4, 2.5);
        let r = 16.0 * k;
        let tier = Tier::for_count(n);
        let (rows_gap, rows_pb, row_gap, row_pt, badge_mt, dbody_gap, fs, lh) = match tier {
            // base：.disk-rows gap .5 / pb .3125；.disk-row gap .6875 / +row padding-top .4375
            Tier::Base => (0.5, 0.3125, 0.6875, 0.4375, 0.39, 0.375, 0.9375, 1.55),
            // d3/d5：gap .1875 / pb .2；row gap .625 / pt .1875；dbody gap .125；字号 1.3
            Tier::D3 | Tier::D5 => (0.1875, 0.2, 0.625, 0.1875, 0.263, 0.125, 0.9375, 1.3),
            // mid：gap .25 / pb .225；row gap .6875 / pt .1875；dbody gap .25；字号 .875, lh 1.5
            Tier::Mid => (0.25, 0.225, 0.6875, 0.1875, 0.305, 0.25, 0.875, 1.5),
        };
        Self {
            w,
            h,
            r,
            radius: 1.0 * r,
            pad_lr: 0.75 * r,
            pad_tb: 0.5 * r,
            rows_gap: rows_gap * r,
            rows_pb: rows_pb * r,
            row_gap: row_gap * r,
            row_pt: row_pt * r,
            badge_mt: badge_mt * r,
            badge_max: 2.25 * r,
            badge_radius: 0.4375 * r,
            dbody_gap: dbody_gap * r,
            fs: fs * r,
            line_h: fs * lh * r,
            bar_h: 0.5 * r,
            pct_fs: fs * r,
        }
    }
}

/// `fmtSize` 的逐条复刻（TB 带一位小数、GB 取整、MB 最小 1）。
pub fn fmt_size(b: u64) -> String {
    const GB: f64 = 1073741824.0;
    const TB: f64 = GB * 1024.0;
    let b = b as f64;
    if b >= TB {
        let tb = b / TB;
        if tb >= 100.0 {
            format!("{} TB", tb.round() as i64)
        } else {
            format!("{:.1} TB", (tb * 10.0).round() / 10.0)
        }
    } else if b >= GB {
        format!("{} GB", (b / GB).round() as i64)
    } else {
        format!("{} MB", ((b / 1048576.0).round()).max(1.0) as i64)
    }
}

pub fn pct_of(d: &Disk) -> f32 {
    if d.total == 0 {
        0.0
    } else {
        (d.used as f64 / d.total as f64 * 100.0).round() as f32
    }
}

pub fn paint(
    g: &Gfx,
    te: &TextEngine,
    pal: &Palette,
    t: &Tokens,
    disks: &[Disk],
    glass: Option<(&ID2D1Bitmap1, D2D_RECT_F)>,
    warn90: bool,
    dim: bool,
) -> Result<()> {
    let tree = tree(t, disks, warn90, dim);
    let mut m: &TextEngine = te;
    let placed = layout(&tree, (t.w, t.h), &mut m);

    unsafe {
        g.begin()?;
        g.push_clip()?;

        if let Some((bmp, rect)) = glass {
            g.d2d.DrawBitmap(
                bmp,
                Some(&rect),
                1.0,
                D2D1_INTERPOLATION_MODE_LINEAR,
                None,
                None,
            );
        }
        let full = D2D_RECT_F {
            left: 0.0,
            top: 0.0,
            right: t.w,
            bottom: t.h,
        };
        set_color(g, pal.veil)?;
        fill_round(g, full, t.radius)?;
        set_color(
            g,
            if g.hover.is_some() {
                pal.fg30
            } else {
                pal.border
            },
        )?;
        stroke_round(
            g,
            D2D_RECT_F {
                left: 0.5,
                top: 0.5,
                right: t.w - 0.5,
                bottom: t.h - 0.5,
            },
            (t.radius - 0.5).max(0.0),
            1.0,
        )?;

        paint_tree(g, te, pal, t, disks, &placed, placed.rect, warn90)?;

        g.pop_clip();
        g.end()?;
        Ok(())
    }
}

/// id 约定：1000+i = 第 i 块的进度条；2000+i = 第 i 块的徽标。
fn tree(t: &Tokens, disks: &[Disk], warn90: bool, dim: bool) -> Node {
    let rows = disks.iter().enumerate().map(|(i, d)| {
        let pct = pct_of(d);
        let warn = warn90 && pct > 90.0;
        let letter = d.name.chars().next().unwrap_or('?').to_string();

        let badge = Node::text(TextSpec {
            s: letter,
            size: 1.0625 * t.r,
            line_h: 1.0625 * t.r, // line-height:1
            weight: 400,
            wrap: false,
            align: 2, // 居中
            tabular: false,
            spacing_em: 0.0,
        })
        .id(2000 + i as u32);

        // 徽标：正方形 + 自身 margin-top（框顶对齐数字墨迹顶）
        let badge_box = Node::new(Dir::Row)
            .id(2500 + i as u32)
            .style(|s| {
                s.aspect(1.0)
                    // CSS 里 `.dbadge{align-self:stretch}` 覆盖了行的 `align-items:center`
                    .self_align(Align::Stretch)
                    .max_cross(t.badge_max)
                    .mar([t.badge_mt, 0.0, 0.0, 0.0])
                    .justify(Justify::Center)
                    .align(Align::Center)
            })
            .child(badge);

        let used = Node::text(TextSpec {
            s: format!("{} / {}", fmt_size(d.used), fmt_size(d.total)),
            size: t.fs,
            line_h: t.line_h,
            weight: 400,
            wrap: false,
            align: 0,
            // 实测 tabular 更接近浏览器（比例数字反而差 13.5 vs 11.6）：
            // CSS 没写 font-variant-numeric，但 MiSans 的默认数字本来就接近等宽
            tabular: true,
            spacing_em: 0.0,
        });
        let pct_txt = Node::text(TextSpec {
            s: format!("{}%", pct),
            size: t.pct_fs,
            line_h: t.line_h,
            weight: if warn { 600 } else { 400 },
            wrap: false,
            align: 1,
            tabular: true,
            spacing_em: 0.0,
        })
        .id(3000 + i as u32);
        let rowline = Node::new(Dir::Row)
            .style(|s| s.align(Align::Baseline).justify(Justify::Between))
            .child(used)
            .child(pct_txt);
        let bar = Node::new(Dir::Row)
            .id(1000 + i as u32)
            .style(|s| s.size(Dim::Pct(1.0), Dim::Px(t.bar_h)));
        let dbody = Node::new(Dir::Column)
            .style(|s| s.grow(1.0).basis(0.0).gap(t.dbody_gap))
            .child(rowline)
            .child(bar);

        Node::new(Dir::Row)
            .style(|s| {
                let mut st = s.align(Align::Center).gap(t.row_gap);
                if i > 0 {
                    st = st.mar([t.row_pt, 0.0, 0.0, 0.0]);
                }
                st
            })
            .child(badge_box)
            .child(dbody)
    });

    Node::new(Dir::Column)
        .style(|s| {
            s.size(Dim::Px(t.w), Dim::Px(t.h))
                .pad([t.pad_tb, t.pad_lr, t.pad_tb, t.pad_lr])
        })
        .child(
            Node::new(Dir::Column)
                .style(|s| {
                    s.grow(1.0)
                        .basis(0.0)
                        .justify(Justify::Center)
                        .gap(t.rows_gap)
                        .pad([0.0, 0.0, if dim { 0.0 } else { t.rows_pb }, 0.0])
                })
                .children_of(rows),
        )
}

/// 给探针用：拿到与 `paint` 同一棵树（避免两处建树漂移）。
pub fn tree_hook(t: &Tokens, disks: &[Disk]) -> Node {
    tree(t, disks, true, false)
}

/// 把排好版的树按行打出来（调几何时用：直接对照浏览器的 getBoundingClientRect）。
pub fn dump_layout(p: &Placed, depth: usize) {
    let pad = " ".repeat(depth * 2);
    let role = if p.text().is_some() {
        format!(
            "text {:?}",
            p.text().map(|s| s.s.clone()).unwrap_or_default()
        )
    } else if (1000..2000).contains(&p.id) {
        "bar".to_string()
    } else if (2500..3000).contains(&p.id) {
        "badge".to_string()
    } else {
        "box".to_string()
    };
    minicard_shell::mlog!(
        "{}id={:<5} {:<28} x={:7.2} y={:7.2} w={:7.2} h={:7.2}",
        pad,
        p.id,
        role,
        p.rect.x,
        p.rect.y,
        p.rect.w,
        p.rect.h
    );
    for c in &p.children {
        dump_layout(c, depth + 1);
    }
}

fn paint_tree(
    g: &Gfx,
    te: &TextEngine,
    pal: &Palette,
    t: &Tokens,
    disks: &[Disk],
    p: &Placed,
    parent: Rect,
    warn90: bool,
) -> Result<()> {
    // 徽标：圆角方框（描边 + 底），文字由子节点正常画
    if (2500..3000).contains(&p.id) {
        set_color(g, pal.badge_bg)?;
        fill_round(g, to_d2d(p.rect), t.badge_radius)?;
        set_color(g, pal.badge_border)?;
        stroke_round(g, to_d2d(p.rect), t.badge_radius, 1.0)?;
    }
    if let Some(spec) = p.text() {
        let right = parent.right().max(p.rect.right());
        let box_w = (right - p.rect.x).max(0.0);
        // 徽标字母在自己的方框里居中：盒子就是父盒（徽标框）
        let (x, w) = if (2000..2500).contains(&p.id) {
            (parent.x, parent.w)
        } else {
            (p.rect.x, box_w)
        };
        let fmt = te.format_cached(spec.size, spec.line_h, spec.align)?;
        let lay = te.layout_cached(
            spec.align,
            &fmt,
            &spec.s,
            w,
            parent.h.max(spec.line_h) + 8.0,
            spec.tabular,
        )?;
        // 徽标字母没有阴影（CSS 只在 .card 上给 text-shadow，理论上所有字都有；
        // 但徽标是 mono 小字，实测与浏览器一致的做法是照画）
        // 硬边副本 → 换成 CSS 的模糊阴影（0 0.833px 1.667px black 45%）
        {
            let mut total = 0.0f32;
            let mut taps = [(0.0f32, 0.0f32, 0.0f32); 9];
            let mut k = 0;
            let sigma = 0.833;
            for i in -1..=1 {
                for j in -1..=1 {
                    let (ox, oy) = (i as f32 * sigma, j as f32 * sigma);
                    let wt = (-(ox * ox + oy * oy) / (2.0 * sigma * sigma)).exp();
                    taps[k] = (ox, oy, wt);
                    total += wt;
                    k += 1;
                }
            }
            for (ox, oy, wt) in taps {
                let a = 0.45 * wt / total;
                set_color(
                    g,
                    D2D1_COLOR_F {
                        r: 0.0,
                        g: 0.0,
                        b: 0.0,
                        a,
                    },
                )?;
                unsafe {
                    g.d2d.DrawTextLayout(
                        Vector2 {
                            X: x + ox,
                            Y: p.rect.y + 0.833 + oy,
                        },
                        &lay,
                        &g.brush,
                        D2D1_DRAW_TEXT_OPTIONS_NONE,
                    );
                }
            }
        }
        let is_badge = (2000..2500).contains(&p.id);
        let warn = if (3000..4000).contains(&p.id) {
            disks
                .get((p.id - 3000) as usize)
                .map(|d| warn90 && pct_of(d) > 90.0)
                .unwrap_or(false)
        } else {
            false
        };
        set_color(
            g,
            if is_badge {
                pal.muted
            } else if warn {
                pal.warn
            } else {
                pal.fg
            },
        )?;
        unsafe {
            g.d2d.DrawTextLayout(
                Vector2 { X: x, Y: p.rect.y },
                &lay,
                &g.brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
            );
        }
    } else if (1000..2000).contains(&p.id) {
        let i = (p.id - 1000) as usize;
        let d = match disks.get(i) {
            Some(d) => d,
            None => return Ok(()),
        };
        let pct = pct_of(d);
        let warn = warn90 && pct > 90.0;
        let r = p.rect.h / 2.0;
        set_color(g, pal.track12)?;
        fill_round(g, to_d2d(p.rect), r)?;
        if pct > 0.0 {
            let w = p.rect.w * (pct / 100.0);
            set_color(g, if warn { pal.warn } else { pal.fill })?;
            fill_round(g, to_d2d(Rect { w, ..p.rect }), r)?;
        }
    }
    for c in &p.children {
        paint_tree(g, te, pal, t, disks, c, p.rect, warn90)?;
    }
    Ok(())
}

fn to_d2d(r: Rect) -> D2D_RECT_F {
    D2D_RECT_F {
        left: r.x,
        top: r.y,
        right: r.right(),
        bottom: r.bottom(),
    }
}
