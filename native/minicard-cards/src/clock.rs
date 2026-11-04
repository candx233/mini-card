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
use minicard_draw::text::{draw_shadow_taps, Mode, TextEngine, TextShadow};
use minicard_shell::gfx::Gfx;

use crate::roll::{cell, draw_digit_shadowed, Roll};

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
        Self {
            digits: [0; 4],
            rolls: [None; 4],
        }
    }

    /// 条上的格子序号（口径在 `roll::cell`）。
    fn cell(d: u8) -> f32 {
        cell(d)
    }

    /// 四位都没在滚 = 可以降频重绘
    pub fn idle(&self, now: Instant) -> bool {
        self.rolls.iter().all(|r| r.map_or(true, |r| r.done(now)))
    }

    /// 四位数字 → 当天的分钟数（用于判断这一拍时间往前走了多少）。
    fn minutes(d: [u8; 4]) -> i32 {
        let hh = (d[0] as i32 % 24) * 10 + d[1] as i32 % 10;
        let mm = (d[2] as i32 % 6) * 10 + d[3] as i32 % 10;
        (hh * 60 + mm).rem_euclid(24 * 60)
    }

    /// 喂新时间（hh, mm），返回是否有滚动在进行。
    ///
    /// 口径照 `card.html::rollSet(el, t, 300, dir=+1)`：
    /// - **正常走时**（这一拍只往前 0~2 分钟）时每一位都滚过去，数字**下降**（5→0、2→0、
    ///   3→0 这种跨零进位）走条尾补的 0 再瞬归 —— 浏览器就是这条路，不是瞬跳；
    /// - **跨多位**（系统校时 / 进程被挂起后第一次喂）直接落位：一口气滚 6~8 格只会糊成一片。
    pub fn set(&mut self, hh: u8, mm: u8, now: Instant) {
        let want = [hh / 10, hh % 10, mm / 10, mm % 10];
        let old = Self::minutes(self.digits);
        let new = (hh as i32 % 24) * 60 + mm as i32 % 60;
        // 0..1440 的「往前走了几分钟」：正常走秒是 0 或 1，跨零点也是 1
        let step = (new - old).rem_euclid(24 * 60);
        let normal = step <= 2;
        for i in 0..4 {
            if want[i] == self.digits[i] {
                continue;
            }
            // 上一段没走完就从当前位置接着走（与 CSS 版 `from = cur` 同口径）
            let cur = self.rolls[i]
                .map(|r| r.at(now))
                .unwrap_or(Self::cell(self.digits[i]));
            self.rolls[i] = if !normal {
                None
            } else if want[i] > self.digits[i] {
                // 顺走（含低位进位把这一位顶上去）：直接滚到目标格
                Some(Roll::new(cur, Self::cell(want[i]), now))
            } else {
                // 下降 = 跨零：走条尾补的 0（第 11 格），滚完瞬归规范格
                Some(Roll::wrap(cur, 11.0, Self::cell(want[i]), now))
            };
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
    })
    .id(3900);
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
        // 滚动数字同样吃 `.card` 的 text-shadow（原来完全没画，字像直接贴在玻璃上）
        return draw_digit_shadowed(
            g,
            te,
            p.rect.x,
            p.rect.y,
            p.rect.w,
            t.time_lh,
            clock.idx(i, now),
            &spec,
            pal.fg,
            Some(TextShadow::CARD.with_color(pal.shadow)),
        );
    } else if let Some(spec) = p.text() {
        // 冒号属于 .clock-time（--fg），不是日期/农历的 --muted
        let color = if p.id == 3900 { pal.fg } else { pal.muted };
        let lay = te.layout_spec(spec, p.rect.w, spec.line_h + 8.0, Mode::Clip)?;
        // CSS 是**模糊**投影（0 .833px 1.667px var(--tshadow)）：原来画的是 y+0.833 的
        // 硬边副本，字缘没有过渡、看着像双影。这里换成 3×3 高斯核的 9 次低透明度副本。
        draw_shadow_taps(
            g,
            &lay,
            p.rect.x,
            p.rect.y,
            &TextShadow::CARD.with_color(pal.shadow),
        )?;
        set_color(g, color)?;
        unsafe {
            g.d2d.DrawTextLayout(
                Vector2 {
                    X: p.rect.x,
                    Y: p.rect.y,
                },
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
        assert!(
            mid > r.from && mid < r.to,
            "中途 {mid} 应在 {}..{} 之间",
            r.from,
            r.to
        );
        // easeOutQuart：前 1/3 时间应走掉一半以上行程
        assert!(
            mid - r.from > (r.to - r.from) * 0.5,
            "前 40% 走了 {:?}",
            mid - r.from
        );
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

#[cfg(test)]
mod roll_tests {
    use super::*;
    use crate::roll::cell_char;
    use std::time::Duration;

    /// 每一次变化都必须**从原数字滚到目标数字**，且滚完停在规范格（`cell(d)`）。
    /// 早先的写法在数字变小时走 `to = cur + 1`：画面上先蹦到隔壁数字、随后瞬归目标，
    /// 看着就是「先跳到 9 再变」；而且索引会一路漂移，永远回不到规范格。
    #[test]
    fn 时钟每位都滚到目标并归位() {
        let t0 = Instant::now();
        let mut c = Clock::new();
        c.set(12, 59, t0);
        let t1 = t0 + Duration::from_millis(400); // 上一段结束
        c.set(13, 0, t1);
        let end = t1 + Duration::from_millis(500);
        let want = [1u8, 3, 0, 0];
        for i in 0..4 {
            let idx = c.idx(i, end);
            assert!(
                (idx - (want[i] as f32 + 1.0)).abs() < 0.01,
                "第 {i} 位滚完停在 {idx}，应在 {}（数字 {want:?}）",
                want[i] as f32 + 1.0
            );
        }
        // 个位 9→0：中途只能出现 9 和 0
        let mut seen = Vec::new();
        for k in 0..=20 {
            let now = t1 + Duration::from_millis(k * 15);
            let idx = c.idx(3, now);
            let ch = cell_char(idx.round() as i32);
            if seen.last() != Some(&ch) {
                seen.push(ch);
            }
        }
        assert_eq!(seen, vec!['9', '0'], "个位滚动轨迹 {:?}", seen);
    }

    /// 采样一整个 300ms 行程，返回「按动画进度看到的数字序列」（相邻重复合并）。
    fn 轨迹(c: &Clock, i: usize, t0: Instant) -> Vec<char> {
        let mut seen: Vec<char> = Vec::new();
        for k in 0..=60 {
            let now = t0 + Duration::from_millis(k * 5);
            let ch = cell_char(c.idx(i, now).round() as i32);
            if seen.last() != Some(&ch) {
                seen.push(ch);
            }
        }
        seen
    }

    /// 分钟十位 5→0（12:59 → 13:00）走的是末尾补格，滚完必须归到 0 那一格
    #[test]
    fn 分钟十位跨零归位() {
        let t0 = Instant::now();
        let mut c = Clock::new();
        c.set(12, 59, t0);
        let t1 = t0 + Duration::from_millis(400);
        c.set(13, 0, t1);
        let end = t1 + Duration::from_millis(500);
        let idx = c.idx(2, end);
        assert!(
            (idx - 1.0).abs() < 0.01,
            "分钟十位应归到 0（格 1），实得 {idx}"
        );
        // 5→0 在浏览器里是**滚过去**（走条尾补的 0），不是瞬跳：
        // 轨迹必须是 5→6→…→9→0 这条顺路，且不许蹦出别的数字。
        let tr = 轨迹(&c, 2, t1);
        assert_eq!(tr, vec!['5', '6', '7', '8', '9', '0'], "分钟十位轨迹 {tr:?}");
    }

    /// 23:59 → 00:00（跨天）：小时的两位都是「下降」—— 2→0、3→0，
    /// 同样要走补格滚过去，滚完各归到 0 的规范格。
    #[test]
    fn 跨零点小时位也滚过去并归位() {
        let t0 = Instant::now();
        let mut c = Clock::new();
        c.snap(23, 59);
        c.set(0, 0, t0);
        let end = t0 + Duration::from_millis(400);
        for i in 0..4 {
            assert!(
                c.rolls[i].is_some(),
                "第 {i} 位应有滚动（23:59 → 00:00 是正常走时）"
            );
            let idx = c.idx(i, end);
            assert!(
                (idx - 1.0).abs() < 0.01,
                "第 {i} 位滚完应归到 0（格 1），实得 {idx}"
            );
        }
        // 小时十位 2→0：轨迹 2→3→…→9→0（顺路，不含其它数字）
        let tr = 轨迹(&c, 0, t0);
        assert_eq!(
            tr,
            vec!['2', '3', '4', '5', '6', '7', '8', '9', '0'],
            "小时十位轨迹 {tr:?}"
        );
        // 分钟两位 59→00 与小时个位 3→0 同理：只走顺路
        let tr1 = 轨迹(&c, 1, t0);
        assert_eq!(tr1, vec!['3', '4', '5', '6', '7', '8', '9', '0'], "小时个位轨迹 {tr1:?}");
        let tr2 = 轨迹(&c, 2, t0);
        assert_eq!(tr2, vec!['5', '6', '7', '8', '9', '0'], "分钟十位轨迹 {tr2:?}");
        let tr3 = 轨迹(&c, 3, t0);
        assert_eq!(tr3, vec!['9', '0'], "分钟个位轨迹 {tr3:?}");
    }

    /// 系统改时间（一次跨好几分钟）**不能**滚：一口气扫 6 格只会糊成一片。
    /// 判据 = 没有 Roll、idx 直接落在目标格。
    #[test]
    fn 跨多位跳变直接落位() {
        let t0 = Instant::now();
        let mut c = Clock::new();
        c.snap(14, 26);
        c.set(8, 41, t0 + Duration::from_millis(400));
        assert!(
            c.rolls.iter().all(|r| r.is_none()),
            "跨多位不该建滚动：{:?}",
            c.rolls
        );
        let want = [0u8, 8, 4, 1];
        for i in 0..4 {
            let idx = c.idx(i, t0 + Duration::from_millis(500));
            assert!(
                (idx - (want[i] as f32 + 1.0)).abs() < 0.01,
                "第 {i} 位应直接落在 {}（格 {}），实得 {idx}",
                want[i],
                want[i] as f32 + 1.0
            );
        }
        // 往回跳一分钟也算跨多位（不是正常走时）
        let mut d = Clock::new();
        d.snap(14, 26);
        d.set(14, 25, t0 + Duration::from_millis(400));
        assert!(d.rolls.iter().all(|r| r.is_none()), "时间回退不该滚");
    }
}
