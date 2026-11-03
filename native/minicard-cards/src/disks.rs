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
use minicard_draw::text::{Mode as TextMode, TextEngine};
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
            weight: 400, // CSS 400 → 字集里的 Regular 面（选面见 TextEngine::css_weight）
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
            weight: 400, // CSS 400
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
        // 用 spec 建布局：缓存键含字重（`layout_cached`/`format_cached` 的口径是固定 400，
        // 而 `.pct` 在告警态是 600 —— 走那条路会把两种字面的布局混用）
        let lay = te.layout_spec(spec, w, parent.h.max(spec.line_h) + 8.0, TextMode::Clip)?;
        // 徽标字母没有阴影（CSS 只在 .card 上给 text-shadow，理论上所有字都有；
        // 但徽标是 mono 小字，实测与浏览器一致的做法是照画）
        // 硬边副本 → 换成 CSS 的模糊阴影（0 0.833px 1.667px black 45%）
        let ty = if (2000..2500).contains(&p.id) {
            // 徽标字母是 grid `place-items:center` 居中的（基线相位与普通行盒不同，实测
            // 不校正就已经对齐），别套一般文字那条 floor-vs-round 校正。
            p.rect.y
        } else {
            snap_text_y(te, spec, p.rect.y)
        };
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
                            Y: ty + 0.833 + oy,
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
                Vector2 { X: x, Y: ty },
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
        // 进度条上下边缘按设备行吸附：浏览器里 `.bar{overflow:hidden}` 的裁剪框是**整像素**的
        // （参考图 y27/36 全底色、y28..35 整行满亮），跟着分数坐标画会在 28/35 各摊半行
        // （实测 175/180 vs 参考 222，每根条两行 × 5 根 = 差 >30 的一千多像素）。
        // 吸附方向是 **round**，不是 floor/ceil：五根条实测 28..35 / 62..68 / 95..101 /
        // 128..135 / 161..168，逐根 = round(top)..round(bottom)（floor/ceil 会让 61.55 那根
        // 多出一行，反而多差 190 px）。
        let bar = snap_bar_rect(p.rect);
        let r = bar.h / 2.0;
        set_color(g, pal.track12)?;
        fill_round(g, to_d2d(bar), r)?;
        if pct > 0.0 {
            let w = bar.w * (pct / 100.0);
            set_color(g, if warn { pal.warn } else { pal.fill })?;
            fill_round(g, to_d2d(Rect { w, ..bar }), r)?;
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

/// 进度条的设备行吸附（浏览器 `.bar` 的 overflow 裁剪框是整像素的）：上下边各四舍五入到
/// 整行。五根条实测 = 28..35 / 62..68 / 95..101 / 128..135 / 161..168。
fn snap_bar_rect(r: Rect) -> Rect {
    let top = r.y.round();
    let bottom = r.bottom().round();
    Rect {
        x: r.x,
        y: top,
        w: r.w,
        h: bottom - top,
    }
}

/// 文字基线的整像素落位：Chrome 把基线**向下取整**（floor），D2D/DWrite 是四舍五入。
/// 于是基线小数部分 ≥0.5 的那些行，自绘整体低 1px（实测 d5 档五行：y 分数 .109/.391 的
/// 三行低 1px、.672/.953 的两行正好，「最佳位移」-1/0 与 floor-vs-round 逐行吻合）。
/// 这里把绘制 y 预挪 `floor(b) − round(b)`，两边就落在同一行。
fn snap_text_y(te: &TextEngine, spec: &TextSpec, y: f32) -> f32 {
    let b = y + te.ascent_of(spec);
    y + (b.floor() - b.round())
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn engine() -> TextEngine {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../assets/fonts");
        let files: Vec<PathBuf> = ["MiSans-Regular.ttf", "MiSans-Medium.ttf", "MiSans-Semibold.ttf"]
            .iter()
            .map(|n| dir.join(n))
            .filter(|p| p.exists())
            .collect();
        let refs: Vec<&std::path::Path> = files.iter().map(|p| p.as_path()).collect();
        TextEngine::new_weights(&refs, "MiSans").expect("字体")
    }

    fn run(s: &str, size: f32, weight: u16) -> f32 {
        let spec = TextSpec {
            s: s.to_string(),
            size,
            line_h: size * 1.3,
            weight,
            wrap: false,
            align: 0,
            tabular: true,
            spacing_em: 0.0,
        };
        engine().measure(&spec, 4096.0).0
    }

    /// `.used` 的数字串宽度必须等于浏览器 `Range` 量出来的 111.234 ——
    /// CSS 400 由 `TextEngine::css_weight` 落到 Regular（内部 OS/2 330）；若又落回
    /// Medium（380）会宽到 112.78（+1.55px），整行数字越往右漂得越多、字形边缘全错位。
    #[test]
    fn 磁盘数字串宽度与浏览器一致() {
        let w = run("279 GB / 466 GB", 14.0625, 400);
        assert!(
            (w - 111.234).abs() < 0.15,
            "「279 GB / 466 GB」宽 {w:.3}，浏览器 111.234"
        );
        // 500（Medium）必须明显更宽：证明两档真是两个字面，不是同一条路
        let w500 = run("279 GB / 466 GB", 14.0625, 500);
        assert!(
            w500 > w + 1.0,
            "500(Medium) 应比 400(Regular) 宽约 1.4%：{w500:.3} vs {w:.3}"
        );
        let p = run("60%", 14.0625, 400);
        assert!((p - 28.226).abs() < 0.15, "「60%」宽 {p:.3}，浏览器 28.226");
    }

    /// 进度条吸附：五根条实测的整行区间（与参考图逐行对过）。
    #[test]
    fn 进度条吸附到设备行() {
        let cases = [
            (28.27f32, 35.77f32, 28.0f32, 36.0f32),
            (61.55, 69.05, 62.0, 69.0),
            (94.83, 102.33, 95.0, 102.0),
            (128.11, 135.61, 128.0, 136.0),
            (161.39, 168.89, 161.0, 169.0),
        ];
        for (top, bottom, want_top, want_bottom) in cases {
            let r = Rect {
                x: 44.34375,
                y: top,
                w: 184.40625,
                h: bottom - top,
            };
            let s = snap_bar_rect(r);
            assert_eq!(s.y, want_top, "{top} 的顶边应吸到 {want_top}");
            assert_eq!(s.bottom(), want_bottom, "{bottom} 的底边应吸到 {want_bottom}");
            assert_eq!(s.x, r.x, "横向不吸附（浏览器只有 overflow 裁剪框吸整行）");
        }
    }

    /// 基线落位：只有小数部分 ≥0.5 的那些行会被挪 1px（实测五行的 y 分数与最佳位移吻合）。
    #[test]
    fn 基线落位只挪半像素以上的相位() {
        let te = engine();
        let spec = TextSpec {
            s: "279 GB / 466 GB".into(),
            size: 14.0625,
            line_h: 18.28125,
            weight: 400,
            wrap: false,
            align: 0,
            tabular: true,
            spacing_em: 0.0,
        };
        for (y, want) in [
            (8.109f32, 7.109f32),
            (41.391, 40.391),
            (74.672, 74.672),
            (107.953, 107.953),
            (141.234, 140.234),
        ] {
            let got = snap_text_y(&te, &spec, y);
            assert!((got - want).abs() < 1e-3, "y={y} → {got:.3}，应为 {want:.3}");
        }
    }
}

