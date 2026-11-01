//! 真数据接线：一个进程、每张启用的卡片一个窗口，共用一份采样快照。
//!
//! 与 `main.rs` 的单窗探针模式分开：探针用写死数据、逐像素对照；这里用 `minicard-core`
//! 的真采样（与 Tauri 外壳同一份实现），窗口位置/尺寸/开关全部来自 `config.toml`。
//!
//! 资源口径：**TextEngine 共享一份**（3.5 MB 字体只装一次），**Gfx 每窗一份**
//! （D2D 设备上下文是窗口绑定的）。绘制层的笔刷/描边/位图缓存键都带上下文身份，
//! 所以共享 TextEngine 不会串。

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows::core::{w, Result};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::ID2D1Bitmap1;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::Graphics::Gdi::HBRUSH;
use windows::Win32::UI::HiDpi::{GetDpiForSystem, GetDpiForWindow};
use windows::Win32::Graphics::Gdi::{CreateRoundRectRgn, ScreenToClient, SetWindowRgn};
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows::Win32::UI::WindowsAndMessaging::*;

use minicard_cards::{calendar, clock, disks, lunar, music, net, perf, pomo, todo, weather};
use minicard_core::config::{CardKind, Config, TodoItem};
use minicard_core::data::{self, SharedSnap};
use minicard_core::{media, weather as wx};
use minicard_draw::color::Palette;
use minicard_draw::glass;
use minicard_draw::text::TextEngine;
use minicard_shell::gfx::Gfx;


/// 窗口线程只往里塞事件，动作在主循环里做（这样能正常借用 wins/cfg，不用满地 static mut）
enum UiEvent {
    Click { hwnd: isize, x: i32, y: i32 },
    DragEnd { hwnd: isize },
}

static EVENTS: Mutex<Vec<UiEvent>> = Mutex::new(Vec::new());

struct Drag {
    hwnd: isize,
    cx: i32,
    cy: i32,
    wx: i32,
    wy: i32,
    moved: bool,
}

static DRAG: Mutex<Option<Drag>> = Mutex::new(None);
/// 所有卡片当前屏幕矩形（物理 px）——磁吸要拿邻居位置，窗口线程里查这张表
static CARD_RECTS: Mutex<Vec<(isize, crate::guide::R4)>> = Mutex::new(Vec::new());
/// 磁吸参数（逻辑 px，按拖动窗口的 DPI 换算成物理 px）
static SNAP_TOL: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(crate::guide::TOL);
static SNAP_GAP: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);
static SNAP_ON: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// 一张卡窗口 + 它自己的绘制状态
struct CardWin {
    #[allow(dead_code)]
    hwnd: HWND,
    gfx: Gfx,
    kind: CardKind,
    w: f32,
    h: f32,
    state: State,
    /// 玻璃位图（按卡片位置裁壁纸）—— 先留空，等玻璃层接线
    #[allow(dead_code)]
    glass: Option<(ID2D1Bitmap1, D2D_RECT_F)>,
    /// 下次重绘时刻：有动画 16.7ms，静置按卡片自己的 refresh_ms
    next_draw: Instant,
    /// 静置节拍（config 的 refresh_ms，钳到 200ms..60s）
    refresh: Duration,
    /// 在 config.cards 里的下标（拖动/勾选要回写）
    cfg_idx: usize,
    /// 该窗口的 DPI（客户区像素 → 逻辑点）
    dpi: f32,
    /// 玻璃位图是按这个屏幕坐标（物理 px）切的
    slice_origin: (i32, i32),
    /// 拖动时把玻璃位图整体偏移这么多（逻辑点）——不用每帧重切/重模糊
    glass_off: (f32, f32),
}

/// Dock 自绘窗口（D0：静态条；D1 接 hover 放大）
struct DockWin {
    hwnd: HWND,
    gfx: Gfx,
    t: minicard_cards::dock::Tokens,
    icons: Vec<Option<ID2D1Bitmap1>>,
    /// 壁纸切片（与卡片同一口径：pad + 高斯模糊）
    glass: Option<(ID2D1Bitmap1, D2D_RECT_F)>,
    /// 窗口左上角（屏幕物理 px）—— 指针换算用
    origin: (i32, i32),
    /// 该窗的 DPI（指针/区域换算）
    dpi: f32,
    /// 布局用的指针 x（窗内逻辑 px）；离场保留最后位置，收缩才不会往左吸
    px: f32,
    /// 进场进度 0..1 与速度（临界阻尼 τ=50ms）
    ease: f32,
    ease_v: f32,
    /// 当前窗口区域是「只有条」还是「含上方 72」
    region_open: bool,
    /// 本条最后画的那一帧的几何指纹（避免静置时重复重绘）
    last_sig: u64,
    /// 醒来的时刻（悬停时 60fps 轮询光标）
    next_draw: Instant,
    /// 下一次「静置兜底重绘」的时刻（1s，与醒来节拍分开）
    next_paint: Instant,
}

/// 窗口区域：`open=false` 只放玻璃条（上方预留既不可见也不吃点击）；
/// `open=true` 往上放开 `HOVER_EXTRA`（放大图标 + 名字气泡要看得见、点得到）。
unsafe fn dock_region(dw: &DockWin, dpi: f32, open: bool) {
    let s = dpi / 96.0;
    let side = dw.t.side * s;
    let top = if open {
        (dw.t.top - minicard_core::config::Dock::HOVER_EXTRA).max(0.0) * s
    } else {
        dw.t.top * s
    };
    let right = (dw.t.w - dw.t.side) * s;
    let bottom = dw.t.h * s;
    let rgn = CreateRoundRectRgn(
        side.round() as i32,
        top.round() as i32,
        right.round() as i32,
        bottom.round() as i32,
        (40.0 * s).round() as i32,
        (40.0 * s).round() as i32,
    );
    let _ = SetWindowRgn(dw.hwnd, Some(rgn), true);
}

impl DockWin {
    /// 指针（屏幕 px）→ 窗内逻辑坐标；返回是否落在「激活区」内
    fn pointer(&self, dpi: f32) -> Option<f32> {
        let mut pt = windows::Win32::Foundation::POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut pt);
        }
        let s = dpi / 96.0;
        let x = (pt.x - self.origin.0) as f32 / s;
        let y = (pt.y - self.origin.1) as f32 / s;
        // 激活区：条本身；进场后往上放开 HOVER_EXTRA
        let grow = if self.ease > 0.002 {
            minicard_core::config::Dock::HOVER_EXTRA
        } else {
            0.0
        };
        let in_bar = x >= 0.0 && x <= self.t.w && y >= self.t.top - grow && y <= self.t.h;
        if in_bar {
            Some(x.clamp(0.0, self.t.w))
        } else {
            None
        }
    }
}


enum State {
    Perf {
        show_gpu: bool,
        /// 三行条的补间（时长 ≈ 采样间隔，否则一秒一格地跳）
        tweens: [net::Tween; 3],
    },
    Disks { warn90: bool },
    Clock(clock::Clock),
    Net(net::NetState),
    Pomo(pomo::Pomo),
    Todo(Vec<todo::Item>),
    Weather,
    Music {
        /// (封面 dataURL, 位图)；换曲才重解码
        cover: Option<(String, ID2D1Bitmap1)>,
        /// 翻转中的旧封面 + 起始时刻（0.22s 转出 + 0.26s 转回）
        prev: Option<ID2D1Bitmap1>,
        flip_t0: Option<Instant>,
        /// +1 = 下一首（向左翻），−1 = 上一首
        flip_dir: f32,
        /// 仅 `MC_FLIP_DEMO=1` 用：下一次自动翻转的时刻（没有真换曲时也能看动画）
        demo_t0: Option<Instant>,
    },
    Calendar(calendar::Calendar, i32, u32),
}

/// 本地时间的一小块：年/月/日/时/分/秒/星期（0=周日）
fn now_parts() -> (i32, u32, u32, u8, u8, u32) {
    let st = unsafe { GetLocalTime() };
    (
        st.wYear as i32,
        st.wMonth as u32,
        st.wDay as u32,
        st.wHour as u8,
        st.wMinute as u8,
        st.wDayOfWeek as u32,
    )
}

const WEEK_CN: [&str; 7] = ["日", "一", "二", "三", "四", "五", "六"];

fn date_text(y: i32, m: u32, d: u32, wd: u32) -> String {
    format!("{}年{}月{}日 周{}", y, m, d, WEEK_CN[(wd as usize) % 7])
}

fn mem_ratio(snap: &data::DataSnapshot) -> f32 {
    if snap.mem_total == 0 {
        0.0
    } else {
        (snap.mem_used as f64 / snap.mem_total as f64 * 100.0) as f32
    }
}

/// 性能卡的行（CPU / RAM / GPU），关掉 GPU 选项时只留两行
fn perf_rows(snap: &data::DataSnapshot, show_gpu: bool) -> Vec<perf::Row> {
    // 口径同 card.html `perfRows()`：三行都是**百分比**，GPU 缺数据时写「未检测到」（不带 .num）
    let cpu = snap.cpu.round();
    let ram = mem_ratio(snap).round();
    let mut rows = vec![
        perf::Row { label: "CPU".into(), text: format!("{cpu:.0}%"), pct: Some(snap.cpu) },
        perf::Row { label: "RAM".into(), text: format!("{ram:.0}%"), pct: Some(mem_ratio(snap)) },
    ];
    if show_gpu {
        match snap.gpus.first() {
            Some(g) => {
                let usage = g.usage;
                rows.push(perf::Row {
                    label: "GPU".into(),
                    text: format!("{:.0}%", usage.round()),
                    pct: Some(usage),
                })
            }
            None => rows.push(perf::Row {
                label: "GPU".into(),
                text: "未检测到".into(),
                pct: None,
            }),
        }
    }
    rows
}

fn todo_items(items: &[TodoItem]) -> Vec<todo::Item> {
    items
        .iter()
        .map(|t| todo::Item::new(t.text.clone(), t.done))
        .collect()
}

/// 当月日历（本月 1 号是周几 + 天数 + 今天）
fn calendar_of(y: i32, m: u32, today: u32) -> calendar::Calendar {
    let days = calendar::days_in_month_of(y, m);
    // 1 号的星期：用 Sakamoto 公式（0=周日），再转成「周一起始」
    let t = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    let yy = if m < 3 { y - 1 } else { y };
    let wd0 = (yy + yy / 4 - yy / 100 + yy / 400 + t[(m - 1) as usize] + 1) % 7; // 0=周日
    let mut c = calendar::Calendar::build(y, m, wd0 as u32, days, today);
    c.holiday = calendar::holiday(y, m, today);
    c
}

/// 壁纸：优先读系统壁纸文件（按卡位切片 + 模糊），读不到就退回「只画白纱」
fn load_wallpaper() -> Option<glass::Wall> {
    use windows::Win32::UI::WindowsAndMessaging::{
        SystemParametersInfoW, SPI_GETDESKWALLPAPER, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
    };
    let mut buf = [0u16; 1024];
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_GETDESKWALLPAPER,
            buf.len() as u32,
            Some(buf.as_mut_ptr() as *mut _),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    };
    if ok.is_err() {
        return None;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    if len == 0 {
        return None;
    }
    let path = std::path::PathBuf::from(String::from_utf16_lossy(&buf[..len]));
    let img = minicard_draw::image::decode_file(&path).ok()?;
    minicard_shell::mlog!("live：壁纸 {}×{} ← {}", img.w, img.h, path.display());
    Some(glass::Wall { bgra: img.bgra, w: img.w, h: img.h })
}

/// 卡片自己的圆角半径 —— **必须**从对应卡的 `Tokens` 取。
/// 曾经统一按性能卡的公式算（`min(w/240, h/120)`），而网络/番茄钟的基准是 120×120
/// （k=1 → 半径 16），于是窗口裁剪半径只有 8，比卡片本身还紧，圆角里多出一块硬边。
pub fn radius_of(kind: CardKind, w: f32, h: f32) -> f32 {
    match kind {
        CardKind::Performance => perf::Tokens::perf(w, h).radius,
        CardKind::Disks => disks::Tokens::disks(w, h, 1).radius,
        CardKind::Clock => clock::Tokens::clock(w, h).radius,
        CardKind::NetRate => net::Tokens::net(w, h, 1.044, 0.282).radius,
        CardKind::Pomodoro => pomo::Tokens::pomo(w, h).radius,
        CardKind::Todo => todo::Tokens::todo(w, h).radius,
        CardKind::Weather => weather::Tokens::weather(w, h).radius,
        CardKind::Music => music::Tokens::music(w, h).radius,
        CardKind::Calendar => calendar::Tokens::cal(w, h).radius,
    }
}

/// 卡片落在哪块屏（物理像素）：返回 (left, top, w, h)
fn monitor_of(x: i32, y: i32) -> (f32, f32, f32, f32) {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    };
    unsafe {
        let m = MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if GetMonitorInfoW(m, &mut info).as_bool() {
            let r = info.rcMonitor;
            (
                r.left as f32,
                r.top as f32,
                (r.right - r.left) as f32,
                (r.bottom - r.top) as f32,
            )
        } else {
            (0.0, 0.0, 1920.0, 1080.0)
        }
    }
}

pub fn run(
    mut cfg: Config,
    te: Arc<TextEngine>,
    pal_in: Palette,
    blur_in: f32,
    topmost: bool,
    exit_after: Option<u64>,
) -> Result<()> {
    // 热载用：强调色/主题/玻璃厚度改完不必重启绘制进程
    let mut pal = pal_in;
    let mut blur = blur_in;
    // 层级以 config 的 z_order 为准；`--live-topmost` 只是取证覆盖
    let z = if topmost {
        minicard_core::config::ZOrder::Top
    } else {
        cfg.settings.z_order
    };
    minicard_shell::mlog!("live：层级 {:?}", z);

    // 单实例：两个 live 叠在同一位置时，点击会落到「上面那个进程」的窗口上，
    // 排查起来像「点了没反应」。用命名互斥量挡掉。
    {
        use windows::Win32::Foundation::ERROR_ALREADY_EXISTS;
        use windows::Win32::System::Threading::CreateMutexW;
        let name: Vec<u16> = "MiniCardNativeLiveSingleInstance".encode_utf16().collect();
        let h = unsafe { CreateMutexW(None, true, windows::core::PCWSTR(name.as_ptr())) };
        match h {
            Ok(m) => {
                if windows::core::Error::from_thread().code() == ERROR_ALREADY_EXISTS.to_hresult() {
                    let _ = unsafe { windows::Win32::Foundation::CloseHandle(m) };
                    minicard_shell::mlog!("已有 live 实例在跑，退出");
                    return Ok(());
                }
                // 句柄故意不关（HANDLE 没有 Drop），进程退出时由系统释放
                let _keep = m;
            }
            Err(e) => minicard_shell::mlog!("单实例互斥量创建失败: {e:?}"),
        }
    }

    let snap: SharedSnap = data::spawn_sampler();
    let wshared = wx::SharedWeather::default();
    wx::seed(&wshared);
    // 天气：起一个刷新线程。以前只有 `seed` —— 卡上那张是启动那一刻的缓存，一整天不动。
    // 现在：城市改了立刻刷，否则 10 分钟一轮（失败 30 秒后重试）。
    {
        let ws = wshared.clone();
        std::thread::spawn(move || {
            let mut seen = String::new();
            let mut last = Instant::now() - Duration::from_secs(3600);
            loop {
                let cfg = Config::load();
                let city = cfg.settings.weather_city.clone();
                let city_changed = city != seen;
                let due = last.elapsed() >= Duration::from_secs(600);
                if city_changed || due {
                    match wx::refresh_local(&ws) {
                        Ok(s) => {
                            seen = city;
                            last = Instant::now();
                            minicard_shell::mlog!("天气已刷新：{} {}°", s.city, s.temp);
                        }
                        Err(e) => {
                            last = Instant::now() - Duration::from_secs(570);
                            minicard_shell::mlog!("天气刷新失败: {e}");
                        }
                    }
                }
                std::thread::sleep(Duration::from_secs(5));
            }
        });
    }

    // SMTC 音乐：core 的 Sampler 每拍更新内部快照，这里只负责起线程 + 读
    std::thread::spawn(|| {
        let mut s = media::Sampler::new();
        loop {
            let _ = s.tick();
            std::thread::sleep(Duration::from_millis(500));
        }
    });

    unsafe {
        let hinst = GetModuleHandleW(None)?;
        let cls = w!("MiniCardNativeLive");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            lpszClassName: cls,
            hbrBackground: HBRUSH::default(),
            ..Default::default()
        };
        if RegisterClassW(&wc) == 0 {
            minicard_shell::mlog!(
                "RegisterClassW 失败: {:?}",
                windows::core::Error::from_thread()
            );
            return Err(windows::core::Error::from_thread());
        }

        let wall = load_wallpaper();
        let dpi_sys = GetDpiForSystem() as f32;
        let mut wins: Vec<CardWin> = Vec::new();
        for (cfg_idx, card) in cfg.cards.iter().enumerate().filter(|(_, c)| c.enabled) {
            let (win_w, win_h) = (card.size[0], card.size[1]);
            let w_px = (win_w * dpi_sys / 96.0).round() as i32;
            let h_px = (win_h * dpi_sys / 96.0).round() as i32;
            let mut ex = WS_EX_NOREDIRECTIONBITMAP | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW;
            if matches!(z, minicard_core::config::ZOrder::Top) {
                ex |= WS_EX_TOPMOST;
            }
            let title: Vec<u16> = format!("MiniCardLive · {:?}", card.kind)
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            let hwnd = CreateWindowExW(
                ex,
                cls,
                windows::core::PCWSTR(title.as_ptr()),
                WS_POPUP,
                card.pos[0].round() as i32,
                card.pos[1].round() as i32,
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
                    dpi_sys as u32
                } else {
                    d
                }
            } as f32;
            let w_px = (win_w * dpi / 96.0).round() as u32;
            let h_px = (win_h * dpi / 96.0).round() as u32;
            let radius = radius_of(card.kind, win_w, win_h);
            let mut gfx = Gfx::new(hwnd, w_px, h_px, dpi, radius)?;
            gfx.text_dx = 0.0;

            // 玻璃：按卡位从壁纸切一块（含 pad）再模糊，与 card.html 的 .wall 同一口径
            let pad_px = ((blur * 3.0).max(60.0) * dpi / 96.0).round() as i32;
            // 每张卡按**它自己那块屏**切（副屏、负坐标屏都要对）
            let mon = monitor_of(card.pos[0] as i32, card.pos[1] as i32);
            let pos_rel = (card.pos[0] - mon.0, card.pos[1] - mon.1);
            let glass_pair = match &wall {
                Some(w) => {
                    let cap = glass::from_wallpaper(
                        w,
                        (mon.2, mon.3),
                        pos_rel,
                        (w_px, h_px),
                        pad_px as f32,
                        blur * dpi / 96.0,
                    );
                    match gfx.bitmap_from_bgra(&cap.bgra, cap.w, cap.h) {
                        Ok(b) => {
                            let pad_dip = pad_px as f32 * 96.0 / dpi;
                            Some((
                                b,
                                D2D_RECT_F {
                                    left: -pad_dip,
                                    top: -pad_dip,
                                    right: win_w + pad_dip,
                                    bottom: win_h + pad_dip,
                                },
                            ))
                        }
                        Err(e) => {
                            minicard_shell::mlog!("玻璃位图失败: {e:?}");
                            None
                        }
                    }
                }
                None => None,
            };

            let now = Instant::now();
            let state = match card.kind {
                CardKind::Performance => State::Perf {
                    show_gpu: card.show_gpu,
                    tweens: [
                        net::Tween::with_ms(0.0, 0.0, now, 950.0),
                        net::Tween::with_ms(0.0, 0.0, now, 950.0),
                        net::Tween::with_ms(0.0, 0.0, now, 950.0),
                    ],
                },
                CardKind::Disks => State::Disks { warn90: card.warn90 },
                CardKind::Clock => {
                    let mut c = clock::Clock::new();
                    let (_, _, _, hh, mm, _) = now_parts();
                    c.snap(hh, mm);
                    State::Clock(c)
                }
                CardKind::NetRate => State::Net(net::NetState::new(0.0, 0.0, now)),
                CardKind::Pomodoro => State::Pomo(pomo::Pomo::new()),
                CardKind::Todo => State::Todo(todo_items(&card.todo)),
                CardKind::Weather => State::Weather,
                CardKind::Music => State::Music { cover: None, prev: None, flip_t0: None, flip_dir: 1.0, demo_t0: None },
                CardKind::Calendar => {
                    let (y, m, d, _, _, _) = now_parts();
                    State::Calendar(calendar_of(y, m, d), 0, d)
                }
            };
            // Desktop 档：插到 Progman/WorkerW 锚点正上方（被普通窗口压住 = 与真卡同层）
            if matches!(z, minicard_core::config::ZOrder::Desktop) {
                if let Err(e) = minicard_shell::desktop::insert_above_desktop(hwnd) {
                    minicard_shell::mlog!("桌面层插入失败: {e:?}");
                }
            }
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            wins.push(CardWin {
                hwnd,
                gfx,
                kind: card.kind,
                w: win_w,
                h: win_h,
                state,
                glass: glass_pair,
                next_draw: Instant::now(),
                refresh: Duration::from_millis(card.refresh_ms.clamp(200, 60_000)),
                cfg_idx,
                dpi,
                slice_origin: (card.pos[0].round() as i32, card.pos[1].round() as i32),
                glass_off: (0.0, 0.0),
            });
        }
        // 磁吸参数 + 辅助线窗口
        SNAP_ON.store(cfg.settings.snap_on_drag, std::sync::atomic::Ordering::Relaxed);
        SNAP_GAP.store(cfg.settings.card_gap.round() as i32, std::sync::atomic::Ordering::Relaxed);
        let _ = crate::guide::ensure_overlay();
        // ── Dock 自绘（dock.enabled && dock.native） ──
        minicard_shell::mlog!(
            "dock 配置：enabled={} native={} items={} path={:?}",
            cfg.dock.enabled,
            cfg.dock.native,
            cfg.dock.items.len(),
            minicard_core::config::path()
        );
        let mut dock_win: Option<DockWin> = None;
        if cfg.dock.enabled && cfg.dock.native {
            let d = cfg.dock.clone();
            let dk = minicard_cards::dock::Tokens::dock(&d);
            let w_px = (dk.w * dpi_sys / 96.0).round() as i32;
            let h_px = (dk.h * dpi_sys / 96.0).round() as i32;
            // 窗口 = 条 + 两侧预留（悬停外扩）/ 条高 + 顶部预留（气泡、菜单）；
            // `d.y` 的口径是「虚拟 92 框的上边缘」= 条顶 − 28（与外壳 dock_window_rect 一致）
            let x = (d.cx - dk.w / 2.0).round() as i32;
            let y = (d.y + minicard_core::config::Dock::MARGIN - dk.top).round() as i32;
            let mut ex = WS_EX_NOREDIRECTIONBITMAP | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW;
            if matches!(d.z, minicard_core::config::ZOrder::Top) {
                ex |= WS_EX_TOPMOST;
            }
            let title: Vec<u16> = "MiniCardLive · Dock"
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            match CreateWindowExW(
                ex,
                cls,
                windows::core::PCWSTR(title.as_ptr()),
                WS_POPUP,
                x,
                y,
                w_px,
                h_px,
                None,
                None,
                Some(hinst.into()),
                None,
            ) {
                Ok(hwnd) => {
                    let dpi = {
                        let d = GetDpiForWindow(hwnd);
                        if d == 0 { dpi_sys as u32 } else { d }
                    } as f32;
                    let w_p = (dk.w * dpi / 96.0).round() as u32;
                    let h_p = (dk.h * dpi / 96.0).round() as u32;
                    let mut gfx = Gfx::new(hwnd, w_p, h_p, dpi, minicard_cards::dock::RADIUS)?;
                    gfx.text_dx = 0.0;
                    // 图标：与外壳同一套缓存命名
                    let mut icons: Vec<Option<ID2D1Bitmap1>> = Vec::new();
                    for it in &d.items {
                        let bmp = dock_icon_path(it)
                            .and_then(|p| minicard_draw::image::decode_file(&p).ok())
                            .and_then(|img| minicard_draw::image::bitmap(&gfx, &img).ok());
                        icons.push(bmp);
                    }
                    // 玻璃：与卡片同口径（按所在屏切壁纸 + pad + 高斯模糊）
                    let pad_px = ((blur * 3.0).max(60.0) * dpi / 96.0).round() as i32;
                    let mon = monitor_of(x, y);
                    let glass_pair = match &wall {
                        Some(w) => {
                            let cap = glass::from_wallpaper(
                                w,
                                (mon.2, mon.3),
                                (x as f32 - mon.0, y as f32 - mon.1),
                                (w_px as u32, h_px as u32),
                                pad_px as f32,
                                blur * dpi / 96.0,
                            );
                            match gfx.bitmap_from_bgra(&cap.bgra, cap.w, cap.h) {
                                Ok(b) => {
                                    let pad_dip = pad_px as f32 * 96.0 / dpi;
                                    Some((
                                        b,
                                        D2D_RECT_F {
                                            left: -pad_dip,
                                            top: -pad_dip,
                                            right: dk.w + pad_dip,
                                            bottom: dk.h + pad_dip,
                                        },
                                    ))
                                }
                                Err(_) => None,
                            }
                        }
                        None => None,
                    };
                    if matches!(d.z, minicard_core::config::ZOrder::Desktop) {
                        if let Err(e) = minicard_shell::desktop::insert_above_desktop(hwnd) {
                            minicard_shell::mlog!("Dock 桌面层插入失败: {e:?}");
                        }
                    }
                    let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                    minicard_shell::mlog!("Dock 自绘：{} 项，窗口 {}×{}", d.items.len(), w_px, h_px);
                    let dk_dpi = dpi;
                    let mut dw = DockWin {
                        hwnd,
                        gfx,
                        t: dk,
                        icons,
                        glass: glass_pair,
                        origin: (x, y),
                        dpi: dk_dpi,
                        px: dk.w / 2.0,
                        ease: 0.0,
                        ease_v: 0.0,
                        region_open: false,
                        last_sig: 0,
                        next_draw: Instant::now(),
                        next_paint: Instant::now(),
                    };
                    dock_region(&dw, dk_dpi, false);
                    dw.next_draw = Instant::now();
                    dock_win = Some(dw);
                }
                Err(e) => minicard_shell::mlog!("Dock 窗口创建失败: {e:?}"),
            }
        }

        minicard_shell::mlog!(
            "live：{} 张卡窗口（磁吸 {}，固定间距 {}px）",
            wins.len(),
            if cfg.settings.snap_on_drag { "开" } else { "关" },
            cfg.settings.card_gap
        );

        // 帧循环：自节拍 16.667ms（与单窗模式同一套），消息循环与绘制交替
        let _timer = windows::Win32::Media::timeBeginPeriod(1);
        let t_start = Instant::now();
        let mut cfg_mtime = std::fs::metadata(minicard_core::config::path())
            .and_then(|m| m.modified())
            .ok();
        loop {
            // 先把消息吃完
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                if msg.message == WM_QUIT {
                    clean_exit();
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            if let Some(limit) = exit_after {
                if t_start.elapsed().as_millis() as u64 >= limit {
                    clean_exit();
                }
            }
            // 窗口消息里只记事件；动作在这里做（能正常借用 wins/cfg/te）
            let events: Vec<UiEvent> = { EVENTS.lock().unwrap().drain(..).collect() };
            for ev in events {
                match ev {
                    UiEvent::Click { hwnd, x, y } => {
                        click(&mut wins, &mut cfg, &te, &pal, HWND(hwnd as *mut _), x, y);
                    }
                    UiEvent::DragEnd { hwnd } => {
                        drag_end(&mut wins, &mut cfg, &wall, blur, HWND(hwnd as *mut _));
                    }
                }
            }

            let now = Instant::now();
            let (y, mo, d, hh, mm, _) = now_parts();
            let date = date_text(y, mo, d, now_parts().5);
            let lunar_txt = lunar::lunar_text(y, mo, d).unwrap_or_default();

            // 悬停：指针在哪张卡上（卡片内坐标），变了就立刻重绘
            let mut cur_pt = POINT::default();
            let _ = GetCursorPos(&mut cur_pt);
            let under = WindowFromPoint(cur_pt);
            let dragging_hwnd = DRAG
                .lock()
                .unwrap()
                .as_ref()
                .filter(|d| d.moved)
                .map(|d| d.hwnd);
            for win in wins.iter_mut() {
                let hov = if dragging_hwnd == Some(win.hwnd.0 as isize) {
                    None
                } else if under == win.hwnd {
                    let mut r = RECT::default();
                    if GetWindowRect(win.hwnd, &mut r).is_ok() {
                        let k = 96.0 / win.dpi.max(1.0);
                        Some(((cur_pt.x - r.left) as f32 * k, (cur_pt.y - r.top) as f32 * k))
                    } else {
                        None
                    }
                } else {
                    None
                };
                if win.gfx.hover != hov {
                    win.gfx.hover = hov;
                    win.next_draw = now;
                }
            }

            // 拖动中：玻璃位图按窗口位移平移（实时跟手），越界才重切
            {
                let dragging = DRAG
                    .lock()
                    .unwrap()
                    .as_ref()
                    .filter(|d| d.moved)
                    .map(|d| d.hwnd);
                if let Some(dh) = dragging {
                    for win in wins.iter_mut() {
                        if win.hwnd.0 as isize != dh {
                            continue;
                        }
                        let mut r = RECT::default();
                        if GetWindowRect(win.hwnd, &mut r).is_err() {
                            break;
                        }
                        let k = 96.0 / win.dpi.max(1.0);
                        let off = (
                            (r.left - win.slice_origin.0) as f32 * k,
                            (r.top - win.slice_origin.1) as f32 * k,
                        );
                        if off.0.abs() > DRAG_MARGIN * 0.55 || off.1.abs() > DRAG_MARGIN * 0.55 {
                            reslice(win, &wall, blur);
                        } else {
                            win.glass_off = off;
                        }
                        win.next_draw = now;
                    }
                }
            }

            // config 热载：设置/卡片选项改完不用重启绘制进程
            {
                let now_m = std::fs::metadata(minicard_core::config::path())
                    .and_then(|m| m.modified())
                    .ok();
                if now_m != cfg_mtime {
                    cfg_mtime = now_m;
                    let nc = Config::load();
                    SNAP_ON.store(nc.settings.snap_on_drag, std::sync::atomic::Ordering::Relaxed);
                    SNAP_GAP.store(
                        nc.settings.card_gap.round() as i32,
                        std::sync::atomic::Ordering::Relaxed,
                    );
                    // 强调色 / 主题 / 玻璃厚度 → 重建调色板
                    let pal_changed = nc.settings.accent != cfg.settings.accent
                        || nc.settings.glass_alpha != cfg.settings.glass_alpha
                        || !matches_theme(nc.settings.theme, cfg.settings.theme);
                    if pal_changed {
                        let dark = !matches!(nc.settings.theme, minicard_core::config::Theme::Light);
                        pal = Palette::new(nc.settings.glass_alpha as f32, false)
                            .with_accent(nc.settings.accent, dark);
                        minicard_shell::mlog!(
                            "config 热载：强调色 rgb{:?} / 玻璃 {}/100 / 主题 {:?}",
                            nc.settings.accent,
                            nc.settings.glass_alpha,
                            nc.settings.theme
                        );
                    }
                    // 模糊强度 → 重切所有玻璃
                    let blur_changed = nc.settings.glass_blur != cfg.settings.glass_blur;
                    if blur_changed {
                        blur = nc.settings.glass_blur as f32;
                        minicard_shell::mlog!("config 热载：模糊 {}px（重切玻璃）", blur);
                    }
                    // 层级档改了：就地重摆（以前要重启绘制进程）
                    if !matches_theme_str(nc.settings.z_order, cfg.settings.z_order) && !topmost {
                        for win in wins.iter() {
                            apply_z(win.hwnd, nc.settings.z_order);
                        }
                        minicard_shell::mlog!("config 热载：层级 {:?}", nc.settings.z_order);
                    }
                    for win in wins.iter_mut() {
                        if blur_changed {
                            reslice(win, &wall, blur);
                        }
                        if let Some(card) = nc.cards.get(win.cfg_idx) {
                            win.refresh =
                                Duration::from_millis(card.refresh_ms.clamp(200, 60_000));
                            match &mut win.state {
                                State::Perf { show_gpu, .. } => *show_gpu = card.show_gpu,
                                State::Disks { warn90 } => *warn90 = card.warn90,
                                _ => {}
                            }
                        }
                        win.next_draw = now;
                    }
                    cfg = nc;
                }
            }

            // 刷新邻居矩形表（磁吸用；每帧一次 GetWindowRect，9 张卡可忽略）
            {
                let mut rects = Vec::with_capacity(wins.len());
                for win in wins.iter() {
                    let mut r = RECT::default();
                    if GetWindowRect(win.hwnd, &mut r).is_ok() {
                        rects.push((win.hwnd.0 as isize, (r.left, r.top, r.right, r.bottom)));
                    }
                }
                *CARD_RECTS.lock().unwrap() = rects;
            }

            // Dock：悬停放大（每帧算，指针不动就不重绘）
            if let Some(dw) = dock_win.as_mut() {
                let dpi = dw.dpi;
                // 验收用：`MC_DOCK_DEMO=<窗内 x>` 把指针钉死，不依赖真鼠标（截图取证）
                let demo_x = std::env::var("MC_DOCK_DEMO").ok().and_then(|s| s.parse::<f32>().ok());
                let hit = match demo_x {
                    Some(x) => Some(x),
                    None => dw.pointer(dpi),
                };
                // 进出场：临界阻尼（τ=50ms），与 dock.html 同一个时钟感
                let target = if hit.is_some() { 1.0 } else { 0.0 };
                if let Some(x) = hit {
                    dw.px = x;
                }
                let moving = (dw.ease - target).abs() > 0.0015 || dw.ease_v.abs() > 0.01;
                if moving {
                    let dt = (now - dw.next_draw.min(now)).as_secs_f32().min(0.05);
                    let dt = if dt <= 0.0 { 1.0 / 60.0 } else { dt };
                    let (e, v) = minicard_cards::dock::step_ease(dw.ease, dw.ease_v, target, dt, 0.05);
                    dw.ease = e;
                    dw.ease_v = v;
                } else if target == 0.0 {
                    dw.ease = 0.0;
                    dw.ease_v = 0.0;
                } else {
                    dw.ease = 1.0;
                    dw.ease_v = 0.0;
                }
                // 区域跟着放/收（放：放大图标 + 气泡要能看见、能点）
                let want_open = dw.ease > 0.002;
                if want_open != dw.region_open {
                    dock_region(dw, dpi, want_open);
                    dw.region_open = want_open;
                }
                if std::env::var("MC_DOCK_LOG").is_ok() {
                    use std::sync::atomic::{AtomicU64, Ordering};
                    static LAST: AtomicU64 = AtomicU64::new(0);
                    let ms = t_start.elapsed().as_millis() as u64;
                    if ms - LAST.load(Ordering::Relaxed) >= 900 {
                        LAST.store(ms, Ordering::Relaxed);
                        let mut pt = windows::Win32::Foundation::POINT::default();
                        let _ = GetCursorPos(&mut pt);
                        minicard_shell::mlog!(
                            "dock 诊断：光标 ({},{}) 原点 {:?} hit={:?} ease={:.3}",
                            pt.x, pt.y, dw.origin, hit, dw.ease
                        );
                    }
                }
                let sig = (dw.ease * 4096.0) as u64 * 1_000_003
                    + (dw.px * 8.0) as u64
                    + hit.is_some() as u64;
                // 静置兜底 1s 一回；悬停但指针不动**不重绘**（否则白烧 60fps）
                let idle_due = now >= dw.next_paint;
                if moving || sig != dw.last_sig || idle_due {
                    dw.next_paint = now + Duration::from_millis(1000);
                    let DockWin { gfx, t: tk, icons, glass, .. } = dw;
                    let refs: Vec<Option<&ID2D1Bitmap1>> =
                        icons.iter().map(|o| o.as_ref()).collect();
                    let running = vec![false; icons.len()];
                    let gg = glass.as_ref().map(|(b, r)| (b, *r));
                    let geo = tk.geo(&cfg.dock, Some(dw.px), dw.ease);
                    let r = minicard_cards::dock::paint(
                        gfx, &te, &pal, tk, &cfg.dock, &geo, &refs, &running, gg,
                    );
                    let _ = r;
                    dw.last_sig = sig;
                }
                // 悬停/动画中 60fps 醒着（只读光标，几乎不费 CPU）；静置 1s 一回
                dw.next_draw = now
                    + if moving || hit.is_some() {
                        Duration::from_micros(16_667)
                    } else {
                        Duration::from_millis(1000)
                    };
            }

            let mut earliest = now + Duration::from_millis(250);
            let mut drawn = 0usize;
            for win in wins.iter_mut() {
                if now < win.next_draw {
                    if win.next_draw < earliest {
                        earliest = win.next_draw;
                    }
                    continue;
                }
                drawn += 1;
                let animating = draw_one(
                    win,
                    &snap,
                    &wshared,
                    &te,
                    &pal,
                    now,
                    (y, mo, d, hh, mm),
                    &date,
                    &lunar_txt,
                )?;
                // 动画中 60fps；静置按 1s 走（采样也是 1s 一拍），空闲几乎不烧 CPU
                win.next_draw = now
                    + if animating {
                        Duration::from_micros(16_667)
                    } else {
                        win.refresh
                    };
                if win.next_draw < earliest {
                    earliest = win.next_draw;
                }
            }

            // Dock 的唤醒时刻也要算进等待（否则放大动画会被 100ms 的睡眠掐成 10fps）
            if let Some(dw) = dock_win.as_ref() {
                if dw.next_draw < earliest {
                    earliest = dw.next_draw;
                }
            }

            if std::env::var("MC_DRAW_LOG").is_ok() {
                use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
                static LAST: AtomicU64 = AtomicU64::new(0);
                static CNT: AtomicUsize = AtomicUsize::new(0);
                let n = CNT.fetch_add(drawn, Ordering::Relaxed) + drawn;
                let ms = t_start.elapsed().as_millis() as u64;
                if ms - LAST.load(Ordering::Relaxed) >= 1000 {
                    minicard_shell::mlog!("draw {} 次/秒", n);
                    LAST.store(ms, Ordering::Relaxed);
                    CNT.store(0, Ordering::Relaxed);
                }
            }
            let now2 = Instant::now();
            if earliest > now2 {
                let wait = (earliest - now2).min(Duration::from_millis(100));
                std::thread::sleep(wait);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_one(
    win: &mut CardWin,
    snap: &SharedSnap,
    wshared: &wx::SharedWeather,
    te: &TextEngine,
    pal: &Palette,
    now: Instant,
    t: (i32, u32, u32, u8, u8),
    date: &str,
    lunar_txt: &str,
) -> Result<bool> {
    let (y, mo, d, hh, mm) = t;
    let (gox, goy) = win.glass_off;
    let glass_pair = win.glass.as_ref().map(|(b, r)| {
        (
            b,
            D2D_RECT_F {
                left: r.left - gox,
                top: r.top - goy,
                right: r.right - gox,
                bottom: r.bottom - goy,
            },
        )
    });
    let snap_r = snap.read().unwrap();
    let animating = match (&mut win.state, win.kind) {
        (State::Perf { show_gpu, tweens }, _) => {
            let mut rows = perf_rows(&snap_r, *show_gpu);
            // 条宽补间：时长照抄 card.html::barMs（采样间隔 ×0.95，钳 300..1500），
            // 短了就是「一秒跳一格」，用户看到的就是不连续
            let ms = (win.refresh.as_millis() as f32 * 0.95).clamp(300.0, 1500.0);
            for (i, row) in rows.iter_mut().enumerate() {
                let Some(target) = row.pct else { continue };
                if (tweens[i].to - target as f64).abs() > 0.01 {
                    let from = tweens[i].value(now);
                    tweens[i] = net::Tween::with_ms(from, target as f64, now, ms);
                }
                // 数字用目标值（文字本来就一秒一跳），条的宽度用补间值
                row.pct = Some(tweens[i].value(now).clamp(0.0, 100.0) as f32);
            }
            let animating = tweens.iter().any(|t| t.live(now));
            let tk = perf::Tokens::perf(win.w, win.h);
            perf::paint(&win.gfx, te, pal, &tk, &rows, glass_pair, false)?;
            animating
        }
        (State::Disks { warn90 }, _) => {
            let list: Vec<disks::Disk> = snap_r
                .disks
                .iter()
                .take(5)
                .map(|d| disks::Disk {
                    name: d.name.chars().next().unwrap_or('?').to_string(),
                    used: d.used,
                    total: d.total,
                })
                .collect();
            let tk = disks::Tokens::disks(win.w, win.h, list.len());
            disks::paint(&win.gfx, te, pal, &tk, &list, glass_pair, *warn90, false)?;
            false
        }
        (State::Clock(c), _) => {
            c.set(hh, mm, now);
            let tk = clock::Tokens::clock(win.w, win.h);
            clock::paint(&win.gfx, te, pal, &tk, c, date, lunar_txt, now, glass_pair)?;
            !c.idle(now)
        }
        (State::Net(st), _) => {
            st.set(snap_r.net.tx, snap_r.net.rx, now);
            let tk = net::Tokens::net(win.w, win.h, 1.044, 0.282);
            net::paint(&win.gfx, te, pal, &tk, st, now, glass_pair)?;
            st.animating(now)
        }
        (State::Pomo(p), _) => {
            p.tick(now);
            let tk = pomo::Tokens::pomo(win.w, win.h);
            pomo::paint(&win.gfx, te, pal, &tk, p, now, glass_pair)?;
            p.is_running()
        }
        (State::Todo(items), _) => {
            let tk = todo::Tokens::todo(win.w, win.h);
            todo::paint(&win.gfx, te, pal, &tk, items, now, glass_pair)?;
            todo::animating(items, now)
        }
        (State::Weather, _) => {
            let ws = wshared.read().unwrap().clone();
            let (city, temp, code, air, hum, days) = match &ws {
                Some(s) => (
                    s.city.clone(),
                    Some(s.temp as f32),
                    s.code as u8,
                    Some(s.air.clone()),
                    Some(s.humidity as i32),
                    s.days
                        .iter()
                        .map(|d| weather::Day {
                            label: d.label.clone(),
                            hi: Some(d.hi as f32),
                            lo: Some(d.lo as f32),
                            code: d.code as u8,
                        })
                        .collect(),
                ),
                None => ("—".to_string(), None, 3, None, None, Vec::new()),
            };
            let wk = weather::Tokens::weather(win.w, win.h);
            let wv = weather::Weather {
                city,
                temp,
                code,
                days,
                air,
                humidity: hum,
            };
            weather::paint(&win.gfx, te, pal, &wk, &wv, glass_pair)?;
            false
        }
        (State::Music { cover, prev, flip_t0, flip_dir, demo_t0 }, _) => {
            let m = media::snapshot();
            let mw = match &m {
                Some(s) => music::Music {
                    title: s.title.clone(),
                    artist: s.artist.clone(),
                    playing: s.status == "playing",
                    position_ms: s.position_ms as u64,
                    duration_ms: s.duration_ms as u64,
                },
                None => music::Music {
                    // 无会话 = 占位样张（字面词，不填样例数据）
                    title: "歌名".into(),
                    artist: "歌手".into(),
                    playing: false,
                    position_ms: 0,
                    duration_ms: 0,
                },
            };
            let mk = music::Tokens::music(win.w, win.h);
            // 封面：换曲/首次才解码（data URL → 缩到封面尺寸 → 圆角烘 alpha → 上传）
            if let Some(s) = &m {
                if !s.cover.is_empty()
                    && cover.as_ref().map(|(u, _)| u.as_str()) != Some(s.cover.as_str())
                {
                    match minicard_draw::image::decode_data_url(&s.cover) {
                        Ok(img) => {
                            let px = (win.h - 2.0 * mk.pad) * win.dpi / 96.0;
                            let side = px.round().max(1.0) as u32;
                            let mut small = minicard_draw::image::resize_cover(&img, side, side);
                            minicard_draw::image::bake_round_alpha(
                                &mut small,
                                mk.cover_r * win.dpi / 96.0,
                            );
                            match win.gfx.bitmap_from_bgra(&small.bgra, small.w, small.h) {
                                Ok(b) => {
                                    // 换曲：旧图留作翻转的「转出面」
                                    if cover.is_some() {
                                        minicard_shell::mlog!("music flip start（dir {}）", *flip_dir);
                                        *prev = cover.take().map(|(_, b)| b);
                                        *flip_t0 = Some(now);
                                    }
                                    *cover = Some((s.cover.clone(), b))
                                }
                                Err(e) => minicard_shell::mlog!("封面上传失败: {e:?}"),
                            }
                        }
                        Err(e) => minicard_shell::mlog!("封面解码失败: {e:?}"),
                    }
                }
            }
            // 开发用：MC_FLIP_DEMO=1 时每 2s 自翻一次（没有真换曲也能验动画）
            if std::env::var_os("MC_FLIP_DEMO").is_some() && cover.is_some() {
                let due = demo_t0.map(|t| now >= t).unwrap_or(true);
                if due && flip_t0.is_none() {
                    *flip_t0 = Some(now);
                    *demo_t0 = Some(now + Duration::from_millis(2000));
                }
            }
            let flip = flip_t0.map(|t0| music::CoverFlip {
                age: (now - t0).as_secs_f32(),
                dir: *flip_dir,
                old: prev.as_ref().or(cover.as_ref().map(|(_, b)| b)),
                new: cover.as_ref().map(|(_, b)| b),
            });
            music::paint(
                &win.gfx,
                te,
                pal,
                &mk,
                &mw,
                glass_pair,
                cover.as_ref().map(|(_, b)| b),
                flip.as_ref(),
            )?;
            let flipping = flip
                .as_ref()
                .map(|f| !f.done())
                .unwrap_or(false);
            if !flipping {
                *flip_t0 = None;
            }
            mw.playing || flipping
        }
        (State::Calendar(c, off, built_day), _) => {
            let (cy, cm) = month_with_offset(y, mo, *off);
            // 回到本月要跟着「今天」走；翻到别的月只在翻页时重建
            let today_day = if *off == 0 { d } else { 0 };
            if c.year != cy || c.month != cm || (*off == 0 && *built_day != d) {
                *c = calendar_for(y, mo, *off, d);
                *built_day = today_day;
            }
            let ck = calendar::Tokens::cal(win.w, win.h);
            calendar::paint(&win.gfx, te, pal, &ck, c, glass_pair)?;
            false
        }
    };
    Ok(animating)
}


/// 目标月份（相对本月偏移）
fn month_with_offset(y: i32, m: u32, off: i32) -> (i32, u32) {
    let total = y * 12 + m as i32 - 1 + off;
    (total.div_euclid(12), (total.rem_euclid(12) + 1) as u32)
}

/// 建某个月的日历（off==0 时把今天高亮，其它月不亮）
fn calendar_for(y: i32, m: u32, off: i32, today: u32) -> calendar::Calendar {
    let (cy, cm) = month_with_offset(y, m, off);
    let days = calendar::days_in_month_of(cy, cm);
    let wd1 = calendar::weekday(cy, cm, 1);
    let mut c = calendar::Calendar::build(cy, cm, wd1, days, if off == 0 { today } else { 0 });
    c.holiday = calendar::holiday(cy, cm, if off == 0 { today } else { 1 });
    c
}

/// 点一下：按卡片类型派发
fn click(
    wins: &mut [CardWin],
    cfg: &mut Config,
    te: &TextEngine,
    _pal: &Palette,
    hwnd: HWND,
    px: i32,
    py: i32,
) {
    let Some(win) = wins.iter_mut().find(|w| w.hwnd == hwnd) else {
        return;
    };
    // 客户区像素 → 逻辑点
    let k = 96.0 / win.dpi.max(1.0);
    let (x, y) = (px as f32 * k, py as f32 * k);
    minicard_shell::mlog!("click {:?} at ({x:.1},{y:.1})", win.kind);
    let now = Instant::now();
    let (yy, mo, dd, _, _, _) = now_parts();

    match &mut win.state {
        State::Todo(items) => {
            let tk = todo::Tokens::todo(win.w, win.h);
            if let Some(i) = todo::hit(te, &tk, items, x, y) {
                let old_order = todo::shown(items);
                if let Some(it) = items.get_mut(i) {
                    let done = !it.done;
                    it.set_done(done, now);
                    if let Some(card) = cfg.cards.get_mut(win.cfg_idx) {
                        if let Some(td) = card.todo.get_mut(i) {
                            td.done = done;
                        }
                    }
                    save_cfg(cfg);
                }
                // FLIP：勾完的顺序会变（完成沉底），给挪位的行记一个起始位移
                let new_order = todo::shown(items);
                let pitch = tk.row_h + tk.row_gap;
                for (new_row, &ii) in new_order.iter().enumerate() {
                    if let Some(orow) = old_order.iter().position(|&x| x == ii) {
                        if orow != new_row {
                            if let Some(it) = items.get_mut(ii) {
                                it.mv_from = (orow as f32 - new_row as f32) * pitch;
                                it.mv_t0 = Some(now);
                            }
                        }
                    }
                }
            }
        }
        State::Pomo(p) => {
            let tk = pomo::Tokens::pomo(win.w, win.h);
            if let Some(h) = pomo::hit(te, &tk, p, now, x, y) {
                match h {
                    pomo::Hit::Minus => {
                        p.dur = p.dur.saturating_sub(5).max(5);
                        p.reset();
                    }
                    pomo::Hit::Plus => {
                        p.dur = p.dur.saturating_add(5).min(120);
                        p.reset();
                    }
                    pomo::Hit::Act(k) => match k {
                        0 => p.start(now),
                        1 => p.toggle(now),
                        2 => p.reset(),
                        3 => p.reset(),
                        _ => {}
                    },
                }
            }
        }
        State::Calendar(c, off, _) => {
            let tk = calendar::Tokens::cal(win.w, win.h);
            if let Some(h) = calendar::hit(te, &tk, c, x, y) {
                *off += match h {
                    calendar::Hit::Prev => -1,
                    calendar::Hit::Next => 1,
                };
                *c = calendar_for(yy, mo, *off, dd);
            }
        }
        State::Music { cover, prev, flip_t0, flip_dir, demo_t0 } => {
            let _ = (&cover, &prev, &flip_t0, &flip_dir, &demo_t0);
            let m = media::snapshot();
            let mw = match &m {
                Some(s) => music::Music {
                    title: s.title.clone(),
                    artist: s.artist.clone(),
                    playing: s.status == "playing",
                    position_ms: s.position_ms as u64,
                    duration_ms: s.duration_ms as u64,
                },
                None => music::Music {
                    title: "歌名".into(),
                    artist: "歌手".into(),
                    playing: false,
                    position_ms: 0,
                    duration_ms: 0,
                },
            };
            let mk = music::Tokens::music(win.w, win.h);
            if let Some(h) = music::hit(te, &mk, &mw, x, y) {
                // 翻转方向跟着按钮走（CSS 里 d=−1 镜像）
                match h {
                    music::Hit::Prev => *flip_dir = -1.0,
                    music::Hit::Next => *flip_dir = 1.0,
                    music::Hit::Play => {}
                }
                let cmd = match h {
                    music::Hit::Prev => "previous",
                    music::Hit::Play => "toggle",
                    music::Hit::Next => "next",
                };
                if let Err(e) = media::action(cmd) {
                    minicard_shell::mlog!("音乐控制 {cmd} 失败: {e}");
                }
            }
        }
        _ => {}
    }
    // 立刻重绘一次（别等下一个节拍）
    win.next_draw = Instant::now();
}

/// 拖完：写回 config 的位置，并按新位置重切玻璃
fn drag_end(wins: &mut [CardWin], cfg: &mut Config, wall: &Option<glass::Wall>, blur: f32, hwnd: HWND) {
    let Some(win) = wins.iter_mut().find(|w| w.hwnd == hwnd) else {
        return;
    };
    let mut r = RECT::default();
    if unsafe { GetWindowRect(hwnd, &mut r) }.is_err() {
        return;
    }
    let (x, y) = (r.left, r.top);
    if let Some(card) = cfg.cards.get_mut(win.cfg_idx) {
        if card.pos != [x as f32, y as f32] {
            card.pos = [x as f32, y as f32];
            save_cfg(cfg);
        }
    }
    reslice(win, wall, blur);
    win.next_draw = Instant::now();
}

/// 玻璃切片在四周多留这么多（逻辑点）——拖动时先平移复用，越界了才重切
const DRAG_MARGIN: f32 = 160.0;

/// 按窗口当前屏幕位置重切玻璃位图
fn reslice(win: &mut CardWin, wall: &Option<glass::Wall>, blur: f32) {
    let Some(w) = wall else { return };
    let mut r = RECT::default();
    if unsafe { GetWindowRect(win.hwnd, &mut r) }.is_err() {
        return;
    }
    let k = win.dpi / 96.0;
    let pad_px = ((blur * 3.0).max(60.0) * k).round() as f32;
    let margin_px = (DRAG_MARGIN * k).round() as f32;
    let mon = monitor_of(r.left, r.top);
    // 四周多切一圈：拖动时靠平移复用，不用每帧重切重模糊
    let cap = glass::from_wallpaper(
        w,
        (mon.2, mon.3),
        ((r.left as f32 - mon.0), (r.top as f32 - mon.1)),
        (
            (win.w * k).round() as u32,
            (win.h * k).round() as u32,
        ),
        pad_px + margin_px,
        blur * k,
    );
    match win.gfx.bitmap_from_bgra(&cap.bgra, cap.w, cap.h) {
        Ok(b) => {
            let pad_dip = pad_px * 96.0 / win.dpi + DRAG_MARGIN;
            win.glass = Some((
                b,
                D2D_RECT_F {
                    left: -pad_dip,
                    top: -pad_dip,
                    right: win.w + pad_dip,
                    bottom: win.h + pad_dip,
                },
            ));
            win.slice_origin = (r.left, r.top);
            win.glass_off = (0.0, 0.0);
        }
        Err(e) => minicard_shell::mlog!("重切玻璃失败: {e:?}"),
    }
}

fn matches_theme_str(
    a: minicard_core::config::ZOrder,
    b: minicard_core::config::ZOrder,
) -> bool {
    format!("{a:?}") == format!("{b:?}")
}

fn matches_theme(a: minicard_core::config::Theme, b: minicard_core::config::Theme) -> bool {
    format!("{a:?}") == format!("{b:?}")
}

/// 按 z 档摆窗口：Top = 置顶带；Desktop = 插到 WorkerW 锚点正上方
fn apply_z(hwnd: HWND, z: minicard_core::config::ZOrder) {
    use minicard_core::config::ZOrder;
    unsafe {
        match z {
            ZOrder::Top => {
                let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
            }
            _ => {
                let _ = SetWindowPos(hwnd, Some(HWND_NOTOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
                if let Err(e) = minicard_shell::desktop::insert_above_desktop(hwnd) {
                    minicard_shell::mlog!("桌面层插入失败: {e:?}");
                }
            }
        }
    }
}

/// Dock 图标缓存路径：与 `src-tauri/src/dock.rs::ensure_icon` 同一套命名
/// （FNV-1a over 小写、反斜杠、去尾斜杠的路径）。缓存没有就返回 None（落首字母色块）。
fn dock_icon_path(it: &minicard_core::config::DockItem) -> Option<std::path::PathBuf> {
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
    if p.exists() { Some(p) } else { None }
}

fn win_dpi(hwnd: HWND) -> f32 {
    unsafe { GetDpiForWindow(hwnd) as f32 }
}

/// 窗口所在显示器的工作区（排除任务栏），物理 px
fn work_area(hwnd: HWND) -> Option<crate::guide::R4> {
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST};
    unsafe {
        let mon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if GetMonitorInfoW(mon, &mut mi).as_bool() {
            let w = mi.rcWork;
            Some((w.left, w.top, w.right, w.bottom))
        } else {
            None
        }
    }
}

/// 显式收尾：D2D/DComp 的窗口资源在进程退出路径上不一定能干净释放，
/// 直接 exit(0) 让脚本/托盘拿到确定的返回码（实测 return Ok 会得到 122/2170 这种怪码）。
fn clean_exit() -> ! {
    std::process::exit(0)
}

fn save_cfg(cfg: &Config) {
    match cfg.save() {
        Ok(()) => minicard_shell::mlog!("config 已保存（卡片 {} 项）", cfg.cards.len()),
        Err(e) => minicard_shell::mlog!("config 保存失败: {e}"),
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_LBUTTONDOWN => {
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            let mut r = RECT::default();
            let _ = GetWindowRect(hwnd, &mut r);
            let _ = SetCapture(hwnd);
            *DRAG.lock().unwrap() = Some(Drag {
                hwnd: hwnd.0 as isize,
                cx: pt.x,
                cy: pt.y,
                wx: r.left,
                wy: r.top,
                moved: false,
            });
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let mut g = DRAG.lock().unwrap();
            if let Some(d) = g.as_mut() {
                if d.hwnd == hwnd.0 as isize {
                    let mut pt = POINT::default();
                    let _ = GetCursorPos(&mut pt);
                    let (dx, dy) = (pt.x - d.cx, pt.y - d.cy);
                    if !d.moved && dx.abs() + dy.abs() > 4 {
                        d.moved = true;
                    }
                    if d.moved {
                        let (mut nx, mut ny) = (d.wx + dx, d.wy + dy);
                        // 磁吸（对齐 + 固定间距）：位移直接并进落点，跟手不抖
                        if SNAP_ON.load(std::sync::atomic::Ordering::Relaxed) {
                            let mut wr = RECT::default();
                            let _ = GetWindowRect(hwnd, &mut wr);
                            let (w, h) = (wr.right - wr.left, wr.bottom - wr.top);
                            let dpi = win_dpi(hwnd);
                            let k = dpi / 96.0;
                            let tol = (SNAP_TOL.load(std::sync::atomic::Ordering::Relaxed) as f32 * k).round() as i32;
                            let gap = (SNAP_GAP.load(std::sync::atomic::Ordering::Relaxed) as f32 * k).round() as i32;
                            let others: Vec<crate::guide::R4> = CARD_RECTS
                                .lock()
                                .unwrap()
                                .iter()
                                .filter(|(id, _)| *id != hwnd.0 as isize)
                                .map(|(_, r)| *r)
                                .collect();
                            let work = work_area(hwnd);
                            let s = crate::guide::compute(
                                (nx, ny, nx + w, ny + h),
                                &others,
                                work,
                                tol,
                                gap,
                            );
                            nx += s.dx;
                            ny += s.dy;
                            if s.lines.is_empty() {
                                crate::guide::hide();
                            } else {
                                crate::guide::show_lines(&s.lines);
                            }
                        }
                        let _ = SetWindowPos(
                            hwnd,
                            None,
                            nx,
                            ny,
                            0,
                            0,
                            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                        );
                    }
                }
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let d = DRAG.lock().unwrap().take();
            let _ = ReleaseCapture();
            crate::guide::hide();
            if let Some(d) = d {
                if d.hwnd == hwnd.0 as isize {
                    if d.moved {
                        let _ = EVENTS.lock().unwrap().push(UiEvent::DragEnd { hwnd: hwnd.0 as isize });
                    } else {
                        let mut pt = POINT::default();
                        let _ = GetCursorPos(&mut pt);
                        let _ = ScreenToClient(hwnd, &mut pt);
                        let _ = EVENTS.lock().unwrap().push(UiEvent::Click {
                            hwnd: hwnd.0 as isize,
                            x: pt.x,
                            y: pt.y,
                        });
                    }
                }
            }
            LRESULT(0)
        }
        WM_CAPTURECHANGED => {
            if let Some(d) = DRAG.lock().unwrap().take() {
                if d.hwnd == hwnd.0 as isize && d.moved {
                    let _ = EVENTS.lock().unwrap().push(UiEvent::DragEnd { hwnd: hwnd.0 as isize });
                }
            }
            crate::guide::hide();
            LRESULT(0)
        }
        WM_DESTROY => {
            crate::guide::hide();
            PostQuitMessage(0);
            LRESULT(0)
        }
        WM_KEYDOWN if wp.0 == 0x1B => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

/// 供 `main` 调用的入口：读 config、装字体、跑循环
pub fn start(font: &std::path::Path, blur: f32, topmost: bool, exit_after: Option<u64>) -> Result<()> {
    let cfg = Config::load();
    let fonts = crate::font_set(font);
    let refs: Vec<&std::path::Path> = fonts.iter().map(|p| p.as_path()).collect();
    let te = Arc::new(TextEngine::new_weights(&refs, "MiSans")?);
    // 强调色/压在强调色上的字色按 config（口径同 card.html:paintOnAccent）
    let dark_theme = !matches!(cfg.settings.theme, minicard_core::config::Theme::Light);
    let pal = Palette::new(cfg.settings.glass_alpha as f32, false)
        .with_accent(cfg.settings.accent, dark_theme);
    minicard_shell::mlog!(
        "live：强调色 rgb{:?} / 玻璃 {}/100 / 模糊 {}px / 主题 {:?}",
        cfg.settings.accent, cfg.settings.glass_alpha, cfg.settings.glass_blur, cfg.settings.theme
    );
    run(cfg, te, pal, blur, topmost, exit_after)
}

