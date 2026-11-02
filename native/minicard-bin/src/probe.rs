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
    // 农历走真换算（原来是写死的字符串）
    let lunar = minicard_cards::lunar::lunar_text(2026, 10, 31).unwrap_or_default();
    if debug_layout {
        let tree = clock::tree_hook(&t, &date, &lunar);
        let mut m: &TextEngine = te;
        let placed = minicard_draw::layout::layout(&tree, (t.w, t.h), &mut m);
        clock::dump_layout(&placed, 0);
        minicard_shell::mlog!(
            "tokens: r={:.3} pad_lr={:.3} pad_tb={:.3} gap={:.3} body_pb={:.3} date_mt={:.3} time_fs={:.3} time_lh={:.3} date_fs={:.3} date_lh={:.3}",
            t.r, t.pad_lr, t.pad_tb, t.gap, t.body_pb, t.date_mt, t.time_fs, t.time_lh, t.date_fs, t.date_lh
        );
    }
    clock::paint(g, te, pal, &t, &c, &date, &lunar, std::time::Instant::now(), None)
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

/// 第八张测试图：网络卡（120×120，上 1.2MB/s、下 8.4MB/s —— 与参考桩同值）。
pub fn net_sheet(
    g: &Gfx,
    te: &TextEngine,
    pal: &minicard_draw::color::Palette,
    debug_layout: bool,
) -> Result<()> {
    use minicard_cards::net::{self, NetState, Tokens};
    use std::time::Instant;

    let (fa, fd) = te.font_box();
    let t = Tokens::net(120.0, 120.0, fa, fd);
    let now = Instant::now();
    let st = NetState::new(1_258_291.0, 8_807_038.0, now);
    if debug_layout {
        let (un, uu) = net::fmt_rate(st.up.value(now));
        let (dn, du) = net::fmt_rate(st.down.value(now));
        minicard_shell::mlog!("net 值：{} {} / {} {}", un, uu, dn, du);
        let tree = net::tree_hook(&t, &un, uu, &dn, du);
        let mut m: &TextEngine = te;
        let placed = minicard_draw::layout::layout(&tree, (t.w, t.h), &mut m);
        net::dump_layout(&placed, 0);
        minicard_shell::mlog!(
            "tokens: r={:.3} pad_lr={:.3} arrow_w={:.3} arrow_fs={:.3} arrow_lh={:.3} arrow_shift={:.3} val_fs={:.3} unit_fs={:.3}",
            t.r, t.pad_lr, t.arrow_w, t.arrow_fs, t.arrow_lh, t.arrow_shift, t.val_fs, t.unit_fs
        );
    }
    net::paint(g, te, pal, &t, &st, now, None)
}

/// 第九张测试图：番茄钟（120×120）。`run_ms > 0` 时进运行态并停在剩余 `run_ms`。
pub fn pomo_sheet(
    g: &Gfx,
    te: &TextEngine,
    pal: &minicard_draw::color::Palette,
    debug_layout: bool,
    run_ms: u64,
) -> Result<()> {
    use minicard_cards::pomo::{self, Pomo, PomoMode, Tokens};
    use std::time::{Duration, Instant};

    let t = Tokens::pomo(120.0, 120.0);
    let now = Instant::now();
    let mut p = Pomo::new();
    if run_ms > 0 {
        p.mode = PomoMode::Run;
        p.end_at = Some(now + Duration::from_millis(run_ms));
        p.shown = p.mmss(now);
    }
    if debug_layout {
        let mode = p.eff_mode(now);
        minicard_shell::mlog!("pomo 模式={:?} dur={} 剩余={}ms 进度={:.1}%",
            mode, p.dur, run_ms, p.pct(now));
        let tree = pomo::tree_hook(&t, &p, mode, now);
        let mut m: &TextEngine = te;
        let placed = minicard_draw::layout::layout(&tree, (t.w, t.h), &mut m);
        pomo::dump_layout(&placed, 0);
        minicard_shell::mlog!(
            "tokens: pad={:.3} gap_set={:.3} head={:.3} phase_fs={:.3} hero_gap={:.3} flank={:.3} num_fs={:.3} bar_h={:.3} btn_h={:.3}",
            t.pad, t.gap_set, t.head_min_h, t.phase_fs, t.hero_gap, t.flank_w, t.num_fs, t.bar_h, t.btn_h
        );
    }
    pomo::paint_static(g, te, pal, &t, &p, now)
}

/// 第十张测试图：待办卡（240×120），6 项 2 完成 → 显示 3 行 + 「还有 3 项」。
pub fn todo_sheet(
    g: &Gfx,
    te: &TextEngine,
    pal: &minicard_draw::color::Palette,
    debug_layout: bool,
) -> Result<()> {
    use minicard_cards::todo::{self, Item, Tokens};
    use std::time::Instant;

    let t = Tokens::todo(240.0, 120.0);
    let items = vec![
        Item::new("改完自绘九张卡", false),
        Item::new("跑一遍像素对表", false),
        Item::new("把勾选动画录下来", true),
        Item::new("整理施工单", false),
        Item::new("给用户过一遍", true),
        Item::new("提交并打 tag", false),
    ];
    let now = Instant::now();
    if debug_layout {
        minicard_shell::mlog!("todo 项={} 显示={:?}", items.len(), todo::shown(&items));
        let tree = todo::tree_hook(&t, &items);
        let mut m: &TextEngine = te;
        let placed = minicard_draw::layout::layout(&tree, (t.w, t.h), &mut m);
        todo::dump_layout(&placed, 0);
        minicard_shell::mlog!(
            "tokens: pad_v={:.2} pad_h={:.2} row_h={:.2} gap={:.2} ck={:.2} text_fs={:.2}",
            t.pad_v, t.pad_h, t.row_h, t.row_gap, t.ck_d, t.text_fs
        );
    }
    todo::paint(g, te, pal, &t, &items, now, None)
}

/// 第十一张测试图：天气卡（320×240 基准）。
pub fn weather_sheet(
    g: &Gfx,
    te: &TextEngine,
    pal: &minicard_draw::color::Palette,
    debug_layout: bool,
) -> Result<()> {
    use minicard_cards::weather::{Day, Tokens, Weather};

    let t = Tokens::weather(320.0, 240.0);
    let w = Weather {
        city: "杭州".into(),
        temp: Some(24.4),
        code: 2,
        air: Some("优".into()),
        humidity: Some(62),
        days: vec![
            Day { label: "今天".into(), hi: Some(26.2), lo: Some(18.4), code: 2 },
            Day { label: "明天".into(), hi: Some(23.1), lo: Some(17.2), code: 61 },
            Day { label: "后天".into(), hi: Some(21.3), lo: Some(16.1), code: 3 },
        ],
    };
    if debug_layout {
        let tree = minicard_cards::weather::tree_hook(&t, &w);
        let mut m: &TextEngine = te;
        let placed = minicard_draw::layout::layout(&tree, (t.w, t.h), &mut m);
        minicard_cards::weather::dump_layout(&placed, 0);
    }
    minicard_cards::weather::paint(g, te, pal, &t, &w, None)
}

/// 第十二张测试图：音乐卡（240×120）。
pub fn music_sheet(
    g: &Gfx,
    te: &TextEngine,
    pal: &minicard_draw::color::Palette,
    debug_layout: bool,
) -> Result<()> {
    use minicard_cards::music::{Music, Tokens};

    let t = Tokens::music(240.0, 120.0);
    let m = Music {
        title: "River Flows in You".into(),
        artist: "Yiruma".into(),
        playing: true,
        position_ms: 83_000,
        duration_ms: 221_000,
    };
    if debug_layout {
        let tree = minicard_cards::music::tree_hook(&t, &m);
        let mut mm: &TextEngine = te;
        let placed = minicard_draw::layout::layout(&tree, (t.w, t.h), &mut mm);
        minicard_cards::music::dump_layout(&placed, 0);
    }
    minicard_cards::music::paint(g, te, pal, &t, &m, None, None, None)
}

/// 第十三张测试图：日历卡（240×180），2026-10（1 号周四，31 天 → 5 行），今天是 31 号。
pub fn calendar_sheet(
    g: &Gfx,
    te: &TextEngine,
    pal: &minicard_draw::color::Palette,
    debug_layout: bool,
) -> Result<()> {
    use minicard_cards::calendar::{Calendar, Tokens};

    let t = Tokens::cal(240.0, 180.0);
    let c = Calendar::build(2026, 10, 4, 31, 31);
    if debug_layout {
        minicard_shell::mlog!("cal 网格 {} 格", c.cells.len());
        let tree = minicard_cards::calendar::tree_hook(&t, &c);
        let mut m: &TextEngine = te;
        let placed = minicard_draw::layout::layout(&tree, (t.w, t.h), &mut m);
        minicard_cards::calendar::dump_layout(&placed, 0);
    }
    minicard_cards::calendar::paint(g, te, pal, &t, &c, None)
}


/// 封面翻转探针：两张纯色封面（红/绿），按 `--roll-ms` 当作翻转进度（0..460ms）出图。
/// 用来验证「转出 → 换图 → 转回」的横向压扁曲线，不依赖真有歌在放。
pub fn music_flip(
    g: &Gfx,
    te: &TextEngine,
    pal: &minicard_draw::color::Palette,
    ms: u64,
) -> Result<()> {
    use minicard_cards::music::{CoverFlip, Music, Tokens};

    let t = Tokens::music(240.0, 120.0);
    let m = Music {
        title: "River Flows in You".into(),
        artist: "Yiruma".into(),
        playing: true,
        position_ms: 83_000,
        duration_ms: 221_000,
    };
    let mk = |src: [u8; 3]| -> Result<windows::Win32::Graphics::Direct2D::ID2D1Bitmap1> {
        let mut bgra = Vec::with_capacity(100 * 100 * 4);
        for y in 0..100u32 {
            for x in 0..100u32 {
                // 圆角 12 烘进 alpha（和 live 里 cover_src 同一口径）
                let inside = {
                    let (cx, cy, r) = if x < 12 && y < 12 {
                        (12.0, 12.0, 12.0)
                    } else if x >= 88 && y < 12 {
                        (88.0, 12.0, 12.0)
                    } else if x < 12 && y >= 88 {
                        (12.0, 88.0, 12.0)
                    } else if x >= 88 && y >= 88 {
                        (88.0, 88.0, 12.0)
                    } else {
                        (50.0, 50.0, 0.0)
                    };
                    let (dx, dy) = (x as f32 - cx, y as f32 - cy);
                    r == 0.0 || dx * dx + dy * dy <= r * r + 0.5
                };
                let a = if inside { 255u8 } else { 0 };
                let p = |c: u8| ((c as u32 * a as u32) / 255) as u8;
                bgra.extend_from_slice(&[p(src[2]), p(src[1]), p(src[0]), a]);
            }
        }
        g.bitmap_from_bgra(&bgra, 100, 100)
    };
    let red = mk([200, 40, 40])?;
    let green = mk([40, 190, 60])?;
    let flip = CoverFlip {
        age: ms as f32 / 1000.0,
        dir: 1.0,
        old: Some(&red),
        new: Some(&green),
    };
    minicard_cards::music::paint(g, te, pal, &t, &m, None, Some(&green), Some(&flip))
}


/// Dock 自绘探针：按 config 的 [dock] 画一条静态条（真图标 + 分组线 + 运行点）。
/// 尺寸用 `--w/--h` 指定，应当等于 `Dock::bar_width()` × (`bar_height()+28`)。
pub fn dock_sheet(
    g: &Gfx,
    te: &TextEngine,
    pal: &minicard_draw::color::Palette,
) -> Result<()> {
    use minicard_cards::dock::{self, Tokens};

    let cfg = minicard_core::config::Config::load();
    let d = cfg.dock.clone();
    let t = Tokens::dock(&d);

    // 图标：%APPDATA%\mini-card\icons\<fnv1a(小写反斜杠路径)>.png
    let mut bitmaps: Vec<Option<windows::Win32::Graphics::Direct2D::ID2D1Bitmap1>> = Vec::new();
    // 验收用：MC_DOCK_NOICONS=1 强制所有条目取不到图标 → 看首字母色块兜底
    let no_icons = std::env::var("MC_DOCK_NOICONS").is_ok();
    for it in &d.items {
        if no_icons {
            bitmaps.push(None);
            continue;
        }
        let path = icon_cache_path(it);
        match path.and_then(|p| minicard_draw::image::decode_file(&p).ok()) {
            Some(img) => match minicard_draw::image::bitmap(g, &minicard_draw::image::downscale(&img, 96)) {
                Ok(b) => bitmaps.push(Some(b)),
                Err(_) => bitmaps.push(None),
            },
            None => bitmaps.push(None),
        }
    }
    let running = vec![false; d.items.len()];
    let refs: Vec<Option<&windows::Win32::Graphics::Direct2D::ID2D1Bitmap1>> =
        bitmaps.iter().map(|o| o.as_ref()).collect();
    let geo = t.geo(&d, None, 0.0);
    dock::paint(g, te, pal, &t, &d, &geo, &refs, &running, None, None, None)
}

/// 与 `src-tauri/src/dock.rs::ensure_icon` 同一套缓存命名（FNV-1a over 规范化路径）
fn icon_cache_path(it: &minicard_core::config::DockItem) -> Option<std::path::PathBuf> {
    if !it.icon.is_empty() {
        let p = std::path::PathBuf::from(&it.icon);
        if p.exists() {
            return Some(p);
        }
    }
    if it.path.trim().is_empty() || it.path.starts_with("http") {
        return None;
    }
    let norm = it
        .path
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_lowercase();
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in norm.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let base = std::env::var_os("APPDATA")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let p = base.join("mini-card").join("icons").join(format!("{h:016x}.png"));
    if p.exists() {
        Some(p)
    } else {
        None
    }
}


/// 日历翻月动效探针：把 `flip_t0` 往前挪 `ms`，画一帧（0 = 刚点下、200 = 收尾）。
/// 验收判据：网格整体相对静止态横向位移 = 16·(1−easeOutQuart(u))，透明度 = 0.2+0.8e。
pub fn cal_flip(
    g: &Gfx,
    te: &TextEngine,
    pal: &minicard_draw::color::Palette,
    ms: u64,
) -> Result<()> {
    use minicard_cards::calendar::{self, Calendar};

    // 固定内容：2026-11，1 号周日（周一起始 offset=6），30 天
    let mut c = Calendar::build(2026, 11, 0, 30, 1);
    c.flip(1);
    c.flip_t0 = Some(std::time::Instant::now() - std::time::Duration::from_millis(ms));

    let t = calendar::Tokens::cal(240.0, 180.0);
    calendar::paint(g, te, pal, &t, &c, None)
}

/// 自绘右键菜单出图（`--probe menu` / `--probe menu-hover`）
pub fn menu_sheet(
    g: &Gfx,
    te: &TextEngine,
    pal: &minicard_draw::color::Palette,
    hover: Option<usize>,
) -> Result<()> {
    let items = vec![
        crate::menu::Item { label: "打开所在文件夹".into(), danger: false, disabled: false },
        crate::menu::Item { label: "从 Dock 移除".into(), danger: true, disabled: false },
        crate::menu::Item { label: "在此前加分隔线".into(), danger: false, disabled: false },
    ];
    // 先铺一层假壁纸（渐变），好看清圆角与描边
    unsafe {
        g.begin()?;
        let full = windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F {
            left: 0.0,
            top: 0.0,
            right: 260.0,
            bottom: 160.0,
        };
        minicard_draw::paint::set_color(
            g,
            windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F {
                r: 0.36,
                g: 0.42,
                b: 0.52,
                a: 1.0,
            },
        )?;
        g.d2d.FillRectangle(&full, &g.brush);
        g.end()?;
    }
    crate::menu::paint(g, te, pal, &items, hover, 208.0, 108.0, None, 0.0)
}

