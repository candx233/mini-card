//! 待办卡：`web/card.html` 的 `.card-todo` / `.td-*` 搬到 D2D（240×120）。
//!
//! 行数规则与 `todoShown` 同源：**未完成在上、完成沉底**，条目 >4 时只显示 3 行 + 「还有 N 项」，
//! 否则显示 4 行。行高 21、行距 4，容器居中。
//!
//! 勾是**描出来**的（不是切图）：`M5.1 12.3 l4.7 4.7 L19.1 6.9`，viewBox 24 → 14×14，
//! 线宽 2.8/24×14 = 1.633，圆头圆角。动画按弧长截断（CSS 的 `stroke-dashoffset 26→0`）。

use std::time::Instant;

use windows::core::Result;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::{ID2D1Bitmap1, D2D1_INTERPOLATION_MODE_LINEAR};

use minicard_draw::color::Palette;
use minicard_draw::layout::{layout, Align, Dim, Dir, Justify, Node, Placed, TextSpec};
use minicard_draw::paint::{fill_round, set_color, stroke_path, stroke_round};
use minicard_draw::path::Path;
use minicard_draw::text::{Mode as TextMode, TextEngine};
use minicard_shell::gfx::Gfx;

#[derive(Clone, Debug)]
pub struct Item {
    pub text: String,
    pub done: bool,
    /// 描线进度 0..1（勾上时从 0 走到 1；非动画时直接 1）
    pub check: f32,
    pub check_t0: Option<Instant>,
    /// FLIP：勾选后行会重排，这里存「起始位移」（px）与起始时刻，画的时候从它衰减到 0
    pub mv_from: f32,
    pub mv_t0: Option<Instant>,
    /// 勾上的「弹一下」（ckPop：0.9 → 1.04 → 1，420ms）
    pub pop_t0: Option<Instant>,
}

impl Item {
    pub fn new(text: impl Into<String>, done: bool) -> Self {
        Self {
            text: text.into(),
            done,
            check: if done { 1.0 } else { 0.0 },
            check_t0: None,
            mv_from: 0.0,
            mv_t0: None,
            pop_t0: None,
        }
    }

    /// 勾上：起描线动画（280ms，与 CSS 的 `stroke-dashoffset 280ms` 同）
    pub fn set_done(&mut self, done: bool, now: Instant) {
        if done == self.done {
            return;
        }
        self.done = done;
        if done {
            self.check = 0.0;
            self.check_t0 = Some(now);
            self.pop_t0 = Some(now);
        } else {
            self.check = 0.0;
            self.check_t0 = None;
        }
    }

    /// FLIP 位移（px）：300ms ease-out 衰减到 0
    pub fn move_dy(&self, now: Instant) -> f32 {
        let Some(t0) = self.mv_t0 else { return 0.0 };
        let u = ((now - t0).as_secs_f32() * 1000.0 / 300.0).clamp(0.0, 1.0);
        let k = 1.0 - (1.0 - u) * (1.0 - u);
        self.mv_from * (1.0 - k)
    }

    /// ckPop 的缩放（0.9 → 1.04 → 1）
    pub fn pop_scale(&self, now: Instant) -> f32 {
        let Some(t0) = self.pop_t0 else { return 1.0 };
        let u = ((now - t0).as_secs_f32() * 1000.0 / 420.0).clamp(0.0, 1.0);
        if u >= 1.0 {
            return 1.0;
        }
        // 0% .9 / 55% 1.04 / 100% 1
        if u < 0.55 {
            0.9 + (1.04 - 0.9) * (u / 0.55)
        } else {
            1.04 + (1.0 - 1.04) * ((u - 0.55) / 0.45)
        }
    }

    fn progress(&self, now: Instant) -> f32 {
        match self.check_t0 {
            Some(t0) if self.done => {
                let u = ((now - t0).as_secs_f32() * 1000.0 / 280.0).clamp(0.0, 1.0);
                // ease-out（CSS var(--ease-out) 近似 cubic-bezier(.2,.8,.2,1)）
                let k = 1.0 - (1.0 - u) * (1.0 - u);
                k.max(self.check)
            }
            _ => self.check,
        }
    }
}

pub struct Tokens {
    pub w: f32,
    pub h: f32,
    pub r: f32,
    pub radius: f32,
    pub pad_v: f32,
    pub pad_h: f32,
    pub row_h: f32,
    pub row_gap: f32,
    pub row_radius: f32,
    pub gap_ck_text: f32,
    pub ck_d: f32,
    pub ck_stroke: f32,
    pub text_fs: f32,
    pub text_lh: f32,
    pub more_fs: f32,
    pub more_lh: f32,
    pub more_pl: f32,
}

impl Tokens {
    pub fn todo(w: f32, h: f32) -> Self {
        let k = (w / 240.0).min(h / 120.0).clamp(0.4, 2.5);
        let r = 16.0 * k;
        Self {
            w,
            h,
            r,
            radius: 1.0 * r,
            pad_v: 0.625 * r,
            pad_h: 0.75 * r,
            row_h: 1.3125 * r,
            row_gap: 0.25 * r,
            row_radius: 0.375 * r,
            gap_ck_text: 0.5 * r,
            ck_d: 1.125 * r,
            ck_stroke: 2.8 / 24.0 * 0.875 * r,
            text_fs: 0.9375 * r,
            text_lh: 0.9375 * r * 1.3,
            more_fs: 0.8125 * r,
            more_lh: 0.8125 * r * 1.3,
            more_pl: 1.625 * r,
        }
    }
}

/// 排序与截断（`todoShown` 的复刻）：未完成在上、完成沉底；>4 项显示 3 行。
pub fn shown(items: &[Item]) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..items.len()).collect();
    idx.sort_by_key(|&i| items[i].done);
    let keep = if items.len() > 4 { 3 } else { 4 };
    idx.into_iter().take(keep).collect()
}

/// 有没有在跑的动效（勾描线 / 弹一下 / 重排）—— live 用它决定要不要 60fps
pub fn animating(items: &[Item], now: Instant) -> bool {
    items.iter().any(|it| {
        let drawing = it.done
            && it
                .check_t0
                .map(|t0| (now - t0).as_secs_f32() < 0.28)
                .unwrap_or(false);
        let popping = it.pop_t0.map(|t0| (now - t0).as_secs_f32() < 0.42).unwrap_or(false);
        let moving = it.mv_t0.map(|t0| (now - t0).as_secs_f32() < 0.30).unwrap_or(false);
        drawing || popping || moving
    })
}

pub fn paint(
    g: &Gfx,
    te: &TextEngine,
    pal: &Palette,
    t: &Tokens,
    items: &[Item],
    now: Instant,
    glass: Option<(&ID2D1Bitmap1, D2D_RECT_F)>,
) -> Result<()> {
    let tree = tree(t, items);
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
        paint_tree(g, te, pal, t, items, &placed, now, 0.0)?;
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
        tabular: false,
        spacing_em: 0.0,
    })
}

fn tree(t: &Tokens, items: &[Item]) -> Node {
    let root = Node::new(Dir::Column)
        .style(|s| {
            s.size(Dim::Px(t.w), Dim::Px(t.h))
                .pad([t.pad_v, t.pad_h, t.pad_v, t.pad_h])
        });

    if items.is_empty() {
        // 空态：两行居中（11px 副行 85% 不透明）
        let empty = Node::new(Dir::Column)
            .style(|s| {
                s.grow(1.0)
                    .basis(0.0)
                    .align(Align::Center)
                    .justify(Justify::Center)
                    .gap(0.125 * t.r)
            })
            .child(text_node("暂无待办", 0.875 * t.r, 0.875 * t.r * 1.3, 400).id(7001))
            .child(
                text_node("在主界面 · 参数 里添加", 0.6875 * t.r, 0.6875 * t.r * 1.3, 400)
                    .id(7002),
            );
        return root.child(empty);
    }

    let idx = shown(items);
    let more = items.len() - idx.len();

    let mut rows = Node::new(Dir::Column)
        .style(|s| {
            s.grow(1.0)
                .basis(0.0)
                .justify(Justify::Center)
                .gap(t.row_gap)
                .align(Align::Stretch)
        });
    for (row, &i) in idx.iter().enumerate() {
        let it = &items[i];
        let ck = Node::new(Dir::Row)
            .id(7100 + row as u32)
            .style(move |s| s.size(Dim::Px(t.ck_d), Dim::Px(t.ck_d)));
        let txt = text_node(&it.text, t.text_fs, t.text_lh, 400)
            .id(7200 + row as u32)
            .style(|s| s.size(Dim::Auto, Dim::Px(t.text_lh)));
        let row_node = Node::new(Dir::Row)
            .id(7300 + row as u32)
            .style(move |s| {
                s.size(Dim::Pct(1.0), Dim::Px(t.row_h))
                    .align(Align::Center)
                    .justify(Justify::Start)
                    .gap(t.gap_ck_text)
            })
            .child(ck)
            .child(txt);
        rows = rows.child(row_node);
    }
    if more > 0 {
        // `.td-more` 是**固定 21 高的行**（与 `.td-row` 同高，文字在其中垂直居中），
        // 不是「一行文字」——按文字高会少 4px，整栈因此下移 2px。
        rows = rows.child(
            Node::new(Dir::Row)
                .id(7400)
                .style(move |s| {
                    s.size(Dim::Pct(1.0), Dim::Px(t.row_h))
                        .pad([0.0, 0.0, 0.0, t.more_pl])
                        .align(Align::Center)
                })
                .child(text_node(&format!("还有 {} 项", more), t.more_fs, t.more_lh, 400).id(7401)),
        );
    }
    root.child(rows)
}

fn paint_tree(
    g: &Gfx,
    te: &TextEngine,
    pal: &Palette,
    t: &Tokens,
    items: &[Item],
    pl: &Placed,
    now: Instant,
    dy: f32,
) -> Result<()> {
    // 行容器：把这一行的 FLIP 位移传给子节点
    if (7300..7400).contains(&pl.id) {
        let row = (pl.id - 7300) as usize;
        let order = shown(items);
        let d = order.get(row).map(|&i| items[i].move_dy(now)).unwrap_or(0.0);
        for c in &pl.children {
            paint_tree(g, te, pal, t, items, c, now, d)?;
        }
        return Ok(());
    }

    // 勾选圆：id 7100+row
    if (7100..7200).contains(&pl.id) {
        let row = (pl.id - 7100) as usize;
        let idx = shown(items);
        let it = idx.get(row).map(|&i| &items[i]);
        let mut r = pl.rect;
        r.y += dy;
        // 勾上的弹一下：绕圆心缩放
        if let Some(it) = it {
            let k = it.pop_scale(now);
            if (k - 1.0).abs() > 0.001 {
                let (cx, cy) = (r.x + r.w / 2.0, r.y + r.h / 2.0);
                r = minicard_draw::layout::Rect {
                    x: cx - r.w * k / 2.0,
                    y: cy - r.h * k / 2.0,
                    w: r.w * k,
                    h: r.h * k,
                };
            }
        }
        let cx = r.x + r.w / 2.0;
        let cy = r.y + r.h / 2.0;
        let rad = r.w / 2.0;
        let done = it.map(|i| i.done).unwrap_or(false);
        if done {
            set_color(g, pal.accent)?;
            fill_round(
                g,
                D2D_RECT_F { left: r.x, top: r.y, right: r.right(), bottom: r.bottom() },
                rad,
            )?;
        } else {
            set_color(g, pal.badge_border)?;
            stroke_round(
                g,
                D2D_RECT_F { left: r.x + 0.5, top: r.y + 0.5, right: r.right() - 0.5, bottom: r.bottom() - 0.5 },
                rad - 0.5,
                1.0,
            )?;
        }
        if let Some(it) = it {
            let p = it.progress(now);
            if p > 0.001 {
                draw_check(g, pal, cx, cy, t.ck_d * 0.875, p, done)?;
            }
        }
        let _ = t;
        return Ok(());
    }

    if let Some(spec) = pl.text() {
        // 完成的行：muted + 删除线
        let done_row = (7200..7300).contains(&pl.id)
            && {
                let row = (pl.id - 7200) as usize;
                shown(items).get(row).map(|&i| items[i].done).unwrap_or(false)
            };
        let color = if done_row || pl.id == 7001 || pl.id == 7002 || pl.id == 7401 {
            pal.muted
        } else {
            pal.fg
        };
        // CSS 的 text-shadow（0 0.833px 1.667px black 45%）——带模糊，不是硬边副本
        minicard_draw::text::draw_shadowed(
            g,
            spec,
            pl.rect.x,
            pl.rect.y + dy,
            pl.rect.w,
            spec.line_h + 6.0,
            TextMode::Ellipsis,
            color,
            0.45,
            0.8335,
            0.833,
            te,
        )?;
        // 删除线：文字中线（CSS text-decoration: line-through 约在基线上方 ~0.3em）
        if done_row {
            let w = te.measure(spec, 0.0).0.min(pl.rect.w);
            let mid = pl.rect.y + spec.line_h * 0.5;
            set_color(g, pal.muted)?;
            let line = Path::polyline(&[(pl.rect.x, mid), (pl.rect.x + w, mid)]);
            stroke_path(g, &line, 1.0, false)?;
        }
    }
    for c in &pl.children {
        paint_tree(g, te, pal, t, items, c, now, dy)?;
    }
    Ok(())
}

/// 勾：用 B3 就写好的 `Path::check_mark`（同一份 SVG 原文、同一套弧长截断）。
fn draw_check(
    g: &Gfx,
    pal: &Palette,
    cx: f32,
    cy: f32,
    size: f32,
    progress: f32,
    on_accent: bool,
) -> Result<()> {
    let s = size / 24.0;
    // check_mark 的 (x, y) 是 24 网格原点 → 平移到勾的左上角
    let path = Path::check_mark(cx - size / 2.0, cy - size / 2.0, s, progress);
    set_color(g, if on_accent { pal.on_accent } else { pal.fg })?;
    stroke_path(g, &path, 2.8 * s, true)
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

pub fn tree_hook(t: &Tokens, items: &[Item]) -> Node {
    tree(t, items)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn five() -> Vec<Item> {
        (0..5).map(|i| Item::new(format!("任务{i}"), i == 1)).collect()
    }

    #[test]
    fn 未完成在上完成沉底() {
        let items = five();
        let idx = shown(&items);
        assert_eq!(idx, vec![0, 2, 3], "5 项只显示 3 行，未完成在前：{idx:?}");
    }

    #[test]
    fn 四项以内全显示() {
        let items: Vec<Item> = (0..4).map(|i| Item::new(format!("{i}"), i == 3)).collect();
        assert_eq!(shown(&items), vec![0, 1, 2, 3], "4 项全显示，完成的沉底");
        let three: Vec<Item> = (0..3).map(|i| Item::new(format!("{i}"), i == 0)).collect();
        assert_eq!(shown(&three), vec![1, 2, 0]);
    }

    #[test]
    fn 勾上起描线且走完() {
        let now = Instant::now();
        let mut it = Item::new("x", false);
        assert_eq!(it.progress(now), 0.0);
        it.set_done(true, now);
        let mid = it.progress(now + std::time::Duration::from_millis(120));
        assert!(mid > 0.0 && mid < 1.0, "中途应在 0..1，实际 {mid}");
        let done = it.progress(now + std::time::Duration::from_millis(400));
        assert!((done - 1.0).abs() < 0.001, "300ms 后应满，实际 {done}");
        // 取消勾选立刻归零，不回放
        it.set_done(false, now + std::time::Duration::from_millis(500));
        assert_eq!(it.progress(now + std::time::Duration::from_millis(600)), 0.0);
    }
}

/// 点击命中：返回**真实 items 下标**（行序走 `shown`，含「还有 N 项」截断口径）
pub fn hit(te: &TextEngine, t: &Tokens, items: &[Item], x: f32, y: f32) -> Option<usize> {
    let tr = tree(t, items);
    let mut m: &TextEngine = te;
    let pl = minicard_draw::layout::layout(&tr, (t.w, t.h), &mut m);
    let id = minicard_draw::layout::hit_id(&pl, x, y)?;
    let row = if (7300..7400).contains(&id) {
        id - 7300
    } else if (7100..7200).contains(&id) {
        id - 7100
    } else if (7200..7300).contains(&id) {
        id - 7200
    } else {
        return None;
    };
    shown(items).get(row as usize).copied()
}
