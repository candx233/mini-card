//! Dock 自绘：玻璃条 + 真图标 + 分组线 + 运行点 + **悬停余弦放大**。
//!
//! 几何口径全部照 `web/dock.html` 抄（那边是验收基准，不是参考）：
//! * 条高 = `size + 16`；条宽 = `n×(size+12) + 24 + 分组线数×8`（16 项/48px/2 线 = 1000）；
//! * 窗口 = 条宽 + 两侧 `side_reserve`、条高 + `TOP_RESERVE`（172，留给气泡/菜单）；
//! * 放大：`u = (rest_i − px)/(5·size) + 0.5`、`b = (1−cos 2πu)/2`（窗口外恒 0）、
//!   `k = 1 + mag/100·b·ease`，上限 `1 + 36/size`；
//! * 布点 = **宽度累加 + 整行居中**（没有锚点，指针只改宽度，行不会漂）；
//! * 条宽 = `bar_w0 ↔ bar_w_max` 按 `ease` 插值、绕窗心；图标以**底边中心**为原点放大。
//!
//! 圆角由窗口区域负责（`Gfx::new(.., radius)`），绘图侧不做路径裁剪。

use minicard_core::config::{Dock, DockItem};
use minicard_draw::color::{oklch, Palette};
use minicard_draw::layout::{Rect, TextSpec};
use minicard_draw::paint::{fill_path, fill_round, set_color, stroke_path, stroke_round};
use minicard_draw::path::Path;
use minicard_draw::text::{Mode, TextEngine};
use minicard_shell::gfx::Gfx;
use windows::core::Result;
use windows::Win32::Graphics::Direct2D::Common::{D2D1_COLOR_F, D2D_RECT_F};
use windows::Win32::Graphics::Direct2D::ID2D1Bitmap1;

/// 圆角（比卡片 16 更圆）
pub const RADIUS: f32 = 20.0;
/// 图标底边距条底
pub const BOTTOM_PAD: f32 = 8.0;
/// 图标圆角（真图标贴图自己那圈角 = 10px @ 84；兜底色块、拖拽阴影都用它，放大时按 k 等比）
pub const ICON_RADIUS: f32 = 10.0;
/// 兜底色块的字号比例：`size × 0.42`（方块里留足四周边距，中文单字也不挤）
pub const FALLBACK_TEXT_K: f32 = 0.42;
/// 空态（0 条目）条的最小宽度：`Dock::bar_width()` 此时只剩 24（PAD 兜底），放不下提示文案
pub const EMPTY_MIN_W: f32 = 240.0;
/// 空态提示文案（0 条目时画在条心，见 `paint`）
pub const EMPTY_HINT: &str = "把程序拖到这里";

#[derive(Clone, Copy)]
pub struct Tokens {
    /// 窗口宽 = 条宽 + 两侧预留
    pub w: f32,
    /// 窗口高 = 条高 + 顶部预留
    pub h: f32,
    /// 常态条宽
    pub bar_w0: f32,
    /// 悬停态条宽（扫出来的最大行宽 + 2×PAD）
    pub bar_w_max: f32,
    pub bar_h: f32,
    pub size: f32,
    pub radius: f32,
    /// 侧向预留（条左缘在窗内的常态 x）
    pub side: f32,
    /// 条顶在窗内的 y（= 顶部预留）
    pub top: f32,
}

impl Tokens {
    pub fn dock(d: &Dock) -> Self {
        let side = d.side_reserve();
        // 空态：`Dock::bar_width()` 在 0 条目时只有 24（PAD 兜底）→ 撑到能放下提示文案的宽度。
        // 窗口尺寸也走这里（`live.rs` 用 `t.w/t.h` 建窗），所以条宽与窗口一起变宽。
        let bar_w0 = if d.items.is_empty() {
            d.bar_width().max(EMPTY_MIN_W)
        } else {
            d.bar_width()
        };
        let bar_h = d.bar_height();
        let w = bar_w0 + 2.0 * side;
        Self {
            w,
            h: bar_h + Dock::TOP_RESERVE,
            bar_w0,
            // 0 条目时 `bar_width_max()` 返回 24 → 取 max 免得进场时条反而缩窄
            bar_w_max: d.bar_width_max(w).max(bar_w0),
            bar_h,
            size: d.size as f32,
            radius: RADIUS,
            side,
            top: Dock::TOP_RESERVE,
        }
    }

    /// 图标底边在窗内的 y（不随放大变：放大以底边中心为原点）
    pub fn icon_bottom(&self) -> f32 {
        self.h - BOTTOM_PAD
    }
}

/// 一帧的放大布局（全部是**窗内**逻辑坐标）
#[derive(Clone)]
pub struct Geo {
    pub ks: Vec<f32>,
    /// 各图标中心 x
    pub cx: Vec<f32>,
    /// 玻璃条
    pub bar: D2D_RECT_F,
    /// 进场进度（0..1）
    pub ease: f32,
}

impl Tokens {
    /// `px` = 指针在窗内的 x（`None` = 指针不在，按最后位置算但不放大）；
    /// `ease` = 进出场进度（0 → 静止态，1 → 完全进场）。
    pub fn geo(&self, d: &Dock, px: Option<f32>, ease: f32) -> Geo {
        let ease = ease.clamp(0.0, 1.0);
        let n = d.items.len();
        let ks = match px {
            Some(x) => d.scales_at(self.side, x, ease),
            None => vec![1.0; n],
        };
        let cx = d.row_centers(self.w, &ks);
        let bw = self.bar_w0 + (self.bar_w_max - self.bar_w0) * ease;
        let bx = (self.w - bw) / 2.0;
        Geo {
            ks,
            cx,
            bar: D2D_RECT_F {
                left: bx,
                top: self.top,
                right: bx + bw,
                bottom: self.h,
            },
            ease,
        }
    }
}

/// 名字气泡：条上方 44px、居中于图标、`2px 8px` 内边距、圆角 8、1px 描边
pub struct Bubble<'a> {
    pub text: &'a str,
    /// 图标视觉中心（窗内 x）
    pub cx: f32,
    /// 进场进度 0..1（`translateY(2px)` → 0，透明度同步）
    pub alpha: f32,
}

/// 气泡的目标矩形（`alpha` 只影响那 2px 位移）
pub fn bubble_rect(t: &Tokens, text_w: f32, cx: f32, alpha: f32) -> D2D_RECT_F {
    let w = (text_w + 18.0).min(t.w * 0.9);
    let x = (cx - w / 2.0).clamp(6.0, (t.w - w - 6.0).max(6.0));
    // 玻璃条下沿就是窗口下沿：`bottom: bar-h + 44` 换算成窗内坐标
    let bottom = t.h - (t.bar_h + 44.0) + 2.0 * (1.0 - alpha.clamp(0.0, 1.0));
    D2D_RECT_F {
        left: x,
        top: bottom - 24.0,
        right: x + w,
        bottom,
    }
}

/// 拖拽排序时额外要画的东西：被拖的那个图标跟着指针，落点位置画一条强调色竖线
pub struct DragDraw {
    /// 被拖起来的条目下标（它的槽位留空）
    pub from: usize,
    /// 指针在窗内的 x
    pub x: f32,
    /// 插入位次（0..=n，画在 insert-1 与 insert 之间）
    pub insert: usize,
}

/// 插入位次 → 落点竖线的 x（两槽之间的空隙中点）
pub fn insert_mark_x(t: &Tokens, cx: &[f32], insert: usize) -> f32 {
    let n = cx.len();
    if n == 0 {
        return t.w / 2.0;
    }
    if insert == 0 {
        cx[0] - t.size / 2.0 - 6.0
    } else if insert >= n {
        cx[n - 1] + t.size / 2.0 + 6.0
    } else {
        (cx[insert - 1] + cx[insert]) / 2.0
    }
}

/// 第 i 个图标的分组线是否在它左边
fn div_before(d: &Dock, i: usize) -> bool {
    i > 0 && d.items.get(i).map(|it| it.divider_before).unwrap_or(false)
}

/// 第 i 个图标左侧那条分组线的 x（窗内）；**不该画时返回 `None`**：
/// 第 0 项即使 `divider_before = true` 也不画（它左边没有图标，线会悬在条内边距上），
/// 没标的、越界的同理。`paint` 与单测都走这一个判据。
fn divider_x(t: &Tokens, d: &Dock, geo: &Geo, i: usize) -> Option<f32> {
    if !div_before(d, i) || i >= geo.cx.len() {
        return None;
    }
    let a = i - 1;
    let ax = geo.cx[a] + t.size * geo.ks.get(a).copied().unwrap_or(1.0) / 2.0;
    let bx = geo.cx[i] - t.size * geo.ks.get(i).copied().unwrap_or(1.0) / 2.0;
    Some((ax + bx) / 2.0 - 0.5)
}

/// 在 `on(ease)` 进度的基础上跑临界阻尼阶跃（`dock.html` 同一套：τ=50ms、无过冲）。
/// 返回新的 `(ease, 速度)`；调用方按帧 `dt` 推进。
pub fn step_ease(ease: f32, vel: f32, target: f32, dt: f32, tau: f32) -> (f32, f32) {
    // x'' = (target − x − 2τx') / τ² 的解析步进（半隐式更稳）
    let w = 1.0 / tau.max(1e-4);
    let acc = w * w * (target - ease) - 2.0 * w * vel;
    let v = vel + acc * dt;
    let x = ease + v * dt;
    (x.clamp(-0.05, 1.05), v)
}

/// 名称哈希：FNV-1a 32 + murmur3 的 `fmix32` 收尾（雪崩）。
/// 不用 std 的 `DefaultHasher`：它不保证跨 Rust 版本稳定，图标颜色会在升级后集体变脸。
fn name_hash(name: &str) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for b in name.as_bytes() {
        h ^= *b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    // 只差最后几个字节的名字（「应用1」/「应用2」）也必须散开 → 走一遍雪崩混合
    h ^= h >> 16;
    h = h.wrapping_mul(0x85eb_ca6b);
    h ^= h >> 13;
    h = h.wrapping_mul(0xc2b2_ae35);
    h ^= h >> 16;
    h
}

/// 色相槽（16 格、22.5° 一格）。槽位 = `hash % 16`，**撞槽的按哈希序顺延到下一个空槽** ——
/// 纯取模 16 个名字撞色概率 ~67%（生日问题），顺延才敢说「16 个图标 16 种色相」。
/// >16 项时盘面不够分，退回直接取模（靠明度/彩度档再拉开）。
fn fallback_slots(hashes: &[u32]) -> Vec<usize> {
    let n = hashes.len();
    if n > 16 {
        return hashes.iter().map(|h| (*h % 16) as usize).collect();
    }
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by_key(|&i| (hashes[i], i));
    let mut taken = [false; 16];
    let mut slots = vec![0usize; n];
    for &i in &order {
        let mut s = (hashes[i] % 16) as usize;
        // n ≤ 16 时最多 n-1 格被占 → 一定有空格，循环必停
        while taken[s] {
            s = (s + 1) % 16;
        }
        taken[s] = true;
        slots[i] = s;
    }
    slots
}

/// 图标取不到时画的**首字母色块**底色（一条 Dock 一批，供 `paint` 直接用）。
/// 色相 = 槽位 × 22.5° + 11.25°（+11.25 避开纯红 0°，一排红方块最难看），
/// 明度三档（0.54/0.58/0.62）与彩度两档（0.14/0.17）取哈希高位 → 同色相也分得开。
/// 颜色只由名字决定（撞槽顺延除外），拖动排序不会换色。
pub fn fallback_colors(items: &[DockItem]) -> Vec<D2D1_COLOR_F> {
    let hashes: Vec<u32> = items.iter().map(|it| name_hash(&it.name)).collect();
    let slots = fallback_slots(&hashes);
    (0..items.len())
        .map(|i| {
            let h = hashes[i];
            let l = [0.54f32, 0.58, 0.62][((h >> 11) % 3) as usize];
            let c = if (h >> 15) & 1 == 0 { 0.14f32 } else { 0.17 };
            oklch(l, c, slots[i] as f32 * 22.5 + 11.25, 0.94)
        })
        .collect()
}

/// 色块上那个字：中文取首字、英文取首字母（跳过空白），空名字给 `?`
fn first_char(name: &str) -> char {
    name.chars().find(|c| !c.is_whitespace()).unwrap_or('?')
}

/// 画一块兜底色块（图标位与拖拽浮起位共用）：`(cx, cy)` = 方块中心，`s` = 边长。
/// 尺寸/位置/圆角都由调用方按「真图标那一格」算好传进来，所以视觉效果与真图标完全重合。
///
/// # Safety
/// 只能在 `paint` 的 `unsafe` 块里调用（内部直接用 `g.d2d`）。
#[allow(clippy::too_many_arguments)]
unsafe fn fallback_tile(
    g: &Gfx,
    te: &TextEngine,
    fill: D2D1_COLOR_F,
    cx: f32,
    cy: f32,
    s: f32,
    radius: f32,
    name: &str,
) -> Result<()> {
    let r = Rect {
        x: cx - s / 2.0,
        y: cy - s / 2.0,
        w: s,
        h: s,
    };
    set_color(g, fill)?;
    fill_round(
        g,
        D2D_RECT_F {
            left: r.x,
            top: r.y,
            right: r.x + r.w,
            bottom: r.y + r.h,
        },
        radius,
    )?;
    let fs = s * FALLBACK_TEXT_K;
    let lh = fs * 1.25;
    let spec = TextSpec {
        s: first_char(name).to_string(),
        size: fs,
        line_h: lh,
        weight: 600,
        wrap: false,
        // 0 = 左对齐（单字会贴着色块左缘）；2 = 居中，字才在方块正中
        align: 2,
        tabular: false,
        spacing_em: 0.0,
    };
    // 布局盒宽 = 方块边长：居中后单字正好落在方块中轴；高 = 一行行高，按中心对齐
    let lay = te.layout_spec(&spec, s, lh, Mode::Clip)?;
    // 色块是中深饱和色（oklch L ≤ 0.62），压白字在两套主题下都够对比
    set_color(g, oklch(0.985, 0.0, 0.0, 0.95))?;
    g.d2d.DrawTextLayout(
        windows_numerics::Vector2 {
            X: r.x,
            Y: cy - lh / 2.0,
        },
        &lay,
        &g.brush,
        windows::Win32::Graphics::Direct2D::D2D1_DRAW_TEXT_OPTIONS_NONE,
    );
    Ok(())
}

/// 画整条 Dock。`icons[i]` 为空 = 该条目没图标（画首字母色块兜底）。
#[allow(clippy::too_many_arguments)]
pub fn paint(
    g: &Gfx,
    te: &TextEngine,
    pal: &Palette,
    t: &Tokens,
    d: &Dock,
    geo: &Geo,
    icons: &[Option<&ID2D1Bitmap1>],
    running: &[bool],
    glass: Option<(&ID2D1Bitmap1, D2D_RECT_F)>,
    bubble: Option<&Bubble>,
    drag: Option<&DragDraw>,
) -> Result<()> {
    let n = d.items.len().min(geo.cx.len());
    let bar = geo.bar;
    // 兜底色块的颜色一次算好（纯函数、16 个哈希，可以每帧算）
    let fills = fallback_colors(&d.items);
    unsafe {
        g.begin()?;

        // 玻璃只铺在条上：条以外的窗口区域必须透（上面 172 是气泡/菜单预留）
        if let Some((bmp, rect)) = glass {
            // 裁成**条的圆角形状**：整窗圆角只在窗口边界上，条自己那圈角是方的
            // （用户「圆角那边没裁好」就是这里：轴对齐裁剪 = 直角露玻璃）
            g.push_clip_round(bar, t.radius)?;
            g.d2d.DrawBitmap(
                bmp,
                Some(&rect),
                1.0,
                windows::Win32::Graphics::Direct2D::D2D1_INTERPOLATION_MODE_LINEAR,
                None,
                None,
            );
            g.pop_clip_round();
        }
        set_color(g, pal.veil)?;
        fill_round(g, bar, t.radius)?;

        // 空态：条心一行提示（条宽已由 `Tokens::dock` 兜到 `EMPTY_MIN_W`）
        if d.items.is_empty() {
            let spec = TextSpec {
                s: EMPTY_HINT.to_string(),
                size: 13.0,
                line_h: 20.0,
                weight: 400,
                wrap: false,
                align: 2, // 居中（0 = 左、1 = 右、2 = 中）
                tabular: false,
                spacing_em: 0.0,
            };
            let inner = (bar.right - bar.left - 24.0).max(20.0);
            let lay = te.layout_spec(&spec, inner, 20.0, Mode::Clip)?;
            // 提示字比正文淡：`fg 55%`，压在玻璃上不抢眼
            set_color(g, D2D1_COLOR_F { a: 0.55, ..pal.fg })?;
            g.d2d.DrawTextLayout(
                windows_numerics::Vector2 {
                    X: bar.left + 12.0,
                    // 条高 = size + 16，文案按 20 行高在条内垂直居中
                    Y: bar.top + (t.bar_h - 20.0) / 2.0,
                },
                &lay,
                &g.brush,
                windows::Win32::Graphics::Direct2D::D2D1_DRAW_TEXT_OPTIONS_NONE,
            );
        }

        // 图标（含首字母兜底）
        for i in 0..n {
            if drag.map(|dg| dg.from == i).unwrap_or(false) {
                continue; // 被拖的那个单独画（跟着指针）
            }
            let k = geo.ks.get(i).copied().unwrap_or(1.0);
            let s = t.size * k;
            let half = s / 2.0;
            let cx = geo.cx[i];
            let cy = t.icon_bottom() - half;

            match icons.get(i).and_then(|o| *o) {
                Some(bmp) => {
                    let (iw, ih) = {
                        let p = bmp.GetPixelSize();
                        (p.width as f32, p.height as f32)
                    };
                    let kk = (s / iw).min(s / ih);
                    let (dw, dh) = (iw * kk, ih * kk);
                    let src = D2D_RECT_F {
                        left: 0.0,
                        top: 0.0,
                        right: iw,
                        bottom: ih,
                    };
                    let dst = D2D_RECT_F {
                        left: cx - dw / 2.0,
                        top: cy - dh / 2.0,
                        right: cx + dw / 2.0,
                        bottom: cy + dh / 2.0,
                    };
                    // 源图在加载时已降到 96（最大视觉尺寸 84）：线性采样足够，别再每帧三次插值
                    minicard_draw::image::draw_linear(g, bmp, src, dst)?;
                }
                None => {
                    // 兜底：跟真图标同尺寸同位置的一块首字母色块
                    // （尺寸/位置/放大动效仍然全部由 `Geo` 决定，这里只接管「画什么」）
                    let fill = fills.get(i).copied().unwrap_or(pal.badge_bg);
                    let name = d.items.get(i).map(|it| it.name.as_str()).unwrap_or("");
                    fallback_tile(g, te, fill, cx, cy, s, ICON_RADIUS * k, name)?;
                }
            }
        }

        // 落点竖线（强调色 2px）+ 浮起的被拖图标
        if let Some(dg) = drag {
            // `from` 越界 = 外部拖进来（Dock 里没有「被拖的条目」）：
            // 只画落点线，不画浮起图标/阴影（否则会凭空冒出一块 "?" 色块跟着指针）
            let ext = dg.from >= n;
            let k = if ext { 1.0 } else { 1.06 };
            let s = t.size * k;
            let bottom = t.icon_bottom();
            // 阴影 + 浮起图标：只有「Dock 内部拖拽」才有；外部拖进来只画落点线
            if !ext {
                set_color(
                    g,
                    windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F { a: 0.35, ..pal.shadow },
                )?;
                fill_round(
                    g,
                    D2D_RECT_F {
                        left: dg.x - s / 2.0 - 2.0,
                        top: bottom - s + 3.0,
                        right: dg.x + s / 2.0 + 2.0,
                        bottom: bottom + 5.0,
                    },
                    ICON_RADIUS,
                )?;
                match icons.get(dg.from).and_then(|o| *o) {
                    Some(bmp) => {
                        let (iw, ih) = {
                            let p = bmp.GetPixelSize();
                            (p.width as f32, p.height as f32)
                        };
                        let kk = (s / iw).min(s / ih);
                        let (dw, dh) = (iw * kk, ih * kk);
                        let src = D2D_RECT_F { left: 0.0, top: 0.0, right: iw, bottom: ih };
                        let dst = D2D_RECT_F {
                            left: dg.x - dw / 2.0,
                            top: bottom - s + (s - dh) / 2.0,
                            right: dg.x + dw / 2.0,
                            bottom: bottom - s + (s - dh) / 2.0 + dh,
                        };
                        minicard_draw::image::draw_linear(g, bmp, src, dst)?;
                    }
                    None => {
                        // 被拖的条目本来就没图标：色块跟着指针走，不然只剩一团阴影
                        let fill = fills.get(dg.from).copied().unwrap_or(pal.badge_bg);
                        let name = d
                            .items
                            .get(dg.from)
                            .map(|it| it.name.as_str())
                            .unwrap_or("");
                        fallback_tile(g, te, fill, dg.x, bottom - s / 2.0, s, ICON_RADIUS, name)?;
                    }
                }
            }
            // 落点竖线：2px 强调色，竖直占满图标高度
            let mx = insert_mark_x(t, &geo.cx, dg.insert);
            set_color(g, pal.accent)?;
            fill_round(
                g,
                D2D_RECT_F {
                    left: mx - 1.0,
                    top: bottom - t.size,
                    right: mx + 1.0,
                    bottom,
                },
                1.0,
            )?;
        }

        // 分组线：两张图标的**视觉**边沿中点（放大让位时线跟着两边居中）
        // 口径：`fg 26%`、高 = size×0.62、距条底 8 + size×0.19，线本身不缩放
        let sep_h = t.size * 0.62;
        let sep_bottom = BOTTOM_PAD + t.size * 0.19;
        let sep_y2 = t.h - sep_bottom;
        let sep_y1 = sep_y2 - sep_h;
        for i in 1..n {
            // 「前面有没有图标」的判据收在 `divider_x` 里：第 0 项即使标了 `divider_before` 也不画
            let Some(x) = divider_x(t, d, geo, i) else {
                continue;
            };
            set_color(
                g,
                windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F { a: 0.26, ..pal.fg },
            )?;
            let line = Path::polyline(&[(x, sep_y1), (x, sep_y2)]);
            stroke_path(g, &line, 1.0, false)?;
        }

        // 运行中指示点：恒 4px、恒在图标视觉底边下 5px（`--ki = 1/k` 抵掉放大）
        for i in 0..n {
            if !running.get(i).copied().unwrap_or(false) {
                continue;
            }
            let dot = Path::circle(geo.cx[i], t.icon_bottom() + 3.0, 2.0);
            set_color(
                g,
                windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F { a: 0.85, ..pal.fg },
            )?;
            fill_path(g, &dot)?;
        }

        // 名字气泡（在条上方，先垫底再写字）
        if let Some(b) = bubble {
            if b.alpha > 0.01 {
                let spec = TextSpec {
                    s: b.text.to_string(),
                    size: 12.0,
                    line_h: 18.0,
                    weight: 400,
                    wrap: false,
                    // 0 = 左对齐：先按 max_w 排版量宽度，再按量出来的宽度贴气泡左内边距画。
                    // 写 2（居中）会在这条 max_w 的布局盒里居中 → 文字被推到气泡外面去。
                    align: 0,
                    tabular: false,
                    spacing_em: 0.0,
                };
                let max_w = (t.w * 0.9 - 20.0).max(20.0);
                let lay = te.layout_spec(&spec, max_w, 20.0, Mode::Clip)?;
                let mut m = windows::Win32::Graphics::DirectWrite::DWRITE_TEXT_METRICS::default();
                let _ = lay.GetMetrics(&mut m);
                let tw = m.widthIncludingTrailingWhitespace;
                let r = bubble_rect(t, tw, b.cx, b.alpha);
                set_color(
                    g,
                    windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F {
                        a: pal.bubble_bg.a * b.alpha,
                        ..pal.bubble_bg
                    },
                )?;
                fill_round(g, r, 8.0)?;
                set_color(
                    g,
                    windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F {
                        a: b.alpha,
                        ..pal.border
                    },
                )?;
                stroke_round(g, r, 8.0, 1.0)?;
                set_color(
                    g,
                    windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F {
                        a: b.alpha,
                        ..pal.bubble_fg
                    },
                )?;
                // 文本在气泡内居中（左右各 9 = padding 8 + 描边 1）
                g.d2d.DrawTextLayout(
                    windows_numerics::Vector2 {
                        X: r.left + 9.0,
                        Y: r.top + (24.0 - 18.0) / 2.0,
                    },
                    &lay,
                    &g.brush,
                    windows::Win32::Graphics::Direct2D::D2D1_DRAW_TEXT_OPTIONS_NONE,
                );
            }
        }

        // 描边：1px 落整像素，跟着条一起宽
        set_color(g, pal.border)?;
        stroke_round(
            g,
            D2D_RECT_F {
                left: bar.left + 0.5,
                top: bar.top + 0.5,
                right: bar.right - 0.5,
                bottom: bar.bottom - 0.5,
            },
            (t.radius - 0.5).max(0.0),
            1.0,
        )?;

        g.end()?;
    }
    Ok(())
}

/// 窗内坐标 (x, y) 命中哪个图标（用**画出来的**那一帧 `Geo`，与绘制同源）。
/// 盒子 = 图标视觉框（`size·k` 见方、底边贴 `icon_bottom`），再往下多认 6px 让运行点也能点。
pub fn hit(t: &Tokens, geo: &Geo, x: f32, y: f32) -> Option<usize> {
    let bottom = t.icon_bottom();
    for (i, &cx) in geo.cx.iter().enumerate() {
        let k = geo.ks.get(i).copied().unwrap_or(1.0);
        let half = t.size * k / 2.0;
        let top = bottom - t.size * k;
        if x >= cx - half && x <= cx + half && y >= top && y <= bottom + 6.0 {
            return Some(i);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use minicard_core::config::DockItem;

    fn dock_of(n: usize, divs: &[usize]) -> Dock {
        let mut d = Dock::default();
        d.size = 48;
        d.mag = 50;
        d.items = (0..n)
            .map(|i| DockItem {
                name: format!("应用{i}"),
                path: format!("C:/x/{i}.exe"),
                divider_before: divs.contains(&i),
                ..Default::default()
            })
            .collect();
        d
    }

    /// 静止态：图标中心 = `side + Dock::item_center_x`，条 = 窗心居中、宽 = bar_w0
    #[test]
    fn 静止态与_dock_同口径() {
        let d = dock_of(16, &[0, 5, 10]);
        let t = Tokens::dock(&d);
        let g = t.geo(&d, None, 0.0);
        for i in 0..16 {
            assert!(
                (g.cx[i] - (t.side + d.item_center_x(i))).abs() < 0.01,
                "第 {i} 个"
            );
            assert!((g.ks[i] - 1.0).abs() < 1e-6);
        }
        assert!((g.bar.right - g.bar.left - t.bar_w0).abs() < 0.01);
        assert!((g.bar.left + g.bar.right) / 2.0 - t.w / 2.0 < 0.01);
        assert!((g.bar.top - Dock::TOP_RESERVE).abs() < 1e-6);
        assert!((g.bar.bottom - t.h).abs() < 1e-6);
    }

    /// 放大窗口是**紧凑支撑**：离指针 ≥ 2.5 个图标就恒 1.0（没有高斯尾巴）
    #[test]
    fn 放大只在五个图标窗口内() {
        let d = dock_of(16, &[0, 5, 10]);
        let t = Tokens::dock(&d);
        let px = t.side + d.item_center_x(8);
        let g = t.geo(&d, Some(px), 1.0);
        // 指针正压第 8 个 → k = 1 + mag/100
        assert!((g.ks[8] - 1.5).abs() < 1e-4, "{}", g.ks[8]);
        // 第 8±3 个（180px 外）在窗口外 → 1.0
        for i in [5usize, 11] {
            assert_eq!(g.ks[i], 1.0, "第 {i} 个");
        }
        // 第 ±1 个在窗口内但明显衰减
        assert!(g.ks[7] > 1.2 && g.ks[7] < 1.4, "{}", g.ks[7]);
    }

    /// 放大上限 1 + 36/48 = 1.75（mag 再大也不会乱长）
    #[test]
    fn 放大上限封顶() {
        let mut d = dock_of(16, &[0, 5, 10]);
        d.mag = 255;   // u8 上限
        let t = Tokens::dock(&d);
        let px = t.side + d.item_center_x(4);
        let g = t.geo(&d, Some(px), 1.0);
        assert!((g.ks[4] - 1.75).abs() < 1e-4);
    }

    /// 进场进度 0.5 → 放大一半；ease 0 = 完全静止
    #[test]
    fn 进场进度线性作用在放大上() {
        let d = dock_of(16, &[0, 5, 10]);
        let t = Tokens::dock(&d);
        let px = t.side + d.item_center_x(4);
        let half = t.geo(&d, Some(px), 0.5);
        assert!((half.ks[4] - 1.25).abs() < 1e-4, "{}", half.ks[4]);
        assert!((half.bar.right - half.bar.left - (t.bar_w0 + t.bar_w_max) / 2.0).abs() < 0.01);
    }

    /// 行始终居中：放大后首尾图标外沿对窗心对称（误差 < 0.02）
    #[test]
    fn 放大后整行仍居中() {
        let d = dock_of(16, &[0, 5, 10]);
        let t = Tokens::dock(&d);
        for probe in [0.0f32, 200.0, 500.0, 900.0, 1200.0] {
            let g = t.geo(&d, Some(probe), 1.0);
            let l = g.cx[0] - t.size * g.ks[0] / 2.0;
            let r = g.cx[15] + t.size * g.ks[15] / 2.0;
            assert!(
                ((l + r) / 2.0 - t.w / 2.0).abs() < 0.02,
                "probe {probe}: {l} {r}"
            );
        }
    }

    /// 窗口够宽：任何指针位置下图标都不越出窗口左右边（否则被区域裁掉）
    #[test]
    fn 放大后不越出窗口() {
        let d = dock_of(16, &[0, 5, 10]);
        let t = Tokens::dock(&d);
        let mut px = 0.0f32;
        while px <= t.w {
            let g = t.geo(&d, Some(px), 1.0);
            let l = g.cx[0] - t.size * g.ks[0] / 2.0;
            let r = g.cx[15] + t.size * g.ks[15] / 2.0;
            assert!(
                l >= -0.01 && r <= t.w + 0.01,
                "px {px}: {l}..{r} of {}",
                t.w
            );
            px += 7.0;
        }
        // 条也不会越界
        assert!(t.bar_w_max <= t.w + 0.01);
    }


    // ── 浏览器真值（`docs/verify/_cdp.mjs` 驱动 dock.html 的 `__setMag(x)`，读 `__getMag()`）──
    // 真值来源：16 项 / 48px / mag=50 / 分组线在 0、5、10；窗口 1068×236、侧留 34、条 1000↔1048。
    // 坐标**写死**，不与 Rust 共享任何计算 —— 一旦算法分叉这条必挂。
    fn dock_real() -> Dock {
        let mut d = dock_of(16, &[0, 5, 10]);
        d.mag = 50;
        d
    }


    /// 静置态（mag=0）图标中心真值：`web/dock.html` 里 `.item` 的 rect 实测
    /// left = 18,78,138,198,258,326,386,446,506,566,634,694,754,814,874,934（相对 .dock），
    /// 宽 48 → 中心 = left + 24 + 条左 34。
    #[test]
    fn 静置态图标中心与浏览器一致() {
        let d = dock_real();
        let t = Tokens::dock(&d);
        let g = t.geo(&d, None, 0.0);
        let want = [
            76.0, 136.0, 196.0, 256.0, 316.0, 384.0, 444.0, 504.0, 564.0, 624.0, 692.0, 752.0,
            812.0, 872.0, 932.0, 992.0,
        ];
        assert_eq!(g.cx.len(), want.len());
        for (i, w) in want.iter().enumerate() {
            assert!((g.cx[i] - w).abs() < 0.01, "cx[{i}] = {} 应为 {w}", g.cx[i]);
        }
    }

    #[test]
    fn 窗口几何与浏览器一致() {
        let d = dock_real();
        let t = Tokens::dock(&d);
        assert_eq!(t.w, 1068.0);            // 页面 innerWidth
        assert_eq!(t.side, 34.0);           // 侧向预留
        assert_eq!(t.bar_w0, 1000.0);
        assert_eq!(t.bar_w_max, 1048.0);    // 页面 barWMax
        assert_eq!(t.h, 236.0);             // 页面 innerHeight
        assert_eq!(t.icon_bottom(), 228.0); // 页面 rect[15].top + 48
    }

    /// x=300：k = [.., 1.0216, 1.3517, 1.4784, 1.1031, ..]，pos[0]=53.0863 / pos[4]=322.4859 / pos[15]=1014.9137
    #[test]
    fn 放大真值_x300() {
        let d = dock_real();
        let t = Tokens::dock(&d);
        let g = t.geo(&d, Some(300.0), 1.0);
        let want_k = [(2usize, 1.0216f32), (3, 1.3517), (4, 1.4784), (5, 1.1031)];
        for (i, k) in want_k {
            assert!((g.ks[i] - k).abs() < 0.0002, "k[{i}] = {} 应为 {k}", g.ks[i]);
        }
        let want_p = [(0usize, 53.0863f32), (4, 322.4859), (15, 1014.9137)];
        for (i, p) in want_p {
            assert!((g.cx[i] - p).abs() < 0.002, "pos[{i}] = {} 应为 {p}", g.cx[i]);
        }
        assert!((g.bar.right - g.bar.left - 1048.0).abs() < 0.01);
        assert!((g.bar.left - 10.0).abs() < 0.01);      // 页面 barX = 10
        assert!((g.bar.top - 172.0).abs() < 1e-6);
        assert!((g.bar.bottom - 236.0).abs() < 1e-6);
    }

    /// x=600：k[7..10] = 1.0477 / 1.3969 / 1.4523 / 1.0642，pos[7]=482.0781 / pos[9]=633.1315
    #[test]
    fn 放大真值_x600() {
        let d = dock_real();
        let t = Tokens::dock(&d);
        let g = t.geo(&d, Some(600.0), 1.0);
        let want_k = [(7usize, 1.0477f32), (8, 1.3969), (9, 1.4523), (10, 1.0642)];
        for (i, k) in want_k {
            assert!((g.ks[i] - k).abs() < 0.0002, "k[{i}] = {} 应为 {k}", g.ks[i]);
        }
        let want_p = [(7usize, 482.0781f32), (9, 633.1315), (15, 1015.0678)];
        for (i, p) in want_p {
            assert!((g.cx[i] - p).abs() < 0.002, "pos[{i}] = {} 应为 {p}", g.cx[i]);
        }
    }

    /// x=820：k[11..14] = 1.1980 / 1.4945 / 1.3020 / 1.0055，pos[12]=809.3739 / pos[15]=1016.0
    #[test]
    fn 放大真值_x820() {
        let d = dock_real();
        let t = Tokens::dock(&d);
        let g = t.geo(&d, Some(820.0), 1.0);
        let want_k = [(11usize, 1.1980f32), (12, 1.4945), (13, 1.3020), (14, 1.0055)];
        for (i, k) in want_k {
            assert!((g.ks[i] - k).abs() < 0.0002, "k[{i}] = {} 应为 {k}", g.ks[i]);
        }
        let want_p = [(12usize, 809.3739f32), (15, 1016.0)];
        for (i, p) in want_p {
            assert!((g.cx[i] - p).abs() < 0.002, "pos[{i}] = {} 应为 {p}", g.cx[i]);
        }
    }

    /// 命中测试：静止态点每个图标中心都命中自己，落在两图标之间返回 None
    #[test]
    fn 命中测试与绘制同源() {
        let d = dock_real();
        let t = Tokens::dock(&d);
        let g = t.geo(&d, None, 0.0);
        for i in 0..16 {
            let hit = hit(&t, &g, g.cx[i], t.icon_bottom() - t.size / 2.0);
            assert_eq!(hit, Some(i), "第 {i} 个中心");
        }
        // 两个图标正中间的空隙（静止态 gap 12，中心右侧 24+6 = 30 处必然是空隙）
        assert_eq!(hit(&t, &g, g.cx[3] + 30.0, t.icon_bottom() - t.size / 2.0), None);
        // 条上方 40px（放大预留区）不该命中
        assert_eq!(hit(&t, &g, g.cx[5], t.icon_bottom() - t.size - 40.0), None);
    }

    /// 放大态：点放大后的图标边缘也算命中（盒子跟着 k 变）
    #[test]
    fn 放大态命中跟着视觉盒子() {
        let d = dock_real();
        let t = Tokens::dock(&d);
        let px = t.side + d.item_center_x(8);
        let g = t.geo(&d, Some(px), 1.0);
        let k = g.ks[8];
        assert!(k > 1.4, "{k}");
        let half = t.size * k / 2.0;
        // 视觉框右沿内 1px：命中
        assert_eq!(hit(&t, &g, g.cx[8] + half - 1.0, t.icon_bottom() - 8.0), Some(8));
        // 视觉框右沿外 3px：不命中（放大后邻居已经让开）
        assert_eq!(hit(&t, &g, g.cx[8] + half + 3.0, t.icon_bottom() - 8.0), None);
    }

    /// 落点竖线：首位在一格外侧、末位在末格外侧、中间卡在两槽中点
    #[test]
    fn 落点线位置() {
        let d = dock_real();
        let t = Tokens::dock(&d);
        let g = t.geo(&d, None, 0.0);
        assert!((insert_mark_x(&t, &g.cx, 0) - (g.cx[0] - 24.0 - 6.0)).abs() < 1e-4);
        let n = g.cx.len();
        assert!((insert_mark_x(&t, &g.cx, n) - (g.cx[n - 1] + 24.0 + 6.0)).abs() < 1e-4);
        assert!((insert_mark_x(&t, &g.cx, 4) - (g.cx[3] + g.cx[4]) / 2.0).abs() < 1e-4);
    }

    /// 气泡几何：高 24、条上方 44、左右夹紧在窗内、进场时下移 2px
    #[test]
    fn 气泡位置与夹紧() {
        let d = dock_real();
        let t = Tokens::dock(&d);
        let r = bubble_rect(&t, 60.0, t.w / 2.0, 1.0);
        assert!((r.bottom - (t.h - t.bar_h - 44.0)).abs() < 1e-4, "{}", r.bottom);
        assert!((r.bottom - r.top - 24.0).abs() < 1e-4);
        assert!((r.right - r.left - 78.0).abs() < 1e-4); // 60 + 18
        // 进场中：底部低 2px
        let r0 = bubble_rect(&t, 60.0, t.w / 2.0, 0.0);
        assert!((r0.bottom - r.bottom - 2.0).abs() < 1e-4);
        // 靠左/靠右都不越出窗口
        let l = bubble_rect(&t, 60.0, 0.0, 1.0);
        assert!(l.left >= 5.99);
        let rr = bubble_rect(&t, 60.0, t.w, 1.0);
        assert!(rr.right <= t.w - 5.99);
    }

    /// 临界阻尼：单步不会过冲，约 3τ 到 95%
    #[test]
    fn 进场不冲过头() {
        let mut e = 0.0f32;
        let mut v = 0.0f32;
        let mut max = 0.0f32;
        for _ in 0..120 {
            let (ne, nv) = step_ease(e, v, 1.0, 1.0 / 120.0, 0.05);
            e = ne;
            v = nv;
            max = max.max(e);
        }
        assert!((e - 1.0).abs() < 0.01, "{e}");
        assert!(max < 1.001, "过冲 {max}");
    }

    /// 图标底边 = 窗高 − 8（放大以底边为原点，视觉上「往上长」）
    #[test]
    fn 放大往上长() {
        let d = dock_of(16, &[0, 5, 10]);
        let t = Tokens::dock(&d);
        assert!((t.icon_bottom() - (t.h - 8.0)).abs() < 1e-6);
        let g = t.geo(&d, Some(t.side + 24.0), 1.0);
        let top = t.icon_bottom() - t.size * g.ks[0];
        assert!(top < t.top, "放大后图标顶 {top} 没长过条顶 {}", t.top);
        assert!(top > 0.0, "放大后图标顶 {top} 跑出窗口");
    }

    // ── 兜底色块 / 空态 / 分组线（都只测 dock.rs 里的纯函数，不建 Gfx）──

    /// 名称哈希：同名字稳定；只差一个字的两个名字（「微信」/「微博」）也得散开
    #[test]
    fn 名称哈希稳定且散得开() {
        assert_eq!(name_hash("微信"), name_hash("微信"));
        assert_ne!(name_hash("微信"), name_hash("微博"));
        assert_ne!(name_hash(""), name_hash(" "));
        // 「应用0..15」这种只差末尾 1~2 字节的名字：原始槽位也得覆盖大半盘面
        let slots: Vec<usize> = (0..16)
            .map(|i| (name_hash(&format!("应用{i}")) % 16) as usize)
            .collect();
        let uniq: std::collections::HashSet<_> = slots.iter().collect();
        assert!(
            uniq.len() >= 8,
            "16 个名字只散到 {} 个色相槽：{slots:?}",
            uniq.len()
        );
    }

    /// 色相槽撞了要顺延：三个哈希都落在槽 0 → 必须分到 0/1/2（谁哈希小谁占原槽）
    #[test]
    fn 兜底色撞槽时顺延() {
        assert_eq!(fallback_slots(&[0, 16, 32]), vec![0, 1, 2]);
        assert_eq!(fallback_slots(&[32, 0, 16]), vec![2, 0, 1]);
        // >16 项时盘面不够分：不 panic、长度对得上
        assert_eq!(fallback_slots(&[0u32; 17]).len(), 17);
    }

    /// 16 个条目 → 16 个色相槽两两不同、颜色两两不同；同一批名字取两次颜色一致
    #[test]
    fn 兜底色块16项不撞色() {
        let d = dock_of(16, &[0, 5, 10]);
        let a = fallback_colors(&d.items);
        let b = fallback_colors(&d.items);
        assert_eq!(a.len(), 16);
        for i in 0..16 {
            assert_eq!(
                (a[i].r, a[i].g, a[i].b),
                (b[i].r, b[i].g, b[i].b),
                "第 {i} 项两次取色不一致（颜色必须只由名字决定）"
            );
        }
        let hashes: Vec<u32> = d.items.iter().map(|it| name_hash(&it.name)).collect();
        let mut slots = fallback_slots(&hashes);
        slots.sort_unstable();
        slots.dedup();
        assert_eq!(slots.len(), 16, "16 个条目应占满 16 个色相槽：{slots:?}");
        for i in 0..16 {
            for j in (i + 1)..16 {
                let dd = (a[i].r - a[j].r).abs() + (a[i].g - a[j].g).abs() + (a[i].b - a[j].b).abs();
                assert!(dd > 0.02, "第 {i}/{j} 项颜色几乎一样（{dd}）");
            }
        }
    }

    /// 色块上的字：中英都取首个非空白字符，空名字给 `?`
    #[test]
    fn 兜底色块取首字符() {
        assert_eq!(first_char("微信"), '微');
        assert_eq!(first_char("WeChat"), 'W');
        assert_eq!(first_char("  记事本"), '记');
        assert_eq!(first_char(""), '?');
        assert_eq!(first_char("  "), '?');
    }

    /// 空态：条宽兜到 `EMPTY_MIN_W`、进场不许变窄、`geo/hit` 不 panic 也不命中
    #[test]
    fn 空态条宽与命中() {
        let mut d = Dock::default();
        assert!(d.items.is_empty());
        d.size = 48;
        let t = Tokens::dock(&d);
        assert!(t.bar_w0 >= EMPTY_MIN_W, "空态条宽 {} < {EMPTY_MIN_W}", t.bar_w0);
        assert!((t.w - (t.bar_w0 + 2.0 * t.side)).abs() < 1e-6);
        assert!(t.bar_w_max >= t.bar_w0, "进场不许把空态的条缩窄");
        let g = t.geo(&d, Some(t.w / 2.0), 1.0);
        assert!(g.cx.is_empty() && g.ks.is_empty());
        assert!((g.bar.right - g.bar.left - t.bar_w0).abs() < 0.01);
        assert!(g.bar.left >= -0.01 && g.bar.right <= t.w + 0.01);
        assert!((g.bar.bottom - t.h).abs() < 1e-6);
        // 空态点哪儿都不命中（否则右键菜单会拿到不存在的下标）
        for (x, y) in [
            (0.0, 0.0),
            (t.w / 2.0, t.icon_bottom() - 20.0),
            (t.w, t.h),
        ] {
            assert_eq!(hit(&t, &g, x, y), None, "({x},{y})");
        }
        // 落点线退化成窗口中心（不是 NaN）
        assert!((insert_mark_x(&t, &g.cx, 0) - t.w / 2.0).abs() < 1e-4);
    }

    /// 分组线只在「前面有图标」时画：第 0 项标了 `divider_before` 也不画，单条目一条线都没有
    #[test]
    fn 分组线只在前面有图标时画() {
        let d = dock_of(4, &[0, 2]); // 第 0 项故意也标 true
        let t = Tokens::dock(&d);
        let g = t.geo(&d, None, 0.0);
        assert!(!div_before(&d, 0));
        assert!(div_before(&d, 2));
        assert_eq!(divider_x(&t, &d, &g, 0), None, "第 0 项左边没图标，不许画线");
        assert_eq!(divider_x(&t, &d, &g, 1), None, "第 1 项没标 divider_before");
        let x2 = divider_x(&t, &d, &g, 2).expect("第 2 项标了且前面有图标 → 要画");
        let want = (g.cx[1] + t.size / 2.0 + g.cx[2] - t.size / 2.0) / 2.0 - 0.5;
        assert!((x2 - want).abs() < 1e-4, "{x2} 应为 {want}");
        assert_eq!(divider_x(&t, &d, &g, 3), None, "第 3 项没标");
        assert_eq!(divider_x(&t, &d, &g, 99), None, "越界不许 panic");
        // 整条只画 1 条线 = `Dock::divider_count()` 的口径（首个不算）
        let drawn = (1..4).filter(|&i| divider_x(&t, &d, &g, i).is_some()).count();
        assert_eq!(drawn, 1);
        assert_eq!(drawn, d.divider_count());
        // 单条目 + divider_before：前面反正没图标，一条线都不画
        let one = dock_of(1, &[0]);
        let t1 = Tokens::dock(&one);
        let g1 = t1.geo(&one, None, 0.0);
        assert_eq!(divider_x(&t1, &one, &g1, 0), None);
        assert_eq!(divider_x(&t1, &one, &g1, 1), None);
    }
}
