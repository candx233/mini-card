//! 音乐卡：`web/card.html` 的 `.card-music` / `.mc-*` 搬到 D2D（240×120）。
//!
//! 左封面（100×100 圆角 12 的 135° 强调色渐变 + 音符）· 右列（歌名 / 歌手 / 进度条 + 时间 /
//! 三个按钮）。图标全是 SVG 路径原文（音符、上一首、播放/暂停、下一首），走 `Path::svg`。

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::time::Instant;

use windows::core::Result;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::{
    ID2D1Bitmap1, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_INTERPOLATION_MODE_LINEAR,
};
use windows_numerics::Vector2;

use minicard_draw::color::Palette;
use minicard_draw::gradient;
use minicard_draw::layout::{layout, Align, Dim, Dir, Justify, Node, Placed, TextSpec};
use minicard_draw::paint::{fill_round, pop_clip, push_clip, set_color, stroke_path, stroke_round};
use minicard_draw::path::Path;
use minicard_draw::text::{draw_shadow_taps, Mode as TextMode, TextEngine, TextShadow};
use minicard_shell::gfx::Gfx;

#[derive(Clone, Debug)]
pub struct Music {
    pub title: String,
    pub artist: String,
    pub playing: bool,
    pub position_ms: u64,
    pub duration_ms: u64,
}

impl Music {
    pub fn pct(&self) -> f32 {
        if self.duration_ms == 0 {
            return 0.0;
        }
        let pos = self.position_ms.min(self.duration_ms);
        (pos as f32 / self.duration_ms as f32 * 100.0).clamp(0.0, 100.0)
    }

    /// `renderMusic` 里的 `posTxt`：分:秒，两位
    pub fn pos_text(&self) -> String {
        let secs = (self.position_ms.min(self.duration_ms.max(self.position_ms)) / 1000) as u64;
        format!("{:02}:{:02}", secs / 60, secs % 60)
    }
}

// ────────────────────────── 长文本跑马灯（musicMarquee） ──────────────────────────
//
// 口径逐条照抄 `web/card.html::musicMarquee`（2026-10-09 定稿版）：
// 两端各停 800 / 1000ms、速度 5px/s、单程上限 40s、缓动 easeInOutQuad，
// 行程 = 溢出量 + 6px。**关键是「按文本记进度」**：同一段文本跨帧/跨重建沿用同一
// 时间基准，换文本才从头开始 —— 否则每帧重建都会把动画打回起点（原地抖）。

/// 起点停留（ms）
pub const MQ_HOLD_A_MS: f32 = 800.0;
/// 终点停留（ms）
pub const MQ_HOLD_B_MS: f32 = 1000.0;
/// 滚动速度（px/s）—— 用户定稿的整数 5
pub const MQ_SPEED: f32 = 5.0;
/// 单程时长上限（ms）
pub const MQ_TR_MAX_MS: f32 = 40_000.0;
/// 溢出多少像素才算装不下（浏览器判据 `scrollWidth - clientWidth > 2`）
pub const MQ_MIN_OVER: f32 = 2.0;
/// 行程额外余量（浏览器 `dist = over + 6`）
pub const MQ_PAD: f32 = 6.0;

/// easeInOutQuad（浏览器 `mqEase`）
pub fn mq_ease(p: f32) -> f32 {
    let p = p.clamp(0.0, 1.0);
    if p < 0.5 {
        2.0 * p * p
    } else {
        1.0 - 2.0 * (1.0 - p) * (1.0 - p)
    }
}

/// 一条跑马灯的进度记忆：`text` 是它代表的文本，`base` 是周期原点。
///
/// 同一段文本反复喂进来时**必须原样沿用 base**，换文本才新建 —— 这条就是 WebView2 版
/// `mqMemo` 的作用（跨重建续播）。自绘里状态常驻，但仍然每帧都会重算偏移，
/// 所以「文本没变不重置」依旧是硬约束。
#[derive(Clone, Debug)]
pub struct Marquee {
    pub text: String,
    pub base: Instant,
}

impl Marquee {
    pub fn new(text: impl Into<String>, now: Instant) -> Self {
        Self {
            text: text.into(),
            base: now,
        }
    }

    /// 单程时长（ms）：`dist / 5px每秒`，上限 40s
    pub fn travel_ms(&self, dist: f32) -> f32 {
        (dist / MQ_SPEED * 1000.0).min(MQ_TR_MAX_MS)
    }

    /// 一个完整来回的周期（ms）
    pub fn period_ms(&self, dist: f32) -> f32 {
        let tr = self.travel_ms(dist);
        MQ_HOLD_A_MS + tr + MQ_HOLD_B_MS + tr
    }

    /// 当前该把文本往左挪多少像素
    pub fn offset(&self, dist: f32, now: Instant) -> f32 {
        if dist <= 0.0 {
            return 0.0;
        }
        let tr = self.travel_ms(dist);
        let period = self.period_ms(dist);
        let age = (now.saturating_duration_since(self.base)).as_secs_f32() * 1000.0;
        let u = age.rem_euclid(period);
        if u < MQ_HOLD_A_MS {
            0.0
        } else if u < MQ_HOLD_A_MS + tr {
            dist * mq_ease((u - MQ_HOLD_A_MS) / tr)
        } else if u < MQ_HOLD_A_MS + tr + MQ_HOLD_B_MS {
            dist
        } else {
            let back = (u - MQ_HOLD_A_MS - tr - MQ_HOLD_B_MS) / tr;
            dist * (1.0 - mq_ease(back))
        }
    }

    /// 是否正在位移（供调用方决定要不要 60fps 重绘；停留段为 false）
    pub fn moving(&self, dist: f32, now: Instant) -> bool {
        let tr = self.travel_ms(dist);
        let u = (now.saturating_duration_since(self.base)).as_secs_f32() * 1000.0
            % self.period_ms(dist);
        (u >= MQ_HOLD_A_MS && u < MQ_HOLD_A_MS + tr)
            || (u >= MQ_HOLD_A_MS + tr + MQ_HOLD_B_MS
                && u < MQ_HOLD_A_MS + tr + MQ_HOLD_B_MS + tr)
    }
}

thread_local! {
    /// 两条槽（歌名 / 歌手）的进度记忆 —— 与浏览器 `mqMemo` 的键一致。
    static MQ_MEMO: RefCell<HashMap<&'static str, Marquee>> = RefCell::new(HashMap::new());
    /// 本帧有没有文本在滚（给 live 的按需重绘用；`paint` 每帧重置）。
    /// 语义是「这张卡眼下有跑马灯」= 值得 60fps（浏览器那边 rAF 也是一直挂着）。
    static MQ_ACTIVE: Cell<bool> = const { Cell::new(false) };
}

/// 取某一条的进度记忆：文本没变就沿用旧基准，变了才从此刻重新开始。
pub fn marquee_for(slot: &'static str, text: &str, now: Instant) -> Marquee {
    MQ_MEMO.with(|m| {
        let mut m = m.borrow_mut();
        if let Some(st) = m.get(slot) {
            if st.text == text {
                return st.clone();
            }
        }
        let st = Marquee::new(text, now);
        m.insert(slot, st.clone());
        st
    })
}

/// 本帧是否有文本在滚动（供 `live` 的重绘节拍判断；调用方须在 `paint` 之后读）。
pub fn marquee_animating() -> bool {
    MQ_ACTIVE.with(|c| c.get())
}

/// 图标路径（viewBox 24）
fn icon(name: &str) -> &'static str {
    match name {
        "note" => "M9 18V6.2l9.5-2v11.6M9 18a2.4 2.4 0 1 1-4.8 0 2.4 2.4 0 0 1 4.8 0M18.5 15.8a2.4 2.4 0 1 1-4.8 0 2.4 2.4 0 0 1 4.8 0",
        "prev" => "M6.4 6.1v11.8M14.7 8Q18.5 5.4 18.5 10L18.5 14Q18.5 18.6 14.7 16L12.6 14.6Q8.8 12 12.6 9.4L14.7 8Z",
        "play" => "M12.9 7.8Q9 5.4 9 10L9 14Q9 18.6 12.9 16.2L16 14.4Q19.9 12 16 9.6L12.9 7.8Z",
        "pause" => "M9.2 6.4v11.2M14.8 6.4v11.2",
        _ => "M17.6 6.1v11.8M9.3 8Q5.5 5.4 5.5 10L5.5 14Q5.5 18.6 9.3 16L11.4 14.6Q15.2 12 11.4 9.4L9.3 8Z",
    }
}

pub struct Tokens {
    pub w: f32,
    pub h: f32,
    pub r: f32,
    pub radius: f32,
    pub pad: f32,
    pub body_gap: f32,
    pub cover_r: f32,
    pub note_sz: f32,
    pub note_stroke: f32,
    pub meta_gap: f32,
    pub meta_mb: f32,
    pub title_fs: f32,
    pub title_lh: f32,
    pub artist_fs: f32,
    pub artist_lh: f32,
    pub prog_gap: f32,
    pub prog_mb: f32,
    pub bar_h: f32,
    pub time_fs: f32,
    pub time_lh: f32,
    pub acts_gap: f32,
    pub btn_w: f32,
    pub btn_h: f32,
    pub btn_r: f32,
    pub btn_ic: f32,
    pub btn_stroke: f32,
}

impl Tokens {
    pub fn music(w: f32, h: f32) -> Self {
        let k = (w / 240.0).min(h / 120.0).clamp(0.4, 2.5);
        let r = 16.0 * k;
        Self {
            w,
            h,
            r,
            radius: 1.0 * r,
            pad: 0.625 * r,
            body_gap: 0.625 * r,
            cover_r: 0.75 * r,
            note_sz: 1.75 * r,
            note_stroke: 1.8,
            meta_gap: 0.3125 * r,
            meta_mb: 0.4375 * r,
            title_fs: 1.0 * r,
            title_lh: 1.0 * r * 1.25,
            artist_fs: 0.8125 * r,
            artist_lh: 0.8125 * r * 1.25,
            prog_gap: 0.375 * r,
            prog_mb: 0.5 * r,
            bar_h: 0.3125 * r, // `.mc-prog .bar` 把通条覆写成 0.3125rem（5px）
            time_fs: 0.625 * r,
            time_lh: 0.625 * r,
            acts_gap: 0.25 * r,
            btn_w: 2.125 * r,
            btn_h: 1.875 * r,
            btn_r: 0.5 * r,
            btn_ic: 1.375 * r,
            btn_stroke: 2.2,
        }
    }
}

/// 切歌封面翻转：照抄 `web/card.html::coverFlip` ——
/// 转出 0.22s `cubic-bezier(.6,0,.8,.4)` 到 −90°，换图，转回 0.26s `cubic-bezier(.2,.6,.4,1)` 到 0。
/// 浏览器那边 `.mc-body` 有 `perspective: 600px`，所以是真透视（远边缩小成梯形）；
/// 纯横向压扁只有「橡皮筋」感，这里按列做透视投影。
pub struct CoverFlip<'a> {
    /// 已经过的时间（秒）
    pub age: f32,
    /// +1 = 下一首（向左翻），−1 = 上一首（镜像）
    pub dir: f32,
    pub old: Option<&'a ID2D1Bitmap1>,
    pub new: Option<&'a ID2D1Bitmap1>,
}

/// 转出时长（CSS 0.22s）
pub const FLIP_OUT_S: f32 = 0.22;
/// 转回时长（CSS 0.26s）
pub const FLIP_IN_S: f32 = 0.26;
/// `.mc-body { perspective: 600px }`
const PERSPECTIVE: f32 = 600.0;

/// CSS 的三次贝塞尔缓动：给定 (x1,y1,x2,y2) 和当前的 x，求 y。
fn cubic_bezier(x1: f32, y1: f32, x2: f32, y2: f32, x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    // 先解 u 使 X(u) = x（牛顿迭代 + 二分兜底，和浏览器一致）
    let bx = |u: f32| {
        let v = 1.0 - u;
        3.0 * v * v * u * x1 + 3.0 * v * u * u * x2 + u * u * u
    };
    let dbx = |u: f32| {
        let v = 1.0 - u;
        3.0 * v * v * x1 + 6.0 * v * u * (x2 - x1) + 3.0 * u * u * (1.0 - x2)
    };
    let mut u = x;
    for _ in 0..8 {
        let d = dbx(u);
        if d.abs() < 1e-6 {
            break;
        }
        let err = bx(u) - x;
        if err.abs() < 1e-6 {
            break;
        }
        u = (u - err / d).clamp(0.0, 1.0);
    }
    if (bx(u) - x).abs() > 1e-4 {
        let (mut lo, mut hi) = (0.0f32, 1.0f32);
        for _ in 0..24 {
            let mid = (lo + hi) / 2.0;
            if bx(mid) < x {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        u = (lo + hi) / 2.0;
    }
    let v = 1.0 - u;
    3.0 * v * v * u * y1 + 3.0 * v * u * u * y2 + u * u * u
}

impl CoverFlip<'_> {
    /// 当前角度（度）；转出段是负、转回段从正回到 0
    pub fn angle_deg(&self) -> f32 {
        if self.age < FLIP_OUT_S {
            let u = (self.age / FLIP_OUT_S).clamp(0.0, 1.0);
            self.dir * -90.0 * cubic_bezier(0.6, 0.0, 0.8, 0.4, u)
        } else {
            let v = ((self.age - FLIP_OUT_S) / FLIP_IN_S).clamp(0.0, 1.0);
            self.dir * 90.0 * (1.0 - cubic_bezier(0.2, 0.6, 0.4, 1.0, v))
        }
    }

    /// 这一刻该画哪张图（220ms 处换图，和 CSS 的 setTimeout 一致）
    fn visible(&self) -> Option<&ID2D1Bitmap1> {
        if self.age < FLIP_OUT_S {
            self.old
        } else {
            self.new
        }
    }

    /// 是否已经翻完
    pub fn done(&self) -> bool {
        self.age >= FLIP_OUT_S + FLIP_IN_S
    }
}

/// `perspective: 600px` 的 rotateY 投影 —— **`perspective-origin` 在 `.mc-body` 中心**
/// （卡片中心，比封面中心右移 60px），不是封面中心。返回该列在卡内的 x。
pub fn flip_project_x(x_card: f32, deg: f32, cx: f32, ox: f32) -> f32 {
    let th = deg.to_radians();
    let u = x_card - cx;
    let z = u * th.sin();
    let s = PERSPECTIVE / (PERSPECTIVE + z);
    ox + (cx + u * th.cos() - ox) * s
}

/// 同一列的**纵向缩放**（远边小、近边大）；只跟这列离旋转轴的偏移有关
pub fn flip_project_scale(x_card: f32, deg: f32, cx: f32) -> f32 {
    let th = deg.to_radians();
    PERSPECTIVE / (PERSPECTIVE + (x_card - cx) * th.sin())
}

/// 按列做透视投影后逐条绘制：`perspective: 600px` 的 rotateY。
/// 每一列的水平位置按 x·cosθ·P/(P+x·sinθ) 投影、纵向按 P/(P+x·sinθ) 缩放 ——
/// 这就是「远边小、近边大」的梯形，也是立体感的来源。
fn draw_flip(
    g: &Gfx,
    bmp: &ID2D1Bitmap1,
    r: minicard_draw::layout::Rect,
    t: &Tokens,
    deg: f32,
) -> Result<()> {
    let (iw, ih) = unsafe {
        let s = bmp.GetPixelSize();
        (s.width as f32, s.height as f32)
    };
    let src = minicard_draw::image::cover_src_px(iw, ih, r.w, r.h);
    let cx = r.x + r.w / 2.0;
    let cy = r.y + r.h / 2.0;
    // CSS：`perspective` 挂在 `.mc-body` 上，默认 `perspective-origin: 50% 50%` = 卡片中心
    let (ox, oy) = (t.w / 2.0, t.h / 2.0);
    // 注意：**不要**因为 cos≈0 就整块跳过 —— 侧对镜头时近边被放大、远边被压缩，
    // 浏览器实测 89°/90° 仍有约 10px 的竖条（见 `透视_与浏览器实测对齐`）。
    let sw = src.right - src.left;

    const N: usize = 36;
    for i in 0..N {
        let x0 = cx - r.w / 2.0 + r.w * i as f32 / N as f32;
        let x1 = cx - r.w / 2.0 + r.w * (i + 1) as f32 / N as f32;
        let (p0, p1) = (
            flip_project_x(x0, deg, cx, ox),
            flip_project_x(x1, deg, cx, ox),
        );
        let s = flip_project_scale((x0 + x1) / 2.0, deg, cx);
        let (left, right) = if p0 <= p1 { (p0, p1) } else { (p1, p0) };
        if right - left < 0.05 {
            continue;
        }
        // 纵向：这一列以 cy 为中心按 s 缩放，再按透视原点做一次偏移
        let yc = oy + (cy - oy) * s;
        let u0 = src.left + sw * (i as f32 / N as f32);
        let u1 = src.left + sw * ((i + 1) as f32 / N as f32);
        // 0.4px 外扩，防止相邻条之间透出 1px 缝
        let pad = 0.4f32.min((right - left) * 0.4);
        let spad = pad * (sw / r.w);
        let dst = D2D_RECT_F {
            left: left - pad,
            top: yc - r.h * s / 2.0,
            right: right + pad,
            bottom: yc + r.h * s / 2.0,
        };
        let src_rect = D2D_RECT_F {
            left: u0 - spad,
            top: src.top,
            right: u1 + spad,
            bottom: src.bottom,
        };
        minicard_draw::image::draw_linear(g, bmp, src_rect, dst)?;
    }
    Ok(())
}

pub fn paint(
    g: &Gfx,
    te: &TextEngine,
    pal: &Palette,
    t: &Tokens,
    m: &Music,
    glass: Option<(&ID2D1Bitmap1, D2D_RECT_F)>,
    // 真专辑封面（圆角已烘进 alpha）；None = 渐变 + 音符占位
    cover: Option<&ID2D1Bitmap1>,
    // 切歌翻转（None = 静止）
    flip: Option<&CoverFlip<'_>>,
) -> Result<()> {
    let tree = tree(t, m);
    let mut mm: &TextEngine = te;
    let placed = layout(&tree, (t.w, t.h), &mut mm);
    MQ_ACTIVE.with(|c| c.set(false));

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
        paint_tree(g, te, pal, t, m, &placed, cover, flip)?;
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

fn tree(t: &Tokens, m: &Music) -> Node {
    let cover = Node::new(Dir::Row)
        .id(9001)
        .style(move |s| s.size(Dim::Px(t.h - 2.0 * t.pad), Dim::Px(t.h - 2.0 * t.pad)));

    let meta = Node::new(Dir::Column)
        .style(move |s| {
            s.gap(t.meta_gap)
                .mar([0.0, 0.0, t.meta_mb, 0.0])
                .align(Align::Stretch)
        })
        .child(
            text_node(
                if m.title.is_empty() { "—" } else { &m.title },
                t.title_fs,
                t.title_lh,
                600,
            )
            .id(9101)
            .style(|s| s.size(Dim::Pct(1.0), Dim::Px(t.title_lh))),
        )
        .child(
            text_node(
                if m.artist.is_empty() {
                    "—"
                } else {
                    &m.artist
                },
                t.artist_fs,
                t.artist_lh,
                400,
            )
            .id(9102)
            .style(|s| s.size(Dim::Pct(1.0), Dim::Px(t.artist_lh))),
        );

    let prog = Node::new(Dir::Row)
        .style(move |s| {
            s.size(Dim::Pct(1.0), Dim::Auto)
                .align(Align::Center)
                .gap(t.prog_gap)
                .mar([0.0, 0.0, t.prog_mb, 0.0])
        })
        .child(
            Node::new(Dir::Row)
                .id(9200)
                .style(move |s| s.grow(1.0).basis(0.0).size(Dim::Auto, Dim::Px(t.bar_h))),
        )
        .child(text_node(&m.pos_text(), t.time_fs, t.time_lh, 400).id(9201));

    let mut acts = Node::new(Dir::Row).style(move |s| s.gap(t.acts_gap).align(Align::Center));
    for i in 0..3u32 {
        acts = acts.child(
            Node::new(Dir::Row)
                .id(9300 + i)
                .style(move |s| s.size(Dim::Px(t.btn_w), Dim::Px(t.btn_h))),
        );
    }

    let col = Node::new(Dir::Column)
        // `.mc-col` 是 `align-self: stretch`：高度撑满 100，内容在内部居中
        .style(|s| {
            s.grow(1.0)
                .basis(0.0)
                .justify(Justify::Center)
                .align(Align::Stretch)
                .self_align(Align::Stretch)
        })
        .child(meta)
        .child(prog)
        .child(acts);

    Node::new(Dir::Column)
        .style(move |s| {
            s.size(Dim::Px(t.w), Dim::Px(t.h))
                .pad([t.pad, t.pad, t.pad, t.pad])
        })
        .child(
            Node::new(Dir::Row)
                .style(move |s| s.grow(1.0).basis(0.0).align(Align::Center).gap(t.body_gap))
                .child(cover)
                .child(col),
        )
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
    m: &Music,
    pl: &Placed,
    cover: Option<&ID2D1Bitmap1>,
    flip: Option<&CoverFlip<'_>>,
) -> Result<()> {
    // 封面：有真专辑图就画图，否则渐变 + 音符占位
    if pl.id == 9001 {
        let r = pl.rect;
        let rect = D2D_RECT_F {
            left: r.x,
            top: r.y,
            right: r.right(),
            bottom: r.bottom(),
        };
        if let Some(f) = flip {
            if let Some(bmp) = f.visible() {
                draw_flip(g, bmp, r, t, f.angle_deg())?;
            }
            return Ok(());
        }
        if let Some(bmp) = cover {
            // 圆角已经烘进位图的 alpha（见 live 的 `bake_round_cover`），这里直接 cover 铺满
            let (iw, ih) = unsafe {
                let sz = bmp.GetPixelSize();
                (sz.width as f32, sz.height as f32)
            };
            let src = minicard_draw::image::cover_src_px(iw, ih, r.w, r.h);
            minicard_draw::image::draw(g, bmp, src, rect)?;
            return Ok(());
        }
        let (grad, stops) = gradient::linear_two(
            135.0,
            rect,
            D2D1_COLOR_F {
                a: 0.72,
                ..pal.accent
            },
            D2D1_COLOR_F {
                a: 0.26,
                ..pal.accent
            },
        );
        gradient::fill_round(g, rect, t.cover_r, &grad, &stops)?;
        // 音符居中
        let sz = t.note_sz;
        let s = sz / 24.0;
        let cx = r.x + (r.w - sz) / 2.0;
        let cy = r.y + (r.h - sz) / 2.0;
        let p = Path::svg(icon("note")).scale(cx, cy, s);
        set_color(g, D2D1_COLOR_F { a: 0.80, ..pal.fg })?;
        stroke_path(g, &p, t.note_stroke * s, true)?;
        return Ok(());
    }

    // 进度条：底 + 填充
    if pl.id == 9200 {
        let r = pl.rect;
        let rad = t.bar_h / 2.0;
        set_color(g, pal.track)?;
        fill_round(
            g,
            D2D_RECT_F {
                left: r.x,
                top: r.y,
                right: r.right(),
                bottom: r.bottom(),
            },
            rad,
        )?;
        let w = r.w * m.pct() / 100.0;
        if w > 0.0 {
            // 基准 `.mc-prog .bar > i { background: var(--accent) }`
            set_color(g, pal.accent)?;
            fill_round(
                g,
                D2D_RECT_F {
                    left: r.x,
                    top: r.y,
                    right: r.x + w,
                    bottom: r.bottom(),
                },
                rad,
            )?;
        }
        return Ok(());
    }

    // 按钮：吸附整像素的 1px 描边 + 居中图标
    if (9300..9400).contains(&pl.id) {
        let i = pl.id - 9300;
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
                t.btn_r,
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
            t.btn_r,
            1.0,
        )?;
        let name = match i {
            0 => "prev",
            1 => {
                if m.playing {
                    "pause"
                } else {
                    "play"
                }
            }
            _ => "next",
        };
        let sz = t.btn_ic;
        let s = sz / 24.0;
        let cx = px + (pw - sz) / 2.0;
        let cy = py + (ph - sz) / 2.0;
        let p = Path::svg(icon(name)).scale(cx, cy, s);
        set_color(g, pal.fg)?;
        stroke_path(g, &p, t.btn_stroke * s, true)?;
        return Ok(());
    }

    if let Some(spec) = pl.text() {
        let color = match pl.id {
            9102 => pal.muted,
            9201 => pal.muted,
            _ => pal.fg,
        };
        // `.card { text-shadow: 0 .833px 1.667px var(--tshadow) }`：卡内所有文字都有
        let shadow = TextShadow::CARD.with_color(pal.shadow);
        let mut marquee_drawn = false;
        // 歌名 / 歌手：放不下（溢出 > 2px）就来回滚 —— 浏览器 `musicMarquee`
        if pl.id == 9101 || pl.id == 9102 {
            let slot = if pl.id == 9101 { "title" } else { "artist" };
            // nowrap 的自然宽（与浏览器 `scrollWidth` 同口径）
            let natural = te.measure(spec, 0.0).0;
            let over = natural - pl.rect.w;
            if over > MQ_MIN_OVER {
                let dist = over + MQ_PAD;
                let now = Instant::now();
                let off = marquee_for(slot, &spec.s, now).offset(dist, now);
                MQ_ACTIVE.with(|c| c.set(true));
                // 布局按自然宽建，否则裁剪模式下会被 Ellipsis 截成「…」
                let lay = te.layout_spec(
                    spec,
                    natural.ceil() + 2.0,
                    spec.line_h + 6.0,
                    TextMode::Clip,
                )?;
                // 纵向裁剪盒 = 元素盒（CSS `overflow:hidden`），横向随位移露字
                push_clip(
                    g,
                    D2D_RECT_F {
                        left: pl.rect.x,
                        top: pl.rect.y,
                        right: pl.rect.right(),
                        bottom: pl.rect.y + spec.line_h,
                    },
                );
                draw_shadow_taps(g, &lay, pl.rect.x - off, pl.rect.y, &shadow)?;
                set_color(g, color)?;
                unsafe {
                    g.d2d.DrawTextLayout(
                        Vector2 {
                            X: pl.rect.x - off,
                            Y: pl.rect.y,
                        },
                        &lay,
                        &g.brush,
                        D2D1_DRAW_TEXT_OPTIONS_NONE,
                    );
                }
                pop_clip(g);
                marquee_drawn = true;
            }
        }
        if !marquee_drawn {
            let mode = if spec.size == t.title_fs || spec.size == t.artist_fs {
                TextMode::Ellipsis
            } else {
                TextMode::Clip
            };
            let lay = te.layout_spec(spec, pl.rect.w, spec.line_h + 6.0, mode)?;
            draw_shadow_taps(g, &lay, pl.rect.x, pl.rect.y, &shadow)?;
            set_color(g, color)?;
            unsafe {
                g.d2d.DrawTextLayout(
                    Vector2 {
                        X: pl.rect.x,
                        Y: pl.rect.y,
                    },
                    &lay,
                    &g.brush,
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                );
            }
        }
    }
    for c in &pl.children {
        paint_tree(g, te, pal, t, m, c, cover, flip)?;
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

pub fn tree_hook(t: &Tokens, m: &Music) -> Node {
    tree(t, m)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 进度与时间文本() {
        let m = Music {
            title: "t".into(),
            artist: "a".into(),
            playing: true,
            position_ms: 61_000,
            duration_ms: 240_000,
        };
        assert!((m.pct() - 25.4166).abs() < 0.01, "pct {}", m.pct());
        assert_eq!(m.pos_text(), "01:01");
        // 超出时长要夹住
        let over = Music {
            position_ms: 999_000,
            ..m.clone()
        };
        assert!((over.pct() - 100.0).abs() < 0.001);
        assert_eq!(over.pos_text(), "16:39");
        // 时长为 0 不能除零
        let zero = Music {
            duration_ms: 0,
            ..m
        };
        assert_eq!(zero.pct(), 0.0);
    }
}

#[cfg(test)]
mod marquee_tests {
    use super::*;
    use std::time::Duration;

    /// 单程时长 = 行程 / 5px每秒（上限 40s）—— 与 `musicMarquee` 的 `trMs` 同口径。
    #[test]
    fn 行程时长按_5px每秒_且封顶四十秒() {
        let m = Marquee::new("x", Instant::now());
        // 100px 溢出 → dist = 106 → 21.2s
        assert!((m.travel_ms(106.0) - 21_200.0).abs() < 1.0, "{}", m.travel_ms(106.0));
        // 只有溢出 0.5px 时也是一整段（上限之外没有下限）
        assert!((m.travel_ms(MQ_PAD + 0.5) - (MQ_PAD + 0.5) / MQ_SPEED * 1000.0).abs() < 1.0);
        // 溢出 1px 的极端：行程 7px 也要 1.4s，不是 0
        assert!(m.travel_ms(7.0) > 1000.0);
        // 封顶
        assert!((m.travel_ms(1000.0) - MQ_TR_MAX_MS).abs() < 0.01);
    }

    /// 停 800 → 滚 tr → 停 1000 → 滚回 tr：四段各占其位，半个行程落在缓动中点。
    #[test]
    fn 偏移按_停滚停回_四段走() {
        let t0 = Instant::now();
        let m = Marquee::new("x", t0);
        let dist = 106.0f32;
        let tr = m.travel_ms(dist);
        let at = |ms: f32| m.offset(dist, t0 + Duration::from_micros((ms * 1000.0) as u64));
        assert_eq!(at(0.0), 0.0);
        assert_eq!(at(MQ_HOLD_A_MS - 5.0), 0.0, "起点停留期间不许动");
        // easeInOutQuad 在 0.5 处正好半个行程
        assert!(
            (at(MQ_HOLD_A_MS + tr / 2.0) - dist / 2.0).abs() < 0.5,
            "半程 {} 应 ≈ {}",
            at(MQ_HOLD_A_MS + tr / 2.0),
            dist / 2.0
        );
        assert!((at(MQ_HOLD_A_MS + tr) - dist).abs() < 0.01, "单程结束应到终点");
        assert!(
            (at(MQ_HOLD_A_MS + tr + MQ_HOLD_B_MS / 2.0) - dist).abs() < 0.01,
            "终点停留期间停在终点"
        );
        assert!(
            (at(MQ_HOLD_A_MS + tr + MQ_HOLD_B_MS + tr / 2.0) - dist / 2.0).abs() < 0.5,
            "回程半程"
        );
        // 一个周期后回到起点
        let period = MQ_HOLD_A_MS + tr + MQ_HOLD_B_MS + tr;
        assert!(at(period) < 0.01, "周期末尾应回到 0，实得 {}", at(period));
        // 单调：去程不许回退
        let mut prev = -1.0;
        for k in 0..=20 {
            let v = at(MQ_HOLD_A_MS + tr * k as f32 / 20.0);
            assert!(v >= prev - 1e-3, "去程出现回退：{v} < {prev}");
            prev = v;
        }
    }

    /// **文本没变必须沿用同一时间基准**（否则每帧重建都从 0 重放 = 原地抖），
    /// 换文本才从头开始；歌名与歌手两条槽互不干扰。
    #[test]
    fn 文本没变续播_换文本从头() {
        let t0 = Instant::now();
        let a = marquee_for("title", "很长的歌名 ABCDEFG", t0);
        let b = marquee_for("title", "很长的歌名 ABCDEFG", t0 + Duration::from_secs(3));
        assert_eq!(a.base, b.base, "同文本必须续播");
        // 3 秒后偏移已经不是起点（真的在滚，而不是每次都归零）
        let dist = 60.0f32;
        assert!(b.offset(dist, t0 + Duration::from_secs(3)) > 0.0);
        let c = marquee_for("title", "换了首歌", t0 + Duration::from_secs(4));
        assert_eq!(c.base, t0 + Duration::from_secs(4), "换文本要从头开始");
        assert_eq!(c.offset(dist, t0 + Duration::from_secs(4)), 0.0);
        // 另一条槽不受影响
        let d = marquee_for("artist", "很长的歌名 ABCDEFG", t0 + Duration::from_secs(5));
        assert_eq!(d.base, t0 + Duration::from_secs(5));
        assert_eq!(d.text, "很长的歌名 ABCDEFG");
    }

    /// 没有溢出就不该位移（绘制层判据是 over > 2px，这里守住 dist <= 0 的下界）
    #[test]
    fn 没溢出就不位移() {
        let m = Marquee::new("x", Instant::now());
        assert_eq!(m.offset(0.0, Instant::now()), 0.0);
        assert!(!m.moving(0.0, Instant::now()));
    }
}

/// 音乐卡可点区域
pub enum Hit {
    Prev,
    Play,
    Next,
}

pub fn hit(te: &TextEngine, t: &Tokens, m_: &Music, x: f32, y: f32) -> Option<Hit> {
    let tr = tree(t, m_);
    let mut m: &TextEngine = te;
    let pl = minicard_draw::layout::layout(&tr, (t.w, t.h), &mut m);
    match minicard_draw::layout::hit_id(&pl, x, y)? {
        9300 => Some(Hit::Prev),
        9301 => Some(Hit::Play),
        9302 => Some(Hit::Next),
        _ => None,
    }
}

#[cfg(test)]
mod flip_tests {
    use super::*;

    #[test]
    fn 缓动端点与单调() {
        assert!((cubic_bezier(0.6, 0.0, 0.8, 0.4, 0.0) - 0.0).abs() < 1e-3);
        assert!((cubic_bezier(0.6, 0.0, 0.8, 0.4, 1.0) - 1.0).abs() < 1e-3);
        assert!((cubic_bezier(0.2, 0.6, 0.4, 1.0, 0.0) - 0.0).abs() < 1e-3);
        assert!((cubic_bezier(0.2, 0.6, 0.4, 1.0, 1.0) - 1.0).abs() < 1e-3);
        let mut prev = -1.0;
        for i in 0..=40 {
            let y = cubic_bezier(0.6, 0.0, 0.8, 0.4, i as f32 / 40.0);
            assert!(y >= prev - 1e-4, "缓动必须单调：{} < {}", y, prev);
            prev = y;
        }
        // ease-in 起步慢：x=0.1 时 y 应当远小于 0.1
        assert!(cubic_bezier(0.6, 0.0, 0.8, 0.4, 0.1) < 0.06);
        // ease-out 起步快：x=0.1 时 y 应当远大于 0.1
        assert!(cubic_bezier(0.2, 0.6, 0.4, 1.0, 0.1) > 0.2);
    }

    #[test]
    fn 翻转角度两段对接() {
        let f = |age: f32| CoverFlip {
            age,
            dir: 1.0,
            old: None,
            new: None,
        };
        assert!((f(0.0).angle_deg()).abs() < 1e-3);
        // 转出段结束 = −90°
        assert!((f(FLIP_OUT_S - 1e-4).angle_deg() + 90.0).abs() < 0.2);
        // 换图瞬间从 +90° 转回
        assert!((f(FLIP_OUT_S + 1e-4).angle_deg() - 90.0).abs() < 0.2);
        // 结束回到 0
        assert!((f(FLIP_OUT_S + FLIP_IN_S).angle_deg()).abs() < 1e-3);
        assert!(!f(FLIP_OUT_S).done());
        assert!(f(FLIP_OUT_S + FLIP_IN_S).done());
        // 方向镜像
        let g = CoverFlip {
            age: FLIP_OUT_S / 2.0,
            dir: -1.0,
            old: None,
            new: None,
        };
        let h = CoverFlip {
            age: FLIP_OUT_S / 2.0,
            dir: 1.0,
            old: None,
            new: None,
        };
        assert!((g.angle_deg() + h.angle_deg()).abs() < 1e-3);
    }

    #[test]
    fn 透视_零度恒等() {
        // 封面中心 (60,60)、透视原点在卡片中心 (120,60)
        for x in [10.0f32, 50.0, 60.0, 70.0, 110.0] {
            assert!((flip_project_x(x, 0.0, 60.0, 120.0) - x).abs() < 1e-3);
            assert!((flip_project_scale(x, 0.0, 60.0) - 1.0).abs() < 1e-6);
        }
    }

    #[test]
    fn 透视_远边缩小近边放大() {
        // deg>0：右边（离轴 +40）转到后面 → 缩放 < 1；左边 > 1
        let s_r = flip_project_scale(100.0, 60.0, 60.0);
        let s_l = flip_project_scale(20.0, 60.0, 60.0);
        assert!(s_r < 1.0, "远边应当缩小，实际 {s_r}");
        assert!(s_l > 1.0, "近边应当放大，实际 {s_l}");
        // 透视原点在右 60px：侧转时整块会往中心拉，跨度比纯 cos 略宽
        let w = flip_project_x(110.0, 60.0, 60.0, 120.0) - flip_project_x(10.0, 60.0, 60.0, 120.0);
        let cos_w = 100.0 * 60.0f32.to_radians().cos();
        assert!(w > cos_w, "带原点偏移应当比纯 cos 宽：{w} vs {cos_w}");
        assert!(w < cos_w * 1.4, "但不该宽太多：{w}");
        // 89° 侧对镜头：**不是零**——近边被放大、远边被压缩，两者投影到不同的 x，
        // 浏览器实测同样是 11.8px 的竖条（见下一条测试）
        let w89 =
            flip_project_x(110.0, 89.0, 60.0, 120.0) - flip_project_x(10.0, 89.0, 60.0, 120.0);
        assert!(
            (w89 - 11.83).abs() < 0.3,
            "89° 竖条宽度 {w89}，浏览器 11.83"
        );
    }

    /// 直接拿浏览器的 `getBoundingClientRect` 真值对表（perspective:600px、
    /// body 220×100 原点 (110,50)、封面 100×100 原点 (50,50)、rotateY(deg)）。
    #[test]
    fn 透视_与浏览器实测对齐() {
        // (角度, 左边缘 x, 宽度 w)
        let cases = [
            (0.0f32, 0.0f32, 100.0f32),
            (30.0, 2.21, 91.76),
            (45.0, 8.67, 78.05),
            (60.0, 18.39, 58.97),
            (75.0, 30.67, 35.77),
            (85.0, 39.82, 18.81),
            (89.0, 43.59, 11.83),
        ];
        for (deg, want_x, want_w) in cases {
            let l = flip_project_x(0.0, deg, 50.0, 110.0);
            let r = flip_project_x(100.0, deg, 50.0, 110.0);
            assert!(
                (l - want_x).abs() < 0.35,
                "{deg}° 左缘 {l}，浏览器 {want_x}"
            );
            assert!(
                (r - l - want_w).abs() < 0.35,
                "{deg}° 宽度 {}，浏览器 {want_w}",
                r - l
            );
        }
        // 纵向：封面中心与透视原点同高时，纵向缩放对称、中心不动
        let s_near = flip_project_scale(0.0, 45.0, 50.0);
        let s_far = flip_project_scale(100.0, 45.0, 50.0);
        assert!((s_near - 1.0627).abs() < 0.002, "近边缩放 {s_near}");
        assert!((s_far - 0.9440).abs() < 0.002, "远边缩放 {s_far}");
    }

    #[test]
    fn 透视_原点偏移把封面往中心拉() {
        // 45° 时：纯中心投影跨度≈75，带原点偏移会更宽且整体右移（朝 120 靠）
        let (l1, r1) = (
            flip_project_x(10.0, 45.0, 60.0, 120.0),
            flip_project_x(110.0, 45.0, 60.0, 120.0),
        );
        let (l0, r0) = (
            flip_project_x(10.0, 45.0, 60.0, 60.0),
            flip_project_x(110.0, 45.0, 60.0, 60.0),
        );
        assert!(
            r1 - l1 > r0 - l0,
            "带原点偏移跨度更大：{} vs {}",
            r1 - l1,
            r0 - l0
        );
        assert!(
            (l1 - 60.0).abs() > (l0 - 60.0).abs() - 1e-3,
            "整体朝 120 移动"
        );
    }
}
