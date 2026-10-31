//! 形状探针：把 B3 的图元各画一个，落成帧缓冲供**逐像素断言**（不靠肉眼）。
//!
//! 版面固定（240×240，等价测试图）：
//! ① 环：圆心 (60,60) r=40、线宽 6、扫 75%（12 点起顺时针）→ 3/4 圈，左上方缺口
//! ② 勾：起点 (120,140)、缩放 4、画到 60%（round cap）
//! ③ 实心圆角矩形：(20,140)-(100,200)、r=12
//! ④ 裁剪：右缘 100 的裁剪盒里画一条超宽文字，右侧越界部分必须没画出来

use windows::Win32::Graphics::Direct2D::Common::{D2D_RECT_F, D2D1_COLOR_F};
use windows::core::Result;

use minicard_draw::paint::{
    fill_round, pop_clip, push_clip, set_color, stroke_path,
};
use minicard_draw::path::Path;
use minicard_draw::text::TextEngine;
use minicard_shell::gfx::Gfx;

const WHITE: D2D1_COLOR_F = D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a: 1.0 };

pub fn shapes(g: &Gfx, te: &TextEngine, w: f32, h: f32) -> Result<()> {
    unsafe {
        g.begin()?;
        g.push_clip()?;

        set_color(g, WHITE)?;

        // ① 环：12 点起顺时针 270°
        let ring = Path::ring_arc(60.0, 60.0, 40.0, 0.75);
        stroke_path(g, &ring, 6.0, true)?;

        // ② 待办勾：画到 60%（dashoffset 等价）
        let check = Path::check_mark(120.0, 140.0, 4.0, 0.6);
        stroke_path(g, &check, 4.0, true)?;

        // ③ 实心圆角矩形
        fill_round(
            g,
            D2D_RECT_F { left: 20.0, top: 140.0, right: 100.0, bottom: 200.0 },
            12.0,
        )?;

        // ④ 裁剪：盒右缘 100，文字自然宽远超它
        let _ = h;
        let clip = D2D_RECT_F { left: 0.0, top: 210.0, right: 100.0, bottom: 240.0 };
        push_clip(g, clip);
        set_color(g, WHITE)?;
        let fmt = te.format_cached(20.0, 24.0, 0)?;
        let lay = te.layout_cached(0, &fmt, "ABCDEFGHIJKLMNOP", 400.0, 24.0, false)?;
        g.d2d.DrawTextLayout(
            windows_numerics::Vector2 { X: 0.0, Y: 212.0 },
            &lay,
            &g.brush,
            windows::Win32::Graphics::Direct2D::D2D1_DRAW_TEXT_OPTIONS_NONE,
        );
        pop_clip(g);

        let _ = w;
        g.pop_clip();
        g.end()?;
    }
    Ok(())
}

/// 第二张测试图：渐变（线性/径向）+ 组不透明度 + 形状阴影。
///
/// 版面（240×240）：
/// ① 线性渐变 135°：(8,8)-(112,112)，黑→白
/// ② 径向渐变：圆心 (180,60) r=50，白→透明
/// ③ 组不透明度：两个白方块在 opacity=0.5 的层里重叠
/// ④ 阴影：白圆角矩形 (150,150)-(220,200) r=10，dy=6 / blur=8 / 黑 34%
pub fn shapes2(g: &Gfx, _te: &TextEngine, _w: f32, _h: f32) -> Result<()> {
    use minicard_draw::gradient::{self, Grad, Stop};

    g.begin()?;
    g.push_clip()?;

        // ① 线性渐变：黑 → 白，135°（左上到右下）
        let rect = D2D_RECT_F { left: 8.0, top: 8.0, right: 112.0, bottom: 112.0 };
        let (grad, stops) = gradient::linear_two(
            135.0,
            rect,
            D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 1.0 },
            D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a: 1.0 },
        );
        gradient::fill_round(g, rect, 10.0, &grad, &stops)?;

        // ② 径向渐变：白 → 透明
        let rg = Grad::Radial(gradient::Radial {
            cx: 180.0,
            cy: 60.0,
            rx: 50.0,
            ry: 50.0,
            fx: 0.0,
            fy: 0.0,
        });
        let rs = [
            Stop { pos: 0.0, color: D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a: 1.0 } },
            Stop { pos: 1.0, color: D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a: 0.0 } },
        ];
        gradient::fill_round(
            g,
            D2D_RECT_F { left: 130.0, top: 10.0, right: 230.0, bottom: 110.0 },
            0.0,
            &rg,
            &rs,
        )?;

        // ③ 组不透明度 0.5：两块重叠白方
        {
            let _layer = minicard_draw::paint::push_opacity(g, 0.5)?;
            set_color(g, WHITE)?;
            fill_round(
                g,
                D2D_RECT_F { left: 20.0, top: 140.0, right: 90.0, bottom: 200.0 },
                0.0,
            )?;
            fill_round(
                g,
                D2D_RECT_F { left: 60.0, top: 170.0, right: 130.0, bottom: 230.0 },
                0.0,
            )?;
        }

        // ④ 阴影 + 白圆角矩形盖在影子上
        let spec = minicard_draw::shadow::Spec {
            w: 70.0,
            h: 50.0,
            radius: 10.0,
            spread: 0.0,
            blur: 8.0,
            dx: 0.0,
            dy: 6.0,
        };
        minicard_draw::paint::draw_shadow(
            g,
            &spec,
            (150.0, 150.0),
            D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.34 },
        )?;
        set_color(g, WHITE)?;
        fill_round(
            g,
            D2D_RECT_F { left: 150.0, top: 150.0, right: 220.0, bottom: 200.0 },
            10.0,
        )?;

    g.pop_clip();
    g.end()?;
    Ok(())
}

/// 第三张测试图：文字三态（省略号 / 裁剪 / 中文省略号）+ 抗锯齿取色。
///
/// 版面（240×240，全部 16px / 行高 18.4）：
/// - A y=16  · 省略号 · 拉丁 26 字母，盒宽 100（x 10..110）
/// - B y=60  · 裁剪   · 同一段文字，布局宽 200（自然宽），裁到 x 10..110
/// - C y=104 · 省略号 · 中文「迷你卡片测试歌名很长很长」，盒宽 100
/// - D y=148 · 抗锯齿 · 单个 `H`，用来抽查边缘像素是不是灰阶（R=G=B）
pub fn text_sheet(g: &Gfx, te: &TextEngine, _w: f32, _h: f32) -> Result<()> {
    use minicard_draw::text::Mode;

    const SIZE: f32 = 16.0;
    const LH: f32 = 18.4;
    const LONG: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZ";

    g.begin()?;
    g.push_clip()?;
    set_color(g, WHITE)?;

    let draw = |fmt_mode: Mode, text: &str, box_w: f32, x: f32, y: f32, tabular: bool| -> Result<()> {
        let fmt = te.format_mode(SIZE, LH, 0, fmt_mode)?;
        let lay = te.layout_mode(0, &fmt, text, box_w, LH, tabular, fmt_mode)?;
        unsafe {
            g.d2d.DrawTextLayout(
                windows_numerics::Vector2 { X: x, Y: y },
                &lay,
                &g.brush,
                windows::Win32::Graphics::Direct2D::D2D1_DRAW_TEXT_OPTIONS_NONE,
            );
        }
        Ok(())
    };

    // A：省略号
    draw(Mode::Ellipsis, LONG, 100.0, 10.0, 16.0, false)?;
    // B：裁剪（布局宽 200 = 自然宽，裁到 110）
    {
        minicard_draw::paint::push_clip(
            g,
            D2D_RECT_F { left: 10.0, top: 56.0, right: 110.0, bottom: 82.0 },
        );
        draw(Mode::Clip, LONG, 200.0, 10.0, 60.0, false)?;
        minicard_draw::paint::pop_clip(g);
    }
    // C：中文省略号
    draw(Mode::Ellipsis, "迷你卡片测试歌名很长很长", 100.0, 10.0, 104.0, false)?;
    // D：抗锯齿抽查
    draw(Mode::Clip, "H", 40.0, 10.0, 148.0, false)?;

    g.pop_clip();
    g.end()?;
    Ok(())
}

/// 第四张测试图：图片解码 + 圆角裁剪。
///
/// 版面（240×240）：把 `--cover` 给的图按 `object-fit:cover` 铺到 (20,20)-(220,220)、圆角 40。
/// 测试图是 64×64 的四象限（红/绿/蓝/黄），所以取样点能同时验「解码对不对」与「圆角真裁了」。
pub fn image_sheet(g: &Gfx, cover: &std::path::Path) -> Result<()> {
    use minicard_draw::image;

    let img = image::decode_file(cover)?;
    let bmp = image::bitmap(g, &img)?;
    let dst = D2D_RECT_F { left: 20.0, top: 20.0, right: 220.0, bottom: 220.0 };
    let src = image::cover_src(&img, dst.right - dst.left, dst.bottom - dst.top);

    g.begin()?;
    g.push_clip()?;
    image::draw_round(g, &bmp, src, dst, 40.0)?;
    g.pop_clip();
    g.end()?;
    Ok(())
}

/// 第五张测试图（连拍）：运动中的字形 / 旋转 / 亚像素条。
///
/// 版面（240×240）：
/// ① 封面 (20,8)-(110,98) 每帧转 0.1 rad —— 目视用（圆角遮罩跟着转的穿帮在这里看）
/// ② 白长条 中心 (175,50)、120×18，每帧转 0.1 rad（放在右上角，测量窗里不混别的元素） —— **数值用**：长条的朝向由协方差主轴量，
///    比「彩色象限质心」稳（质心会被方形框的裁切偏差带跑）
/// ③ 文字「运动中的字形」每帧右移 2px（y 110..136）
/// ④ 圆角条 (10,200) 宽 20 + 0.37px/帧
pub fn motion(
    g: &Gfx,
    te: &TextEngine,
    cover: Option<&std::path::Path>,
    frame: u32,
) -> Result<()> {
    use minicard_draw::image;
    use windows_numerics::Matrix3x2;

    let f = frame as f32;
    g.begin()?;
    g.push_clip()?;

    // ① 旋转封面（圆角遮罩与位图同一变换）
    if let Some(p) = cover {
        if let Ok(img) = image::decode_file(p) {
            let bmp = image::bitmap(g, &img)?;
            let dst = D2D_RECT_F { left: 10.0, top: 8.0, right: 90.0, bottom: 88.0 };
            let src = image::cover_src(&img, dst.right - dst.left, dst.bottom - dst.top);
            image::draw_round_rotated(g, &bmp, src, dst, 18.0, f * 0.1)?;
        }
    }

    // ② 旋转白长条（数值判据用）
    set_color(g, WHITE)?;
    unsafe {
        let mut old = Matrix3x2::default();
        g.d2d.GetTransform(&mut old);
        let m = minicard_draw::paint::rotation_rad(f * 0.1, 175.0, 50.0) * old;
        g.d2d.SetTransform(&m);
    }
    fill_round(
        g,
        D2D_RECT_F { left: 115.0, top: 41.0, right: 235.0, bottom: 59.0 },
        9.0,
    )?;
    unsafe {
        let mut old = Matrix3x2::default();
        g.d2d.GetTransform(&mut old);
        // 复位回上面的基准（begin 之后是单位阵）
        g.d2d.SetTransform(&Matrix3x2 {
            M11: 1.0,
            M12: 0.0,
            M21: 0.0,
            M22: 1.0,
            M31: 0.0,
            M32: 0.0,
        });
        let _ = old;
    }

    // ③ 平移文字
    let fmt = te.format_mode(20.0, 23.0, 0, minicard_draw::text::Mode::Clip)?;
    let lay = te.layout_mode(0, &fmt, "运动中的字形", 200.0, 23.0, false, minicard_draw::text::Mode::Clip)?;
    unsafe {
        g.d2d.DrawTextLayout(
            windows_numerics::Vector2 { X: 10.0 + f * 2.0, Y: 110.0 },
            &lay,
            &g.brush,
            windows::Win32::Graphics::Direct2D::D2D1_DRAW_TEXT_OPTIONS_NONE,
        );
    }

    // ④ 亚像素条
    let w = 20.0 + f * 0.37;
    fill_round(
        g,
        D2D_RECT_F { left: 10.0, top: 200.0, right: 10.0 + w, bottom: 210.0 },
        5.0,
    )?;

    g.pop_clip();
    g.end()?;
    Ok(())
}

/// 第六张测试图：磁盘卡（240×180、5 盘、FIXED_SCALE 档）。
///
/// 固定数值与 `docs/verify/minicard_center/_card_ref.py` 里的 SNAP 一致，
/// 所以能和无头 Edge 渲出的真 WebView2 版逐像素比。
pub fn disks_sheet(
    g: &Gfx,
    te: &TextEngine,
    pal: &minicard_draw::color::Palette,
    debug_layout: bool,
) -> Result<()> {
    use minicard_cards::disks::{self, Disk, Tokens};

    let list = vec![
        Disk { name: "C:".into(), used: 300_000_000_000, total: 500_000_000_000 },
        Disk { name: "D:".into(), used: 420_000_000_000, total: 500_000_000_000 },
        Disk { name: "E:".into(), used: 120_000_000_000, total: 400_000_000_000 },
        Disk { name: "F:".into(), used: 60_000_000_000, total: 300_000_000_000 },
        Disk { name: "G:".into(), used: 190_000_000_000, total: 200_000_000_000 },
    ];
    let t = Tokens::disks(240.0, 180.0, list.len());
    if debug_layout {
        let tree = disks::tree_hook(&t, &list);
        let mut m: &TextEngine = te;
        let placed = minicard_draw::layout::layout(&tree, (t.w, t.h), &mut m);
        disks::dump_layout(&placed, 0);
        minicard_shell::mlog!(
            "tokens: r={:.2} pad_lr={:.2} pad_tb={:.2} rows_gap={:.2} rows_pb={:.3} row_gap={:.2} row_pt={:.3} badge_mt={:.3} dbody_gap={:.3} fs={:.3} lh={:.4} bar_h={:.2}",
            t.r, t.pad_lr, t.pad_tb, t.rows_gap, t.rows_pb, t.row_gap, t.row_pt, t.badge_mt, t.dbody_gap, t.fs, t.line_h, t.bar_h
        );
    }
    disks::paint(g, te, pal, &t, &list, None, true, false)
}

/// 第七张测试图：时钟卡（240×120，静止态；日期取固定串以便与浏览器对比）。
pub fn clock_sheet(
    g: &Gfx,
    te: &TextEngine,
    pal: &minicard_draw::color::Palette,
    debug_layout: bool,
) -> Result<()> {
    use minicard_cards::clock::{self, Clock, Tokens};

    let t = Tokens::clock(240.0, 120.0);
    let mut c = Clock::new();
    c.snap(14, 26);
    let date = "2026年10月31日 周六";
    let lunar = "农历九月廿二";
    if debug_layout {
        let tree = clock::tree_hook(&t, date, lunar);
        let mut m: &TextEngine = te;
        let placed = minicard_draw::layout::layout(&tree, (t.w, t.h), &mut m);
        clock::dump_layout(&placed, 0);
        minicard_shell::mlog!(
            "tokens: r={:.3} pad_lr={:.3} pad_tb={:.3} gap={:.3} body_pb={:.3} date_mt={:.3} time_fs={:.3} time_lh={:.3} date_fs={:.3} date_lh={:.3}",
            t.r, t.pad_lr, t.pad_tb, t.gap, t.body_pb, t.date_mt, t.time_fs, t.time_lh, t.date_fs, t.date_lh
        );
    }
    clock::paint(g, te, pal, &t, &c, date, lunar, std::time::Instant::now(), None)
}

/// 滚动中途的一帧：从 14:26 跳到 14:27，并把时间轴拨到滚动开始后 120ms。
pub fn clock_roll(g: &Gfx, te: &TextEngine, pal: &minicard_draw::color::Palette, roll_ms: u64) -> Result<()> {
    use minicard_cards::clock::{self, Clock, Tokens};
    use std::time::{Duration, Instant};

    let t = Tokens::clock(240.0, 120.0);
    let now = Instant::now();
    let mut c = Clock::new();
    c.snap(14, 26);
    c.set(14, 27, now - Duration::from_millis(roll_ms));
    clock::paint(g, te, pal, &t, &c, "2026年10月31日 周六", "农历九月廿二", now, None)
}
