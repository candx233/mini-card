//! 日历卡：`web/card.html` 的 `.card-cal` / `.cal-*` 搬到 D2D（240×180）。
//!
//! 结构：标题行（年/月 + 今日节日 + ‹ ›）· 星期行（周一起始）· 6×7 网格（当天有强调色圆角块）。
//! 网格行数按月份算（5 或 6 行），跨月格子 32% 不透明。

use windows::core::Result;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::{
    ID2D1Bitmap1, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_INTERPOLATION_MODE_LINEAR,
};
use windows_numerics::Vector2;

use minicard_draw::color::Palette;
use minicard_draw::layout::{layout, Align, Dim, Dir, Justify, Node, Placed, TextSpec};
use minicard_draw::paint::{fill_round, set_color, stroke_path, stroke_round};
use minicard_draw::path::Path;
use minicard_draw::text::{Mode as TextMode, TextEngine};
use minicard_shell::gfx::Gfx;

#[derive(Clone, Debug)]
pub struct Cell {
    pub day: u32,
    pub other: bool,
    pub today: bool,
}

#[derive(Clone, Debug)]
pub struct Calendar {
    pub year: i32,
    pub month: u32,
    pub holiday: String,
    pub cells: Vec<Cell>,
}

impl Calendar {
    pub fn title(&self) -> String {
        format!("{}年{}月", self.year, self.month)
    }

    /// 从「今天是几号」推出整月网格（周一起始）——与 `calPageHtml` 同口径：
    /// `offset = (day_of_week(1号) + 6) % 7`、`total = ceil((offset+天数)/7)*7`。
    pub fn build(year: i32, month: u32, first_weekday: u32, days_in_month: u32, today: u32) -> Self {
        let offset = (first_weekday + 6) % 7;
        let total = ((offset + days_in_month + 6) / 7) * 7;
        let mut cells = Vec::with_capacity(total as usize);
        for i in 0..total {
            let d = i as i32 - offset as i32 + 1;
            if d < 1 {
                // 上月末：JS 用 `new Date(y, m, i - offset + 1)` 自动归一，**数字要照常显示**
                // （只是 32% 不透明）。留空是我第一版的错，整卡差 7.7% 就是这个。
                let prev = days_in_month_of(year, if month > 1 { month - 1 } else { 12 });
                cells.push(Cell {
                    // d 是负数（-2/-1…）：上月天数 + d 就是上月的日号
                    day: (prev as i32 + d) as u32,
                    other: true,
                    today: false,
                });
            } else if d as u32 > days_in_month {
                cells.push(Cell { day: (d as u32 - days_in_month), other: true, today: false });
            } else {
                cells.push(Cell { day: d as u32, other: false, today: d as u32 == today });
            }
        }
        Self { year, month, holiday: String::new(), cells }
    }
}

pub struct Tokens {
    pub w: f32,
    pub h: f32,
    pub r: f32,
    pub radius: f32,
    pub pad_v: f32,
    pub pad_h: f32,
    pub body_gap: f32,
    pub head_gap: f32,
    pub lead_gap: f32,
    pub title_fs: f32,
    pub title_lh: f32,
    pub holiday_fs: f32,
    pub holiday_lh: f32,
    pub nav_btn: f32,
    pub nav_gap: f32,
    pub nav_r: f32,
    pub chev: f32,
    pub page_gap: f32,
    pub wd_fs: f32,
    pub wd_lh: f32,
    pub day_fs: f32,
    pub day_lh: f32,
    pub chip: f32,
    pub chip_r: f32,
}

impl Tokens {
    pub fn cal(w: f32, h: f32) -> Self {
        let k = (w / 240.0).min(h / 180.0).clamp(0.4, 2.5);
        let r = 16.0 * k;
        Self {
            w,
            h,
            r,
            radius: 1.0 * r,
            pad_v: 0.625 * r,
            pad_h: 0.75 * r,
            body_gap: 0.25 * r,
            head_gap: 0.375 * r,
            lead_gap: 0.4375 * r,
            title_fs: 1.0 * r,
            title_lh: 1.0 * r * 1.3,
            holiday_fs: 0.8125 * r,
            holiday_lh: 0.8125 * r * 1.3,
            nav_btn: 1.25 * r,
            nav_gap: 0.125 * r,
            nav_r: 0.375 * r,
            chev: 1.0 * r,
            page_gap: 0.25 * r,
            wd_fs: 0.8125 * r,
            wd_lh: 0.8125 * r * 1.4,
            day_fs: 0.875 * r,
            day_lh: 0.875 * r * 1.25,
            chip: 1.375 * r,
            chip_r: 0.4375 * r,
        }
    }
}

pub fn paint(
    g: &Gfx,
    te: &TextEngine,
    pal: &Palette,
    t: &Tokens,
    c: &Calendar,
    glass: Option<(&ID2D1Bitmap1, D2D_RECT_F)>,
) -> Result<()> {
    let tree = tree(t, c);
    let mut m: &TextEngine = te;
    let placed = layout(&tree, (t.w, t.h), &mut m);

    unsafe {
        g.begin()?;
        g.push_clip()?;
        if let Some((bmp, rect)) = glass {
            g.d2d.DrawBitmap(bmp, Some(&rect), 1.0, D2D1_INTERPOLATION_MODE_LINEAR, None, None);
        }
        let full = D2D_RECT_F { left: 0.0, top: 0.0, right: t.w, bottom: t.h };
        set_color(g, pal.veil)?;
        fill_round(g, full, t.radius)?;
        set_color(g, pal.border)?;
        stroke_round(
            g,
            D2D_RECT_F { left: 0.5, top: 0.5, right: t.w - 0.5, bottom: t.h - 0.5 },
            (t.radius - 0.5).max(0.0),
            1.0,
        )?;
        paint_tree(g, te, pal, t, c, &placed)?;
        g.pop_clip();
        g.end()?;
        Ok(())
    }
}

fn text_node(s: &str, size: f32, lh: f32, weight: u16) -> Node {
    text_node_aligned(s, size, lh, weight, 0)
}

/// `align`: 0 左 / 1 右 / 2 居中（走 DWrite 的文本对齐，盒子有多宽就按多宽居中）
fn text_node_aligned(s: &str, size: f32, lh: f32, weight: u16, align: u8) -> Node {
    Node::text(TextSpec {
        s: s.to_string(),
        size,
        line_h: lh,
        weight,
        wrap: false,
        align,
        tabular: true,
        spacing_em: 0.0,
    })
}

fn tree(t: &Tokens, c: &Calendar) -> Node {
    // 标题 + 节日（基线对齐）
    let lead = Node::new(Dir::Row)
        .style(move |s| {
            s.align(Align::Baseline)
                .gap(t.lead_gap)
                .size(Dim::Auto, Dim::Px(t.title_lh))
        })
        .child(text_node(&c.title(), t.title_fs, t.title_lh, 600).id(9501));
    let lead = if c.holiday.is_empty() {
        lead
    } else {
        lead.child(text_node(&c.holiday, t.holiday_fs, t.holiday_lh, 500).id(9502))
    };

    // 右：‹ ›（两个 20×20 按钮）
    let nav = Node::new(Dir::Row)
        .style(move |s| s.gap(t.nav_gap).align(Align::Center))
        .child(
            Node::new(Dir::Row)
                .id(9510)
                .style(move |s| s.size(Dim::Px(t.nav_btn), Dim::Px(t.nav_btn))),
        )
        .child(
            Node::new(Dir::Row)
                .id(9511)
                .style(move |s| s.size(Dim::Px(t.nav_btn), Dim::Px(t.nav_btn))),
        );

    let head = Node::new(Dir::Row)
        .style(move |s| {
            s.size(Dim::Pct(1.0), Dim::Px(t.title_lh))
                .align(Align::Center)
                .justify(Justify::Between)
                .gap(t.head_gap)
        })
        .child(lead)
        .child(nav);

    // 星期行
    let mut wd = Node::new(Dir::Row).style(move |s| s.size(Dim::Pct(1.0), Dim::Px(t.wd_lh)));
    for (i, w) in ["一", "二", "三", "四", "五", "六", "日"].iter().enumerate() {
        wd = wd.child(
            text_node_aligned(w, t.wd_fs, t.wd_lh, 400, 2)
                .id(9520 + i as u32)
                .style(|s| {
                    s.grow(1.0)
                        .basis(0.0)
                        .align(Align::Center)
                        .justify(Justify::Center)
                }),
        );
    }

    // 日期网格：每行 7 格，格内数字居中（today 的底色由绘制层垫）
    let n_rows = (c.cells.len() / 7).max(1);
    let mut grid = Node::new(Dir::Column).style(move |s| {
        s.size(Dim::Pct(1.0), Dim::Auto)
            .grow(1.0)
            .align(Align::Stretch)
    });
    for r in 0..n_rows {
        let mut row = Node::new(Dir::Row)
            .style(move |s| s.grow(1.0).basis(0.0).size(Dim::Pct(1.0), Dim::Auto));
        for col in 0..7 {
            let idx = r * 7 + col;
            let cell = c.cells.get(idx);
            // 跨月格子也要画数字（只是 32% 不透明）—— 留空会整卡差 7.7%
            let txt = cell.map(|x| x.day.to_string()).unwrap_or_default();
            let id = 9600 + idx as u32;
            let is_today = cell.map(|x| x.today && !x.other).unwrap_or(false);
            row = row.child(
                text_node_aligned(&txt, t.day_fs, t.day_lh, if is_today { 600 } else { 400 }, 2)
                    .id(id)
                    .style(move |s| {
                        // 今日格的 `<i>` 是 22×22，会把整格撑到 22 高（比普通格高 4.5），
                        // 居中后数字比邻居高 2.25px —— 这是 CSS 的实际行为，必须照做
                        let h = if is_today { Dim::Px(t.chip) } else { Dim::Px(t.day_lh) };
                        // 交叉轴居中要靠 **self_align**：写 align 只是设置「自己的子元素」怎么排，
                        // 父行的 align-items 默认 stretch 会把格子拉满行高、数字贴顶（差 2.5px）
                        s.grow(1.0)
                            .basis(0.0)
                            .size(Dim::Auto, h)
                            .self_align(Align::Center)
                            .justify(Justify::Center)
                    }),
            );
        }
        grid = grid.child(row);
    }

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
                        .gap(t.body_gap)
                        .align(Align::Stretch)
                })
                .child(head)
                .child(
                    Node::new(Dir::Column)
                        .style(move |s| {
                            s.grow(1.0)
                                .basis(0.0)
                                .gap(t.page_gap)
                                .align(Align::Stretch)
                        })
                        .child(wd)
                        .child(grid),
                ),
        )
}

fn paint_tree(g: &Gfx, te: &TextEngine, pal: &Palette, t: &Tokens, c: &Calendar, pl: &Placed) -> Result<()> {
    // 导航按钮：1px 描边吸附 + 居中箭头
    if pl.id == 9510 || pl.id == 9511 {
        let px = pl.rect.x.round();
        let py = pl.rect.y.round();
        let pw = pl.rect.w.round();
        let ph = pl.rect.h.round();
        let sz = t.chev;
        let s = sz / 24.0;
        let cx = px + (pw - sz) / 2.0;
        let cy = py + (ph - sz) / 2.0;
        let d = if pl.id == 9510 { "M14.5 6l-6 6 6 6" } else { "M9.5 6l6 6-6 6" };
        let p = Path::svg(d).scale(cx, cy, s);
        set_color(g, pal.muted)?;
        stroke_path(g, &p, 2.0 * s, true)?;
        return Ok(());
    }

    // 日期格：today 先垫强调色圆角块，再画数字（数字走下面的通用文字分支）
    if (9600..9700).contains(&pl.id) {
        let idx = (pl.id - 9600) as usize;
        if let Some(cell) = c.cells.get(idx) {
            if cell.today && !cell.other {
                // 块 = 22×22，横向居中于列、纵向就是节点盒子（节点高已经设成 22）
                let r = pl.rect;
                let left = r.x + (r.w - t.chip) / 2.0;
                let top = r.y;
                set_color(g, pal.accent)?;
                fill_round(
                    g,
                    D2D_RECT_F {
                        left,
                        top,
                        right: left + t.chip,
                        bottom: top + t.chip,
                    },
                    t.chip_r,
                )?;
            }
        }
    }

    if let Some(spec) = pl.text() {
        let is_today = {
            let idx = pl.id.saturating_sub(9600) as usize;
            (9600..9700).contains(&pl.id) && c.cells.get(idx).map(|x| x.today).unwrap_or(false)
        };
        let other = {
            let idx = pl.id.saturating_sub(9600) as usize;
            (9600..9700).contains(&pl.id) && c.cells.get(idx).map(|x| x.other).unwrap_or(false)
        };
        let color = if is_today {
            pal.on_accent
        } else if (9520..9530).contains(&pl.id) || pl.id == 9502 {
            pal.muted
        } else {
            pal.fg
        };
        // 跨月格 32% 不透明：用颜色 alpha（套 opacity 层会叠加，实测 0.32 → 0.08）
        let color = if other { D2D1_COLOR_F { a: 0.32, ..color } } else { color };
        set_color(g, color)?;
        // 宽度必须**恰好**等于盒子：多给 2px 在居中时会把文字推右 1px（实测 dx 趋势揭示的）
        let lay = te.layout_spec(spec, pl.rect.w, spec.line_h + 6.0, TextMode::Clip)?;
        // 今日数字要在 22 高的块里居中（普通格是 17.5 的行盒，直接画即可）
        let (dx, dy) = if is_today {
            let tw = te.measure(spec, 0.0).0;
            ((pl.rect.w - tw) / 2.0, (pl.rect.h - spec.line_h) / 2.0)
        } else {
            (0.0, 0.0)
        };
        unsafe {
            g.d2d.DrawTextLayout(
                Vector2 { X: pl.rect.x + dx + g.text_dx, Y: pl.rect.y + dy + g.text_dy },
                &lay,
                &g.brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
            );
            // 笔画加粗实验：Chromium 的 AA 会做 stem darkening（同一覆盖率下更暗），
            // D2D 是纯覆盖率。用两个亚像素偏移的低透明度副本把边缘补厚。
            let (off, a) = (g.embolden_off, g.embolden_a);
            if off > 0.0 && a > 0.0 {
                for (ox, oy) in [(-off, 0.0), (off, 0.0)] {
                    set_color(g, D2D1_COLOR_F { a, ..color })?;
                    g.d2d.DrawTextLayout(
                        Vector2 { X: pl.rect.x + dx + ox, Y: pl.rect.y + dy + oy },
                        &lay,
                        &g.brush,
                        D2D1_DRAW_TEXT_OPTIONS_NONE,
                    );
                }
                set_color(g, color)?;
            }
        }
    }
    for ch in &pl.children {
        paint_tree(g, te, pal, t, c, ch)?;
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

pub fn tree_hook(t: &Tokens, c: &Calendar) -> Node {
    tree(t, c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 网格行数按月份算() {
        // 2026-10：1 号是周四（周一起始 → 4），31 天 → 5 行
        let c = Calendar::build(2026, 10, 4, 31, 31);
        assert_eq!(c.cells.len(), 35, "应 35 格（5 行）");
        assert!(c.cells[3].day == 1, "偏移 3 格后是 1 号");
        assert!(c.cells[33].today, "31 号是 today");
        // 2026-08：1 号周六（6），31 天 → offset 5 + 31 = 36 → 42 格（6 行）
        let c = Calendar::build(2026, 8, 6, 31, 1);
        assert_eq!(c.cells.len(), 42, "应 42 格（6 行）");
        assert_eq!(c.title(), "2026年8月");
    }

    #[test]
    fn 跨月格子标记为_other() {
        let c = Calendar::build(2026, 10, 4, 31, 1);
        assert!(c.cells[0].other && c.cells[1].other && c.cells[2].other);
        assert!(!c.cells[3].other);
        // 末尾补的格子也是 other
        assert!(c.cells[34].other, "最后一格属次月");
        assert!(!c.cells[33].other, "30 号在本月");
        // 跨月格子要显示真实日期（9 月 28/29/30、11 月 1），不是空白
        assert_eq!(c.cells[0].day, 28, "10 月 1 日周四 → 前 3 格是 9 月 28/29/30");
        assert_eq!(c.cells[2].day, 30);
        assert_eq!(c.cells[34].day, 1, "末尾是 11 月 1 日");
    }
}

/// 某月的天数（含闰年）——`build` 补相邻月份格子用。
pub fn days_in_month_of(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
            if leap {
                29
            } else {
                28
            }
        }
        _ => 30,
    }
}
