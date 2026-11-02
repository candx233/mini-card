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

use minicard_core::config::Dock;
use minicard_draw::color::Palette;
use minicard_draw::layout::{Rect, TextSpec};
use minicard_draw::paint::{fill_path, fill_round, set_color, stroke_path, stroke_round};
use minicard_draw::path::Path;
use minicard_draw::text::{Mode, TextEngine};
use minicard_shell::gfx::Gfx;
use windows::core::Result;
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::Direct2D::ID2D1Bitmap1;

/// 圆角（比卡片 16 更圆）
pub const RADIUS: f32 = 20.0;
/// 图标底边距条底
pub const BOTTOM_PAD: f32 = 8.0;

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
        let bar_w0 = d.bar_width();
        let bar_h = d.bar_height();
        let w = bar_w0 + 2.0 * side;
        Self {
            w,
            h: bar_h + Dock::TOP_RESERVE,
            bar_w0,
            bar_w_max: d.bar_width_max(w),
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

/// 第 i 个图标的分组线是否在它左边
fn div_before(d: &Dock, i: usize) -> bool {
    i > 0 && d.items.get(i).map(|it| it.divider_before).unwrap_or(false)
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
) -> Result<()> {
    let n = d.items.len().min(geo.cx.len());
    let bar = geo.bar;
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

        // 图标（含首字母兜底）
        for i in 0..n {
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
                    let r = Rect {
                        x: cx - half,
                        y: cy - half,
                        w: s,
                        h: s,
                    };
                    set_color(g, pal.badge_bg)?;
                    fill_round(
                        g,
                        D2D_RECT_F {
                            left: r.x,
                            top: r.y,
                            right: r.x + r.w,
                            bottom: r.y + r.h,
                        },
                        10.0 * k,
                    )?;
                    if let Some(it) = d.items.get(i) {
                        let ch = it.name.chars().next().unwrap_or('?').to_string();
                        let fs = s * 0.5;
                        let spec = TextSpec {
                            s: ch,
                            size: fs,
                            line_h: fs,
                            weight: 600,
                            wrap: false,
                            align: 1,
                            tabular: false,
                            spacing_em: 0.0,
                        };
                        let lay = te.layout_spec(&spec, s, fs + 8.0, Mode::Clip)?;
                        set_color(g, pal.fg)?;
                        g.d2d.DrawTextLayout(
                            windows_numerics::Vector2 {
                                X: r.x,
                                Y: cy - fs / 2.0,
                            },
                            &lay,
                            &g.brush,
                            windows::Win32::Graphics::Direct2D::D2D1_DRAW_TEXT_OPTIONS_NONE,
                        );
                    }
                }
            }
        }

        // 分组线：两张图标的**视觉**边沿中点（放大让位时线跟着两边居中）
        // 口径：`fg 26%`、高 = size×0.62、距条底 8 + size×0.19，线本身不缩放
        let sep_h = t.size * 0.62;
        let sep_bottom = BOTTOM_PAD + t.size * 0.19;
        let sep_y2 = t.h - sep_bottom;
        let sep_y1 = sep_y2 - sep_h;
        for i in 1..n {
            if !div_before(d, i) {
                continue;
            }
            let a = i - 1;
            let ax = geo.cx[a] + t.size * geo.ks.get(a).copied().unwrap_or(1.0) / 2.0;
            let bx = geo.cx[i] - t.size * geo.ks.get(i).copied().unwrap_or(1.0) / 2.0;
            let x = (ax + bx) / 2.0 - 0.5;
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
}
