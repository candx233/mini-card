//! 天气卡：`web/card.html` 的 `.card-weather` / `.w-*` 搬到 D2D（320×240 基准）。
//!
//! 三块：城市 + 大温度 + 当前图标（左右分置）· 三日预报（三列，上下各一条 1px 分隔线）·
//! 底行空气质量 / 湿度。图标是 `card.html` 里那 7 个 WMO 小图，路径原文搬过来走
//! `Path::svg`（含 SVG 端点式圆弧的换算），不是切图。

use windows::core::Result;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::{
    ID2D1Bitmap1, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_INTERPOLATION_MODE_LINEAR,
};
use windows_numerics::Vector2;

use minicard_draw::color::Palette;
use minicard_draw::layout::{layout, Align, Dim, Dir, Justify, Node, Placed, TextSpec};
use minicard_draw::paint::{fill_rect, fill_round, set_color, stroke_path, stroke_round};
use minicard_draw::path::Path;
use minicard_draw::text::{Mode as TextMode, TextEngine};
use minicard_shell::gfx::Gfx;

#[derive(Clone, Debug)]
pub struct Day {
    pub label: String,
    pub hi: Option<f32>,
    pub lo: Option<f32>,
    pub code: u8,
}

#[derive(Clone, Debug)]
pub struct Weather {
    pub city: String,
    pub temp: Option<f32>,
    pub code: u8,
    pub days: Vec<Day>,
    pub air: Option<String>,
    pub humidity: Option<i32>,
}

/// WMO 天气码 → 图标名（与 `wmoIcon` 一致）
pub fn icon_of(code: u8) -> &'static str {
    match code {
        0 | 1 => "sun",
        2 => "cloudSun",
        3 | 45 | 48 => "cloud",
        51..=57 => "drizzle",
        61..=67 | 80..=82 => "rain",
        71..=77 | 85 | 86 => "snow",
        c if c >= 95 => "thunder",
        _ => "cloud",
    }
}

/// 图标：路径原文 + `viewBox` 偏移（原版用平移 viewBox 把墨迹摆到画框正中）
fn icon_paths(name: &str) -> (&'static str, (f32, f32), [(f32, f32, f32); 3], usize) {
    // (path, viewBox 偏移, 圆 [cx,cy,r] 最多三个, 圆个数)
    match name {
        "sun" => (
            "M12 3.4v1.8M12 18.8v1.8M3.4 12h1.8M18.8 12h1.8M6 6l1.3 1.3M16.7 16.7L18 18M18 6l-1.3 1.3M7.3 16.7L6 18",
            (0.0, 0.0),
            [(12.0, 12.0, 4.5), (0.0, 0.0, 0.0), (0.0, 0.0, 0.0)],
            1,
        ),
        "cloudSun" => (
            // 三段：太阳弧 / 光芒 / **云体**（第三段曾经漏掉，图标只剩一小块，实测 7×6px）
            "M12.23 6.74A2.9 2.9 0 1 1 16.8 9.87M15 3v1.4M11.8 4.4l1 1M18.2 4.4l-1 1M19.6 7.6h-1.4M7.5 18h9a3.7 3.7 0 0 0 .6-7.35A5.5 5.5 0 0 0 7 9.6 4.2 4.2 0 0 0 7.5 18z",
            (-0.38, -1.5),
            [(0.0, 0.0, 0.0), (0.0, 0.0, 0.0), (0.0, 0.0, 0.0)],
            0,
        ),
        "cloud" => (
            "M7.5 18h9a3.7 3.7 0 0 0 .6-7.35A5.5 5.5 0 0 0 7 9.6 4.2 4.2 0 0 0 7.5 18z",
            (-0.38, 0.37),
            [(0.0, 0.0, 0.0), (0.0, 0.0, 0.0), (0.0, 0.0, 0.0)],
            0,
        ),
        "rain" => (
            "M7.5 15.5h9a3.7 3.7 0 0 0 .6-7.35A5.5 5.5 0 0 0 7 7.1 4.2 4.2 0 0 0 7.5 15.5zM8.4 18.5l-.8 2M12.4 18.5l-.8 2M16.4 18.5l-.8 2",
            (-0.38, 0.36),
            [(0.0, 0.0, 0.0), (0.0, 0.0, 0.0), (0.0, 0.0, 0.0)],
            0,
        ),
        "drizzle" => (
            "M7.5 15.5h9a3.7 3.7 0 0 0 .6-7.35A5.5 5.5 0 0 0 7 7.1 4.2 4.2 0 0 0 7.5 15.5zM10.25 19l-.5 1.4M14.25 19l-.5 1.4",
            (-0.38, 0.31),
            [(0.0, 0.0, 0.0), (0.0, 0.0, 0.0), (0.0, 0.0, 0.0)],
            0,
        ),
        "snow" => (
            "M7.5 15.5h9a3.7 3.7 0 0 0 .6-7.35A5.5 5.5 0 0 0 7 7.1 4.2 4.2 0 0 0 7.5 15.5z",
            (-0.38, 0.51),
            [(8.4, 19.8, 1.0), (12.0, 19.8, 1.0), (15.6, 19.8, 1.0)],
            3,
        ),
        _ => (
            "M7.5 15.5h9a3.7 3.7 0 0 0 .6-7.35A5.5 5.5 0 0 0 7 7.1 4.2 4.2 0 0 0 7.5 15.5zM12.8 17.4l-2 3.1h1.9l-.8 2.3",
            (-0.38, 1.51),
            [(0.0, 0.0, 0.0), (0.0, 0.0, 0.0), (0.0, 0.0, 0.0)],
            0,
        ),
    }
}

pub struct Tokens {
    pub w: f32,
    pub h: f32,
    pub r: f32,
    pub radius: f32,
    pub pad_v: f32,
    pub pad_h: f32,
    pub stack_pt: f32,
    pub top_gap: f32,
    pub city_fs: f32,
    pub city_lh: f32,
    pub city_mb: f32,
    pub temp_fs: f32,
    pub temp_lh: f32,
    pub sun_sz: f32,
    pub sun_stroke: f32,
    pub d3_gap: f32,
    pub d3_mt: f32,
    pub d3_pv: f32,
    pub day_fs: f32,
    pub day_lh: f32,
    pub day_gap: f32,
    pub day_ic: f32,
    pub day_stroke: f32,
    pub day_ic_mt: f32,
    pub day_ic_mb: f32,
    pub t_fs: f32,
    pub air_mt: f32,
    pub air_fs: f32,
    pub air_lh: f32,
}

impl Tokens {
    pub fn weather(w: f32, h: f32) -> Self {
        let k = (w / 320.0).min(h / 240.0).clamp(0.4, 2.5);
        let r = 16.0 * k;
        Self {
            w,
            h,
            r,
            radius: 1.0 * r,
            pad_v: 0.625 * r,
            pad_h: 1.0 * r,
            stack_pt: 0.0625 * r, // 实测：0.1667rem 会把内容压下去 1px（与 604 行的 padding-block 冲突，按实测取 1px）
            top_gap: 0.75 * r,
            city_fs: 1.25 * r,
            city_lh: 1.25 * r * 1.15,
            city_mb: 0.25 * r,
            temp_fs: 4.25 * r,
            temp_lh: 4.25 * r * 0.92,
            sun_sz: 3.375 * r,
            sun_stroke: 1.5,
            d3_gap: 0.5 * r,
            d3_mt: 0.4375 * r,
            d3_pv: 0.5625 * r,
            day_fs: 0.84375 * r,
            day_lh: 0.84375 * r * 1.15,
            day_gap: 0.34375 * r,
            day_ic: 1.5625 * r,
            day_stroke: 1.6,
            day_ic_mt: -0.02 * r,
            day_ic_mb: 0.02 * r,
            t_fs: 0.90625 * r,
            air_mt: 0.4375 * r,
            air_fs: 1.0625 * r,
            air_lh: 1.0625 * r * 1.55, // .w-air 没写 line-height，实测行盒 26.34（17px × 1.55）
        }
    }
}

pub fn paint(
    g: &Gfx,
    te: &TextEngine,
    pal: &Palette,
    t: &Tokens,
    w: &Weather,
    glass: Option<(&ID2D1Bitmap1, D2D_RECT_F)>,
) -> Result<()> {
    let tree = tree(t, w);
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
        paint_tree(g, te, pal, t, w, &placed)?;
        g.pop_clip();
        g.end()?;
        Ok(())
    }
}

fn text_node(s: &str, size: f32, lh: f32, weight: u16) -> Node {
    Node::text(TextSpec {
        s: s.to_string(),
        size,
        line_h: lh,
        weight,
        wrap: false,
        align: 0,
        tabular: true,
        spacing_em: 0.0,
    })
}

fn deg(v: Option<f32>) -> String {
    match v {
        Some(x) if x.is_finite() => format!("{}°", x.round() as i32),
        _ => "--".to_string(),
    }
}

fn tree(t: &Tokens, w: &Weather) -> Node {
    // 顶部：左（城市 + 大温度）右（图标）
    let left = Node::new(Dir::Column)
        .style(|s| s.align(Align::Start))
        .child(
            text_node(&w.city, t.city_fs, t.city_lh, 400)
                .id(8001)
                .style(|s| s.mar([0.0, 0.0, t.city_mb, 0.0])),
        )
        .child(
            text_node(&deg(w.temp), t.temp_fs, t.temp_lh, 500)
                .id(8002)
                .style(|s| s.size(Dim::Auto, Dim::Px(t.temp_lh))),
        );
    let top = Node::new(Dir::Row)
        .style(move |s| {
            s.justify(Justify::Between)
                .align(Align::Center)
                .gap(t.top_gap)
                .size(Dim::Pct(1.0), Dim::Auto)
        })
        .child(left)
        .child(
            Node::new(Dir::Row)
                .id(8010)
                .style(move |s| s.size(Dim::Px(t.sun_sz), Dim::Px(t.sun_sz))),
        );

    // 三日预报：三列等分
    let mut d3 = Node::new(Dir::Row).style(move |s| {
        s.size(Dim::Pct(1.0), Dim::Auto)
            .gap(t.d3_gap)
            .align(Align::Stretch)
    });
    for i in 0..3u32 {
        let d = w.days.get(i as usize);
        let label = d.map(|x| x.label.clone()).unwrap_or_default();
        let mut col = Node::new(Dir::Column)
            .id(8100 + i)
            .style(move |s| s.grow(1.0).basis(0.0).align(Align::Center).gap(t.day_gap))
            .child(text_node(&label, t.day_fs, t.day_lh, 400).id(8110 + i))
            .child(Node::new(Dir::Row).id(8120 + i).style(move |s| {
                s.size(Dim::Px(t.day_ic), Dim::Px(t.day_ic)).mar([
                    t.day_ic_mt,
                    0.0,
                    t.day_ic_mb,
                    0.0,
                ])
            }));
        // 温度行：hi(白) / sep(灰) / lo(灰)，基线对齐
        let (hi, lo) = match d {
            Some(x) => (deg(x.hi), deg(x.lo)),
            None => ("--".to_string(), "--".to_string()),
        };
        col = col.child(
            Node::new(Dir::Row)
                .style(move |s| s.align(Align::Baseline))
                .child(text_node(&hi, t.t_fs, t.t_fs, 400).id(8130 + i))
                .child(text_node("/", t.t_fs, t.t_fs, 400).id(8140 + i))
                .child(text_node(&lo, t.t_fs, t.t_fs, 400).id(8150 + i)),
        );
        d3 = d3.child(col);
    }

    // 底行：空气质量 / 湿度
    let air = match (&w.air, w.humidity) {
        (Some(a), _) => format!("空气质量 {a}"),
        (None, _) => "空气质量 --".to_string(),
    };
    let hum = match w.humidity {
        Some(h) if h >= 0 => format!("湿度 {h}%"),
        _ => "湿度 --".to_string(),
    };
    let air_row = Node::new(Dir::Row)
        .style(move |s| {
            s.justify(Justify::Between)
                .align(Align::Baseline)
                .gap(t.air_mt)
                .size(Dim::Pct(1.0), Dim::Px(t.air_lh))
        })
        .child(text_node(&air, t.air_fs, t.air_lh, 400).id(8200))
        .child(text_node(&hum, t.air_fs, t.air_lh, 400).id(8201));

    Node::new(Dir::Column)
        .style(move |s| {
            s.size(Dim::Px(t.w), Dim::Px(t.h))
                .pad([t.pad_v, t.pad_h, t.pad_v, t.pad_h])
        })
        .child(
            Node::new(Dir::Column)
                .style(move |s| {
                    s.grow(1.0)
                        .basis(0.0)
                        .justify(Justify::Center)
                        .align(Align::Stretch)
                        // `.w-stack` 的 padding 是 2.667px 0 **1px** —— 下边距那 1px 漏了的话，
                        // 居中时整栈会低 0.5px（w-top 12.88 → 13.37）
                        .pad([t.stack_pt, 0.0, 0.0625 * t.r, 0.0])
                })
                .child(top)
                .child(
                    Node::new(Dir::Row)
                        .id(8020)
                        .style(move |s| {
                            s.size(Dim::Pct(1.0), Dim::Auto)
                                .pad([t.d3_pv + 1.0, 0.0, t.d3_pv + 1.0, 0.0])
                                .mar([t.d3_mt, 0.0, 0.0, 0.0])
                        })
                        .child(d3),
                )
                .child(
                    Node::new(Dir::Row)
                        .style(move |s| {
                            s.size(Dim::Pct(1.0), Dim::Px(t.air_lh))
                                .mar([t.air_mt, 0.0, 0.0, 0.0])
                        })
                        .child(air_row),
                ),
        )
}

/// 画一个 24 网格的图标：`size` 是边长，`off` 是原版对 viewBox 的平移
fn draw_icon(
    g: &Gfx,
    pal: &Palette,
    name: &str,
    x: f32,
    y: f32,
    size: f32,
    stroke: f32,
    opacity: f32,
) -> Result<()> {
    let (d, (ox, oy), circles, nc) = icon_paths(name);
    let s = size / 24.0;
    // viewBox 平移 (ox, oy) 等价于把内容反向平移 (-ox, -oy)
    let tx = -ox;
    let ty = -oy;
    let to = |px: f32, py: f32| (x + (px + tx) * s, y + (py + ty) * s);

    // 先画圆（sun 的太阳本体 / snow 的三粒雪）
    for c in circles.iter().take(nc) {
        let p = Path::circle(to(c.0, c.1).0, to(c.0, c.1).1, c.2 * s);
        set_color(g, pal.fg)?;
        stroke_path(g, &p, stroke * s, false)?;
    }
    // 再把 `M` 起头的路径按子路径分组（SVG 的 M 也会开新子路径）
    for seg in d.split('M').filter(|s| !s.is_empty()) {
        let p = Path::svg(&format!("M{seg}"));
        set_color(g, pal.fg)?;
        stroke_path(g, &p.scale(x + tx * s, y + ty * s, s), stroke * s, false)?;
    }
    let _ = opacity;
    Ok(())
}

fn paint_tree(
    g: &Gfx,
    te: &TextEngine,
    pal: &Palette,
    t: &Tokens,
    w: &Weather,
    pl: &Placed,
) -> Result<()> {
    // 当前天气大图标
    if pl.id == 8010 {
        draw_icon(
            g,
            pal,
            icon_of(w.code),
            pl.rect.x,
            pl.rect.y,
            t.sun_sz,
            t.sun_stroke,
            0.9,
        )?;
        return Ok(());
    }
    // 三日小图标
    if (8120..8130).contains(&pl.id) {
        let i = (pl.id - 8120) as usize;
        let code = w.days.get(i).map(|d| d.code).unwrap_or(3);
        draw_icon(
            g,
            pal,
            icon_of(code),
            pl.rect.x,
            pl.rect.y,
            t.day_ic,
            t.day_stroke,
            1.0,
        )?;
        return Ok(());
    }
    // 三日块的两条分隔线
    if pl.id == 8020 {
        // 1px 分隔线吸附整行（CSS 同样会吸附；跟着分数坐标画会摊成两行各半）
        let r = pl.rect;
        // 向下取整：浏览器把 109.42 处的 1px 边框画在第 109 行（不是 110）
        let top = r.y.floor();
        let bottom = r.bottom().floor() - 1.0;
        set_color(
            g,
            if g.hover.is_some() {
                pal.fg30
            } else {
                pal.border
            },
        )?;
        fill_rect(
            g,
            D2D_RECT_F {
                left: r.x,
                top,
                right: r.right(),
                bottom: top + 1.0,
            },
        )?;
        fill_rect(
            g,
            D2D_RECT_F {
                left: r.x,
                top: bottom,
                right: r.right(),
                bottom: bottom + 1.0,
            },
        )?;
    }

    if let Some(spec) = pl.text() {
        let color = match pl.id {
            8002 => pal.fg,
            8001 | 8140 | 8150 | 8200 | 8201 => pal.muted,
            8130..=8139 => pal.fg,
            _ => pal.muted,
        };
        set_color(g, color)?;
        let lay = te.layout_spec(spec, pl.rect.w, spec.line_h + 8.0, TextMode::Clip)?;
        let dx = if (8130..8160).contains(&pl.id) {
            // hi/sep/lo 三个节点在行里是按各自宽度排的，schema 里给的是自动宽
            0.0
        } else {
            0.0
        };
        unsafe {
            g.d2d.DrawTextLayout(
                Vector2 {
                    X: pl.rect.x + dx,
                    Y: pl.rect.y,
                },
                &lay,
                &g.brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
            );
        }
    }
    for c in &pl.children {
        paint_tree(g, te, pal, t, w, c)?;
    }
    Ok(())
}

pub fn dump_layout(p: &Placed, depth: usize) {
    let pad = " ".repeat(depth * 2);
    let role = if let Some(s) = p.text() {
        format!("text {:?}", s.s)
    } else {
        "box".to_string()
    };
    minicard_shell::mlog!(
        "{}id={:<5} {:<22} x={:7.2} y={:7.2} w={:7.2} h={:7.2}",
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

pub fn tree_hook(t: &Tokens, w: &Weather) -> Node {
    tree(t, w)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 天气码到图标的分组() {
        assert_eq!(icon_of(0), "sun");
        assert_eq!(icon_of(1), "sun");
        assert_eq!(icon_of(2), "cloudSun");
        assert_eq!(icon_of(3), "cloud");
        assert_eq!(icon_of(45), "cloud");
        assert_eq!(icon_of(53), "drizzle");
        assert_eq!(icon_of(61), "rain");
        assert_eq!(icon_of(82), "rain");
        assert_eq!(icon_of(73), "snow");
        assert_eq!(icon_of(86), "snow");
        assert_eq!(icon_of(96), "thunder");
        // 未知码回落成云
        assert_eq!(icon_of(99), "thunder");
        assert_eq!(icon_of(30), "cloud");
    }

    #[test]
    fn 温度取整与缺省() {
        assert_eq!(deg(Some(24.6)), "25°");
        assert_eq!(deg(Some(-3.4)), "-3°");
        assert_eq!(deg(None), "--");
        assert_eq!(deg(Some(f32::NAN)), "--");
    }
}

#[cfg(test)]
mod icon_tests {
    use minicard_draw::path::Path;

    /// 七个 WMO 图标的路径必须真能解析出足够多的线段 ——
    /// 之前 cloudSun 只画出 7×6 px 的一小块，就是弧段没被解析出来。
    #[test]
    fn 七个图标都能解析出形体() {
        for name in [
            "sun", "cloudSun", "cloud", "drizzle", "rain", "snow", "thunder",
        ] {
            let (d, _, circles, nc) = super::icon_paths(name);
            let mut total = 0usize;
            for seg in d.split('M').filter(|s| !s.is_empty()) {
                let p = Path::svg(&format!("M{seg}"));
                assert!(!p.cmds.is_empty(), "图标 {name} 的子路径 {seg:?} 解析为空");
                total += p.cmds.len();
            }
            let len: f32 = d
                .split('M')
                .filter(|s| !s.is_empty())
                .map(|s| Path::svg(&format!("M{s}")).length())
                .sum();
            assert!(total >= 4, "图标 {name} 只解析出 {total} 段");
            // 圆体（太阳本体 / 雪粒）不在 path 里，按周长补上
            let circ: f32 = circles
                .iter()
                .take(nc)
                .map(|c| 2.0 * std::f32::consts::PI * c.2)
                .sum();
            assert!(
                len + circ > 25.0,
                "图标 {name} 总长只有 {:.1}（viewBox 24 单位内应 >25，疑似漏了主体段）",
                len + circ
            );
        }
    }
}
