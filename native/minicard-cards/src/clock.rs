//! 时钟卡：`web/card.html` 的 `.card-clock` / `.clock-*` 搬到 D2D，含**逐位滚动**。
//!
//! 滚动口径与 `card.html::rollSet` 同源：
//! - 每位一根「条」，条上是 `[9,0,1,…,9,0]` 十二格（前后各补一格，跨 9→0 不用跳）；
//! - 条的位移 = `-idx em`，`idx = from + (to-from) * (1 - (1-u)^4)`（easeOutQuart，300ms）；
//! - 只画可见的两格（`floor(idx)` 与 `+1`），不是把十二格都画一遍再裁。
//!
//! 与 WebView2 版的差别：那边每次内容变化都重建 DOM、靠「按文本记进度」续播；
//! 自绘里状态常驻，滚动天然连续，不需要记忆层。

use std::time::Instant;

use windows::core::Result;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::{
    ID2D1Bitmap1, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_INTERPOLATION_MODE_LINEAR,
};
use windows_numerics::Vector2;

use minicard_draw::color::Palette;
use minicard_draw::layout::{layout, Align, Dim, Dir, Justify, Node, Placed, TextSpec};
use minicard_draw::paint::{fill_round, set_color, stroke_round};
use minicard_draw::text::TextEngine;
use minicard_shell::gfx::Gfx;

use crate::roll::{cell, draw_digit, Roll};

pub struct Tokens {
    pub w: f32,
    pub h: f32,
    pub r: f32,
    pub radius: f32,
    pub pad_lr: f32,
    pub pad_tb: f32,
    pub gap: f32,
    pub body_pb: f32,
    pub date_mt: f32,
    pub time_fs: f32,
    pub time_lh: f32,
    pub date_fs: f32,
    pub date_lh: f32,
    pub lunar_fs: f32,
    pub lunar_lh: f32,
    pub spacing_em: f32,
}

impl Tokens {
    /// k = min(w/240, h/144)（纯文字卡按两边取小，多出来的那一边自然变成留白）。
    pub fn clock(w: f32, h: f32) -> Self {
        let k = (w / 240.0).min(h / 144.0).clamp(0.4, 2.5);
        let r = 16.0 * k;
        Self {
            w,
            h,
            r,
            radius: 1.0 * r,
            pad_lr: 1.0 * r,
            pad_tb: 0.875 * r,
            gap: 0.125 * r,
            body_pb: 0.15625 * r,
            date_mt: 0.1875 * r,
            time_fs: 4.5 * r,
            time_lh: 4.5 * r, // line-height: 1
            date_fs: 1.125 * r,
            date_lh: 1.125 * r * 1.55,
            lunar_fs: 1.0 * r,
            lunar_lh: 1.5 * r,
            spacing_em: -0.02,
        }
    }
}

/// 一位数字的滚动状态。
/// 时钟状态：四位数字 + 每位的滚动。
#[derive(Clone, Debug)]
pub struct Clock {
    pub digits: [u8; 4],
    pub rolls: [Option<Roll>; 4],
}

impl Clock {
    pub fn new() -> Self {
        Self { digits: [0; 4], rolls: [None; 4] }
    }

    /// 条上的格子序号（口径在 `roll::cell`）。
    fn cell(d: u8) -> f32 {
        cell(d)
    }

    /// 喂新时间（hh, mm），返回是否有滚动在进行。
    pub fn set(&mut self, hh: u8, mm: u8, now: Instant) {
        let want = [hh / 10, hh % 10, mm / 10, mm % 10];
        for i in 0..4 {
            if want[i] == self.digits[i] {
                continue;
            }
            // 上一段没走完就停在那，从当前位置起步（与 CSS 版 `from = cur` 同口径）
            let cur = self.rolls[i].map(|r| r.at(now)).unwrap_or(Self::cell(self.digits[i]));
            let mut to = Self::cell(want[i]);
            // 9 → 0 往上滚要经过补的那一格（第 11 格），到了再瞬归
            if want[i] == 0 && self.digits[i] == 9 {
                to = 11.0;
            }
            if to < cur {
                // 倒着走（时间回拨）就退一格，视觉上仍是往上
                to = cur + 1.0;
            }
            self.rolls[i] = Some(Roll::new(cur, to, now));
            self.digits[i] = want[i];
        }
    }

    /// 直接落位（无动画）—— 静态对比与首帧用。
    pub fn snap(&mut self, hh: u8, mm: u8) {
        self.digits = [hh / 10, hh % 10, mm / 10, mm % 10];
        self.rolls = [None; 4];
    }

    pub fn animating(&self, now: Instant) -> bool {
        self.rolls.iter().flatten().any(|r| !r.done(now))
    }

    /// 各位当前条位移（单位 = 格）。
    fn idx(&self, i: usize, now: Instant) -> f32 {
        match &self.rolls[i] {
            Some(r) if !r.done(now) => r.at(now),
            _ => Self::cell(self.digits[i]),
        }
    }
}

impl Default for Clock {
    fn default() -> Self {
        Self::new()
    }
}

pub fn paint(
    g: &Gfx,
    te: &TextEngine,
    pal: &Palette,
    t: &Tokens,
    clock: &Clock,
    date: &str,
    lunar: &str,
    now: Instant,
    glass: Option<(&ID2D1Bitmap1, D2D_RECT_F)>,
) -> Result<()> {
    let tree = tree(t, date, lunar);
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

        paint_tree(g, te, pal, t, clock, &placed, now)?;

        g.pop_clip();
        g.end()?;
        Ok(())
    }
}

/// id 约定：4000+i = 第 i 位数字（要滚动绘制）；其余走普通文字。
fn tree(t: &Tokens, date: &str, lunar: &str) -> Node {
    let digit_spec = |ch: &str| TextSpec {
        s: ch.to_string(),
        size: t.time_fs,
        line_h: t.time_lh,
        weight: 600,
        wrap: false,
        align: 0,
        tabular: true,
        spacing_em: t.spacing_em,
    };
    let colon = Node::text(TextSpec {
        s: ":".into(),
        size: t.time_fs,
        line_h: t.time_lh,
        weight: 600,
        wrap: false,
        align: 0,
        tabular: true,
        spacing_em: t.spacing_em,
    });
    let time = Node::new(Dir::Row)
        .style(|s| s.align(Align::Center).justify(Justify::Center))
        .child(Node::text(digit_spec("0")).id(4000))
        .child(Node::text(digit_spec("0")).id(4001))
        .child(colon)
        .child(Node::text(digit_spec("0")).id(4002))
        .child(Node::text(digit_spec("0")).id(4003));

    let date_node = Node::text(TextSpec {
        s: date.to_string(),
        size: t.date_fs,
        line_h: t.date_lh,
        weight: 400,
        wrap: false,
        align: 0,
        tabular: true,
        spacing_em: 0.0,
    })
    .style(|s| s.mar([t.date_mt, 0.0, 0.0, 0.0]));

    // 第三行：农历（与日期同为 muted 小字，但字号小一档、无上边距）
    let lunar_node = Node::text(TextSpec {
        s: lunar.to_string(),
        size: t.lunar_fs,
        line_h: t.lunar_lh,
        weight: 400,
        wrap: false,
        align: 0,
        tabular: true,
        spacing_em: 0.0,
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
                        .align(Align::Center)
                        .justify(Justify::Center)
                        .gap(t.gap)
                        .pad([0.0, 0.0, t.body_pb, 0.0])
                })
                .child(time)
                .child(date_node)
                .child(lunar_node),
        )
}

/// 条上第 `cell` 格的字符：`[9,0,1,…,9,0]`
fn paint_tree(
    g: &Gfx,
    te: &TextEngine,
    pal: &Palette,
    t: &Tokens,
    clock: &Clock,
    p: &Placed,
    now: Instant,
) -> Result<()> {
    if (4000..4100).contains(&p.id) {
        let i = (p.id - 4000) as usize;
        let spec = TextSpec {
            s: String::new(),
            size: t.time_fs,
            line_h: t.time_lh,
            weight: 600,
            wrap: false,
            align: 0,
            tabular: true,
            spacing_em: t.spacing_em,
        };
        return draw_digit(
            g,
            te,
            pal,
            p.rect.x,
            p.rect.y,
            p.rect.w,
            t.time_lh,
            clock.idx(i, now),
            &spec,
            pal.fg,
        );
    } else if let Some(spec) = p.text() {
        let fmt = te.format_spec(spec, minicard_draw::text::Mode::Clip)?;
        let lay = te.layout_spec(spec, p.rect.w + 2.0, spec.line_h + 8.0, minicard_draw::text::Mode::Clip)?;
        let _ = fmt;
        set_color(g, pal.shadow)?;
        unsafe {
            g.d2d.DrawTextLayout(
                Vector2 { X: p.rect.x, Y: p.rect.y + 0.833 },
                &lay,
                &g.brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
            );
        }
        set_color(g, pal.muted)?;
        unsafe {
            g.d2d.DrawTextLayout(
                Vector2 { X: p.rect.x, Y: p.rect.y },
                &lay,
                &g.brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
            );
        }
    }
    for c in &p.children {
        paint_tree(g, te, pal, t, clock, c, now)?;
    }
    Ok(())
}

/// 把排好版的树按行打出来（几何对表用）。
pub fn dump_layout(p: &Placed, depth: usize) {
    let pad = " ".repeat(depth * 2);
    let role = if (4000..4100).contains(&p.id) {
        format!("dgt#{}", p.id - 4000)
    } else if let Some(s) = p.text() {
        format!("text {:?}", s.s)
    } else {
        "box".to_string()
    };
    minicard_shell::mlog!(
        "{}id={:<5} {:<24} x={:7.2} y={:7.2} w={:7.2} h={:7.2}",
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

pub fn tree_hook(t: &Tokens, date: &str, lunar: &str) -> Node {
    tree(t, date, lunar)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::roll::cell_char;
    use std::time::Duration;

    fn at(ms: u64) -> Instant {
        Instant::now() + Duration::from_millis(ms)
    }

    #[test]
    fn 条上的格子映射() {
        // [9, 0, 1, …, 9, 0]：数字 d 在 d+1 格
        assert_eq!(cell_char(0), '9');
        assert_eq!(cell_char(1), '0');
        assert_eq!(cell_char(2), '1');
        assert_eq!(cell_char(10), '9');
        assert_eq!(cell_char(11), '0');
        // 数字 → 格子
        assert_eq!(Clock::cell(0), 1.0);
        assert_eq!(Clock::cell(9), 10.0);
    }

    #[test]
    fn 滚动从原位起步并单调推到目标() {
        let now = Instant::now();
        let mut c = Clock::new();
        c.snap(14, 26);
        // 直接跳到 14:27：只有最后一位动
        c.set(14, 27, now);
        let r = c.rolls[3].expect("最后一位应开始滚动");
        assert_eq!(r.from, Clock::cell(6));
        assert_eq!(r.to, Clock::cell(7));
        // t=0 在原位、t=300ms 到目标、中途在两者之间
        assert!((r.at(now) - r.from).abs() < 0.01, "起步 {:?}", r.at(now));
        assert!((r.at(now + Duration::from_millis(300)) - r.to).abs() < 0.01);
        let mid = r.at(now + Duration::from_millis(120));
        assert!(mid > r.from && mid < r.to, "中途 {mid} 应在 {}..{} 之间", r.from, r.to);
        // easeOutQuart：前 1/3 时间应走掉一半以上行程
        assert!(mid - r.from > (r.to - r.from) * 0.5, "前 40% 走了 {:?}", mid - r.from);
    }

    #[test]
    fn 九到零走补格再瞬归() {
        let now = Instant::now();
        let mut c = Clock::new();
        c.snap(14, 29);
        c.set(14, 30, now);
        let r = c.rolls[3].expect("应滚动");
        // 个位 9 → 0：目标是补的第 11 格（0），不是第 1 格
        assert_eq!(r.to, 11.0, "9→0 应走补格");
        assert_eq!(cell_char(11), '0');
        // 滚完之后 idx 落到 0 所在格（第 1 格）
        assert_eq!(c.idx(3, now + Duration::from_millis(320)), Clock::cell(0));
    }

    #[test]
    fn 时间没变不起滚动() {
        let now = Instant::now();
        let mut c = Clock::new();
        c.snap(14, 26);
        c.set(14, 26, now);
        assert!(c.rolls.iter().all(|r| r.is_none()));
        assert!(!c.animating(now));
    }
}
