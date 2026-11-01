//! 番茄钟卡：`web/card.html` 的 `.card-pomo` / `.pm-*` 搬到 D2D（120×120）。
//!
//! 四态（`data-mode`）决定显示哪些块：
//! - `set`    ：头（准备专注）+ hero（− 25 +）+ 「开始」按钮，`gap 0.5rem`
//! - `run`    ：头 + hero + 时间 + 进度条 + 暂停/重设，`gap 0.125rem`
//! - `paused` ：同 run，按钮文案变「继续」
//! - `done`   ：头 + hero + 时间（55% 不透明）+ 进度条 + 「回到设定」
//! 时间数字走 `roll::draw_digit`（与时钟同一条带、同一缓动）。

use std::time::{Duration, Instant};

use windows::core::Result;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::{
    ID2D1Bitmap1, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_INTERPOLATION_MODE_LINEAR,
};
use windows_numerics::Vector2;

use minicard_draw::color::Palette;
use minicard_draw::layout::{layout, Align, Dim, Dir, Justify, Node, Placed, TextSpec};
use minicard_draw::paint::{fill_round, set_color, stroke_round};
use minicard_draw::text::{Mode as TextMode, TextEngine};
use minicard_shell::gfx::Gfx;

use crate::roll::{cell, draw_digit, Roll, ROLL_MS};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PomoMode {
    Set,
    Run,
    Paused,
    Done,
}

impl PomoMode {
    pub fn phase(self) -> &'static str {
        match self {
            PomoMode::Set => "准备专注",
            PomoMode::Paused => "已暂停",
            PomoMode::Done => "本轮完成",
            PomoMode::Run => "专注中",
        }
    }
}

pub struct Tokens {
    pub w: f32,
    pub h: f32,
    pub r: f32,
    pub radius: f32,
    pub pad: f32,
    pub gap_set: f32,
    pub gap_run: f32,
    pub head_min_h: f32,
    pub phase_fs: f32,
    pub phase_lh: f32,
    pub phase_dy: f32,
    pub hero_gap: f32,
    pub flank_w: f32,
    pub flank_fs: f32,
    pub flank_lh: f32,
    /// `.pomo-body .pm-flank { padding: 0 0 0.15rem }` —— grid 居中是在内容盒里，扣掉它再居中
    pub flank_pad_b: f32,
    pub num_fs: f32,
    pub num_lh: f32,
    pub num_min_w: f32,
    pub spacing_em: f32,
    pub bar_h: f32,
    pub track_gap: f32,
    pub acts_gap: f32,
    pub btn_h: f32,
    pub btn_fs: f32,
}

impl Tokens {
    pub fn pomo(w: f32, h: f32) -> Self {
        let k = (w / 120.0).min(h / 120.0).clamp(0.4, 2.5);
        let r = 16.0 * k;
        Self {
            w,
            h,
            r,
            radius: 1.0 * r,
            pad: 0.625 * r,
            gap_set: 0.5 * r,
            gap_run: 0.125 * r,
            head_min_h: 1.0625 * r,
            phase_fs: 0.875 * r,
            phase_lh: 0.875 * r * 1.4,
            phase_dy: -0.5,
            hero_gap: 0.125 * r,
            flank_w: 1.75 * r,
            flank_fs: 1.0 * r,
            flank_pad_b: 0.15 * r,
            flank_lh: 1.0 * r,
            num_fs: 2.125 * r,
            num_lh: 2.125 * r,
            num_min_w: 2.5 * r,
            spacing_em: -0.02,
            bar_h: 0.4375 * r,
            track_gap: 0.3125 * r,
            acts_gap: 0.375 * r,
            btn_h: 1.5625 * r,
            btn_fs: 0.875 * r, // CSS: .pm-btn font-size 0.875rem（13→14px 六改定稿）
        }
    }
}

/// 状态：设定分钟数 + 剩余毫秒 + 绝对结束时刻（`run` 时以它为准，窗口重开也对得上）
#[derive(Clone, Debug)]
pub struct Pomo {
    pub mode: PomoMode,
    pub dur: u8,
    pub remain_ms: f64,
    pub end_at: Option<Instant>,
    pub rolls: [Option<Roll>; 4],
    pub shown: [u8; 4],
}

impl Pomo {
    pub fn new() -> Self {
        Self {
            mode: PomoMode::Set,
            dur: 25,
            remain_ms: 25.0 * 60000.0,
            end_at: None,
            rolls: [None; 4],
            shown: [2, 5, 0, 0],
        }
    }

    /// 运行态才需要 60fps（秒数在跳），设置/暂停/完成都按 1Hz 就够
    pub fn is_running(&self) -> bool {
        matches!(self.mode, PomoMode::Run)
    }

    pub fn total_ms(&self) -> f64 {
        self.dur as f64 * 60000.0
    }

    pub fn remain(&self, now: Instant) -> f64 {
        match (self.mode, self.end_at) {
            (PomoMode::Run, Some(e)) => {
                if e > now {
                    (e - now).as_secs_f64() * 1000.0
                } else {
                    0.0
                }
            }
            (PomoMode::Done, _) => 0.0,
            (PomoMode::Set, _) => self.total_ms(),
            _ => self.remain_ms,
        }
    }

    /// 有效模式：`run` 且没时间了就自动是 `done`（与 `pomoMode()` 同口径）。
    pub fn eff_mode(&self, now: Instant) -> PomoMode {
        if self.mode == PomoMode::Run && self.end_at.map(|e| e <= now).unwrap_or(false) {
            PomoMode::Done
        } else {
            self.mode
        }
    }

    /// 进度百分比（`pm-fill` 的宽度）
    pub fn pct(&self, now: Instant) -> f32 {
        let total = self.total_ms();
        if total <= 0.0 {
            return 0.0;
        }
        (((total - self.remain(now)) / total) * 100.0).clamp(0.0, 100.0) as f32
    }

    /// `mmss` 的逐条复刻：向上取整到秒，两位分钟 + 两位秒。
    pub fn mmss(&self, now: Instant) -> [u8; 4] {
        let secs = (self.remain(now) / 1000.0).ceil().max(0.0) as u64;
        let m = (secs / 60).min(99) as u8;
        let s = (secs % 60) as u8;
        [m / 10, m % 10, s / 10, s % 10]
    }

    pub fn start(&mut self, now: Instant) {
        self.mode = PomoMode::Run;
        self.end_at = Some(now + Duration::from_millis(self.remain_ms.max(1.0) as u64));
    }

    pub fn toggle(&mut self, now: Instant) {
        match self.mode {
            PomoMode::Run => {
                self.remain_ms = self.remain(now);
                self.end_at = None;
                self.mode = PomoMode::Paused;
            }
            PomoMode::Paused => {
                self.mode = PomoMode::Run;
                self.end_at = Some(now + Duration::from_millis(self.remain_ms.max(1.0) as u64));
            }
            _ => {}
        }
    }

    pub fn reset(&mut self) {
        self.mode = PomoMode::Set;
        self.remain_ms = self.total_ms();
        self.end_at = None;
    }

    /// 每帧喂「现在」：把 mm:ss 推成滚动条。
    pub fn tick(&mut self, now: Instant) {
        if !matches!(self.mode, PomoMode::Run | PomoMode::Paused) {
            return;
        }
        let want = self.mmss(now);
        for i in 0..4 {
            if want[i] == self.shown[i] {
                continue;
            }
            let cur = self.rolls[i]
                .map(|r| r.at(now))
                .unwrap_or(cell(self.shown[i]));
            // 倒计时只有「往下走一格」才滚（0→9 走开头补格）；跨多位直接落位 ——
            // 25:00 → 24:59 的秒十位是 0→5（跳 5 格），以前会绕补格闪一下 9 → 看上去像 99。
            let down_one = crate::roll::step_delta(self.shown[i], want[i]) == 9;
            self.rolls[i] = if down_one {
                Some(if want[i] == 9 {
                    Roll::wrap(cur, 0.0, cell(want[i]), now)
                } else {
                    Roll::new(cur, cell(want[i]), now)
                })
            } else {
                None
            };
            self.shown[i] = want[i];
        }
    }

    pub fn animating(&self, now: Instant) -> bool {
        self.rolls.iter().flatten().any(|r| !r.done(now))
    }

    fn idx(&self, i: usize, now: Instant) -> f32 {
        match &self.rolls[i] {
            Some(r) if !r.done(now) => r.at(now),
            _ => cell(self.shown[i]),
        }
    }
}

impl Default for Pomo {
    fn default() -> Self {
        Self::new()
    }
}

pub fn paint(
    g: &Gfx,
    te: &TextEngine,
    pal: &Palette,
    t: &Tokens,
    p: &Pomo,
    now: Instant,
    glass: Option<(&ID2D1Bitmap1, D2D_RECT_F)>,
) -> Result<()> {
    let mode = p.eff_mode(now);
    let tree = tree(t, p, mode, now);
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

        // 进度条底与填充（id 6000 = 底、6001 = 填充；按钮 id 6100+i）
        let fill_pct = p.pct(now);
        paint_tree(g, te, pal, t, p, &placed, mode, now, fill_pct)?;
        g.pop_clip();
        g.end()?;
        Ok(())
    }
}

fn text_node(s: &str, size: f32, lh: f32, weight: u16, ls: f32) -> Node {
    Node::text(TextSpec {
        s: s.to_string(),
        size,
        line_h: lh,
        weight,
        wrap: false,
        align: 0,
        tabular: true,
        spacing_em: ls,
    })
}

fn tree(t: &Tokens, p: &Pomo, mode: PomoMode, _now: Instant) -> Node {
    let gap = if mode == PomoMode::Set {
        t.gap_set
    } else {
        t.gap_run
    };

    // 头：min-height 撑住，文字自身居中且上移 0.5px
    let head = Node::new(Dir::Row)
        .style(|s| {
            // 头高 = max(min-height 17, 文字行盒)：CSS 里 min-height 只是下限，
            // 14px/1.4 的「准备专注」行盒 19.6 才是实际高度
            s.size(Dim::Pct(1.0), Dim::Px(t.head_min_h.max(t.phase_lh)))
                .align(Align::Center)
                .justify(Justify::Center)
        })
        .child(
            text_node(mode.phase(), t.phase_fs, t.phase_lh, 400, 0.0)
                .id(6002)
                .style(|s| s.mar([t.phase_dy, 0.0, 0.0, 0.0])),
        );

    // hero：− 25 +
    let flank = |s: &str, id: u32| {
        text_node(s, t.flank_fs, t.flank_lh, 400, 0.0)
            .id(id)
            .style(|s| {
                s.size(Dim::Px(t.flank_w), Dim::Px(t.flank_w))
                    .align(Align::Center)
                    .justify(Justify::Center)
            })
    };
    let hero = Node::new(Dir::Row)
        .style(|s| {
            s.align(Align::Center)
                .justify(Justify::Center)
                .gap(t.hero_gap)
        })
        .child(flank("−", 6100))
        .child(
            text_node(&p.dur.to_string(), t.num_fs, t.num_lh, 600, t.spacing_em)
                .id(6003)
                .style(|s| {
                    s.size(Dim::Px(t.num_min_w), Dim::Px(t.num_lh))
                        .align(Align::Center)
                        .justify(Justify::Center)
                }),
        )
        .child(flank("+", 6101));

    // 时间：五位（四位数字 + 冒号占位），数字位由 roll 画
    let time = {
        let mut row = Node::new(Dir::Row)
            .id(6010)
            .style(|s| s.align(Align::Center).justify(Justify::Center));
        for i in 0..4 {
            let id = 4000 + i as u32;
            row = row.child(
                text_node("0", t.num_fs, t.num_lh, 600, t.spacing_em)
                    .id(id)
                    .style(|s| s.size(Dim::Px(t.num_fs * 0.62), Dim::Px(t.num_lh))),
            );
            if i == 1 {
                row = row.child(
                    text_node(":", t.num_fs, t.num_lh, 600, t.spacing_em)
                        .id(6004)
                        .style(|s| s.size(Dim::Px(t.num_fs * 0.42), Dim::Px(t.num_lh))),
                );
            }
        }
        row
    };

    // 进度条：底 + 一条 fill（宽度由 paint 阶段按百分比画）
    let track = Node::new(Dir::Row)
        .id(6000)
        .style(|s| s.size(Dim::Pct(1.0), Dim::Px(t.bar_h)));

    // 按钮组：按模式决定几个
    let (labels, acts_idx): (Vec<&str>, Vec<u32>) = match mode {
        PomoMode::Set => (vec!["开始"], vec![0]),
        PomoMode::Run => (vec!["暂停", "重设"], vec![1, 2]),
        PomoMode::Paused => (vec!["继续", "重设"], vec![1, 2]),
        PomoMode::Done => (vec!["回到设定"], vec![3]),
    };
    let acts_mt = if mode == PomoMode::Set {
        0.0
    } else {
        0.375 * t.r
    };
    let mut acts = Node::new(Dir::Row).style(move |s| {
        s.size(Dim::Pct(1.0), Dim::Px(t.btn_h))
            .gap(t.acts_gap)
            .align(Align::Stretch)
            .mar([acts_mt, 0.0, 0.0, 0.0])
    });
    for (i, l) in labels.iter().enumerate() {
        acts = acts.child(
            text_node(l, t.btn_fs, t.btn_h, 400, 0.0)
                .id(6200 + acts_idx[i])
                .style(|s| {
                    s.grow(1.0)
                        .basis(0.0)
                        .align(Align::Center)
                        .justify(Justify::Center)
                }),
        );
    }

    let mut body = Node::new(Dir::Column)
        .style(move |s| {
            s.grow(1.0)
                .basis(0.0)
                .justify(Justify::Center)
                .gap(gap)
                .align(Align::Stretch)
                // `.card-body` 自带 2px 下内边距（滚动余量），居中时它让整栈上移 1px
                .pad([0.0, 0.0, 2.0, 0.0])
        })
        .child(head);
    if mode == PomoMode::Set {
        body = body.child(hero);
    } else {
        body = body.child(time).child(track);
    }
    body = body.child(acts);

    Node::new(Dir::Column)
        .style(|s| {
            s.size(Dim::Px(t.w), Dim::Px(t.h))
                .pad([t.pad, t.pad, t.pad, t.pad])
                .align(Align::Stretch)
        })
        .child(body)
}

/// 鼠标是否落在某个矩形里（卡片内坐标）
fn hovering(g: &Gfx, r: &minicard_draw::layout::Rect) -> bool {
    match g.hover {
        Some((x, y)) => x >= r.x && y >= r.y && x < r.x + r.w && y < r.y + r.h,
        None => false,
    }
}

fn paint_tree(
    g: &Gfx,
    te: &TextEngine,
    pal: &Palette,
    t: &Tokens,
    p: &Pomo,
    pl: &Placed,
    mode: PomoMode,
    now: Instant,
    fill_pct: f32,
) -> Result<()> {
    // 进度条：先画底，再画填充（fill 的宽度 = 51.2px * pct）
    if pl.id == 6000 {
        let r = pl.rect;
        set_color(g, pal.track)?;
        fill_round(
            g,
            D2D_RECT_F {
                left: r.x,
                top: r.y,
                right: r.right(),
                bottom: r.bottom(),
            },
            t.bar_h / 2.0,
        )?;
        let w = r.w * fill_pct / 100.0;
        if w > 0.0 {
            set_color(g, pal.fill)?;
            fill_round(
                g,
                D2D_RECT_F {
                    left: r.x,
                    top: r.y,
                    right: r.x + w,
                    bottom: r.bottom(),
                },
                t.bar_h / 2.0,
            )?;
        }
        return Ok(());
    }

    // 数字位
    if (4000..4010).contains(&pl.id) {
        let i = (pl.id - 4000) as usize;
        let spec = TextSpec {
            s: String::new(),
            size: t.num_fs,
            line_h: t.num_lh,
            weight: 600,
            wrap: false,
            align: 0,
            tabular: true,
            spacing_em: t.spacing_em,
        };
        let col = if mode == PomoMode::Done {
            pal.muted
        } else {
            pal.fg
        };
        return draw_digit(
            g,
            te,
            pal,
            pl.rect.x,
            pl.rect.y,
            pl.rect.w,
            t.num_lh,
            p.idx(i, now),
            &spec,
            col,
        );
    }

    // 按钮：1px 26% 描边 + 文字
    if (6100..6200).contains(&pl.id) && hovering(g, &pl.rect) {
        // hover：−/+ 从 fg55 提到 fg（CSS `.pm-flank:hover { color: var(--fg) }`）
        // 文字在下面统一画，这里只记一个"要提亮"的标记
    }
    if (6200..6300).contains(&pl.id) {
        // 边框必须落在设备像素上：CSS 会把 1px 边框吸附到整行（参考图 y=81 是纯色 82），
        // 跟着分数坐标画会摊成两行各 13%，看着像「没有边框」。
        let px = pl.rect.x.round();
        let py = pl.rect.y.round();
        let pw = pl.rect.w.round();
        let ph = pl.rect.h.round();
        let hov = hovering(g, &pl.rect);
        if hov {
            set_color(g, pal.fg_soft)?;
            fill_round(
                g,
                D2D_RECT_F {
                    left: px + 0.5,
                    top: py + 0.5,
                    right: px + pw - 0.5,
                    bottom: py + ph - 0.5,
                },
                6.0, // 按钮圆角（CSS 0.375rem @16px = 6）
            )?;
        }
        set_color(g, if hov { pal.fg44 } else { pal.badge_border })?;
        stroke_round(
            g,
            D2D_RECT_F {
                left: px + 0.5,
                top: py + 0.5,
                right: px + pw - 0.5,
                bottom: py + ph - 0.5,
            },
            6.0,
            1.0,
        )?;
    }

    if let Some(spec) = pl.text() {
        // 「−」/「+」用 55% 前景；阶段文字与按钮文字满色
        let color = match pl.id {
            6100 | 6101 => pal.fg55,
            6002 => pal.muted,
            _ => pal.fg,
        };
        let lay = te.layout_spec(spec, pl.rect.w, spec.line_h + 8.0, TextMode::Clip)?;
        let id = pl.id;
        if id == 6200 + 1 || id == 6200 + 2 || id == 6200 || id == 6200 + 3 {
            set_color(g, pal.fg)?;
        } else {
            set_color(g, color)?;
        }
        // 文字在自己的盒里居中（模拟 text-align:center）
        let tw = te.measure(spec, 0.0).0;
        let dx = ((pl.rect.w - tw) / 2.0).max(0.0);
        // 按钮里 `place-items:center` 居中的是**行盒**，这里用字面框把文字摆到视觉中线
        // DWrite 在给足行盒高度时自己会把行放进盒里，不需要再补偏移
        // 按钮文字同样吸附整像素（参考图墨迹 y 86..98，不吸附会低 1px）
        let dy = if (6100..6200).contains(&id) {
            // `.pm-flank` 是 28×28 的 grid 盒、`place-items:center` 居中的是**行盒**，
            // 而且它的内容盒要扣掉 padding-bottom 0.15rem(2.4)：
            // (28 − 2.4 − 16) / 2 = 4.8 —— 少了这一档 −/+ 会整体高 4.8px
            (pl.rect.h - t.flank_pad_b - spec.line_h) / 2.0
        } else if (6200..6300).contains(&id) {
            pl.rect.y.round() - pl.rect.y
        } else {
            0.0
        };
        unsafe {
            g.d2d.DrawTextLayout(
                Vector2 {
                    X: pl.rect.x + dx,
                    Y: pl.rect.y + dy,
                },
                &lay,
                &g.brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
            );
        }
    }
    for c in &pl.children {
        paint_tree(g, te, pal, t, p, c, mode, now, fill_pct)?;
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
        "{}id={:<5} {:<20} x={:7.2} y={:7.2} w={:7.2} h={:7.2}",
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

pub fn tree_hook(t: &Tokens, p: &Pomo, mode: PomoMode, now: Instant) -> Node {
    tree(t, p, mode, now)
}

pub fn paint_static(
    g: &Gfx,
    te: &TextEngine,
    pal: &Palette,
    t: &Tokens,
    p: &Pomo,
    now: Instant,
) -> Result<()> {
    paint(g, te, pal, t, p, now, None)
}

const _: () = {
    let _ = ROLL_MS;
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mmss_向上取整到秒() {
        let now = Instant::now();
        let mut p = Pomo::new();
        p.dur = 25;
        // 设定态：剩满 25 分钟
        assert_eq!(p.mmss(now), [2, 5, 0, 0]);
        // 运行中剩 59.2 秒 → 向上取整 60 → 01:00
        p.mode = PomoMode::Run;
        p.end_at = Some(now + Duration::from_millis(59_200));
        assert_eq!(p.mmss(now), [0, 1, 0, 0]);
        // 剩 0 → 00:00
        p.end_at = Some(now - Duration::from_millis(1));
        assert_eq!(p.mmss(now), [0, 0, 0, 0]);
    }

    #[test]
    fn 进度百分比从零到百() {
        let now = Instant::now();
        let mut p = Pomo::new();
        assert!((p.pct(now) - 0.0).abs() < 0.01, "设定态应为 0");
        p.mode = PomoMode::Run;
        p.end_at = Some(now + Duration::from_millis(p.total_ms() as u64 / 2));
        assert!(
            (p.pct(now) - 50.0).abs() < 0.5,
            "过半应为 50，实际 {}",
            p.pct(now)
        );
        p.end_at = Some(now);
        assert!((p.pct(now) - 100.0).abs() < 0.01);
    }

    #[test]
    fn 运行到点自动完成() {
        let now = Instant::now();
        let mut p = Pomo::new();
        p.dur = 5;
        p.remain_ms = 5.0 * 60000.0;
        p.start(now);
        assert_eq!(p.eff_mode(now), PomoMode::Run);
        let later = now + Duration::from_secs(301);
        assert_eq!(p.eff_mode(later), PomoMode::Done, "到点应自动 done");
        assert_eq!(p.remain(later), 0.0);
        assert!((p.pct(later) - 100.0).abs() < 0.01);
    }

    #[test]
    fn 暂停与继续保住剩余时间() {
        let now = Instant::now();
        let mut p = Pomo::new();
        p.dur = 25;
        p.start(now);
        let t1 = now + Duration::from_secs(60);
        p.toggle(t1);
        assert_eq!(p.mode, PomoMode::Paused);
        let left = p.remain_ms;
        assert!(
            (left - 24.0 * 60000.0).abs() < 1000.0,
            "暂停应记住约 24 分钟，实际 {left}"
        );
        // 暂停期间不流逝
        let t2 = t1 + Duration::from_secs(30);
        assert_eq!(p.remain(t2), left);
        p.toggle(t2);
        assert_eq!(p.mode, PomoMode::Run);
        assert!((p.remain(t2) - left).abs() < 1.0);
    }

    #[test]
    fn 秒数变化才有滚动() {
        let now = Instant::now();
        let mut p = Pomo::new();
        p.mode = PomoMode::Run;
        p.end_at = Some(now + Duration::from_secs(60));
        p.shown = [0, 1, 0, 0];
        p.tick(now);
        assert!(!p.animating(now), "秒数没变不该起滚动");
        // 到 00:59 只剩一位变
        p.end_at = Some(now + Duration::from_millis(59_000));
        p.tick(now);
        assert!(p.rolls[3].is_some(), "个位应起滚动");
        assert!(p.animating(now));
    }
}

/// 番茄钟可点区域
pub enum Hit {
    /// −（缩短 5 分钟）
    Minus,
    /// +（延长 5 分钟）
    Plus,
    /// 底部动作按钮（0 开始 / 1 暂停-继续 / 2 重设 / 3 回到设定）
    Act(u32),
}

pub fn hit(te: &TextEngine, t: &Tokens, p: &Pomo, now: Instant, x: f32, y: f32) -> Option<Hit> {
    let tr = tree(t, p, p.mode, now);
    let mut m: &TextEngine = te;
    let pl = minicard_draw::layout::layout(&tr, (t.w, t.h), &mut m);
    let id = minicard_draw::layout::hit_id(&pl, x, y)?;
    match id {
        6100 => Some(Hit::Minus),
        6101 => Some(Hit::Plus),
        _ if (6200..6300).contains(&id) => Some(Hit::Act(id - 6200)),
        _ => None,
    }
}

#[cfg(test)]
mod countdown_roll_tests {
    use super::*;
    use crate::roll::cell_char;
    use std::time::Duration;

    /// 倒计时每次「减一」都要**往下**滚一格：30→29 的个位是 0→9，
    /// 只许经过补的 9（格 0），不能顺着 1..8 一路走 —— 那就是「先跳到 9」。
    /// 25:00 → 24:59：秒十位 0→5 是跨 5 格，必须**直接落位**，
    /// 不许绕补格闪一下 9（那会显示成 24:99）。
    #[test]
    fn 秒十位跨多位直接落位() {
        let t0 = Instant::now();
        let mut p = Pomo::new();
        p.start(t0);
        let t1 = t0 + Duration::from_millis(1000);
        p.tick(t1);
        let mut seen = Vec::new();
        for k in 0..=20 {
            let now = t1 + Duration::from_millis(k * 15);
            let idx = p.idx(2, now);
            let ch = cell_char(idx.round() as i32);
            if seen.last() != Some(&ch) {
                seen.push(ch);
            }
        }
        assert_eq!(
            seen,
            vec!['5'],
            "秒十位轨迹 {:?}（应直接是 5，不许经过 9）",
            seen
        );
    }

    #[test]
    fn 倒计时减一不绕远路() {
        let t0 = Instant::now();
        let mut p = Pomo::new();
        p.mode = PomoMode::Run;
        p.remain_ms = 30_000.0;
        p.end_at = Some(t0 + Duration::from_millis(30_000));
        p.tick(t0);
        let t1 = t0 + Duration::from_millis(1000); // 30s → 29s
        p.tick(t1);
        let mut seen = Vec::new();
        for k in 0..=20 {
            let now = t1 + Duration::from_millis(k * 15);
            let idx = p.idx(3, now);
            let ch = cell_char(idx.round() as i32);
            if seen.last() != Some(&ch) {
                seen.push(ch);
            }
        }
        assert_eq!(
            seen,
            vec!['0', '9'],
            "个位轨迹 {:?}（应 0→9，不许经过 1..8）",
            seen
        );
        let end = t1 + Duration::from_millis(400);
        let idx = p.idx(3, end);
        assert!(
            (idx - 10.0).abs() < 0.01,
            "滚完应归到 9 的规范格 10，实得 {idx}"
        );
    }
}
