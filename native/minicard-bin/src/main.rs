//! 自绘卡进程（0.1.2 只接性能卡；其余 8 张按 `docs/TODO-012-renderer.md` C–E 段逐张接）。
//!
//! 与 M0 spike 的区别：不再自带采样与绘制实现 —— 数据来自 `minicard_core`（和 Tauri 主界面
//! 共用同一份 config/采样），绘制来自 `minicard_draw` / `minicard_cards`，窗口与合成来自
//! `minicard_shell`。这个 crate 只负责「参数 → 窗口 → 帧循环 → 收尾」。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod anim;
mod guide;
mod appwin;
mod live;
mod probe;

use std::path::PathBuf;
use std::time::Instant;

use windows::core::{w, Result};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::Direct2D::ID2D1Bitmap1;
use windows::Win32::Graphics::Gdi::{HBRUSH, ValidateRect};
use windows::Win32::Media::timeBeginPeriod;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    GetDpiForSystem, GetDpiForWindow, SetProcessDpiAwarenessContext,
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use anim::Tween;
use minicard_cards::perf::{self, Row, Tokens};
use minicard_core::data::{spawn_sampler, SharedSnap};
use minicard_draw::color::Palette;
use minicard_draw::glass::{self, Wall};
use minicard_draw::text::TextEngine;
use minicard_shell::desktop;
use minicard_shell::mem;
use minicard_shell::{gfx, mlog};

const TIMER_TICK: usize = 1;

struct Args {
    x: i32,
    y: i32,
    w: f32,
    h: f32,
    font: PathBuf,
    glass: f32,
    blur: f32,
    no_glass: bool,
    exit_after: Option<u64>,
    wall_raw: Option<PathBuf>,
    wall_w: u32,
    wall_h: u32,
    mon: (f32, f32),
    dump: Option<PathBuf>,
    dump_seq: Option<PathBuf>,
    debug_alpha: bool,
    freeze: Option<(f32, f32, Option<f32>)>,
    snap_text: bool,
    animate: bool,
    desktop_layer: bool,
    viewer: Option<(i32, i32, i32, i32)>,
    probe: Option<String>,
    cover: Option<PathBuf>,
    debug_layout: bool,
    roll_ms: u64,
    pomo_remain: u64,
    gamma: f32,
    contrast: f32,
    rmode: u8,
    live: bool,
    live_topmost: bool,
    emb_off: f32,
    emb_a: f32,
    text_dx: f32,
    text_dy: f32,
}

impl Args {
    fn parse() -> Self {
        let mut a = Self {
            x: 300,
            y: 300,
            w: 240.0,
            h: 120.0,
            font: PathBuf::from(r"..\assets\fonts\MiSans-Regular.ttf"),
            glass: 30.0,
            blur: 30.0,
            no_glass: false,
            exit_after: None,
            wall_raw: None,
            wall_w: 0,
            wall_h: 0,
            mon: (1920.0, 1080.0),
            dump: None,
            dump_seq: None,
            debug_alpha: false,
            freeze: None,
            snap_text: false,
            animate: false,
            desktop_layer: false,
            viewer: None,
            probe: None,
            cover: None,
            debug_layout: false,
            roll_ms: 120,
            pomo_remain: 0,
            gamma: 0.0,
            contrast: 0.0,
            rmode: 0,
            live: false,
            live_topmost: false,
            emb_off: 0.0,
            emb_a: 0.0,
            text_dx: 0.0,
            text_dy: 0.0,
            // (duplicate gamma/contrast removed)
        };
        let argv: Vec<String> = std::env::args().skip(1).collect();
        let mut i = 0;
        while i < argv.len() {
            let next = |i: &mut usize| -> String {
                *i += 1;
                argv.get(*i).cloned().unwrap_or_default()
            };
            match argv[i].as_str() {
                "--x" => a.x = next(&mut i).parse().unwrap_or(a.x),
                "--y" => a.y = next(&mut i).parse().unwrap_or(a.y),
                "--w" => a.w = next(&mut i).parse().unwrap_or(a.w),
                "--h" => a.h = next(&mut i).parse().unwrap_or(a.h),
                "--font" => a.font = PathBuf::from(next(&mut i)),
                "--glass" => a.glass = next(&mut i).parse().unwrap_or(a.glass),
                "--blur" => a.blur = next(&mut i).parse().unwrap_or(a.blur),
                "--no-glass" => a.no_glass = true,
                "--exit-after" => a.exit_after = next(&mut i).parse().ok(),
                "--wall-raw" => a.wall_raw = Some(PathBuf::from(next(&mut i))),
                "--wall-w" => a.wall_w = next(&mut i).parse().unwrap_or(0),
                "--wall-h" => a.wall_h = next(&mut i).parse().unwrap_or(0),
                "--mon" => {
                    let s = next(&mut i);
                    let mut it = s.split(['x', ',']).filter_map(|v| v.parse::<f32>().ok());
                    if let (Some(w), Some(h)) = (it.next(), it.next()) {
                        a.mon = (w, h);
                    }
                }
                "--dump" => a.dump = Some(PathBuf::from(next(&mut i))),
                "--dump-seq" => a.dump_seq = Some(PathBuf::from(next(&mut i))),
                "--debug-alpha" => a.debug_alpha = true,
                "--snap-text" => a.snap_text = true,
                "--animate" => a.animate = true,
                "--desktop" => a.desktop_layer = true,
                "--probe" => a.probe = Some(next(&mut i)),
                "--cover" => a.cover = Some(PathBuf::from(next(&mut i))),
                "--debug-layout" => a.debug_layout = true,
                "--roll-ms" => a.roll_ms = next(&mut i).parse().unwrap_or(120),
                "--pomo-remain" => a.pomo_remain = next(&mut i).parse().unwrap_or(0),
                "--gamma" => a.gamma = next(&mut i).parse().unwrap_or(0.0),
                "--contrast" => a.contrast = next(&mut i).parse().unwrap_or(0.0),
                "--live" => a.live = true,
                "--live-topmost" => a.live_topmost = true,
                "--rmode" => a.rmode = next(&mut i).parse().unwrap_or(0),
                "--embolden-off" => a.emb_off = next(&mut i).parse().unwrap_or(0.0),
                "--embolden-a" => a.emb_a = next(&mut i).parse().unwrap_or(0.0),
                "--text-dx" => a.text_dx = next(&mut i).parse().unwrap_or(0.0),
                "--text-dy" => a.text_dy = next(&mut i).parse().unwrap_or(0.0),
                "--viewer" => {
                    let s = next(&mut i);
                    let mut it = s.split([',', 'x']).filter_map(|v| v.parse::<i32>().ok());
                    if let (Some(x), Some(y), Some(w), Some(h)) =
                        (it.next(), it.next(), it.next(), it.next())
                    {
                        a.viewer = Some((x, y, w, h));
                    }
                }
                "--freeze" => {
                    let s = next(&mut i);
                    let v: Vec<Option<f32>> = s
                        .split(',')
                        .map(|p| {
                            let p = p.trim();
                            if p == "-" || p == "none" {
                                None
                            } else {
                                p.parse::<f32>().ok()
                            }
                        })
                        .collect();
                    let get = |i: usize| v.get(i).copied().flatten();
                    a.freeze = Some((get(0).unwrap_or(0.0), get(1).unwrap_or(0.0), get(2)));
                }
                other => mlog!("未知参数 {other}"),
            }
            i += 1;
        }
        a
    }
}

struct App {
    gfx: gfx::Gfx,
    te: TextEngine,
    pal: Palette,
    tokens: Tokens,
    glass_bmp: Option<ID2D1Bitmap1>,
    glass_rect: D2D_RECT_F,
    /// 采样快照（core 的 1s 线程在写，这里只读）
    snap: SharedSnap,
    t0: Instant,
    first_frame: bool,
    exit_after: Option<u64>,
    frames: u32,
    freeze: Option<(f32, f32, Option<f32>)>,
    animate: bool,
    tweens: Vec<Option<Tween>>,
    last_frame: Option<Instant>,
    frame_dt: Vec<f32>,
    frame_draw_us: Vec<f32>,
    snap_text: bool,
    probe: Option<String>,
    cover: Option<PathBuf>,
    debug_layout: bool,
    roll_ms: u64,
    pomo_remain: u64,
}

impl App {
    fn rows(&self) -> Vec<Row> {
        if self.animate {
            let t = self.t0.elapsed().as_secs_f32();
            let tau = std::f32::consts::TAU;
            let c = 50.0 + 45.0 * (t * tau / 3.0).sin();
            let r = 45.0 + 35.0 * (t * tau / 5.0).sin();
            let g = 50.0 + 40.0 * (t * tau / 2.3).sin();
            return vec![
                Row { label: "CPU".into(), text: format!("{c:.0}%"), pct: Some(c) },
                Row { label: "RAM".into(), text: format!("{r:.0}%"), pct: Some(r) },
                Row { label: "GPU".into(), text: format!("{g:.0}%"), pct: Some(g) },
            ];
        }
        if let Some((c, r, g)) = self.freeze {
            let mut out = vec![
                Row { label: "CPU".into(), text: format!("{c:.0}%"), pct: Some(c) },
                Row { label: "RAM".into(), text: format!("{r:.0}%"), pct: Some(r) },
            ];
            out.push(match g {
                Some(v) => Row { label: "GPU".into(), text: format!("{v:.0}%"), pct: Some(v) },
                None => Row { label: "GPU".into(), text: "未检测到".into(), pct: None },
            });
            return out;
        }
        let s = self.snap.read().map(|g| g.clone()).unwrap_or_default();
        let cpu = s.cpu.round();
        let ram = if s.mem_total > 0 {
            (s.mem_used as f64 / s.mem_total as f64 * 100.0).round()
        } else {
            0.0
        };
        let mut out = vec![
            Row { label: "CPU".into(), text: format!("{cpu:.0}%"), pct: Some(cpu) },
            Row { label: "RAM".into(), text: format!("{ram:.0}%"), pct: Some(ram as f32) },
        ];
        out.push(match s.gpus.first() {
            Some(g) => Row {
                label: "GPU".into(),
                text: format!("{:.0}%", g.usage.round()),
                pct: Some(g.usage),
            },
            None => Row { label: "GPU".into(), text: "未检测到".into(), pct: None },
        });
        out
    }

    fn animating(&self) -> bool {
        // 连拍探针要按 60fps 跑满（否则帧循环停在等消息上、只出零星几帧）
        if self.probe.as_deref() == Some("motion") {
            return true;
        }
        let now = Instant::now();
        self.tweens.iter().flatten().any(|t| t.live(now))
    }

    fn repaint(&mut self) {
        // 形状探针：只画测试图，不跑卡片视图
        if let Some(name) = self.probe.clone() {
            if name == "shapes" || name == "shapes2" || name == "text" || name == "image"
                || name == "motion" || name == "disks" || name == "clock"
                || name == "clock-roll" || name == "net" || name == "pomo" || name == "todo" || name == "weather" || name == "music" || name == "music-flip" || name == "calendar" || name == "dock" || name == "cal-flip"
            {
                let r = match name.as_str() {
                    "shapes" => probe::shapes(&self.gfx, &self.te, self.tokens.w, self.tokens.h),
                    "shapes2" => probe::shapes2(&self.gfx, &self.te, self.tokens.w, self.tokens.h),
                    "image" => match &self.cover {
                        Some(p) => probe::image_sheet(&self.gfx, p),
                        None => {
                            mlog!("--probe image 需要 --cover <文件>");
                            Ok(())
                        }
                    },
                    "calendar" => probe::calendar_sheet(&self.gfx, &self.te, &self.pal, self.debug_layout),
                    "music" => probe::music_sheet(&self.gfx, &self.te, &self.pal, self.debug_layout),
                    "music-flip" => probe::music_flip(&self.gfx, &self.te, &self.pal, self.roll_ms),
                    "dock" => probe::dock_sheet(&self.gfx, &self.te, &self.pal),
                    "cal-flip" => probe::cal_flip(&self.gfx, &self.te, &self.pal, self.roll_ms),
                    "weather" => probe::weather_sheet(&self.gfx, &self.te, &self.pal, self.debug_layout),
                    "todo" => probe::todo_sheet(&self.gfx, &self.te, &self.pal, self.debug_layout),
                    "pomo" => probe::pomo_sheet(&self.gfx, &self.te, &self.pal, self.debug_layout, self.pomo_remain),
                    "net" => probe::net_sheet(&self.gfx, &self.te, &self.pal, self.debug_layout),
                    "clock-roll" => probe::clock_roll(&self.gfx, &self.te, &self.pal, self.roll_ms),
                    "clock" => probe::clock_sheet(
                        &self.gfx,
                        &self.te,
                        &self.pal,
                        self.debug_layout,
                    ),
                    "disks" => probe::disks_sheet(
                        &self.gfx,
                        &self.te,
                        &self.pal,
                        self.debug_layout,
                    ),
                    "motion" => probe::motion(
                        &self.gfx,
                        &self.te,
                        self.cover.as_deref(),
                        self.frames,
                    ),
                    _ => probe::text_sheet(&self.gfx, &self.te, self.tokens.w, self.tokens.h),
                };
                if let Err(e) = r {
                    mlog!("探针 paint 失败: {e:?}");
                }
                self.frames += 1;
                if !self.first_frame {
                    self.first_frame = true;
                    mlog!("探针 {} 首帧 {:.1} ms", name, self.t0.elapsed().as_secs_f64() * 1000.0);
                }
                return;
            }
        }
        let mut rows = self.rows();
        let now = Instant::now();
        for (i, row) in rows.iter_mut().enumerate() {
            let tgt = match row.pct {
                Some(v) => v,
                None => continue,
            };
            if self.tweens.len() <= i {
                self.tweens.push(None);
            }
            let slot = self.tweens.get_mut(i).unwrap();
            match slot {
                None => *slot = Some(Tween::new(tgt, tgt, now)),
                Some(tw) => {
                    if (tw.to - tgt).abs() > 0.001 {
                        let from = tw.value(now);
                        *tw = Tween::new(from, tgt, now);
                    }
                    row.pct = Some(tw.value(now));
                }
            }
        }
        let glass = self.glass_bmp.as_ref().map(|b| (b, self.glass_rect));
        if let Err(e) = perf::paint(
            &self.gfx,
            &self.te,
            &self.pal,
            &self.tokens,
            &rows,
            glass,
            self.snap_text,
        ) {
            mlog!("paint 失败: {e:?}");
            return;
        }
        self.frames += 1;
        if !self.first_frame {
            self.first_frame = true;
            mlog!(
                "首帧上屏 {:.1} ms（进程启动→Present 返回），窗口 {}×{}，dpi {}",
                self.t0.elapsed().as_secs_f64() * 1000.0,
                self.tokens.w,
                self.tokens.h,
                self.gfx.dpi
            );
        }
    }

    fn frame_tick(&mut self) {
        let now = Instant::now();
        if let Some(prev) = self.last_frame {
            let dt = now.duration_since(prev).as_secs_f32() * 1000.0;
            if self.frame_dt.len() < 600 {
                self.frame_dt.push(dt);
            }
        }
        self.last_frame = Some(now);
        let t0 = Instant::now();
        self.repaint();
        let draw_ms = t0.elapsed().as_secs_f32() * 1000.0;
        if self.frame_draw_us.len() < 600 {
            self.frame_draw_us.push(draw_ms * 1000.0);
        }
        if self.frame_dt.len() == 300 {
            self.log_frame_stats("动效中");
        }
        if !self.animating() {
            self.last_frame = None;
            self.log_frame_stats("动效收尾");
        }
    }

    fn log_frame_stats(&mut self, tag: &str) {
        let mut dts = std::mem::take(&mut self.frame_dt);
        let mut draws = std::mem::take(&mut self.frame_draw_us);
        if dts.is_empty() {
            return;
        }
        dts.sort_by(|a, b| a.partial_cmp(b).unwrap());
        draws.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let mean = dts.iter().sum::<f32>() / dts.len() as f32;
        let p = |v: &Vec<f32>, q: f32| v[((v.len() - 1) as f32 * q) as usize];
        let over = dts.iter().filter(|d| **d > 20.0).count();
        mlog!(
            "{tag}：{} 帧  间隔 均值 {:.2} ms / p95 {:.2} / max {:.2}（≈{:.1} fps，>20ms {} 帧）；帧内 CPU 绘制 p50 {:.3} ms / p95 {:.3} ms；Present 等待 {:.2} ms",
            dts.len(),
            mean,
            p(&dts, 0.95),
            dts[dts.len() - 1],
            1000.0 / mean,
            over,
            p(&draws, 0.50) / 1000.0,
            p(&draws, 0.95) / 1000.0,
            self.gfx.t_present.get()
        );
    }
}

/// 同目录下的三个字面（Regular/Medium/Semibold），供 live 模式装字体用
pub fn font_set(font: &std::path::Path) -> Vec<std::path::PathBuf> {
    let dir = font.parent().unwrap_or(std::path::Path::new("."));
    let mut files: Vec<std::path::PathBuf> = ["MiSans-Regular.ttf", "MiSans-Medium.ttf", "MiSans-Semibold.ttf"]
        .iter()
        .map(|n| dir.join(n))
        .filter(|p| p.exists())
        .collect();
    if files.is_empty() {
        files.push(font.to_path_buf());
    }
    files
}

fn main() -> Result<()> {
    let t0 = Instant::now();
    minicard_shell::log::init(&std::env::temp_dir().join("minicard-native.log"));
    let args = Args::parse();

    // 真数据接线模式：读 config.toml → 每张启用的卡一个窗口 → 共用采样线程
    if args.live {
        let _ = unsafe {
            windows::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(
                windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
            )
        };
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(
                None,
                windows::Win32::System::Com::COINIT_APARTMENTTHREADED,
            );
        }
        return live::start(&args.font, args.blur, args.live_topmost, args.exit_after);
    }

    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        mem::log("启动");

        let hinst = GetModuleHandleW(None)?;
        let cls = w!("MiniCardNative");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            lpszClassName: cls,
            hbrBackground: HBRUSH::default(),
            ..Default::default()
        };
        if RegisterClassW(&wc) == 0 {
            mlog!("RegisterClassW 失败: {:?}", windows::core::Error::from_thread());
            return Err(windows::core::Error::from_thread());
        }

        let dpi_hint = GetDpiForSystem();
        let w_px = (args.w * dpi_hint as f32 / 96.0).round() as i32;
        let h_px = (args.h * dpi_hint as f32 / 96.0).round() as i32;
        let ex_style =
            WS_EX_NOREDIRECTIONBITMAP | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST;
        let hwnd = CreateWindowExW(
            ex_style,
            cls,
            w!("MiniCard · 自绘"),
            WS_POPUP,
            args.x,
            args.y,
            w_px,
            h_px,
            None,
            None,
            Some(hinst.into()),
            None,
        )?;

        let dpi = {
            let d = GetDpiForWindow(hwnd);
            if d == 0 {
                dpi_hint
            } else {
                d
            }
        } as f32;
        let w_px = (args.w * dpi / 96.0).round() as u32;
        let h_px = (args.h * dpi / 96.0).round() as u32;
        SetWindowPos(
            hwnd,
            None,
            args.x,
            args.y,
            w_px as i32,
            h_px as i32,
            SWP_NOZORDER | SWP_NOACTIVATE,
        )?;

        let tokens = Tokens::perf(args.w, args.h);
        // 裁剪半径必须跟「正在画的那张卡」一致，否则小卡（网络/番茄钟，半径 16）会被裁紧
        let clip_radius = match args.probe.as_deref() {
            Some("disks") => live::radius_of(minicard_core::config::CardKind::Disks, args.w, args.h),
            Some("clock") => live::radius_of(minicard_core::config::CardKind::Clock, args.w, args.h),
            Some("net") => live::radius_of(minicard_core::config::CardKind::NetRate, args.w, args.h),
            Some("pomo") => live::radius_of(minicard_core::config::CardKind::Pomodoro, args.w, args.h),
            Some("todo") => live::radius_of(minicard_core::config::CardKind::Todo, args.w, args.h),
            Some("weather") => live::radius_of(minicard_core::config::CardKind::Weather, args.w, args.h),
            Some("music") => live::radius_of(minicard_core::config::CardKind::Music, args.w, args.h),
            Some("calendar") => live::radius_of(minicard_core::config::CardKind::Calendar, args.w, args.h),
            Some("dock") => minicard_cards::dock::RADIUS,
            _ => tokens.radius,
        };
        let mut gfx = gfx::Gfx::new(hwnd, w_px, h_px, dpi, clip_radius)?;
        gfx.dump = args.dump.clone();
        gfx.embolden_off = args.emb_off;
        gfx.embolden_a = args.emb_a;
        gfx.text_dx = args.text_dx;
        gfx.text_dy = args.text_dy;
        if let Some(d) = &args.dump_seq {
            std::fs::create_dir_all(d).ok();
            gfx.dump_seq = Some(d.clone());
            gfx.dump_seq_left.set(400);
        }
        mem::log("D3D11+D2D+DComp 后");

        // 玻璃：窗口还没 show，此刻拿到的就是「窗口背后那块」
        let mut glass_bmp = None;
        let mut glass_rect = D2D_RECT_F::default();
        if !args.no_glass {
            let pad_px = ((args.blur * 3.0).max(60.0) * dpi / 96.0).round() as i32;
            let from_wall = args.wall_raw.as_ref().and_then(|p| {
                let bgra = std::fs::read(p).ok()?;
                if args.wall_w == 0 || args.wall_h == 0 {
                    mlog!("--wall-raw 需要同时给 --wall-w/--wall-h");
                    return None;
                }
                Some(Wall { bgra, w: args.wall_w, h: args.wall_h })
            });
            let captured = match from_wall {
                Some(wall) => {
                    let t = Instant::now();
                    let cap = glass::from_wallpaper(
                        &wall,
                        args.mon,
                        (args.x as f32, args.y as f32),
                        (args.w as u32, args.h as u32),
                        pad_px as f32,
                        args.blur * dpi / 96.0,
                    );
                    mlog!(
                        "壁纸切片 {}×{}（{}×{} → cover，显示器 {:?}，卡位 {},{})，含模糊 {:.1} ms",
                        cap.w,
                        cap.h,
                        wall.w,
                        wall.h,
                        args.mon,
                        args.x,
                        args.y,
                        t.elapsed().as_secs_f64() * 1000.0
                    );
                    Ok(cap)
                }
                None => glass::capture(
                    args.x - pad_px,
                    args.y - pad_px,
                    w_px as i32 + pad_px * 2,
                    h_px as i32 + pad_px * 2,
                )
                .map(|mut cap| {
                    let t = Instant::now();
                    glass::blur(&mut cap, args.blur * dpi / 96.0);
                    mlog!(
                        "截屏 {}×{}，模糊 {:.1} ms",
                        cap.w,
                        cap.h,
                        t.elapsed().as_secs_f64() * 1000.0
                    );
                    cap
                }),
            };
            match captured {
                Ok(cap) => match gfx.bitmap_from_bgra(&cap.bgra, cap.w, cap.h) {
                    Ok(b) => {
                        let pad_dip = pad_px as f32 * 96.0 / dpi;
                        glass_rect = D2D_RECT_F {
                            left: -pad_dip,
                            top: -pad_dip,
                            right: args.w + pad_dip,
                            bottom: args.h + pad_dip,
                        };
                        glass_bmp = Some(b);
                    }
                    Err(e) => mlog!("玻璃位图创建失败: {e:?}"),
                },
                Err(e) => mlog!("截屏失败: {e:?}"),
            }
        }
        mem::log("玻璃准备后");

        // 按字重装真字面：只装 Regular 时 600 是 DWrite 合成加粗，advance 与字形都会偏
        let dir = args.font.parent().unwrap_or(std::path::Path::new("."));
        let mut files: Vec<std::path::PathBuf> = ["MiSans-Regular.ttf", "MiSans-Medium.ttf", "MiSans-Semibold.ttf"]
            .iter()
            .map(|n| dir.join(n))
            .filter(|p| p.exists())
            .collect();
        if files.is_empty() {
            files.push(args.font.clone());
        }
        let refs: Vec<&std::path::Path> = files.iter().map(|p| p.as_path()).collect();
        let te = TextEngine::new_weights(&refs, "MiSans")?;
        mlog!("字体：{} 个字面", refs.len());
        mlog!(
            "字体 MiSans 载入：ascent {:.4}em / descent {:.4}em / upem {}",
            te.ascent,
            te.descent,
            te.upem
        );
        mem::log("字体载入后");

        let animate_probe = args.probe.as_deref() == Some("motion");
        let mut app = Box::new(App {
            gfx,
            te,
            pal: Palette::new(args.glass, args.debug_alpha),
            tokens,
            glass_bmp,
            glass_rect,
            snap: spawn_sampler(),
            t0,
            first_frame: false,
            exit_after: args.exit_after.or(if args.dump.is_some() { Some(2500) } else { None }),
            frames: 0,
            freeze: args.freeze,
            animate: args.animate || animate_probe,
            tweens: Vec::new(),
            last_frame: None,
            frame_dt: Vec::new(),
            frame_draw_us: Vec::new(),
            snap_text: args.snap_text,
            probe: args.probe.clone(),
            cover: args.cover.clone(),
            debug_layout: args.debug_layout,
            roll_ms: args.roll_ms,
            pomo_remain: args.pomo_remain,
        });

    // 文字栅格化参数（只有显式给了 --gamma 才动，默认沿用系统）
    if args.gamma > 0.0 {
        // 实测结论（2026-10-31）：这条路上 SetTextRenderingParams **不改变输出**。
        // 引擎把 SetTextAntialiasMode 设成 GRAYSCALE 后参数被忽略；改成 DEFAULT 也没变化。
        // 保留开关只为将来换字体/换目标时能快速复验，别指望它调 AA。
        match app.te.rendering_params(args.gamma, args.contrast, args.rmode) {
            Ok(rp) => {
                let _ = app.gfx.d2d.SetTextRenderingParams(&rp);
            }
            Err(e) => crate::mlog!("文字参数设置失败: {e:?}"),
        }
    }

        app.repaint();

        let ptr = Box::into_raw(app);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, ptr as isize);

        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        if args.desktop_layer {
            match desktop::insert_above_desktop(hwnd) {
                Ok(anchor) => {
                    let (above, below) = desktop::neighbors(hwnd);
                    mlog!(
                        "已插到桌面锚点正上方：anchor={:?}，z 序上一窗=«{}» 下一窗=«{}»",
                        anchor.0,
                        above,
                        below
                    );
                }
                Err(e) => mlog!("插桌面层失败: {e:?}"),
            }
        }
        if let Some((vx, vy, vw, vh)) = args.viewer {
            match desktop::spawn_viewer(hwnd, vw, vh, vx, vy) {
                Ok(v) => mlog!("取证宿主窗 {:?} 已开", v.0),
                Err(e) => mlog!("宿主窗失败: {e:?}"),
            }
        }
        {
            let mut r = RECT::default();
            let _ = GetWindowRect(hwnd, &mut r);
            mlog!(
                "show 后窗口 rect = {},{},{},{}  visible={}",
                r.left,
                r.top,
                r.right,
                r.bottom,
                IsWindowVisible(hwnd).as_bool()
            );
        }
        SetTimer(Some(hwnd), TIMER_TICK, 1000, None);

        // 帧循环：静止期阻塞等消息（零成本），动效期自己按 60fps 节流
        let _ = timeBeginPeriod(1);
        let mut next_frame = Instant::now();
        let mut msg = MSG::default();
        'outer: loop {
            let p = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut App;
            if p.is_null() {
                break;
            }
            let animating = (*p).animating();
            if !animating {
                if GetMessageW(&mut msg, None, 0, 0).0 <= 0 {
                    break;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            } else {
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    if msg.message == WM_QUIT {
                        break 'outer;
                    }
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                let p = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut App;
                if p.is_null() {
                    break;
                }
                (*p).frame_tick();
                next_frame += std::time::Duration::from_micros(16_667);
                let now = Instant::now();
                if next_frame > now {
                    std::thread::sleep(next_frame - now);
                } else {
                    next_frame = now;
                }
            }
        }
    }
    Ok(())
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let app_ptr = || GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut App;
    match msg {
        WM_TIMER if wp.0 == TIMER_TICK => {
            let p = app_ptr();
            if !p.is_null() {
                let app = &mut *p;
                app.repaint();
                if let Some(ms) = app.exit_after {
                    if app.t0.elapsed().as_millis() as u64 >= ms {
                        mlog!("--exit-after 到点，退出（共 {} 帧）", app.frames);
                        let _ = DestroyWindow(hwnd);
                    }
                }
            }
            LRESULT(0)
        }
        WM_NCHITTEST => LRESULT(2), // HTCAPTION：整卡可拖动（磁吸在 F 段接）
        WM_RBUTTONDOWN => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_LBUTTONDOWN => LRESULT(0),
        WM_PAINT => {
            let _ = ValidateRect(Some(hwnd), None);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_CLOSE => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            let p = app_ptr();
            if !p.is_null() {
                drop(Box::from_raw(p));
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            }
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}
