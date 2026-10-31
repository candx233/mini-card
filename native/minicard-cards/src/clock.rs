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
use minicard_draw::paint::{fill_round, push_clip, pop_clip, set_color, stroke_round};
use minicard_draw::text::TextEngine;
use minicard_shell::gfx::Gfx;

/// 滚动时长（与 card.html 的 `el.__ms || 300` 一致）
pub const ROLL_MS: f32 = 300.0;

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
            spacing_em: -0.02,
        }
    }
}

/// 一位数字的滚动状态。
#[derive(Clone, Copy, Debug)]
pub struct Roll {
    pub from: f32,
    pub to: f32,
    pub t0: Instant,
    pub ms: f32,
}

impl Roll {
    fn at(&self, now: Instant) -> f32 {
        let u = ((now - self.t0).as_secs_f32() * 1000.0 / self.ms).clamp(0.0, 1.0);
        let k = 1.0 - (1.0 - u).powi(4);
        self.from + (self.to - self.from) * k
    }
    fn done(&self, now: Instant) -> bool {
        (now - self.t0).as_secs_f32() * 1000.0 >= self.ms
    }
}

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

    /// 条上的格子序号：`[9,0,1..9,0]` → 数字 d 在 `d + 1` 格。
    fn cell(d: u8) -> f32 {
        (d as f32) + 1.0
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
            self.rolls[i] = Some(Roll { from: cur, to, t0: now, ms: ROLL_MS });
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
    now: Instant,
    glass: Option<(&ID2D1Bitmap1, D2D_RECT_F)>,
) -> Result<()> {
    let tree = tree(t, date);
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
fn tree(t: &Tokens, date: &str) -> Node {
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
                .child(date_node),
        )
}

/// 条上第 `cell` 格的字符：`[9,0,1,…,9,0]`
fn cell_char(cell: i32) -> char {
    match cell {
        0 => '9',
        11 => '0',
        c => char::from_digit(((c - 1).clamp(0, 9)) as u32, 10).unwrap_or('0'),
    }
}

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
        let idx = clock.idx(i, now);
        let h = t.time_lh;
        // 裁到这一位的 1em 高盒子里
        push_clip(
            g,
            D2D_RECT_F { left: p.rect.x, top: p.rect.y, right: p.rect.right(), bottom: p.rect.y + h },
        );
        set_color(g, pal.fg)?;
        let lo = idx.floor() as i32;
        for c in [lo, lo + 1] {
            let ch = cell_char(c);
            let spec = TextSpec {
                s: ch.to_string(),
                size: t.time_fs,
                line_h: t.time_lh,
                weight: 600,
                wrap: false,
                align: 0,
                tabular: true,
                spacing_em: t.spacing_em,
            };
            let fmt = te.format_spec(&spec, minicard_draw::text::Mode::Clip)?;
            let lay = te.layout_spec(
                &spec,
                (p.rect.w + 2.0).max(4.0),
                t.time_lh + 8.0,
                minicard_draw::text::Mode::Clip,
            )?;
            let _ = fmt;
            let y = p.rect.y + (c as f32 - idx) * h;
            unsafe {
                g.d2d.DrawTextLayout(
                    Vector2 { X: p.rect.x, Y: y },
                    &lay,
                    &g.brush,
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                );
            }
        }
        pop_clip(g);
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

pub fn tree_hook(t: &Tokens, date: &str) -> Node {
    tree(t, date)
}
