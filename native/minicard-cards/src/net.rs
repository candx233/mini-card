//! 网络卡：`web/card.html` 的 `.card-net` / `.net-*` 搬到 D2D（120×120）。
//!
//! 排版口径（`card.html` 七改之后）：
//! - 两行 `flex:1 1 0` 各占半卡、行内垂直居中 → 数值「到卡边」与「到分隔线」等距（30/30）；
//! - 分隔线不挂在行上，画在 rows 盒的 50%（y = 60），走 padding box 所以恒在正中；
//! - 箭头是 22px 白字装进 12px **固定列**（x 恒为左 padding），并用 `left:-0.4375rem`
//!   把墨迹整体左移 —— CSS 的相对定位**不影响布局**，所以这里画的时候减偏移、布局不占位。
//!
//! 数值补间在字节空间做（`barEase` 梯形速度、320ms），每帧再格式化；换单位（KB/s↔MB/s）时
//! 计数中跨过去，不会「一换单位就瞬跳」。

use std::time::Instant;

use windows::core::Result;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::{
    ID2D1Bitmap1, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_INTERPOLATION_MODE_LINEAR,
};
use windows_numerics::Vector2;

use minicard_draw::color::Palette;
use minicard_draw::layout::{layout, Align, Dim, Dir, Justify, Node, Placed, TextSpec};
use minicard_draw::paint::{fill_rect, fill_round, set_color, stroke_round};
use minicard_draw::text::TextEngine;
use minicard_shell::gfx::Gfx;

/// 补间时长（与 card.html 的 `netTick` 一致）
pub const TWEEN_MS: f32 = 320.0;

pub struct Tokens {
    pub w: f32,
    pub h: f32,
    pub r: f32,
    pub radius: f32,
    pub pad_lr: f32,
    pub row_gap: f32,
    pub arrow_w: f32,
    pub arrow_fs: f32,
    pub arrow_lh: f32,
    pub arrow_shift: f32,
    pub val_fs: f32,
    pub val_lh: f32,
    pub unit_fs: f32,
    pub unit_lh: f32,
    pub unit_gap: f32,
}

impl Tokens {
    /// 纯文字卡：k = min(w/120, h/120)（BASE 120×120，方形基准）。
    pub fn net(w: f32, h: f32, asc: f32, desc: f32) -> Self {
        let k = (w / 120.0).min(h / 120.0).clamp(0.4, 2.5);
        let r = 16.0 * k;
        Self {
            w,
            h,
            r,
            radius: 1.0 * r,
            pad_lr: 0.5 * r,
            row_gap: 0.0625 * r,
            arrow_w: 0.75 * r,
            arrow_fs: 1.375 * r,
            arrow_lh: 1.375 * r * 1.3,
            arrow_shift: 0.4375 * r,
            val_fs: 2.375 * r,
            val_lh: 2.375 * r,
            unit_fs: 0.6875 * r,
            // 单位是行内 `<i>`，盒高取字面框（不是行高）—— CSS 的 inline box 就是字体框
            unit_lh: 0.6875 * r * (asc + desc),
            unit_gap: 0.0625 * r,
        }
    }
}

/// `fmtRate` 的逐条复刻：返回 (数值串, 单位)。口径 = 数值最多 3 位。
pub fn fmt_rate(bps: f64) -> (String, &'static str) {
    let v = bps.max(0.0);
    let mb = v / 1048576.0;
    if mb >= 1000.0 {
        return (format!("{:.1}", v / 1073741824.0), "GB/s");
    }
    if mb >= 10.0 {
        return (format!("{}", mb.round() as i64), "MB/s");
    }
    if mb >= 0.05 {
        return (format!("{:.1}", mb), "MB/s");
    }
    let kb = v / 1024.0;
    if kb >= 1.0 {
        return (format!("{}", kb.round() as i64), "KB/s");
    }
    ("0".to_string(), "B/s")
}

/// 字节空间的梯形速度补间（与 `barEase` 同一套）：起步 v=0、主体匀速、收尾渐停。
pub struct Tween {
    pub from: f64,
    pub to: f64,
    pub t0: Instant,
}

impl Tween {
    pub fn new(from: f64, to: f64, now: Instant) -> Self {
        Self { from, to, t0: now }
    }

    pub fn value(&self, now: Instant) -> f64 {
        let u = ((now - self.t0).as_secs_f32() * 1000.0 / TWEEN_MS).clamp(0.0, 1.0);
        // 梯形速度：s(u) = 2u² (u<.5) / 1-2(1-u)² (u≥.5)
        let k = if u < 0.5 { 2.0 * u * u } else { 1.0 - 2.0 * (1.0 - u) * (1.0 - u) };
        self.from + (self.to - self.from) * k as f64
    }

    pub fn live(&self, now: Instant) -> bool {
        (now - self.t0).as_secs_f32() * 1000.0 < TWEEN_MS
    }
}

pub struct NetState {
    pub up: Tween,
    pub down: Tween,
}

impl NetState {
    pub fn new(up: f64, down: f64, now: Instant) -> Self {
        Self {
            up: Tween::new(up, up, now),
            down: Tween::new(down, down, now),
        }
    }

    /// 喂新采样：目标变了就**从当前显示值**重新起补间（不从目标重来）。
    pub fn set(&mut self, up: f64, down: f64, now: Instant) {
        if (self.up.to - up).abs() > 0.5 {
            let from = self.up.value(now);
            self.up = Tween::new(from, up, now);
        }
        if (self.down.to - down).abs() > 0.5 {
            let from = self.down.value(now);
            self.down = Tween::new(from, down, now);
        }
    }

    pub fn animating(&self, now: Instant) -> bool {
        self.up.live(now) || self.down.live(now)
    }
}

pub fn paint(
    g: &Gfx,
    te: &TextEngine,
    pal: &Palette,
    t: &Tokens,
    st: &NetState,
    now: Instant,
    glass: Option<(&ID2D1Bitmap1, D2D_RECT_F)>,
) -> Result<()> {
    let (un, uu) = fmt_rate(st.up.value(now));
    let (dn, du) = fmt_rate(st.down.value(now));
    let tree = tree(t, &un, uu, &dn, du, pal);
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

        paint_tree(g, te, pal, t, &placed)?;

        // 分隔线：rows 盒的 50%，走 padding box（1px）
        set_color(g, pal.border)?;
        let y = t.h / 2.0;
        fill_rect(
            g,
            D2D_RECT_F { left: t.pad_lr, top: y, right: t.w - t.pad_lr, bottom: y + 1.0 },
        )?;

        g.pop_clip();
        g.end()?;
        Ok(())
    }
}

/// id：5000+i = 第 i 行的箭头（绘制时左移），其余普通文字。
fn tree(t: &Tokens, up_n: &str, up_u: &str, dn_n: &str, dn_u: &str, pal: &Palette) -> Node {
    let _ = pal;
    let row = |arrow: &str, idx: u32, num: &str, unit: &str| {
        let a = Node::text(TextSpec {
            s: arrow.to_string(),
            size: t.arrow_fs,
            line_h: t.arrow_lh,
            weight: 400,
            wrap: false,
            align: 0,
            tabular: false,
            spacing_em: 0.0,
        })
        .id(5000 + idx)
        .style(|s| s.size(Dim::Px(t.arrow_w), Dim::Auto));
        let n = Node::text(TextSpec {
            s: num.to_string(),
            size: t.val_fs,
            line_h: t.val_lh,
            weight: 600,
            wrap: false,
            align: 0,
            tabular: true,
            spacing_em: -0.02,
        });
        let u = Node::text(TextSpec {
            s: unit.to_string(),
            size: t.unit_fs,
            line_h: t.unit_lh,
            weight: 400,
            wrap: false,
            align: 0,
            tabular: false,
            spacing_em: 0.02,
        })
        .style(|s| s.mar([0.0, 0.0, 0.0, t.unit_gap]).self_align(Align::Baseline));
        Node::new(Dir::Row)
            .style(|s| {
                s.grow(1.0)
                    .basis(0.0)
                    .align(Align::Center)
                    .justify(Justify::Start)
                    .gap(t.row_gap)
            })
            .child(a)
            .child(Node::new(Dir::Row).style(|s| s.align(Align::Baseline)).child(n).child(u))
    };

    Node::new(Dir::Column)
        .style(|s| {
            s.size(Dim::Px(t.w), Dim::Px(t.h))
                .pad([0.0, t.pad_lr, 0.0, t.pad_lr])
        })
        .child(
            Node::new(Dir::Column)
                .style(|s| s.grow(1.0).basis(0.0).justify(Justify::Between).gap(0.0))
                .child(row("↑", 0, up_n, up_u))
                .child(row("↓", 1, dn_n, dn_u)),
        )
}

fn paint_tree(g: &Gfx, te: &TextEngine, pal: &Palette, t: &Tokens, p: &Placed) -> Result<()> {
    if let Some(spec) = p.text() {
        let fmt = te.format_spec(spec, minicard_draw::text::Mode::Clip)?;
        let _ = fmt;
        let lay = te.layout_spec(spec, p.rect.w + 4.0, spec.line_h + 8.0, minicard_draw::text::Mode::Clip)?;
        // 箭头：CSS 的相对定位只挪墨迹、不占布局 → 这里画的时候左移
        let dx = if (5000..5100).contains(&p.id) { -t.arrow_shift } else { 0.0 };
        let muted = (5000..5100).contains(&p.id) == false && spec.size == t.unit_fs;
        set_color(g, if muted { pal.muted } else { pal.fg })?;
        unsafe {
            g.d2d.DrawTextLayout(
                Vector2 { X: p.rect.x + dx, Y: p.rect.y },
                &lay,
                &g.brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
            );
        }
    }
    for c in &p.children {
        paint_tree(g, te, pal, t, c)?;
    }
    Ok(())
}

/// 布局转储（几何对表用）
pub fn dump_layout(p: &Placed, depth: usize) {
    let pad = " ".repeat(depth * 2);
    let role = if (5000..5100).contains(&p.id) {
        format!("arrow#{}", p.id - 5000)
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

pub fn tree_hook(t: &Tokens, un: &str, uu: &str, dn: &str, du: &str) -> Node {
    tree(t, un, uu, dn, du, &Palette::new(0.0, false))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 速率格式化的档位() {
        // <1KB/s → B/s 兜底 0
        assert_eq!(fmt_rate(10.0), ("0".to_string(), "B/s"));
        // 1KB..约 51KB（mb < 0.05）走整 KB/s —— 注意 ≥0.05MB 就进 MB 档了，
        // 所以「999 KB/s」这种读数永远不会出现（JS 原版同样如此）
        assert_eq!(fmt_rate(2048.0), ("2".to_string(), "KB/s"));
        assert_eq!(fmt_rate(1024.0 * 50.0), ("50".to_string(), "KB/s"));
        assert_eq!(fmt_rate(1024.0 * 51.3), ("0.1".to_string(), "MB/s"));
        // 0.05MB..10MB 保留一位
        assert_eq!(fmt_rate(1048576.0 * 1.25), ("1.2".to_string(), "MB/s"));
        assert_eq!(fmt_rate(1048576.0 * 9.94), ("9.9".to_string(), "MB/s"));
        // ≥10MB 取整（保证数值 ≤3 位）
        assert_eq!(fmt_rate(1048576.0 * 10.4), ("10".to_string(), "MB/s"));
        assert_eq!(fmt_rate(1048576.0 * 999.6), ("1000".to_string(), "MB/s"));
        // ≥1000MB 切 GB/s
        assert_eq!(fmt_rate(1073741824.0 * 1.5), ("1.5".to_string(), "GB/s"));
    }

    #[test]
    fn 补间在字节空间且从当前值起步() {
        let t0 = Instant::now();
        let mut st = NetState::new(0.0, 0.0, t0);
        st.set(1_000_000.0, 0.0, t0);
        // 中途把目标改成另一个值：起点必须是「当前显示值」，不是旧目标
        let mid = t0 + std::time::Duration::from_millis(160);
        let shown = st.up.value(mid);
        assert!(shown > 0.0 && shown < 1_000_000.0, "中途值 {shown}");
        st.set(2_000_000.0, 0.0, mid);
        assert!((st.up.from - shown).abs() < 1.0, "应从 {} 起步，实际 {}", shown, st.up.from);
        // 走完落在新目标
        let done = mid + std::time::Duration::from_millis(400);
        assert!((st.up.value(done) - 2_000_000.0).abs() < 1.0);
        assert!(!st.animating(done));
    }

    #[test]
    fn 换单位时数值连续() {
        let t0 = Instant::now();
        let mut st = NetState::new(900_000.0, 0.0, t0);
        // 从 0.86MB/s 补到 1.1MB/s：中途跨过 1.0MB 这个单位关口
        st.set(1_100_000.0, 0.0, t0);
        let mut prev = None;
        let mut jumps = 0;
        for i in 0..40 {
            let now = t0 + std::time::Duration::from_millis(i * 8);
            let (n, _u) = fmt_rate(st.up.value(now));
            let v: f64 = n.parse().unwrap_or(0.0);
            if let Some(p) = prev {
                // 数值本身允许单调增大；「瞬跳」指的是超过总行程的一次性大跳
                if v - p > 0.5 {
                    jumps += 1;
                }
            }
            prev = Some(v);
        }
        assert_eq!(jumps, 0, "逐帧数值出现 {jumps} 次大跳");
    }
}
