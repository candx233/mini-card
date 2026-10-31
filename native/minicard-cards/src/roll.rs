//! 逐位滚动的共用件：时钟卡的分钟、番茄钟的倒计时都走这一套。
//!
//! 口径与 `card.html::rollSet` 同源：
//! - 每位一根「条」，条上是 `[9,0,1,…,9,0]` 十二格（前后各补一格，跨 9→0 不用跳）；
//! - 位移 `-idx em`，`idx = from + (to-from)·(1-(1-u)^4)`（easeOutQuart，300ms）；
//! - 只画可见的两格（`floor(idx)` 与 `+1`），不是把十二格都画一遍再裁。

use std::time::Instant;

use windows::core::Result;
use windows::Win32::Graphics::Direct2D::Common::{D2D1_COLOR_F, D2D_RECT_F};
use windows::Win32::Graphics::Direct2D::D2D1_DRAW_TEXT_OPTIONS_NONE;
use windows_numerics::Vector2;

use minicard_draw::layout::TextSpec;
use minicard_draw::paint::{push_clip, set_color};
use minicard_draw::text::{Mode, TextEngine};
use minicard_shell::gfx::Gfx;

/// 滚动时长（与 CSS 的 `--dur-roll` 一致）
pub const ROLL_MS: f32 = 300.0;

/// 一位数字的一次滚动。
#[derive(Clone, Copy, Debug)]
pub struct Roll {
    pub from: f32,
    pub to: f32,
    pub t0: Instant,
    pub ms: f32,
}

impl Roll {
    pub fn new(from: f32, to: f32, t0: Instant) -> Self {
        Self { from, to, t0, ms: ROLL_MS }
    }

    pub fn at(&self, now: Instant) -> f32 {
        let u = ((now - self.t0).as_secs_f32() * 1000.0 / self.ms).clamp(0.0, 1.0);
        let k = 1.0 - (1.0 - u).powi(4);
        self.from + (self.to - self.from) * k
    }

    pub fn done(&self, now: Instant) -> bool {
        (now - self.t0).as_secs_f32() * 1000.0 >= self.ms
    }
}

/// 条上的格子序号：`[9,0,1..9,0]` → 数字 d 在 `d + 1` 格。
pub fn cell(d: u8) -> f32 {
    (d as f32) + 1.0
}

/// 第 `cell` 格上的字符（0 = 补的 9、11 = 补的 0）。
pub fn cell_char(cell: i32) -> char {
    match cell {
        0 => '9',
        11 => '0',
        c => char::from_digit(((c - 1).clamp(0, 9)) as u32, 10).unwrap_or('0'),
    }
}

/// 画一位：把条上的两格画进 `(x, y, w, cell_h)` 的裁剪盒里。
#[allow(clippy::too_many_arguments)]
pub fn draw_digit(
    g: &Gfx,
    te: &TextEngine,
    pal: &minicard_draw::color::Palette,
    x: f32,
    y: f32,
    w: f32,
    cell_h: f32,
    idx: f32,
    spec: &TextSpec,
    color: D2D1_COLOR_F,
) -> Result<()> {
    push_clip(
        g,
        D2D_RECT_F { left: x, top: y, right: x + w, bottom: y + cell_h },
    );
    set_color(g, color)?;
    let lo = idx.floor() as i32;
    for c in [lo, lo + 1] {
        let ch = cell_char(c);
        let mut s = spec.clone();
        s.s = ch.to_string();
        let lay = te.layout_spec(&s, w.max(1.0), cell_h + 8.0, Mode::Clip)?;
        let yy = y + (c as f32 - idx) * cell_h;
        unsafe {
            g.d2d.DrawTextLayout(
                Vector2 { X: x, Y: yy },
                &lay,
                &g.brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
            );
        }
    }
    minicard_draw::paint::pop_clip(g);
    let _ = pal;
    Ok(())
}
