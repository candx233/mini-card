//! 真数据接线：一个进程、每张启用的卡片一个窗口，共用一份采样快照。
//!
//! 与 `main.rs` 的单窗探针模式分开：探针用写死数据、逐像素对照；这里用 `minicard-core`
//! 的真采样（与 Tauri 外壳同一份实现），窗口位置/尺寸/开关全部来自 `config.toml`。
//!
//! 资源口径：**TextEngine 共享一份**（3.5 MB 字体只装一次），**Gfx 每窗一份**
//! （D2D 设备上下文是窗口绑定的）。绘制层的笔刷/描边/位图缓存键都带上下文身份，
//! 所以共享 TextEngine 不会串。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use windows::core::{w, Result};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::{
    D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_INTERPOLATION_MODE_LINEAR,
};
use windows::Win32::Graphics::Direct2D::ID2D1Bitmap1;
use windows_numerics::Vector2;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::Graphics::Gdi::HBRUSH;
use windows::Win32::UI::HiDpi::{GetDpiForSystem, GetDpiForWindow};
use windows::Win32::Graphics::Gdi::ScreenToClient;
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows::Win32::UI::WindowsAndMessaging::*;

use minicard_cards::{calendar, clock, disks, lunar, music, net, notify as notify_card, perf, pomo, todo, weather};
use minicard_core::config::{self, CardKind, Config, TodoItem};
use minicard_core::data::{self, SharedSnap};
use minicard_core::notify::{NotifyService, NotifySnapshot};
use minicard_core::{media, weather as wx};
use minicard_draw::color::Palette;
use minicard_draw::glass;
use minicard_draw::paint::{fill_round, set_color, stroke_round};
use minicard_draw::text::TextEngine;
use minicard_shell::gfx::Gfx;

use crate::display::{advance_deadline, DisplayTiming};

/// 超采样倍率（环境变量控制，默认 1 = 现有行为）：`MC_SS=2` 时卡片/Dock 按 2× 像素渲染、
/// DComp 0.5 缩放显示 —— 文字边缘多 4 倍采样。取证/对照阶段由脚本设，稳定后再改默认。
/// 超采样倍率：默认 **2×**（0.1.3 起，文字边缘更软/台阶更少；release 实测 9 卡
/// 只 +0.9～1.8 单核百分点、私有内存 +1.4～1.9 MB）。`MC_SS=1` 可退回 1×（A/B 对照用）。
pub(crate) fn ss_factor() -> u32 {
    std::env::var("MC_SS")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(2)
        .clamp(1, 4)
}

/// 当前 unix 毫秒（番茄钟持久化用绝对时刻，跨进程/跨重启都对得上）
fn unix_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 通知卡服务（进程内单例）：第一次画通知卡时才启动，避免没这张卡时也轮询通知。
/// 0.1.3 D5 会在开关关闭时 `stop()` 并清空（届时改这里）。
static NOTIFY_SERVICE: Mutex<Option<Arc<NotifyService>>> = Mutex::new(None);
/// 通知卡总开关（config.settings.notify_card）；关闭时停掉服务、卡面画「未启用」。
static NOTIFY_ON: AtomicBool = AtomicBool::new(false);

fn notify_enabled() -> bool {
    NOTIFY_ON.load(Ordering::Relaxed)
}

fn set_notify_enabled(on: bool) {
    NOTIFY_ON.store(on, Ordering::Relaxed);
    if !on {
        if let Some(svc) = NOTIFY_SERVICE.lock().unwrap().take() {
            svc.stop();
            minicard_shell::mlog!("通知卡关闭：NotifyService 已停止");
        }
    }
}

fn notify_service() -> Arc<NotifyService> {
    let mut slot = NOTIFY_SERVICE.lock().unwrap();
    if slot.is_none() {
        *slot = Some(NotifyService::spawn());
    }
    slot.as_ref().unwrap().clone()
}

/// 通知卡应用图标：CPU 侧缓存「AUMID → 96px 预乘图」，每帧只负责建 D2D 位图。
static NOTIFY_ICON_IMGS: OnceLock<Mutex<HashMap<String, Option<minicard_draw::image::Image>>>> =
    OnceLock::new();

/// 当前快照里可见应用的真图标。取不到（或 shell 只给通用文件图标）→ 不放进 map，
/// 卡面自动回退首字母色块。
pub(crate) fn notify_icon_map(
    gfx: &Gfx,
    snap: &NotifySnapshot,
) -> HashMap<String, ID2D1Bitmap1> {
    let cache = NOTIFY_ICON_IMGS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut out = HashMap::new();
    for it in &snap.items {
        if it.aumid.trim().is_empty() || out.contains_key(&it.aumid) {
            continue;
        }
        let img = {
            let mut m = cache.lock().unwrap();
            if let Some(v) = m.get(&it.aumid) {
                v.clone()
            } else {
                let v = crate::icon::ensure_aumid(&it.aumid)
                    .and_then(|p| minicard_draw::image::decode_file(&p).ok())
                    .map(|img| minicard_draw::image::downscale(&img, 96));
                m.insert(it.aumid.clone(), v.clone());
                v
            }
        };
        if let Some(img) = img {
            if let Ok(bmp) = minicard_draw::image::bitmap(gfx, &img) {
                out.insert(it.aumid.clone(), bmp);
            }
        }
    }
    out
}

/// 是否真的需要读通知：总开关打开 + config 里至少有一张启用的通知卡。
fn notify_wanted(cfg: &Config) -> bool {
    cfg.settings.notify_card
        && cfg
            .cards
            .iter()
            .any(|c| c.kind == CardKind::Notify && c.enabled)
}

/// 从 config 恢复番茄钟（0.1.3 持久化）：run 态按绝对结束时刻补偿；end 已过 = 「本轮完成」。
fn pomo_from_state(st: Option<&config::PomoState>, now: Instant) -> pomo::Pomo {
    let mut p = pomo::Pomo::new();
    let Some(st) = st else { return p };
    p.dur = st.dur.clamp(5, 120);
    p.remain_ms = st.remain_ms.clamp(0.0, p.total_ms());
    match st.mode.as_str() {
        "run" => {
            p.mode = pomo::PomoMode::Run;
            let end_ms = st.end_unix_ms.unwrap_or_else(|| unix_now_ms() + p.remain_ms as i64);
            let left = end_ms.saturating_sub(unix_now_ms()).max(0);
            p.end_at = Some(now + Duration::from_millis(left as u64));
        }
        "paused" => p.mode = pomo::PomoMode::Paused,
        "done" => {
            p.mode = pomo::PomoMode::Done;
            p.remain_ms = 0.0;
            p.end_at = None;
        }
        _ => {
            p.mode = pomo::PomoMode::Set;
            p.remain_ms = p.total_ms();
            p.end_at = None;
        }
    }
    // 直接落成当前 mm:ss，避免重建后第一帧显示默认的 25:00
    p.shown = p.mmss(now);
    p
}

/// 把番茄钟写回 config（每次用户动作后调用；run 态存绝对结束时刻）。
fn save_pomo(p: &pomo::Pomo) {
    let now = Instant::now();
    let mode = match p.mode {
        pomo::PomoMode::Set => "set",
        pomo::PomoMode::Run => "run",
        pomo::PomoMode::Paused => "paused",
        pomo::PomoMode::Done => "done",
    };
    let end_unix_ms = if matches!(p.mode, pomo::PomoMode::Run) {
        p.end_at.map(|e| unix_now_ms() + (e - now).as_millis() as i64)
    } else {
        None
    };
    let mut cfg = Config::load();
    cfg.pomodoro = Some(config::PomoState {
        mode: mode.to_owned(),
        dur: p.dur,
        remain_ms: p.remain(now),
        end_unix_ms,
    });
    if let Err(e) = cfg.save() {
        minicard_shell::mlog!("番茄钟状态落盘失败: {e}");
    }
}


/// 窗口线程只往里塞事件，动作在主循环里做（这样能正常借用 wins/cfg，不用满地 static mut）
enum UiEvent {
    Click { hwnd: isize, x: i32, y: i32 },
    /// 鼠标滚轮：通知卡翻组用（delta > 0 向上）
    Wheel { hwnd: isize, delta: i32 },
    DragEnd { hwnd: isize },
    /// Dock 图标点击（窗内逻辑坐标）
    DockClick { x: f32, y: f32 },
    /// Dock 拖拽松手（窗内逻辑 x）
    DockDrop { x: f32 },
    /// Dock 右键（窗内坐标 + 屏幕坐标：TrackPopupMenu 要屏幕点）
    DockMenu { x: f32, y: f32, sx: i32, sy: i32 },
    /// 显示器增删 / DPI 变化（WM_DISPLAYCHANGE / WM_DPICHANGED）：0.1.3 保底重采样
    DisplayChanged,
}

static EVENTS: Mutex<Vec<UiEvent>> = Mutex::new(Vec::new());

/// 最近一帧画出来的 Dock 几何（含 Tokens）：点击命中必须走这一份（与绘制同源）
static DOCK_SNAP: Mutex<Option<(minicard_cards::dock::Tokens, minicard_cards::dock::Geo)>> =
    Mutex::new(None);

/// 点击动作在**工作线程**跑（`ShellExecuteW` 会阻塞到目标进程真正起来，Steam/WeGame 那种
/// 能卡 1~3 秒），结果通过这里回主循环，再由主循环改气泡文案。
static CLICK_RESULT: Mutex<Option<(usize, String)>> = Mutex::new(None);
/// 已经在启动/切换中：期间不再重复派活，避免连点起多份
static CLICK_BUSY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// 主循环上一拍的时刻 + 点击期间的最大停顿（判据：点了之后 UI 不该停）
static ITER_LAST: Mutex<Option<Instant>> = Mutex::new(None);
static CLICK_MAXGAP_US: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// 结果回填后置位，下一拍把「停顿」打出来（同一拍里刚点完还没量到）
static CLICK_REPORT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 「哪些应用在跑」的快照：由后台线程每 2s 刷一次。
/// 枚举所有顶层窗口 + 逐个取 exe 路径在主线程要 30~150ms，2s 一次就是固定停顿。
static RUNNING_SET: Mutex<Option<std::collections::HashSet<String>>> = Mutex::new(None);
static RUNNING_WATCHER: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 常驻的「点击执行」线程：`ShellExecuteW` 会阻塞到目标进程起来，但**不能占主循环**；
/// 每次 `thread::spawn` 又要 ~80ms（加载器锁 + 栈分配），所以线程只起一次、用信道派活。
static CLICK_TX: Mutex<Option<std::sync::mpsc::Sender<(usize, String, String, String)>>> =
    Mutex::new(None);
static CLICK_WORKER: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn spawn_click_worker() {
    if CLICK_WORKER.swap(true, Ordering::Relaxed) {
        return;
    }
    let (tx, rx) = std::sync::mpsc::channel::<(usize, String, String, String)>();
    *CLICK_TX.lock().unwrap() = Some(tx);
    std::thread::spawn(move || {
        for (i, name, path, args) in rx {
            let t0 = Instant::now();
            let r = crate::appwin::click(&path, &args);
            let ms = t0.elapsed().as_secs_f32() * 1000.0;
            minicard_shell::mlog!("dock 点击 [{i}] {name} → {r}（{ms:.0} ms）");
            if let Ok(mut g) = CLICK_RESULT.lock() {
                *g = Some((i, r));
            }
        }
    });
}

fn spawn_running_watcher() {
    if RUNNING_WATCHER.swap(true, Ordering::Relaxed) {
        return;
    }
    std::thread::spawn(|| {
        // 钩子必须装在「有消息循环的线程」上：这里就是
        let hooked = crate::winevent::install();
        let mut last = Instant::now() - Duration::from_secs(10);
        loop {
            // 抽 100ms 消息（钩子回调在这里跑），有事件或者超过 3s 就补扫一次
            let ev = crate::winevent::pump(100);
            let due = last.elapsed() > Duration::from_secs(3);
            if ev || due || !hooked {
                let t0 = Instant::now();
                let set = crate::appwin::running_set();
                if let Ok(mut g) = RUNNING_SET.lock() {
                    *g = Some(set);
                }
                last = Instant::now();
                if std::env::var("MC_DOCK_LOG").is_ok() {
                    minicard_shell::mlog!(
                        "运行点扫描：{} → {} ms（{}）",
                        if ev { "事件" } else { "兜底" },
                        t0.elapsed().as_millis(),
                        if hooked { "钩子" } else { "轮询" }
                    );
                }
            }
        }
    });
}

/// Dock 窗在屏幕上的原点 + dpi（菜单要按屏幕像素定位，窗口线程拿不到 `dw`）
static DOCK_ORIGIN: Mutex<Option<(i32, i32, f32)>> = Mutex::new(None);

/// 按下但还没抬手：`(图标下标, 按下时的窗内坐标)`。超过 4px 才算拖拽（与卡片同阈值）
static DOCK_PRESS: Mutex<Option<(usize, f32, f32)>> = Mutex::new(None);

/// 窗内坐标 → 命中图标（走最近一次绘制的几何，与画面同源）
fn dock_hit_at(x: f32, y: f32) -> Option<usize> {
    let snap = DOCK_SNAP.lock().unwrap().clone();
    snap.and_then(|(tk, geo)| minicard_cards::dock::hit(&tk, &geo, x, y))
}

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
    /// `Option`：磁盘卡按盘数改窗口高度时要「先丢旧 Gfx 再建新的」
    /// （`CreateTargetForHwnd` 在旧合成目标还在时会报 WINDOW_ALREADY_COMPOSED），
    /// 中间那一瞬没有 Gfx —— 见 `set_card_height`。
    gfx: Option<Gfx>,
    kind: CardKind,
    w: f32,
    h: f32,
    state: State,
    /// 玻璃位图（按卡片位置裁壁纸）—— 先留空，等玻璃层接线
    #[allow(dead_code)]
    glass: Option<(ID2D1Bitmap1, D2D_RECT_F)>,
    /// 下次重绘时刻（绝对 deadline：动画期按显示帧整步推进，静置按 refresh_ms）
    next_draw: Instant,
    /// 静置节拍（config 的 refresh_ms，钳到 200ms..60s）
    refresh: Duration,
    /// 这张卡所在显示器的刷新率与动画节拍（建窗 / 跨屏 / 显示器变化时重新探测）
    timing: DisplayTiming,
    /// `MC_FPS_LOG=1` 时的动画帧间隔采样（默认 None：不分配、不动）
    fps: Option<FpsProbe>,
    /// 在 config.cards 里的下标（拖动/勾选要回写）
    cfg_idx: usize,
    /// 该窗口的 DPI（客户区像素 → 逻辑点）
    dpi: f32,
    /// 玻璃位图是按这个屏幕坐标（物理 px）切的
    slice_origin: (i32, i32),
    /// 拖动时把玻璃位图整体偏移这么多（逻辑点）——不用每帧重切/重模糊
    glass_off: (f32, f32),
    /// 遮挡闸门：下次复查时刻 + 上次判定。整块被普通窗口盖住时画了也看不见，
    /// 直接跳过重绘（露出来立刻补一帧）。
    cover_at: Instant,
    covered: bool,
}

/// Dock 自绘窗口（D0：静态条；D1 接 hover 放大）
struct DockWin {
    hwnd: HWND,
    /// 换尺寸时要先断开旧合成再建新的，所以包一层 Option
    gfx: Option<Gfx>,
    t: minicard_cards::dock::Tokens,
    icons: Vec<Option<ID2D1Bitmap1>>,
    /// 壁纸切片（与卡片同一口径：pad + 高斯模糊）
    glass: Option<(ID2D1Bitmap1, D2D_RECT_F)>,
    /// 窗口左上角（屏幕物理 px）—— 指针换算用
    origin: (i32, i32),
    /// 该窗的 DPI（指针/区域换算）
    dpi: f32,
    /// 该窗所在显示器的刷新率（交互动画每个显示帧一步）
    timing: DisplayTiming,
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
    /// 上一帧的时刻：动画的 dt 必须用它算（用 `next_draw` 会得到 0 → 退回 1/60，
    /// 在 120Hz 下会让 260ms 的进场变成 520ms）
    last_t: Instant,
    /// 运行中指示点（每 2s 扫一次可见窗口）
    running: Vec<bool>,
    next_running: Instant,
    /// 拖拽排序：被拖条目 + 指针 x；松手后 140ms 回弹（各图标从旧槽滑到新槽）
    drag_from: Option<usize>,
    drag_x: f32,
    settle_from: Vec<f32>,
    settle_t0: Option<Instant>,
    /// 名字气泡：进场进度 / 点击反馈文案（+ 显示时长）
    bubble_a: f32,
    flash: Option<(String, Instant)>,
    flash_ms: f32,
}

/// Dock 窗的「可点区域」（窗内逻辑矩形 + 是否已放开上方预留）。
///
/// **不能用 `SetWindowRgn`**：`WS_EX_NOREDIRECTIONBITMAP` + DComp 的窗一旦设了小于整窗的
/// 区域，DWM 会把**整个合成内容**裁没（实测：静止态区域 (34,172)-(1033,235) 时整条不可见，
/// 放开成 135 高反而能看见）。改用 `WM_NCHITTEST` 返回 `HTTRANSPARENT` —— 内容照常合成，
/// 只有点击穿透；这也正是上方 172px 预留想要的语义。
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
static DOCK_HWND: AtomicIsize = AtomicIsize::new(0);
/// (hwnd, pinned)：原生卡的「固定位置」只在主界面写 config，wndproc 得有个
/// 不读盘的查询表；建窗/热载时同步。
static PINNED: Mutex<Vec<(isize, bool)>> = Mutex::new(Vec::new());

fn is_pinned(hwnd: HWND) -> bool {
    PINNED
        .lock()
        .unwrap()
        .iter()
        .any(|(h, p)| *h == hwnd.0 as isize && *p)
}
/// (left, top, right, bottom, 已放开)
static DOCK_HIT: Mutex<(f32, f32, f32, f32, bool)> = Mutex::new((0.0, 0.0, 0.0, 0.0, false));

fn dock_set_hit(t: &minicard_cards::dock::Tokens, open: bool) {
    let top = if open {
        (t.top - minicard_core::config::Dock::HOVER_EXTRA).max(0.0)
    } else {
        t.top
    };
    *DOCK_HIT.lock().unwrap() = (t.side, top, t.w - t.side, t.h, open);
}

impl DockWin {
    /// 指针（屏幕 px）→ 窗内逻辑坐标；返回是否落在「激活区」内
    fn pointer(&self, dpi: f32) -> Option<(f32, f32)> {
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
            Some((x.clamp(0.0, self.t.w), y))
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
    /// 通知卡（0.1.3）：数据在 `NotifyService` 全局服务里，绘制时读快照。
    Notify {
        /// 连续滚动 / 到达 / 删除 / hover / 清除确认 / 状态淡入的时间基（B 案 Motion）
        motion: notify_card::Motion,
        show_body: bool,
        per_app: usize,
        /// 抬头「清除」第一次点击后的确认截止时刻（3 秒内再点才真清）
        clear_armed: Option<Instant>,
    },
    /// `seen_at` = 上一帧画上去的 `WeatherSnap.at`。快照换了一版就立刻重绘：
    /// 天气卡的 refresh_ms 被钳到 60s，光等节拍最多要 60s 才看到新天气。
    Weather { seen_at: i64 },
    Music {
        /// (封面 dataURL, 位图)：**屏上正在显示的那张**；换图才重解码
        cover: Option<(String, ID2D1Bitmap1)>,
        /// 翻转中的旧封面（屏上换下来那张）+ 起始时刻（0.22s 转出 + 0.26s 转回）
        prev: Option<ID2D1Bitmap1>,
        flip_t0: Option<Instant>,
        /// 下一次翻转要用的方向：+1 = 下一首（向左翻），−1 = 上一首。
        /// 由卡上 ⏮/⏭ 点击写入，**起翻那一刻**才用掉并复位 —— 不能每帧复位：
        /// 新封面可能晚到 1~2s，复位早了方向就丢了（0.1.3 前的老问题）。
        flip_dir: f32,
        /// 正在跑的那次翻转的方向（起翻时从 `flip_dir` 拷贝，动画期间不再变）
        flip_dir_run: f32,
        /// 仅 `MC_FLIP_DEMO=1` 用：下一次自动翻转的时刻（没有真换曲时也能看动画）
        demo_t0: Option<Instant>,
        /// 换曲 → 换图判定：缩略图常比歌名晚到，晚到时要补翻（2026-11-15 用户报）
        plan: music::FlipPlan,
        /// 解码/上传失败过的封面 URL：同一张别每帧重试（下一首/换图自然会清）
        bad_cover: Option<String>,
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

fn wmo_u8(code: i32) -> u8 {
    // -1 / 缺字段在 core 里用 unwrap_or(-1) 兜底；直接 as u8 会变 255 → 雷暴
    if (0..=99).contains(&code) {
        code as u8
    } else {
        3
    }
}

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
        CardKind::Notify => notify_card::Tokens::notify(w, h).radius,
    }
}

/// 采样多久没有新值算「数据异常」。
/// 与浏览器 `card.html::computeState` 的 `Date.now() - lastAt > 3500` 同一个口径。
const DATA_STALE_MS: u64 = 3500;

/// 采样健康度：性能 / 磁盘 / 网络三张卡的空态、异常态都由它决定。
///
/// 浏览器是「没收到过快照 → `empty`」「3.5s 没新值 → `error`」两档；原生这边
/// 快照自带 `at`（采样线程每拍打的 unix 毫秒，见 `data::DataSnapshot::at`），
/// `at == 0` 就是还没采到。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum DataHealth {
    Empty,
    Fresh,
    Stale,
}

impl DataHealth {
    fn of(at: u64, now_ms: u64) -> Self {
        if at == 0 {
            Self::Empty
        } else if now_ms.saturating_sub(at) > DATA_STALE_MS {
            Self::Stale
        } else {
            Self::Fresh
        }
    }

    /// 整卡文案（`None` = 正常态，画各自的内容）
    fn text(self) -> Option<&'static str> {
        match self {
            Self::Empty => Some("等待数据…"),
            Self::Stale => Some("数据异常"),
            Self::Fresh => None,
        }
    }
}

/// unix 毫秒（与 `DataSnapshot.at` 同一时钟）
fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 「整卡一行灰字」的几何：照 `card.html` 的
/// `.card > .card-body > .empty { flex:1; display:grid; place-items:center; font-size:.78125rem }`。
///
/// 居中盒是**卡片自己的 padding box**（每张卡的 padding 写在 `.card-*` 里，这里按下表取），
/// 字号 = 0.78125rem = 0.78125 × 16k；k 与圆角同源（`border-radius:1rem` = 16k），
/// 所以直接从 `radius_of` 反推，不用再抄一遍各卡的 BASE。
#[derive(Clone, Copy, Debug)]
struct StatusTok {
    pad_lr: f32,
    pad_tb: f32,
    fs: f32,
    line_h: f32,
    radius: f32,
}

fn status_tokens(kind: CardKind, w: f32, h: f32) -> StatusTok {
    let r = radius_of(kind, w, h); // = 1rem = 16k
    let (pad_lr, pad_tb) = match kind {
        // .card-perf { padding: .4375rem 1rem }
        CardKind::Performance => (1.0 * r, 0.4375 * r),
        // .card-disk { padding: .5rem .75rem }
        CardKind::Disks => (0.75 * r, 0.5 * r),
        // .card-net { padding: 0 .5rem }
        CardKind::NetRate => (0.5 * r, 0.0),
        // 其余卡的空态在各自模块里画；这里给 .card 的 0.875rem/1rem 兜底
        _ => (1.0 * r, 0.875 * r),
    };
    let fs = 0.78125 * r;
    StatusTok {
        pad_lr,
        pad_tb,
        fs,
        line_h: fs * 1.55, // body { line-height:1.55 }
        radius: r,
    }
}

/// 空态 / 异常态的整卡：玻璃 + 白纱 + 描边 + 正中一行灰字（文字只画一遍，无阴影）。
#[allow(clippy::too_many_arguments)]
fn paint_status(
    g: &Gfx,
    te: &TextEngine,
    pal: &Palette,
    t: &StatusTok,
    w: f32,
    h: f32,
    text: &str,
    glass: Option<(&ID2D1Bitmap1, D2D_RECT_F)>,
) -> Result<()> {
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
            right: w,
            bottom: h,
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
                right: w - 0.5,
                bottom: h - 0.5,
            },
            (t.radius - 0.5).max(0.0),
            1.0,
        )?;

        let box_w = (w - 2.0 * t.pad_lr).max(0.0);
        let box_h = (h - 2.0 * t.pad_tb).max(0.0);
        let y = t.pad_tb + (box_h - t.line_h).max(0.0) / 2.0;
        let fmt = te.format_cached(t.fs, t.line_h, 2)?; // 2 = 居中
        let lay = te.layout_cached(2, &fmt, text, box_w, t.line_h + 8.0, false)?;
        set_color(g, pal.muted)?;
        g.d2d.DrawTextLayout(
            Vector2 { X: t.pad_lr, Y: y },
            &lay,
            &g.brush,
            D2D1_DRAW_TEXT_OPTIONS_NONE,
        );

        g.pop_clip();
        g.end()?;
    }
    Ok(())
}

/// 磁盘卡此刻该不该换高度、换成多少（`None` = 不动）。
///
/// 三个「不动」的条件都很重要：
/// - 不是磁盘卡（其它卡尺寸由 config 定死）；
/// - 拖动中（改窗口尺寸会跟 guide.rs 的磁吸 / 落点吸附打架，外壳同一条规则）；
/// - 采样不是新鲜态（没到 / 断了都保持当前高度，别先缩成 180 的空档再弹回来）。
///
/// 高度是**派生值**，只用于建窗 / SetWindowPos，**绝不写回 config**：
/// 写回去的话，3 盘机器上跑一次就把 180 存成 120，换到 5 盘机器就是 5 行塞进 120 高。
fn disk_height_for(
    kind: CardKind,
    cur_h: f32,
    disk_n: usize,
    health: DataHealth,
    dragging: bool,
) -> Option<f32> {
    if !matches!(kind, CardKind::Disks) || dragging || health != DataHealth::Fresh {
        return None;
    }
    let want = config::disk_height_px(disk_n);
    // 幂等：已经是目标高度就别每秒 SetWindowPos（会抖）
    if (cur_h - want).abs() < 0.5 {
        None
    } else {
        Some(want)
    }
}

/// 磁盘卡的窗口高度 = **派生值**：1 盘 60 / 2–3 盘 120 / 4–5 盘 180（`config::disk_height_px`）。
///
/// 只改窗口和 Gfx，**不写回 config**（写回去就成了「3 盘机器上跑一次 → 5 盘机器按 3 盘高显示」）。
/// 换尺寸必须重造 Gfx：swapchain / D2D 目标位图都是按窗口像素建的，
/// 旧的得先 `detach` 再丢掉，否则 `CreateTargetForHwnd` 报 WINDOW_ALREADY_COMPOSED
/// （`dock_rebuild` 同一条路）。
fn set_card_height(
    win: &mut CardWin,
    new_h: f32,
    wall: &Option<glass::Wall>,
    blur: f32,
) -> Result<()> {
    let old_h = win.h;
    let w_px = (win.w * win.dpi / 96.0).round().max(1.0) as i32;
    let old_h_px = (old_h * win.dpi / 96.0).round().max(1.0) as i32;
    let h_px = (new_h * win.dpi / 96.0).round().max(1.0) as i32;
    unsafe {
        let _ = SetWindowPos(
            win.hwnd,
            None,
            0,
            0,
            w_px,
            h_px,
            SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
    if let Some(old) = win.gfx.take() {
        old.detach();
    }
    match Gfx::new_scaled(
        win.hwnd,
        w_px as u32,
        h_px as u32,
        win.dpi,
        radius_of(win.kind, win.w, new_h),
        ss_factor(),
    ) {
        Ok(mut g) => {
            g.text_dx = 0.0;
            win.gfx = Some(g);
            win.h = new_h;
        }
        Err(e) => {
            // 回滚：窗口退回旧高度，尽量重造一份能画的 Gfx（至少别留一块空白卡）
            unsafe {
                let _ = SetWindowPos(
                    win.hwnd,
                    None,
                    0,
                    0,
                    w_px,
                    old_h_px,
                    SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
            win.gfx = Gfx::new_scaled(
                win.hwnd,
                w_px as u32,
                old_h_px as u32,
                win.dpi,
                radius_of(win.kind, win.w, old_h),
                ss_factor(),
            )
            .ok();
            if let Some(g) = win.gfx.as_mut() {
                g.text_dx = 0.0;
            }
            return Err(e);
        }
    }
    // 玻璃是按窗口像素切的：高度变了必须重切，否则磨砂层错位
    reslice(win, wall, blur);
    win.next_draw = Instant::now();
    Ok(())
}

/// 卡片落在哪块屏（物理像素）：返回 (left, top, w, h)
fn monitor_of(x: i32, y: i32) -> (f32, f32, f32, f32) {
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
    // 通知卡总开关：关着时即使 config 里有通知卡，也不启动轮询
    set_notify_enabled(notify_wanted(&cfg));
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
        // 取证/沙箱实例可用 MC_LIVE_INSTANCE=<后缀> 起第二份，不动用户正在跑的 live；
        // 生产不设该变量 → 名字与以前完全一致。
        let mut instance = "MiniCardNativeLiveSingleInstance".to_owned();
        if let Ok(suffix) = std::env::var("MC_LIVE_INSTANCE") {
            let suffix = suffix.trim();
            if !suffix.is_empty() {
                instance.push('-');
                instance.push_str(suffix);
            }
        }
        let name: Vec<u16> = instance.encode_utf16().chain(std::iter::once(0)).collect();
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
                            if s.days.is_empty() {
                                minicard_shell::mlog!(
                                    "天气已刷新：{} {}°（三日预报为空，源不可达）",
                                    s.city,
                                    s.temp
                                );
                            } else {
                                minicard_shell::mlog!(
                                    "天气已刷新：{} {}° 预报 {} 天",
                                    s.city,
                                    s.temp,
                                    s.days.len()
                                );
                            }
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

    // SMTC 音乐：core 的 Sampler 每拍更新内部快照，这里只负责起线程 + 读。
    // 音乐卡不可见时降到 5s 一拍（SMTC 的 WinRT 查询不便宜）；200ms 一片的短睡保证
    // 「切回桌面」那拍 200ms 内就补一次，不会挂着上一首。
    std::thread::spawn(|| {
        let mut s = media::Sampler::new();
        let mut last = Instant::now() - Duration::from_secs(60);
        loop {
            let visible = minicard_core::util::kind_visible(CardKind::Music);
            let interval = if visible {
                Duration::from_millis(media::TICK_MS)
            } else {
                Duration::from_secs(5)
            };
            if last.elapsed() >= interval {
                let _ = s.tick();
                last = Instant::now();
            }
            std::thread::sleep(Duration::from_millis(200));
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
        // 启动日志去重：同一档刷新率只打一行（多屏不同档才各打一行）
        let mut timing_seen: Vec<(u32, &'static str)> = Vec::new();
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
            let mut gfx = Gfx::new_scaled(hwnd, w_px, h_px, dpi, radius, ss_factor())?;
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
                CardKind::Pomodoro => State::Pomo(pomo_from_state(cfg.pomodoro.as_ref(), now)),
                CardKind::Todo => State::Todo(todo_items(&card.todo)),
                CardKind::Weather => State::Weather {
                    // 初值就取当前快照的 at：首次绘制之后只有「换了一版」才触发额外重绘
                    seen_at: wshared
                        .read()
                        .unwrap()
                        .as_ref()
                        .map(|s| s.at)
                        .unwrap_or(i64::MIN),
                },
                CardKind::Music => State::Music {
                    cover: None,
                    prev: None,
                    flip_t0: None,
                    flip_dir: 1.0,
                    flip_dir_run: 1.0,
                    demo_t0: None,
                    plan: music::FlipPlan::new(),
                    bad_cover: None,
                },
                CardKind::Calendar => {
                    let (y, m, d, _, _, _) = now_parts();
                    State::Calendar(calendar_of(y, m, d), 0, d)
                }
                CardKind::Notify => State::Notify {
                    motion: notify_card::Motion::new(),
                    show_body: cfg.settings.notify_show_body,
                    per_app: cfg.settings.notify_max_per_app.clamp(1, 3) as usize,
                    clear_armed: None,
                },
            };
            // Desktop 档：插到 Progman/WorkerW 锚点正上方（被普通窗口压住 = 与真卡同层）
            if matches!(z, minicard_core::config::ZOrder::Desktop) {
                if let Err(e) = minicard_shell::desktop::insert_above_desktop(hwnd) {
                    minicard_shell::mlog!("桌面层插入失败: {e:?}");
                }
            }
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            PINNED.lock().unwrap().push((hwnd.0 as isize, card.pinned));
            // 显示同步：按这张卡所在显示器探测刷新率（多屏可以不同档）
            let timing = crate::display::detect(hwnd);
            log_timing_once(&timing, &mut timing_seen);
            wins.push(CardWin {
                hwnd,
                gfx: Some(gfx),
                kind: card.kind,
                w: win_w,
                h: win_h,
                state,
                glass: glass_pair,
                next_draw: Instant::now(),
                refresh: Duration::from_millis(card.refresh_ms.clamp(200, 60_000)),
                timing,
                fps: FpsProbe::enabled().then(FpsProbe::new),
                cfg_idx,
                dpi,
                slice_origin: (card.pos[0].round() as i32, card.pos[1].round() as i32),
                glass_off: (0.0, 0.0),
                cover_at: Instant::now(),
                covered: false,
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
                    let mut gfx = Gfx::new_scaled(
                        hwnd,
                        w_p,
                        h_p,
                        dpi,
                        minicard_cards::dock::RADIUS,
                        ss_factor(),
                    )?;
                    gfx.text_dx = 0.0;
                    // 图标：与外壳同一套缓存命名
                    let mut icons: Vec<Option<ID2D1Bitmap1>> = Vec::new();
                    for it in &d.items {
                        // 图标每帧都要缩放：先在 CPU 降到 96（> 最大视觉尺寸 84），
                        // 之后逐帧走线性采样就够干净，也省掉每帧的高质量三次插值
                        let bmp = dock_icon_path(it)
                            .and_then(|p| minicard_draw::image::decode_file(&p).ok())
                            .and_then(|img| {
                                let small = minicard_draw::image::downscale(&img, 96);
                                minicard_draw::image::bitmap(&gfx, &small).ok()
                            });
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
                    *DOCK_ORIGIN.lock().unwrap() = Some((x, y, dpi));
                    crate::drop::install(hwnd, d.clone(), (x, y), dpi);
                    spawn_running_watcher();
                    spawn_click_worker();
                    let dk_dpi = dpi;
                    let timing = crate::display::detect(hwnd);
                    log_timing_once(&timing, &mut timing_seen);
                    let mut dw = DockWin {
                        hwnd,
                        gfx: Some(gfx),
                        t: dk,
                        icons,
                        glass: glass_pair,
                        origin: (x, y),
                        dpi: dk_dpi,
                        timing,
                        px: dk.w / 2.0,
                        ease: 0.0,
                        ease_v: 0.0,
                        region_open: false,
                        last_sig: 0,
                        next_draw: Instant::now(),
                        next_paint: Instant::now(),
                        last_t: Instant::now(),
                        running: vec![false; d.items.len()],
                        next_running: Instant::now(),
                        drag_from: None,
                        drag_x: 0.0,
                        settle_from: Vec::new(),
                        settle_t0: None,
                        bubble_a: 0.0,
                        flash: None,
                        flash_ms: 1.2,
                    };
                    DOCK_HWND.store(dw.hwnd.0 as isize, Ordering::Relaxed);
                    dock_set_hit(&dw.t, false);
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
        // 上一拍的数据健康度（只在换档时唤醒卡片，别每帧都叫）
        let mut last_health = DataHealth::Empty;
        let mut cfg_mtime = std::fs::metadata(minicard_core::config::path())
            .and_then(|m| m.modified())
            .ok();
        // 诊断（MC_DRAW_LOG=1）：帧循环的耗时账本，2 秒打一行 —— 用来区分
        // 「画得太多」和「单帧太贵」。只读不改变行为。
        let mut st_last = Instant::now();
        let mut st_iters = 0u64;
        let mut st_busy_us = 0u64;
        let mut st_draws = 0u64;
        let mut st_draw_us = 0u64;
        let mut st_end_us = 0u64;
        let mut st_present_us = 0u64;
        let mut st_dock_draws = 0u64;
        let mut st_dock_us = 0f64;
        let mut st_kind_draws = [0u64; 10];
        let mut st_cover_skips = 0u64;
        let mut rects_at = Instant::now();
        loop {
            st_iters += 1;
            let st_iter_t0 = Instant::now();
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
            let mut dock_need_rebuild = false;
            for ev in events {
                match ev {
                    UiEvent::DisplayChanged => {
                        // 屏/DPI 变了：按当前窗口 DPI 重采样 + 重切玻璃 + 夹回工作区
                        handle_display_changed(&mut wins, dock_win.as_mut(), &cfg, &wall, blur);
                    }
                    UiEvent::Click { hwnd, x, y } => {
                        click(&mut wins, &mut cfg, &te, &pal, HWND(hwnd as *mut _), x, y);
                    }
                    UiEvent::Wheel { hwnd, delta } => {
                        let Some(win) = wins.iter_mut().find(|w| w.hwnd.0 as isize == hwnd) else {
                            continue;
                        };
                        if let State::Notify {
                            motion,
                            show_body,
                            per_app,
                            clear_armed,
                        } = &mut win.state
                        {
                            let tk = notify_card::Tokens::notify(win.w, win.h);
                            let snap = notify_service().snapshot();
                            let opts = notify_card::Options {
                                show_body: *show_body,
                                per_app: *per_app,
                                clear_armed: clear_armed.is_some(),
                            };
                            // 连续滚动（2026-11-09 用户拍板）：按像素补间，不再整页翻
                            let max = notify_card::max_scroll_px(&tk, Some(&snap), opts, motion);
                            let step = notify_card::wheel_step_px(&tk);
                            // WM_MOUSEWHEEL：delta > 0 = 向上滚 = 内容下移 = offset 减小
                            let dir = if delta > 0 { -1.0 } else { 1.0 };
                            motion.scroll_by(dir * step, max, Instant::now());
                            win.next_draw = Instant::now();
                        }
                    }
                    UiEvent::DockClick { x, y } => {
                        let snap = DOCK_SNAP.lock().unwrap().clone();
                        if let Some((tk, geo)) = snap {
                            if let Some(i) = minicard_cards::dock::hit(&tk, &geo, x, y) {
                                if let Some(item) = cfg.dock.items.get(i) {
                                    // 气泡先说「启动中…」，真正的结果由工作线程回填
                                    if let Some(dw) = dock_win.as_mut() {
                                        dw.flash = Some(("启动中…".to_string(), Instant::now()));
                                        dw.flash_ms = 1.5;
                                        dw.last_sig = 0;
                                    }
                                    // 变异检验口：MC_DOCK_INLINE=1 走旧的「主线程直接执行」，
                                    // 用来证明停顿测量是灵敏的（不带这个变量才是线上路径）
                                    if std::env::var("MC_DOCK_INLINE").is_ok() {
                                        CLICK_BUSY.store(true, Ordering::Relaxed);
                                        let t_inline = Instant::now();
                                        let r = crate::appwin::click(&item.path, &item.args);
                                        // 主线程被这次调用整段占住，停顿就是它本身的耗时
                                        CLICK_MAXGAP_US.store(
                                            t_inline.elapsed().as_micros() as u64,
                                            Ordering::Relaxed,
                                        );
                                        let _ = CLICK_RESULT.lock().map(|mut g| *g = Some((i, r)));
                                        continue;
                                    }
                                    let busy = CLICK_BUSY.swap(true, Ordering::Relaxed);
                                    if busy {
                                        minicard_shell::mlog!("dock 点击 [{i}] {}：上一次还没结束，忽略", item.name);
                                    } else {
                                        let job = (
                                            i,
                                            item.name.clone(),
                                            item.path.clone(),
                                            item.args.clone(),
                                        );
                                        let sent = CLICK_TX
                                            .lock()
                                            .ok()
                                            .and_then(|g| g.as_ref().cloned())
                                            .map(|tx| tx.send(job).is_ok())
                                            .unwrap_or(false);
                                        if !sent {
                                            CLICK_BUSY.store(false, Ordering::Relaxed);
                                            minicard_shell::mlog!("dock 点击：[{i}] 派活失败（线程没起）");
                                        }
                                    }
                                }
                            } else {
                                minicard_shell::mlog!("dock 点击 ({x:.0},{y:.0}) 未命中图标");
                            }
                        }
                    }
                    UiEvent::DockDrop { x } => {
                        let snap = DOCK_SNAP.lock().unwrap().clone();
                        if let Some((tk, geo)) = snap {
                            let from = dock_win.as_ref().and_then(|dw| dw.drag_from);
                            if let Some(from) = from {
                                // 插入位次：落在第 i 个槽中心右边 → 插到 i+1（去掉自己那一位）
                                let mut insert = geo.cx.iter().filter(|&&c| c < x).count();
                                if insert > from {
                                    insert -= 1;
                                }
                                let n = cfg.dock.items.len();
                                let insert = insert.min(n.saturating_sub(1));
                                if insert != from && from < n {
                                    let item = cfg.dock.items.remove(from);
                                    cfg.dock.items.insert(insert, item);
                                    save_cfg(&cfg);
                                    minicard_shell::mlog!("dock 拖拽：[{from}] → [{insert}]");
                                    if let Some(dw) = dock_win.as_mut() {
                                        dw.settle_from = geo.cx.clone();
                                        dw.settle_t0 = Some(Instant::now());
                                        dw.drag_from = None;
                                        dw.last_sig = 0;
                                    }
                                } else if let Some(dw) = dock_win.as_mut() {
                                    dw.drag_from = None;
                                    dw.last_sig = 0;
                                }
                                let _ = tk;
                            }
                        }
                    }
                    UiEvent::DockMenu { x, y, sx, sy } => {
                        let snap = DOCK_SNAP.lock().unwrap().clone();
                        if let Some((tk, geo)) = snap {
                            let hwnd = dock_win.as_ref().map(|d| d.hwnd.0 as isize).unwrap_or(0);
                            match minicard_cards::dock::hit(&tk, &geo, x, y) {
                                Some(i) => {
                                    let Some(item) = cfg.dock.items.get(i).cloned() else {
                                        continue;
                                    };
                                    let is_url = item.path.starts_with("http");
                                    let mut rows: Vec<crate::menu::Item> = vec![
                                        crate::menu::Item {
                                            label: "打开所在文件夹".into(),
                                            danger: false,
                                            disabled: is_url,
                                        },
                                        crate::menu::Item {
                                            label: "从 Dock 移除".into(),
                                            danger: true,
                                            disabled: false,
                                        },
                                    ];
                                    if i > 0 {
                                        rows.push(crate::menu::Item {
                                            label: if item.divider_before {
                                                "移除前面的分隔线".into()
                                            } else {
                                                "在此前加分隔线".into()
                                            },
                                            danger: false,
                                            disabled: false,
                                        });
                                    }
                                    minicard_shell::mlog!(
                                        "dock 右键：条目 [{i}] {} 菜单 {} 项",
                                        item.name,
                                        rows.len()
                                    );
                                    let anchor = dock_anchor(&tk, &geo, Some(i), x);
                                    let pick = crate::menu::show(
                                        anchor,
                                        &rows,
                                        &te,
                                        &pal,
                                        hwnd,
                                        wall.as_ref(),
                                        blur,
                                        monitor_of(sx, sy),
                                    );
                                    minicard_shell::mlog!("dock 右键选择：{pick:?}");
                                    match pick {
                                        Some(0) => {
                                            if let Err(e) =
                                                crate::appwin::reveal_in_explorer(&item.path)
                                            {
                                                minicard_shell::mlog!("打开文件夹失败：{e}");
                                            }
                                        }
                                        Some(1) => {
                                            cfg.dock.items.remove(i);
                                            save_cfg(&cfg);
                                            dock_need_rebuild = true;
                                        }
                                        Some(2) => {
                                            if let Some(it) = cfg.dock.items.get_mut(i) {
                                                it.divider_before = !it.divider_before;
                                            }
                                            save_cfg(&cfg);
                                            dock_need_rebuild = true;
                                        }
                                        _ => {}
                                    }
                                }
                                None => {
                                    // 条空白区：添加应用 / Dock 设置
                                    let anchor = dock_anchor(&tk, &geo, None, x);
                                    let rows = vec![
                                        crate::menu::Item {
                                            label: "添加应用…".into(),
                                            danger: false,
                                            disabled: false,
                                        },
                                        crate::menu::Item {
                                            label: "Dock 设置".into(),
                                            danger: false,
                                            disabled: false,
                                        },
                                    ];
                                    let pick = crate::menu::show(
                                        anchor,
                                        &rows,
                                        &te,
                                        &pal,
                                        hwnd,
                                        wall.as_ref(),
                                        blur,
                                        monitor_of(sx, sy),
                                    );
                                    minicard_shell::mlog!("dock 空白区菜单选择：{pick:?}");
                                    match pick {
                                        Some(0) => {
                                            if let Some(path) = crate::appwin::pick_exe() {
                                                let name = std::path::Path::new(&path)
                                                    .file_stem()
                                                    .map(|s| s.to_string_lossy().to_string())
                                                    .unwrap_or_else(|| path.clone());
                                                cfg.dock.items.push(minicard_core::config::DockItem {
                                                    name,
                                                    path: path.clone(),
                                                    ..Default::default()
                                                });
                                                save_cfg(&cfg);
                                                dock_need_rebuild = true;
                                                minicard_shell::mlog!("dock 添加：{path}");
                                            }
                                        }
                                        Some(1) => {
                                            if !crate::appwin::raise_main_window() {
                                                minicard_shell::mlog!("没找到主窗口（可能在托盘里）");
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                            }
                        }
                    }
                    UiEvent::DragEnd { hwnd } => {
                        drag_end(&mut wins, &mut cfg, &wall, blur, HWND(hwnd as *mut _));
                    }
                }
            }

            if let Some(i) = std::env::var("MC_DOCK_RM")
                .ok()
                .and_then(|s| s.parse::<usize>().ok())
            {
                use std::sync::OnceLock;
                static T0: OnceLock<Instant> = OnceLock::new();
                let t0 = *T0.get_or_init(Instant::now);
                if t0.elapsed().as_secs_f32() > 2.0 && cfg.dock.items.get(i).is_some() {
                    let n = cfg.dock.items.remove(i);
                    save_cfg(&cfg);
                    minicard_shell::mlog!(
                        "dock 移除测试：条目 [{i}] {}，剩 {} 项",
                        n.name,
                        cfg.dock.items.len()
                    );
                    dock_need_rebuild = true;
                }
            }
            // 验收钩子：MC_DOCK_DROP=<路径> 在 3s 后走一遍「拖入」的落库链路（不依赖真鼠标拖放）
            if let Ok(p) = std::env::var("MC_DOCK_DROP") {
                use std::sync::OnceLock;
                static T0: OnceLock<Instant> = OnceLock::new();
                let t0 = *T0.get_or_init(Instant::now);
                if t0.elapsed().as_secs_f32() > 3.0 {
                    let n = crate::drop::add_paths(&[p.clone()], cfg.dock.items.len());
                    if n > 0 {
                        minicard_shell::mlog!("MC_DOCK_DROP 生效：{p}");
                        dock_need_rebuild = true;
                    }
                }
            }
            // 验收钩子：MC_DOCK_CLICK=<i> 在 3s 后按图标中心模拟一次点击（不依赖真鼠标）
            if let Some(i) = std::env::var("MC_DOCK_CLICK")
                .ok()
                .and_then(|s| s.parse::<usize>().ok())
            {
                use std::sync::OnceLock;
                static T0: OnceLock<Instant> = OnceLock::new();
                let t0 = *T0.get_or_init(Instant::now);
                static DONE: std::sync::atomic::AtomicBool =
                    std::sync::atomic::AtomicBool::new(false);
                if t0.elapsed().as_secs_f32() > 3.0
                    && !DONE.swap(true, Ordering::Relaxed)
                    && i < cfg.dock.items.len()
                {
                    if let Some((tk, geo)) = DOCK_SNAP.lock().unwrap().clone() {
                        let x = geo.cx.get(i).copied().unwrap_or(tk.w / 2.0);
                        let y = tk.icon_bottom() - tk.size / 2.0;
                        let _ = EVENTS.lock().unwrap().push(UiEvent::DockClick { x, y });
                        minicard_shell::mlog!("MC_DOCK_CLICK：派发点击 [{i}]");
                    }
                }
            }
            // 工作线程跑完的点击结果 → 改气泡文案（与 dock.html 的
            // 「已最小化 / 已切到前台 / 失败：…」同口径）
            let click_done = CLICK_RESULT.lock().ok().and_then(|mut g| g.take());
            if let Some((i, r)) = click_done {
                CLICK_BUSY.store(false, Ordering::Relaxed);
                CLICK_REPORT.store(true, Ordering::Relaxed);
                if let Some(dw) = dock_win.as_mut() {
                    let (txt, ms) = match r.as_str() {
                        "launched" => ("已启动".to_string(), 1.5),
                        "minimized" => ("已最小化".to_string(), 1.2),
                        "activated" => ("已切到前台".to_string(), 1.2),
                        other => (format!("打开失败：{other}"), 2.2),
                    };
                    let _ = i;
                    dw.flash = Some((txt, Instant::now()));
                    dw.flash_ms = ms;
                    dw.last_sig = 0;
                }
            }
            if crate::drop::take_dropped() {
                dock_need_rebuild = true;
            }
            if dock_need_rebuild {
                if let Some(dw) = dock_win.as_mut() {
                    if let Err(e) = dock_rebuild(dw, &cfg, &wall, blur) {
                        minicard_shell::mlog!("dock 重建失败：{e:?}");
                    }
                }
            }

            let now = Instant::now();
            // 点击期间主循环的最大停顿（判据：点了之后 UI 不该停）——真机验收用
            if let Ok(mut g) = ITER_LAST.lock() {
                if let Some(prev) = *g {
                    let us = now.duration_since(prev).as_micros() as u64;
                    if CLICK_BUSY.load(Ordering::Relaxed)
                        && us > CLICK_MAXGAP_US.load(Ordering::Relaxed)
                    {
                        CLICK_MAXGAP_US.store(us, Ordering::Relaxed);
                    }
                }
                *g = Some(now);
            }
            if CLICK_REPORT.swap(false, Ordering::Relaxed) {
                minicard_shell::mlog!(
                    "点击期间主循环最大停顿 {:.1} ms",
                    CLICK_MAXGAP_US.swap(0, Ordering::Relaxed) as f32 / 1000.0
                );
            }
            let (y, mo, d, hh, mm, _) = now_parts();
            let date = date_text(y, mo, d, now_parts().5);
            let lunar_txt = lunar::lunar_text(y, mo, d).unwrap_or_default();

            // ── 采样健康度：性能/磁盘/网络三卡的「等待数据…／数据异常」都按它判 ──
            // 判据与浏览器同一条（`card.html::computeState`）：没有快照 = 空态，
            // 3.5s 没有新值 = 异常。`at` 由采样线程每拍打（`data::DataSnapshot::at`），
            // 不能拿「数值变了没有」去推 —— 空载机器的 CPU/内存/网速/盘占用能一连几秒一模一样。
            let health = {
                let s = snap.read().unwrap();
                DataHealth::of(s.at, unix_ms())
            };
            // 健康度换档（正常 ↔ 空/异常）的那一拍立刻重绘：数据断了以后不会再有
            // 采样事件叫醒卡片，光等 refresh 节拍最多要 60s 才看见「数据异常」。
            if health != last_health {
                last_health = health;
                for win in wins.iter_mut() {
                    if matches!(
                        win.kind,
                        CardKind::Performance | CardKind::Disks | CardKind::NetRate
                    ) {
                        win.next_draw = now;
                    }
                }
            }

            // ── 天气刷新后立刻重绘 ──
            // 天气卡的 refresh_ms 会被钳到 60s，而天气线程是 10 分钟一轮；不在这里唤醒的话，
            // 新天气最多要等 60s 才上屏（且「城市改了 → 立刻刷」也看不见）。
            {
                let cur = wshared
                    .read()
                    .unwrap()
                    .as_ref()
                    .map(|s| s.at)
                    .unwrap_or(i64::MIN);
                for win in wins.iter_mut() {
                    if let State::Weather { seen_at } = &win.state {
                        if *seen_at != cur {
                            win.next_draw = now;
                        }
                    }
                }
            }

            // ── 磁盘卡高度跟盘数走（派生值，绝不写回 config） ──
            // 1 盘 60 / 2–3 盘 120 / 4–5 盘 180，与 `config::disk_height_px` 同源。
            // 只在数据新鲜时跟随：采样没到、或中途断了，都保持当前高度
            // （不然启动瞬间会先缩成 180 的空档再弹回来）。拖动中跳过 ——
            // 换尺寸会跟磁吸/落点吸附打架（外壳 `sync_disk_height` 同一条规则）。
            {
                let disk_n = snap.read().unwrap().disks.len();
                let dragging = DRAG.lock().unwrap().is_some();
                for win in wins.iter_mut() {
                    let Some(want) =
                        disk_height_for(win.kind, win.h, disk_n, health, dragging)
                    else {
                        continue;
                    };
                    match set_card_height(win, want, &wall, blur) {
                        Ok(()) => minicard_shell::mlog!(
                            "磁盘卡高度：{disk_n} 盘 → {want:.0}px（窗口 {:.0}×{:.0}）",
                            win.w,
                            win.h
                        ),
                        Err(e) => minicard_shell::mlog!("磁盘卡换高度失败：{e:?}"),
                    }
                }
            }

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
                let Some(g) = win.gfx.as_mut() else { continue };
                if g.hover != hov {
                    g.hover = hov;
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
                    set_notify_enabled(notify_wanted(&nc));
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
                            .with_theme(dark)
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
                    let dragging_now = DRAG.lock().unwrap().is_some();
                    for win in wins.iter_mut() {
                        if blur_changed {
                            reslice(win, &wall, blur);
                        }
                        if let Some(card) = nc.cards.get(win.cfg_idx) {
                            win.refresh =
                                Duration::from_millis(card.refresh_ms.clamp(200, 60_000));
                            if let Some(e) = PINNED
                                .lock()
                                .unwrap()
                                .iter_mut()
                                .find(|(h, _)| *h == win.hwnd.0 as isize)
                            {
                                e.1 = card.pinned;
                            }
                            // 主界面位置面板 / 外部改 config：原生窗不在主进程里，
                            // 只能由绘制进程按盘上的 pos 自己搬；拖动中不动（磁吸在跟手）。
                            if !dragging_now {
                                let mut r = RECT::default();
                                if GetWindowRect(win.hwnd, &mut r).is_ok() {
                                    let tx = card.pos[0].round() as i32;
                                    let ty = card.pos[1].round() as i32;
                                    if r.left != tx || r.top != ty {
                                        let _ = SetWindowPos(
                                            win.hwnd,
                                            None,
                                            tx,
                                            ty,
                                            0,
                                            0,
                                            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                                        );
                                        reslice(win, &wall, blur);
                                        // 位置热载可能把卡搬到另一块屏：刷新率跟着重探
                                        retime_card(win);
                                    }
                                }
                            }
                            match &mut win.state {
                                State::Perf { show_gpu, .. } => *show_gpu = card.show_gpu,
                                State::Disks { warn90 } => *warn90 = card.warn90,
                                State::Notify {
                                    show_body,
                                    per_app,
                                    ..
                                } => {
                                    *show_body = nc.settings.notify_show_body;
                                    *per_app = nc.settings.notify_max_per_app.clamp(1, 3) as usize;
                                }
                                _ => {}
                            }
                        }
                        win.next_draw = now;
                    }
                    // Dock 的尺寸/条目数/位置/圆角相关项变了 → 重建 Dock 窗
                    // （settings 页里改 48↔64 档、隐藏桌面图标、Dock 换边都在这里生效）
                    let dock_geom_changed = {
                        let a = &cfg.dock;
                        let b = &nc.dock;
                        a.size != b.size
                            || a.items.len() != b.items.len()
                            || a.cx != b.cx
                            || a.y != b.y
                            || a.enabled != b.enabled
                            || a.z != b.z
                    };
                    cfg = nc;
                    if dock_geom_changed {
                        if cfg.dock.enabled {
                            if let Some(dw) = dock_win.as_mut() {
                                match dock_rebuild(dw, &cfg, &wall, blur) {
                                    Ok(()) => minicard_shell::mlog!("config 热载：Dock 重建"),
                                    Err(e) => minicard_shell::mlog!("config 热载：Dock 重建失败 {e:?}"),
                                }
                            }
                        }
                    } else if blur_changed {
                        // 模糊变了但几何没变：Dock 的玻璃也得重切
                        if let Some(dw) = dock_win.as_mut() {
                            if let Err(e) = dock_rebuild(dw, &cfg, &wall, blur) {
                                minicard_shell::mlog!("Dock 玻璃重切失败：{e:?}");
                            }
                        }
                    }
                }
            }

            // 刷新邻居矩形表（磁吸用）：拖动时每帧刷新；平时 250ms 一次就够，
            // 不必 60 次/秒白打 10 个 GetWindowRect。
            let drag_now = DRAG.lock().unwrap().is_some();
            if drag_now || now >= rects_at {
                rects_at = if drag_now { now } else { now + Duration::from_millis(250) };
                let mut rects = Vec::with_capacity(wins.len());
                for win in wins.iter() {
                    let mut r = RECT::default();
                    if GetWindowRect(win.hwnd, &mut r).is_ok() {
                        rects.push((win.hwnd.0 as isize, (r.left, r.top, r.right, r.bottom)));
                    }
                }
                *CARD_RECTS.lock().unwrap() = rects;
            }

            // Dock：悬停放大 + 名字气泡（每帧算，指针不动就不重绘）
            let mut dock_busy = false;
            let mut dock_paint_ms = 0.0f32;
            // Dock 的显示帧长（无 Dock 时给个无害的 60Hz；dock_busy=false 时不会用到）
            let mut dock_frame_step = Duration::from_micros(16_667);
            if let Some(dw) = dock_win.as_mut() {
                let dpi = dw.dpi;
                // Dock 交互/放大动画：每个显示帧一步（200Hz → 5ms）；MC_DOCK_HZ 只留给台架覆盖
                let dock_step = dock_step_of(&dw.timing);
                dock_frame_step = dock_step;
                // 验收用：`MC_DOCK_DEMO=<窗内 x>` 把指针钉死，不依赖真鼠标（截图取证）
                let demo_x = std::env::var("MC_DOCK_DEMO").ok().and_then(|s| s.parse::<f32>().ok());
                // 性能台架：`MC_DOCK_SWEEP=1` 让指针在条上来回扫（真鼠标会被抢走，量不了帧率）
                let sweep_x = if std::env::var("MC_DOCK_SWEEP").is_ok() {
                    use std::sync::OnceLock;
                    static T0: OnceLock<Instant> = OnceLock::new();
                    let t0 = T0.get_or_init(Instant::now);
                    let s = t0.elapsed().as_secs_f32();
                    // 扫掠幅度不出条：旧的 380 比条半宽还大（条宽 520 → 半宽 260），
                    // 约 47% 时间指针被 clamp 停在条外、内容不变 → 合法跳帧，会把
                    // 「两次 paint 的间隔」拉成 ~150 ms 的假停顿（2026-11-05 已归因）
                    let amp = (dw.t.w / 2.0 - 60.0).max(40.0);
                    Some((dw.t.w / 2.0 + amp * (s * 1.4).sin()).clamp(0.0, dw.t.w))
                } else {
                    None
                };
                let ptr = dw.pointer(dpi);
                let px_in = match (demo_x, sweep_x) {
                    (Some(x), _) => Some(x),
                    (None, Some(x)) => Some(x),
                    _ => ptr.map(|p| p.0),
                };
                let hit = px_in;
                // 进出场：临界阻尼（τ=50ms），与 dock.html 同一个时钟感
                let target = if hit.is_some() { 1.0 } else { 0.0 };
                if let Some(x) = px_in {
                    dw.px = x;
                }
                let moving = (dw.ease - target).abs() > 0.0015 || dw.ease_v.abs() > 0.01;
                let frame_dt = (now - dw.last_t).as_secs_f32().clamp(1.0 / 240.0, 0.05);
                dw.last_t = now;
                if moving {
                    let dt = frame_dt;
                    let (e, v) =
                        minicard_cards::dock::step_ease(dw.ease, dw.ease_v, target, dt, 0.05);
                    dw.ease = e;
                    dw.ease_v = v;
                } else if target == 0.0 {
                    dw.ease = 0.0;
                    dw.ease_v = 0.0;
                } else {
                    dw.ease = 1.0;
                    dw.ease_v = 0.0;
                }
                dock_busy = moving || hit.is_some();
                // 可点区域跟着放/收（放：放大图标 + 名字气泡要能点；收：预留区还给下面）
                let want_open = dw.ease > 0.002;
                if want_open != dw.region_open {
                    dock_set_hit(&dw.t, want_open);
                    dw.region_open = want_open;
                }
                // 运行点：2s 一轮（可见顶层窗口 → exe），变了就强制重绘一次
                if now >= dw.next_running {
                    let set = RUNNING_SET
                        .lock()
                        .ok()
                        .and_then(|g| g.clone())
                        .unwrap_or_default();
                    let want: Vec<bool> = cfg
                        .dock
                        .items
                        .iter()
                        .map(|it| crate::appwin::item_running(&crate::appwin::match_key(&it.path), &set))
                        .collect();
                    if want != dw.running {
                        let n_on = want.iter().filter(|b| **b).count();
                        minicard_shell::mlog!("dock 运行点：{n_on}/{}", want.len());
                        dw.running = want;
                        dw.last_sig = 0;
                    }
                    dw.next_running = now + Duration::from_millis(120);
                }
                // 验收钩子 `MC_DOCK_DRAG=<from>:<窗内 x>`：一次跑完「按下 → 拖 → 松手」，
                // 不依赖真鼠标（用户鼠标一动，注入的拖拽就被抢走，量不了）
                if let Ok(spec) = std::env::var("MC_DOCK_DRAG") {
                    use std::sync::OnceLock;
                    use std::sync::atomic::{AtomicBool, Ordering as AO};
                    static FIRED: AtomicBool = AtomicBool::new(false);
                    static T0: OnceLock<Instant> = OnceLock::new();
                    if !FIRED.load(AO::Relaxed) {
                        let t0 = *T0.get_or_init(Instant::now);
                        let el = t0.elapsed().as_secs_f32();
                        if let Some((from_s, to_s)) = spec.split_once(':') {
                            let from: usize = from_s.parse().unwrap_or(0);
                            let to: f32 = to_s.parse().unwrap_or(0.0);
                            if el > 0.4 && el < 0.8 {
                                let cx = dw.t.side + cfg.dock.item_center_x(from);
                                *DOCK_PRESS.lock().unwrap() = Some((from, cx, 204.0));
                            } else if el >= 0.8 && el < 1.6 {
                                if dw.drag_from.is_none() {
                                    dw.drag_from = Some(from);
                                    minicard_shell::mlog!("dock 拖拽开始：from={from} → x={to}");
                                }
                                dw.drag_x = to;
                                dw.last_sig = 0;
                            } else if el >= 1.6 && dw.drag_from.is_some() {
                                let _ = EVENTS.lock().unwrap().push(UiEvent::DockDrop { x: to });
                                *DOCK_PRESS.lock().unwrap() = None;
                                FIRED.store(true, AO::Relaxed);
                            }
                        }
                    }
                }
                // 拖着走：按下时命中的图标 + 挪动超过 4px → 进入拖拽排序
                if dw.drag_from.is_none() {
                    if let (Some((i, px0, py0)), Some((x, y))) =
                        (*DOCK_PRESS.lock().unwrap(), ptr)
                    {
                        if (x - px0).abs() + (y - py0).abs() > 4.0 {
                            dw.drag_from = Some(i);
                            dw.drag_x = x;
                            dw.bubble_a = 0.0;
                            dw.flash = None;
                            dw.last_sig = 0;
                        }
                    }
                }
                if dw.drag_from.is_some() {
                    if let Some((x, _)) = ptr {
                        dw.drag_x = x.clamp(0.0, dw.t.w);
                    }
                }
                // 几何每帧算一份：命中与绘制必须同源
                let tk = dw.t;
                let dragging = dw.drag_from.is_some();
                let mut geo = tk.geo(&cfg.dock, if dragging { None } else { Some(dw.px) }, dw.ease);
                // 松手回弹：各槽从「旧位置」滑到「新位置」（140ms easeOutCubic）
                if let Some(t0) = dw.settle_t0 {
                    let u = now.duration_since(t0).as_secs_f32() / 0.14;
                    if u < 1.0 {
                        let e = 1.0 - (1.0 - u).powi(3);
                        for (i, c) in geo.cx.iter_mut().enumerate() {
                            if let Some(old) = dw.settle_from.get(i) {
                                *c += (old - *c) * (1.0 - e);
                            }
                        }
                    } else {
                        dw.settle_t0 = None;
                    }
                }
                // demo/sweep 时用钉住的 x（否则真鼠标在哪气泡就去哪，截图不可复现）
                let hover_pt = if demo_x.is_some() || sweep_x.is_some() {
                    px_in.map(|x| (x, tk.icon_bottom() - tk.size / 2.0))
                } else {
                    ptr
                };
                let hover_i = hover_pt.and_then(|(x, y)| minicard_cards::dock::hit(&tk, &geo, x, y));
                if std::env::var("MC_DOCK_LOG").is_ok() {
                    use std::sync::atomic::{AtomicU64, Ordering as O};
                    static LAST: AtomicU64 = AtomicU64::new(0);
                    let ms = t_start.elapsed().as_millis() as u64;
                    if ms - LAST.load(O::Relaxed) >= 900 {
                        LAST.store(ms, O::Relaxed);
                        minicard_shell::mlog!(
                            "dock 状态：px_in={:?} hover={:?} a={:.2} ease={:.2} cx8={:?} k8={:?} bottom={:.1} pt={:?}",
                            px_in, hover_i, dw.bubble_a, dw.ease,
                            geo.cx.get(8).copied(), geo.ks.get(8).copied(),
                            tk.icon_bottom(), hover_pt
                        );
                    }
                }

                // 气泡：点击反馈（"启动中…"）优先，其次悬停的名字；120ms 淡入淡出
                let flash_left = dw
                    .flash
                    .as_ref()
                    .map(|(_, t0)| dw.flash_ms - now.duration_since(*t0).as_secs_f32())
                    .unwrap_or(-1.0);
                if flash_left <= 0.0 {
                    dw.flash = None;
                }
                let bubble_target =
                    if dragging { 0.0 } else if flash_left > 0.0 || hover_i.is_some() { 1.0 } else { 0.0 };
                let dt_b = frame_dt;
                let bubble_moving = (dw.bubble_a - bubble_target).abs() > 0.001;
                if bubble_moving {
                    let dv = dt_b / 0.12;
                    dw.bubble_a = if bubble_target > dw.bubble_a {
                        (dw.bubble_a + dv).min(1.0)
                    } else {
                        (dw.bubble_a - dv).max(0.0)
                    };
                }
                let bubble_txt: Option<String> = if flash_left > 0.0 {
                    dw.flash.as_ref().map(|(s, _)| s.clone())
                } else if dw.bubble_a > 0.01 {
                    hover_i.and_then(|i| cfg.dock.items.get(i).map(|it| it.name.clone()))
                } else {
                    None
                };
                let bubble = bubble_txt.as_ref().map(|s| minicard_cards::dock::Bubble {
                    text: s.as_str(),
                    cx: hover_i
                        .and_then(|i| geo.cx.get(i).copied())
                        .unwrap_or(dw.px),
                    alpha: dw.bubble_a,
                });

                // 外部拖进来的落点线优先（拖放时条不放大，用静止态几何）
                let ext_drop = crate::drop::ui();
                let drag_draw = ext_drop
                    .map(|u| minicard_cards::dock::DragDraw {
                        from: usize::MAX,
                        x: u.x,
                        insert: u.insert,
                    })
                    .or_else(|| dw.drag_from.map(|from| {
                    // 插入位次：落在第 i 个槽中心右侧 → 插到 i+1；走到自己左边那半算原位
                    let mut insert = geo.cx.iter().filter(|&&c| c < dw.drag_x).count();
                    if insert > from {
                        insert -= 1;
                    }
                    minicard_cards::dock::DragDraw { from, x: dw.drag_x, insert }
                    }));
                let sig = (dw.ease * 4096.0) as u64 * 1_000_003
                    + (dw.px * 8.0) as u64
                    + hit.is_some() as u64
                    + (dw.bubble_a * 1024.0) as u64 * 7919
                    + hover_i.unwrap_or(999) as u64 * 31
                    + flash_left.max(0.0) as u64 * 104_729
                    + dw.drag_from.unwrap_or(999) as u64 * 31_337
                    + (dw.drag_x * 8.0) as u64
                    + dw.settle_t0.is_some() as u64 * 7
                    + ext_drop.map(|u| u.insert as u64 + 1).unwrap_or(0) * 1_009;
                // 静置兜底 1s 一回；悬停但指针不动**不重绘**（否则白烧 60fps）
                let idle_due = now >= dw.next_paint;
                if moving || bubble_moving || sig != dw.last_sig || idle_due || dw.settle_t0.is_some() {
                    dw.next_paint = now + Duration::from_millis(1000);
                    let DockWin { gfx, t: tk, icons, glass, .. } = dw;
                    let refs: Vec<Option<&ID2D1Bitmap1>> =
                        icons.iter().map(|o| o.as_ref()).collect();
                    let running = dw.running.clone();
                    let gg = if std::env::var("MC_DOCK_NOGLASS").is_ok() {
                        None
                    } else {
                        glass.as_ref().map(|(b, r)| (b, *r))
                    };
                    let paint_t0 = Instant::now();
                    *DOCK_SNAP.lock().unwrap() = Some((*tk, geo.clone()));
                    let Some(gfx) = gfx.as_ref() else { continue };
                    let r = minicard_cards::dock::paint(
                        gfx, &te, &pal, tk, &cfg.dock, &geo, &refs, &running, gg,
                        bubble.as_ref(), drag_draw.as_ref(),
                    );
                    let _ = r;
                    dock_paint_ms = paint_t0.elapsed().as_secs_f32() * 1000.0;
                    st_dock_draws += 1;
                    st_dock_us += dock_paint_ms as f64;
                    dw.last_sig = sig;
                    if std::env::var("MC_DOCK_LOG").is_ok() {
                        static LAST_PAINT: std::sync::Mutex<Option<Instant>> = std::sync::Mutex::new(None);
                        static DTS: std::sync::Mutex<Vec<f32>> = std::sync::Mutex::new(Vec::new());
                        static DROPPED: std::sync::atomic::AtomicUsize =
                            std::sync::atomic::AtomicUsize::new(0);
                        let prev = *LAST_PAINT.lock().unwrap();
                        *LAST_PAINT.lock().unwrap() = Some(now);
                        if let Some(pv) = prev {
                            let d = (now - pv).as_secs_f32() * 1000.0;
                            if d > 0.0 && d < 500.0 {
                                DTS.lock().unwrap().push(d);
                            } else if d >= 500.0 {
                                // 静置/跳帧（paint gating）造成的长间隔单独计数：
                                // 它们是「没变化所以没画」，不是卡顿，不该混进 max
                                DROPPED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            }
                        }
                        let mut v = DTS.lock().unwrap();
                        if v.len() >= 120 {
                            let n = v.len();
                            v.sort_by(|a, b| a.partial_cmp(b).unwrap());
                            let p50 = v[n / 2];
                            let p95 = v[n * 95 / 100];
                            let mx = v[n - 1];
                            let dropped = DROPPED.swap(0, std::sync::atomic::Ordering::Relaxed);
                            minicard_shell::mlog!(
                                "dock 帧间隔 p50 {p50:.1} / p95 {p95:.1} / max {mx:.1} ms（{n} 帧，跳帧 {dropped} 次）绘制 {dock_paint_ms:.2} ms"
                            );
                            v.clear();
                        }
                    }
                }
                // 悬停/动画中：**每个显示帧**（绝对节拍 `next = prev + step`，不是 `now + step`），
                // 落后整帧就跳到下一个显示帧。刷新率来自所在显示器（MC_DOCK_HZ 只做台架覆盖）。
                if moving || hit.is_some() || bubble_moving || dragging || dw.settle_t0.is_some() {
                    let (next, _) = advance_deadline(dw.next_draw, now, dock_step);
                    dw.next_draw = next;
                } else {
                    dw.next_draw = now + Duration::from_millis(1000);
                }
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
                // Dock 正在跑显示帧、且上一帧已经吃掉一半帧预算：这一轮不画卡，下一轮再画
                // （卡只晚一个显示帧）。预算按 Dock 所在屏的帧长算：200Hz → 2.5ms，
                // 60Hz → 8.33ms（原写死 4ms 在 200Hz 下等于吃掉 80% 预算，所以跟着刷新率走）。
                let dock_yield_ms = (dock_frame_step.as_secs_f32() * 500.0).max(2.0);
                if dock_busy && dock_paint_ms > dock_yield_ms {
                    if win.next_draw < earliest {
                        earliest = win.next_draw;
                    }
                    continue;
                }
                // 遮挡闸门：整块被别的窗口盖住 → 这一拍不画（4/s 复查）。
                // 露出来的那一拍 `covered=false`，会立刻补一帧当前状态。
                if now >= win.cover_at {
                    win.covered = card_fully_covered(win.hwnd, win.w, win.h);
                    win.cover_at = now + Duration::from_millis(250);
                }
                if win.covered {
                    st_cover_skips += 1;
                    win.next_draw = now + Duration::from_millis(250);
                    if win.next_draw < earliest {
                        earliest = win.next_draw;
                    }
                    continue;
                }
                drawn += 1;
                let st_draw_t0 = Instant::now();
                let animating = draw_one(
                    win,
                    &snap,
                    &wshared,
                    &te,
                    &pal,
                    health,
                    now,
                    (y, mo, d, hh, mm),
                    &date,
                    &lunar_txt,
                )?;
                st_draws += 1;
                st_draw_us += st_draw_t0.elapsed().as_micros() as u64;
                if let Some(g) = win.gfx.as_ref() {
                    st_end_us += (g.t_end_raw.get() * 1000.0) as u64;
                    st_present_us += (g.t_present.get() * 1000.0) as u64;
                }
                st_kind_draws[kind_index(win.kind)] += 1;
                // 动画中按显示器节拍：短动效（待办勾选/FLIP、翻月、时钟翻位）每显示帧一步；
                // 连续补间（性能/网络条、音乐播放、番茄钟进度）按 max(1,floor(hz/60)) 帧抽帧
                // （保证 ≥60fps，别让常驻卡白烧 CPU）；静置按卡片自己的 refresh。
                let anim_step = anim_step(win.kind, animating, win.refresh, &win.timing);
                // 绝对 deadline：从上一拍按整步推进（不是 now + step），落后跳帧、不累积漂移
                let (next, skipped) = advance_deadline(win.next_draw, now, anim_step);
                win.next_draw = next;
                if win.next_draw < earliest {
                    earliest = win.next_draw;
                }
                // MC_FPS_LOG=1：按卡统计动画帧间隔（静置帧不计入）
                if let Some(fps) = win.fps.as_mut() {
                    if animating {
                        fps.note(now, anim_step, skipped);
                        fps.log_if_due(win.kind, win.timing.hz);
                    } else {
                        fps.reset();
                    }
                }
            }

            // 可见性位图：采样/轮询线程据此降频（卡片全被盖住时没必要 500ms 查一次 WinRT/PDH）
            {
                let mut vis = 0u64;
                for w in wins.iter() {
                    if !w.covered {
                        vis |= 1u64 << (w.kind as u64);
                    }
                }
                minicard_core::util::set_visible_mask(vis);
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
                // 2 秒一行的耗时账本：迭代率 / 画卡率 / 单帧拆分 / 循环忙时
                let since = st_last.elapsed();
                if since >= Duration::from_secs(2) {
                    let s = since.as_secs_f64();
                    let d = st_draws.max(1) as f64;
                    let dock_avg = if st_dock_draws > 0 {
                        st_dock_us / st_dock_draws as f64
                    } else {
                        0.0
                    };
                    minicard_shell::mlog!(
                        "stat {s:.1}s：iter {:.0}/s busy {:.0} ms/s（{:>4.1}%）| 卡 {:.1}/s 单帧 {:.2} ms（EndDraw {:.2} + Present {:.2}）合计 {:.0} ms/s | dock {:.1}/s 单帧 {:.2} ms | 遮挡跳过 {:.0}/s | 分布 {:?}",
                        st_iters as f64 / s,
                        st_busy_us as f64 / (s * 1000.0),
                        st_busy_us as f64 / (s * 10_000.0),
                        st_draws as f64 / s,
                        st_draw_us as f64 / d / 1000.0,
                        st_end_us as f64 / d / 1000.0,
                        st_present_us as f64 / d / 1000.0,
                        st_draw_us as f64 / (s * 1000.0),
                        st_dock_draws as f64 / s,
                        dock_avg,
                        st_cover_skips as f64 / s,
                        st_kind_draws
                    );
                    st_last = Instant::now();
                    st_iters = 0;
                    st_busy_us = 0;
                    st_draws = 0;
                    st_draw_us = 0;
                    st_end_us = 0;
                    st_present_us = 0;
                    st_dock_draws = 0;
                    st_dock_us = 0.0;
                    st_kind_draws = [0u64; 10];
                    st_cover_skips = 0;
                }
            }
            // 注意：DComp 桌面层窗的 Present(1) **不等 vblank**（实测不睡会跑成 200fps），
            // 所以节拍必须自己用 deadline 控；`dock_painted` 只用来决定卡片要不要让路。

            st_busy_us += st_iter_t0.elapsed().as_micros() as u64;
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
    health: DataHealth,
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
    // 磁盘卡换高度的那一拍没有 Gfx（旧的已拆、新的还没建好）：这一帧跳过
    let Some(gfx) = win.gfx.as_ref() else {
        return Ok(false);
    };
    // 性能 / 磁盘 / 网络：采样没到 → 「等待数据…」，3.5s 没新值 → 「数据异常」。
    // 浏览器口径是整卡替换（`card.html::computeState` + `.empty`），内容/进度条都不画。
    if let Some(txt) = health.text() {
        if matches!(
            win.kind,
            CardKind::Performance | CardKind::Disks | CardKind::NetRate
        ) {
            let tk = status_tokens(win.kind, win.w, win.h);
            paint_status(gfx, te, pal, &tk, win.w, win.h, txt, glass_pair)?;
            return Ok(false);
        }
    }
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
            perf::paint(gfx, te, pal, &tk, &rows, glass_pair, false)?;
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
            disks::paint(gfx, te, pal, &tk, &list, glass_pair, *warn90, false)?;
            false
        }
        (State::Clock(c), _) => {
            c.set(hh, mm, now);
            let tk = clock::Tokens::clock(win.w, win.h);
            clock::paint(gfx, te, pal, &tk, c, date, lunar_txt, now, glass_pair)?;
            !c.idle(now)
        }
        (State::Net(st), _) => {
            st.set(snap_r.net.tx, snap_r.net.rx, now);
            let tk = net::Tokens::net(win.w, win.h, 1.044, 0.282);
            net::paint(gfx, te, pal, &tk, st, now, glass_pair)?;
            st.animating(now)
        }
        (State::Pomo(p), _) => {
            p.tick(now);
            let tk = pomo::Tokens::pomo(win.w, win.h);
            pomo::paint(gfx, te, pal, &tk, p, now, glass_pair)?;
            // 只有「有效模式仍是运行中」才需要持续高帧率重绘；到点后 eff_mode 变 Done，
            // 若还按 mode==Run 返回 true，会永远每显示帧重画（实测空转 30% CPU）。
            p.eff_mode(now) == pomo::PomoMode::Run || p.animating(now)
        }
        (State::Todo(items), _) => {
            let tk = todo::Tokens::todo(win.w, win.h);
            todo::paint(gfx, te, pal, &tk, items, now, glass_pair)?;
            todo::animating(items, now)
        }
        (State::Notify { motion, show_body, per_app, clear_armed }, _) => {
            // 服务每 500ms 轮询一次；卡片按自身 refresh 节拍读快照（默认 1s，够及时也不白烧）
            let ns = if notify_enabled() {
                Some(notify_service().snapshot())
            } else {
                None
            };
            let tk = notify_card::Tokens::notify(win.w, win.h);
            let icons = ns
                .as_ref()
                .map(|s| notify_icon_map(gfx, s))
                .unwrap_or_default();
            if clear_armed.is_some_and(|t| t.elapsed() >= Duration::from_secs(3)) {
                *clear_armed = None;
            }
            let opts = notify_card::Options {
                show_body: *show_body,
                per_app: *per_app,
                clear_armed: clear_armed.is_some(),
            };
            notify_card::paint(
                gfx,
                te,
                pal,
                &tk,
                ns.as_ref(),
                &icons,
                opts,
                motion,
                now,
                glass_pair,
            )?;
            // 到达/删除/hover/滚动/清除/状态淡入还在跑：按显示帧继续重绘
            motion.animating(now)
        }
        (State::Weather { seen_at }, _) => {
            let ws = wshared.read().unwrap().clone();
            // 浏览器口径（card.html::computeState）：>35 分钟没新快照 = 整卡「数据异常」
            let now_secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let stale = ws
                .as_ref()
                .map(|s| s.at > 0 && now_secs.saturating_sub(s.at) > 35 * 60)
                .unwrap_or(false);
            let (city, temp, code, air, hum, days) = match &ws {
                Some(s) => (
                    s.city.clone(),
                    Some(s.temp as f32),
                    wmo_u8(s.code),
                    Some(s.air.clone()),
                    Some(s.humidity as i32),
                    s.days
                        .iter()
                        .map(|d| weather::Day {
                            label: d.label.clone(),
                            hi: Some(d.hi as f32),
                            lo: Some(d.lo as f32),
                            code: wmo_u8(d.code),
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
                empty: ws.is_none(),
                error: stale,
            };
            weather::paint(gfx, te, pal, &wk, &wv, glass_pair)?;
            // 记下画的是哪一版（主循环拿它判断「天气刷新了 → 立刻重绘」）
            *seen_at = ws.as_ref().map(|s| s.at).unwrap_or(i64::MIN);
            false
        }
        (State::Music { cover, prev, flip_t0, flip_dir, flip_dir_run, demo_t0, plan, bad_cover }, _) => {
            let m = media::snapshot();
            let mw = match &m {
                Some(s) if s.present => music::Music {
                    title: s.title.clone(),
                    artist: s.artist.clone(),
                    playing: s.status == "playing",
                    position_ms: s.position_ms as u64,
                    duration_ms: s.duration_ms as u64,
                },
                _ => music::Music {
                    // 无会话 / present=false = 占位样张（字面词，不填样例数据）
                    title: "歌名".into(),
                    artist: "歌手".into(),
                    playing: false,
                    position_ms: 0,
                    duration_ms: 0,
                },
            };
            let mk = music::Tokens::music(win.w, win.h);
            // 封面：换曲 → 换图判定交给 `music::FlipPlan`（缩略图常比歌名晚到一拍，
            // 旧实现只在「同拍」才起翻 → 晚到的封面直接换图 = 用户所见「翻转没生效，直接换封面」）。
            if let Some(s) = m.as_ref().filter(|s| s.present) {
                let px = (win.h - 2.0 * mk.pad) * win.dpi / 96.0;
                let cover_r = mk.cover_r * win.dpi / 96.0;
                let shown = cover.as_ref().map(|(u, _)| u.as_str()).unwrap_or("");
                let step = plan.step(
                    &format!("{}|{}", s.title, s.artist),
                    &s.cover,
                    shown,
                    now,
                );
                // 先处理「不动 / 放弃 / 清占位」，再把「要换的那张」交给统一的解码分支
                let to_show = match step {
                    music::FlipStep::Idle => None,
                    music::FlipStep::GaveUp => {
                        minicard_shell::mlog!(
                            "music cover: 换曲后 {}s 没等到新封面（沿用上一张）",
                            music::FLIP_WAIT.as_secs()
                        );
                        None
                    }
                    music::FlipStep::Clear => {
                        // 这歌确实没封面：别把上一首的图一直挂着（回落渐变 + 音符）
                        *cover = None;
                        *prev = None;
                        None
                    }
                    music::FlipStep::Show(url) => Some((url, false, Duration::ZERO)),
                    music::FlipStep::Flip { url, waited } => Some((url, true, waited)),
                };
                if let Some((url, flip, waited)) = to_show {
                    // 解码失败过的那张别每帧重试（一帧 60 次解码会把卡拖死）
                    if bad_cover.as_deref() != Some(url.as_str()) {
                        let side = px.round().max(1.0) as u32;
                        let made = minicard_draw::image::decode_data_url(&url).and_then(|img| {
                            let mut small = minicard_draw::image::resize_cover(&img, side, side);
                            minicard_draw::image::bake_round_alpha(&mut small, cover_r);
                            gfx.bitmap_from_bgra(&small.bgra, small.w, small.h)
                        });
                        match made {
                            Ok(b) => {
                                if flip && cover.is_some() {
                                    *prev = cover.take().map(|(_, b)| b);
                                    *flip_t0 = Some(now);
                                    *flip_dir_run = *flip_dir;   // 用掉方向提示（下次翻默认下一首）
                                    *flip_dir = 1.0;
                                    minicard_shell::mlog!(
                                        "music flip start（dir {}，封面晚到 {}ms）",
                                        *flip_dir_run,
                                        waited.as_millis()
                                    );
                                }
                                *cover = Some((url, b));
                                *bad_cover = None;
                            }
                            Err(e) => {
                                minicard_shell::mlog!(
                                    "封面解码/上传失败: {e:?}（dataURL {} 字节）",
                                    url.len()
                                );
                                *bad_cover = Some(url);
                            }
                        }
                    }
                }
            } else {
                // 无会话/断流：清掉旧封面，回到渐变 + 音符占位
                *cover = None;
                *prev = None;
            }
            // 开发用：MC_FLIP_DEMO=1 时每 2s 自翻一次（没有真换曲也能验动画）
            if std::env::var_os("MC_FLIP_DEMO").is_some() && cover.is_some() {
                let due = demo_t0.map(|t| now >= t).unwrap_or(true);
                if due && flip_t0.is_none() {
                    *flip_t0 = Some(now);
                    *flip_dir_run = *flip_dir;
                    *flip_dir = 1.0;
                    *demo_t0 = Some(now + Duration::from_millis(2000));
                }
            }
            let flip = flip_t0.map(|t0| music::CoverFlip {
                age: (now - t0).as_secs_f32(),
                dir: *flip_dir_run,
                old: prev.as_ref().or(cover.as_ref().map(|(_, b)| b)),
                new: cover.as_ref().map(|(_, b)| b),
            });
            music::paint(
                gfx,
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
                // 方向在起翻那一刻就用掉了（见上方 flip_dir_run），这里别再复位：
                // 否则新封面晚到时方向等不到翻开就被清掉
            }
            mw.playing || flipping || music::marquee_animating()
        }
        (State::Calendar(c, off, built_day), _) => {
            let (cy, cm) = month_with_offset(y, mo, *off);
            // 回到本月要跟着「今天」走；翻到别的月只在翻页时重建
            let today_day = if *off == 0 { d } else { 0 };
            if c.year != cy || c.month != cm || (*off == 0 && *built_day != d) {
                *c = calendar_for(y, mo, *off, d);
                *built_day = today_day;
            }
            // 台架钩子（默认关）：没有用户点击也能持续产生短动效，用来量「每显示帧」的节拍。
            // 每 N 毫秒调一次 flip + roll_header（真实点击时两者同时起跑；月份不变，只验动效）。
            if let Some(period) = cal_demo_period() {
                static NEXT: Mutex<Option<Instant>> = Mutex::new(None);
                let due = {
                    let mut g = NEXT.lock().unwrap();
                    let due = g.map(|t| now >= t).unwrap_or(true);
                    if due {
                        *g = Some(now + period);
                    }
                    due
                };
                if due {
                    let prev_title = c.title();
                    let prev_holiday = c.holiday.clone();
                    c.flip(1);
                    c.roll_header(prev_title, prev_holiday, 1);
                }
            }
            let ck = calendar::Tokens::cal(win.w, win.h);
            calendar::paint(gfx, te, pal, &ck, c, glass_pair)?;
            // 翻月/按压缩放还在跑：按显示帧节拍继续重绘，别等静置节拍
            c.animating(now)
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
    // 只在本月视图显示今天的节日；翻到别的月标题行留空（别拿 1 号的节日冒充今天）
    c.holiday = if off == 0 {
        calendar::holiday(cy, cm, today)
    } else {
        String::new()
    };
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
                // 0.1.3：状态变化即时落盘（重启/电脑重启后计时不丢）
                save_pomo(p);
            }
        }
        State::Calendar(c, off, built_day) => {
            let tk = calendar::Tokens::cal(win.w, win.h);
            if let Some(h) = calendar::hit(te, &tk, c, x, y) {
                let dir = match h {
                    calendar::Hit::Prev => -1,
                    calendar::Hit::Next => 1,
                };
                // 建新页之前先记住旧标题/节日：头部要像滚轮一样「旧字滚出、新字滚入」
                let prev_title = c.title();
                let prev_holiday = c.holiday.clone();
                *off += dir;
                *c = calendar_for(yy, mo, *off, dd);
                *built_day = if *off == 0 { dd } else { 0 };
                // 只有用户点 ‹ › 才带动画（跨天刷新走 0，不重放）
                c.flip(dir);
                c.tap(dir);
                c.roll_header(prev_title, prev_holiday, dir);
            }
        }
        State::Music { flip_dir, .. } => {
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
                }
                .to_string();
                // 播放器不响应时 RequestAsync().join() 会阻塞；别冻住九张卡共用的消息循环
                std::thread::spawn(move || {
                    if let Err(e) = media::action(&cmd) {
                        minicard_shell::mlog!("音乐控制 {cmd} 失败: {e}");
                    }
                });
            }
        }
        State::Notify {
            motion,
            show_body,
            per_app,
            clear_armed,
        } => {
            if !notify_enabled() {
                return;
            }
            let snap = notify_service().snapshot();
            let tk = notify_card::Tokens::notify(win.w, win.h);
            let opts = notify_card::Options {
                show_body: *show_body,
                per_app: *per_app,
                clear_armed: clear_armed.is_some(),
            };
            let mut m: &TextEngine = te;
            if let Some(h) =
                notify_card::hit_at(&mut m, &tk, Some(&snap), opts, motion, x, y)
            {
                match h {
                    notify_card::Hit::Item { id, aumid } => {
                        notify_service().mark_read(id);
                        let app = snap
                            .items
                            .iter()
                            .find(|it| it.id == id)
                            .map(|it| it.app.clone())
                            .unwrap_or_default();
                        std::thread::spawn(move || {
                            // ① 复刻系统 toast 自己的激活：`activationType="protocol"` 时
                            //    Windows 点它只是 ShellExecute(launch)（QQ 邮件提醒就是这条，
                            //    打开的是那封邮件的页面）。比猜 hwnd 可靠，也不会点出空壳窗。
                            if let Some(act) =
                                minicard_core::notify_launch::for_notification(id)
                            {
                                if act.kind
                                    == minicard_core::notify_launch::ActivationKind::Protocol
                                {
                                    // 日志只记 app / 方式，URI（含邮箱与 mailid）不落盘
                                    minicard_shell::mlog!("通知打开：{app} → toast 协议激活");
                                    match crate::appwin::launch(&act.launch, "") {
                                        Ok(()) => return,
                                        Err(e) => minicard_shell::mlog!(
                                            "通知协议激活失败（回落窗口激活）：{e}"
                                        ),
                                    }
                                }
                            }
                            // ② 再按应用名/AUMID 切回**已在运行**的窗口（托盘隐藏也能拉起来）；
                            //    找到就绝不 ShellExecute，避免点通知又开第二个 QQ。
                            if crate::appwin::activate_app(&app, &aumid) {
                                // 日志只记 app / 路径，不含通知正文
                                minicard_shell::mlog!("通知打开：{app} → 激活已有窗口");
                                return;
                            }
                            let target = format!("shell:AppsFolder\\{aumid}");
                            if !aumid.trim().is_empty() {
                                minicard_shell::mlog!("通知打开：{app} → ShellExecute 启动");
                                if let Err(e) = crate::appwin::launch(&target, "") {
                                    minicard_shell::mlog!("通知打开发送方失败: {e}");
                                }
                            }
                        });
                    }
                    notify_card::Hit::Remove { id } => {
                        // 本地先退场（180ms 塌缩 + suppress 压住快照），Windows 删除是异步的
                        motion.mark_removed(id, Instant::now());
                        notify_service().remove(id);
                    }
                    notify_card::Hit::ClearAll => {
                        let armed = clear_armed
                            .is_some_and(|t| t.elapsed() < Duration::from_secs(3));
                        if armed {
                            // 立即本地退场：所有行同一帧进入 180ms 塌缩 + suppress（不等
                            // Windows 异步删除 / 下一次快照收敛的 ≤1.5s，见 AC1）
                            motion.mark_cleared(&snap.items, Instant::now());
                            // Windows 没有可靠的 ClearNotifications（0x80070490）→ 逐条 Remove
                            let svc = notify_service();
                            for it in &snap.items {
                                svc.remove(it.id);
                            }
                            *clear_armed = None;
                            motion.reset_scroll();
                            minicard_shell::mlog!("通知清除：{} 条", snap.items.len());
                        } else {
                            // 第一次点击先挂确认，3 秒内再点才真清；
                            // 150ms 交叉淡入由 paint 按 opts.clear_armed 同步给 Motion
                            *clear_armed = Some(Instant::now());
                            minicard_shell::mlog!("通知清除：等待确认");
                        }
                    }
                    notify_card::Hit::Authorize => {
                        std::thread::spawn(|| {
                            if let Err(e) =
                                crate::appwin::launch("ms-settings:privacy-notifications", "")
                            {
                                minicard_shell::mlog!("打开通知设置失败: {e}");
                            }
                        });
                    }
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
    // 拖到另一块屏：动画节拍跟着那块屏的刷新率走
    retime_card(win);
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
    let Some(gfx) = win.gfx.as_ref() else { return };
    match gfx.bitmap_from_bgra(&cap.bgra, cap.w, cap.h) {
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

/// 把窗口左上角夹进工作区（物理 px）。显示器被拔掉 / 改主屏后窗口可能整个落在屏外。
fn clamp_pos(pos: (i32, i32), size: (i32, i32), work: crate::guide::R4) -> (i32, i32) {
    let max_x = (work.2 - size.0).max(work.0);
    let max_y = (work.3 - size.1).max(work.1);
    (pos.0.clamp(work.0, max_x), pos.1.clamp(work.1, max_y))
}

/// 显示器增删 / DPI 变化后的保底处理（0.1.3）：
/// 逐卡按「当前窗口 DPI」重设物理尺寸 → 重建 Gfx（swapchain 跟 DPI 走）→ 重切玻璃 →
/// 位置夹回可见工作区；Dock 按 config 重建一次。逐像素重排留给后续版本，这里只保证不跑丢/不错位。
fn handle_display_changed(
    wins: &mut [CardWin],
    dock_win: Option<&mut DockWin>,
    cfg: &Config,
    wall: &Option<glass::Wall>,
    blur: f32,
) {
    // 每个窗口都会收到一次 WM_DPICHANGED / WM_DISPLAYCHANGE → 合并成一次处理
    static LAST: Mutex<Option<Instant>> = Mutex::new(None);
    {
        let mut g = LAST.lock().unwrap();
        let now = Instant::now();
        if let Some(t) = *g {
            if now.duration_since(t) < Duration::from_millis(300) {
                return;
            }
        }
        *g = Some(now);
    }
    for win in wins.iter_mut() {
        let new_dpi = win_dpi(win.hwnd);
        if new_dpi > 0.0 && (new_dpi - win.dpi).abs() > 0.01 {
            win.dpi = new_dpi;
            let s = new_dpi / 96.0;
            let (w_px, h_px) = ((win.w * s).round() as i32, (win.h * s).round() as i32);
            // 换尺寸必须先断开旧合成（同一 HWND 再 CreateTargetForHwnd 会报 WINDOW_ALREADY_COMPOSED）
            if let Some(old) = win.gfx.take() {
                old.detach();
            }
            unsafe {
                let _ = SetWindowPos(
                    win.hwnd,
                    None,
                    0,
                    0,
                    w_px,
                    h_px,
                    SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
            match Gfx::new_scaled(
                win.hwnd,
                w_px as u32,
                h_px as u32,
                new_dpi,
                radius_of(win.kind, win.w, win.h),
                ss_factor(),
            ) {
                Ok(g) => win.gfx = Some(g),
                Err(e) => minicard_shell::mlog!("DPI 变化后重建 Gfx 失败: {e:?}"),
            }
            minicard_shell::mlog!("卡 {:?} DPI → {new_dpi}，窗口 {w_px}×{h_px}", win.kind);
        }
        if let Some(work) = work_area(win.hwnd) {
            let mut r = RECT::default();
            if unsafe { GetWindowRect(win.hwnd, &mut r) }.is_ok() {
                let (w, h) = (r.right - r.left, r.bottom - r.top);
                let (nx, ny) = clamp_pos((r.left, r.top), (w, h), work);
                if nx != r.left || ny != r.top {
                    unsafe {
                        let _ = SetWindowPos(
                            win.hwnd,
                            None,
                            nx,
                            ny,
                            0,
                            0,
                            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                        );
                    }
                    minicard_shell::mlog!("卡 {:?} 跑出工作区 → 夹回 ({nx},{ny})", win.kind);
                }
            }
        }
        reslice(win, wall, blur);
        // 显示器增删/改刷新率/跨屏之后，这张卡的节拍要按新屏重探
        retime_card(win);
        win.next_draw = Instant::now();
    }
    if let Some(dw) = dock_win {
        let new_dpi = win_dpi(dw.hwnd);
        if new_dpi > 0.0 {
            dw.dpi = new_dpi;
        }
        if let Err(e) = dock_rebuild(dw, cfg, wall, blur) {
            minicard_shell::mlog!("显示器变化后 Dock 重建失败: {e:?}");
        }
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
    // 统一走 icon::ensure：缓存里是很小的旧 PNG（32 档）时会自动重提一次
    crate::icon::ensure(&it.path)
}

/// 条目增减（右键「从 Dock 移除」/改分隔线）后重建 Dock 窗：
/// 尺寸变了 → SetWindowPos + 换一份 Gfx（swapchain 跟着尺寸）+ 重切玻璃 + 重载图标。
fn dock_rebuild(
    dw: &mut DockWin,
    cfg: &Config,
    wall: &Option<glass::Wall>,
    blur: f32,
) -> Result<()> {
    use minicard_core::config::Dock;
    let d = cfg.dock.clone();
    let dk = minicard_cards::dock::Tokens::dock(&d);
    let s = dw.dpi / 96.0;
    let w_px = (dk.w * s).round() as i32;
    let h_px = (dk.h * s).round() as i32;
    let x = (d.cx - dk.w / 2.0).round() as i32;
    let y = (d.y + Dock::MARGIN - dk.top).round() as i32;
    unsafe {
        let _ = SetWindowPos(
            dw.hwnd,
            None,
            x,
            y,
            w_px,
            h_px,
            SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
    dw.t = dk;
    crate::drop::uninstall(dw.hwnd);
    if let Some(old) = &dw.gfx {
        old.detach();
    }
    dw.gfx = None; // 先把旧 Gfx 丢干净，否则 CreateTargetForHwnd 会报 WINDOW_ALREADY_COMPOSED
    dw.gfx = Some(Gfx::new_scaled(
        dw.hwnd,
        w_px as u32,
        h_px as u32,
        dw.dpi,
        minicard_cards::dock::RADIUS,
        ss_factor(),
    )?);
    // 玻璃：与建窗时同一口径
    let pad_px = ((blur * 3.0).max(60.0) * dw.dpi / 96.0).round() as i32;
    let mon = monitor_of(x, y);
    dw.glass = match wall {
        Some(w) => {
            let cap = glass::from_wallpaper(
                w,
                (mon.2, mon.3),
                (x as f32 - mon.0, y as f32 - mon.1),
                (w_px as u32, h_px as u32),
                pad_px as f32,
                blur * dw.dpi / 96.0,
            );
            match dw.gfx.as_ref().unwrap().bitmap_from_bgra(&cap.bgra, cap.w, cap.h) {
                Ok(b) => {
                    let pad_dip = pad_px as f32 * 96.0 / dw.dpi;
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
    // 图标重载（下标全变了）
    dw.icons.clear();
    for it in &d.items {
        let bmp = dock_icon_path(it)
            .and_then(|p| minicard_draw::image::decode_file(&p).ok())
            .and_then(|img| {
                let small = minicard_draw::image::downscale(&img, 96);
                minicard_draw::image::bitmap(dw.gfx.as_ref().unwrap(), &small).ok()
            });
        dw.icons.push(bmp);
    }
    dw.origin = (x, y);
    *DOCK_ORIGIN.lock().unwrap() = Some((x, y, dw.dpi));
    // Dock 可能换了屏/换了刷新率：交互动画节拍跟着重探（没变就静默）
    retime_dock(dw);
    crate::drop::install(dw.hwnd, d.clone(), (x, y), dw.dpi);
    dw.running = vec![false; d.items.len()];
    dw.drag_from = None;
    dw.settle_t0 = None;
    dw.bubble_a = 0.0;
    dw.last_sig = 0;
    dw.region_open = false;
    dock_set_hit(&dw.t, false);
    *DOCK_SNAP.lock().unwrap() = None;
    minicard_shell::mlog!(
        "dock 重建：{} 项，窗口 {}×{}",
        d.items.len(),
        w_px,
        h_px
    );
    Ok(())
}

/// 菜单锚点：图标中心（空白区就用指针 x）+ 条顶往上 6px，全是屏幕像素。
fn dock_anchor(
    tk: &minicard_cards::dock::Tokens,
    geo: &minicard_cards::dock::Geo,
    hit: Option<usize>,
    fallback_dip: f32,
) -> crate::menu::Anchor {
    let (ox, oy, dpi) = DOCK_ORIGIN
        .lock()
        .ok()
        .and_then(|g| *g)
        .unwrap_or((0, 0, 96.0));
    let s = if dpi > 0.0 { dpi / 96.0 } else { 1.0 };
    // 有命中就对齐图标中心；空白区就对齐指针 x（再让 place() 做屏幕夹紧）
    let cx_dip = hit.map(|i| geo.cx[i]).unwrap_or(fallback_dip);
    crate::menu::Anchor {
        cx: ox + (cx_dip * s).round() as i32,
        bottom: oy + (tk.top * s).round() as i32 - (6.0 * s).round() as i32,
    }
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
        WM_NCHITTEST => {
            if hwnd.0 as isize != DOCK_HWND.load(Ordering::Relaxed) {
                return LRESULT(HTCLIENT as isize);
            }
            {
                let (l, t, r, b, _) = *DOCK_HIT.lock().unwrap();
                // lp = 屏幕像素坐标（低 16 位 x、高 16 位 y，带符号）
                let x = (lp.0 & 0xFFFF) as i16 as f32;
                let y = ((lp.0 >> 16) & 0xFFFF) as i16 as f32;
                let mut wr = RECT::default();
                let _ = GetWindowRect(hwnd, &mut wr);
                let dpi = GetDpiForWindow(hwnd) as f32;
                let s = if dpi > 0.0 { dpi / 96.0 } else { 1.0 };
                let cx = (x - wr.left as f32) / s;
                let cy = (y - wr.top as f32) / s;
                if cx < l || cx > r || cy < t || cy > b {
                    // 条以外的窗口区域：既不可见也不该吃点击
                    return LRESULT(HTTRANSPARENT as isize);
                }
                return LRESULT(HTCLIENT as isize);
            }
        }
        WM_MOUSEWHEEL => {
            let delta = ((wp.0 >> 16) & 0xFFFF) as i16 as i32;
            let _ = EVENTS.lock().unwrap().push(UiEvent::Wheel {
                hwnd: hwnd.0 as isize,
                delta,
            });
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            // Dock 窗不拖整条（D3 再做），按下直接吞掉，抬手时才判断点了哪个图标
            if hwnd.0 as isize == DOCK_HWND.load(Ordering::Relaxed) {
                let mut pt = POINT::default();
                let _ = GetCursorPos(&mut pt);
                let mut r = RECT::default();
                let _ = GetWindowRect(hwnd, &mut r);
                let dpi = GetDpiForWindow(hwnd) as f32;
                let s = if dpi > 0.0 { dpi / 96.0 } else { 1.0 };
                let x = (pt.x - r.left) as f32 / s;
                let y = (pt.y - r.top) as f32 / s;
                let _ = SetCapture(hwnd);
                *DOCK_PRESS.lock().unwrap() = dock_hit_at(x, y).map(|i| (i, x, y));
                return LRESULT(0);
            }
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
                        // 固定位置的卡：算一次「拖过」但不移动，松手不当点击，
                        // 避免在卡上按住划一下误触待办/番茄钟按钮。
                        if is_pinned(hwnd) {
                            d.moved = true;
                            return LRESULT(0);
                        }
                        d.moved = true;
                    }
                    if d.moved && !is_pinned(hwnd) {
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
            if hwnd.0 as isize == DOCK_HWND.load(Ordering::Relaxed) {
                let _ = ReleaseCapture();
                let mut pt = POINT::default();
                let _ = GetCursorPos(&mut pt);
                let mut r = RECT::default();
                let _ = GetWindowRect(hwnd, &mut r);
                let dpi = GetDpiForWindow(hwnd) as f32;
                let s = if dpi > 0.0 { dpi / 96.0 } else { 1.0 };
                let x = (pt.x - r.left) as f32 / s;
                let y = (pt.y - r.top) as f32 / s;
                // 按下时命中的是某个图标、且横向挪动超过 4px → 拖拽排序；否则算点击
                let moved = DOCK_PRESS
                    .lock()
                    .unwrap()
                    .map(|(_, px, py)| ((x - px).abs() + (y - py).abs()) > 4.0)
                    .unwrap_or(false);
                if moved {
                    let _ = EVENTS.lock().unwrap().push(UiEvent::DockDrop { x });
                } else {
                    let _ = EVENTS.lock().unwrap().push(UiEvent::DockClick { x, y });
                }
                *DOCK_PRESS.lock().unwrap() = None;
                return LRESULT(0);
            }
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
        WM_RBUTTONUP => {
            if hwnd.0 as isize == DOCK_HWND.load(Ordering::Relaxed) {
                let mut pt = POINT::default();
                let _ = GetCursorPos(&mut pt);
                let mut r = RECT::default();
                let _ = GetWindowRect(hwnd, &mut r);
                let dpi = GetDpiForWindow(hwnd) as f32;
                let s = if dpi > 0.0 { dpi / 96.0 } else { 1.0 };
                let _ = EVENTS.lock().unwrap().push(UiEvent::DockMenu {
                    x: (pt.x - r.left) as f32 / s,
                    y: (pt.y - r.top) as f32 / s,
                    sx: pt.x,
                    sy: pt.y,
                });
                return LRESULT(0);
            }
            return LRESULT(0);
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
        // 显示器增删 / 每显示器 DPI 变化：只入队，真正的重采样在主循环做（能借用 wins/cfg）
        WM_DPICHANGED | WM_DISPLAYCHANGE => {
            let _ = EVENTS.lock().unwrap().push(UiEvent::DisplayChanged);
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        WM_KEYDOWN if wp.0 == 0x1B => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        // 验收钩子：外部脚本 PostMessage(card, MC_CLICK_MSG, 0x4D43, MAKELPARAM(x,y)) 注入一次
        // 客户区像素点击（游戏全屏时 SetCursorPos 会被 ClipCursor 吃掉，只能走这条）。
        // 入队的是与 WM_LBUTTONUP 完全相同的 UiEvent::Click，走同一套 hit/click 代码。
        // wParam 必须为 'MC'（0x4D43），防其他进程误触/误投。
        m if m == debug_click_msg() => {
            if wp.0 == 0x4D43 {
                let x = (lp.0 & 0xFFFF) as u16 as i16 as i32;
                let y = ((lp.0 >> 16) & 0xFFFF) as u16 as i16 as i32;
                let _ = EVENTS.lock().unwrap().push(UiEvent::Click {
                    hwnd: hwnd.0 as isize,
                    x,
                    y,
                });
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

/// 验收钩子用的窗口消息（RegisterWindowMessageW；名字全系统唯一）
fn debug_click_msg() -> u32 {
    static MSG: OnceLock<u32> = OnceLock::new();
    *MSG.get_or_init(|| unsafe { RegisterWindowMessageW(w!("MiniCardDebugClick")) })
}

/// 诊断用：把卡型映射到固定下标（`st_kind_draws` 的分布列）。
fn kind_index(k: CardKind) -> usize {
    match k {
        CardKind::Performance => 0,
        CardKind::Disks => 1,
        CardKind::Clock => 2,
        CardKind::Weather => 3,
        CardKind::NetRate => 4,
        CardKind::Pomodoro => 5,
        CardKind::Music => 6,
        CardKind::Calendar => 7,
        CardKind::Todo => 8,
        CardKind::Notify => 9,
    }
}

/// 这张卡是不是**整块**被别的窗口盖住了（5 点采样：中心 + 四角内缩）。
///
/// 桌面层的卡被普通窗口盖住时，画出来的像素用户一行也看不到 —— 继续按 60fps 重绘
/// 纯属白烧 CPU 和 GPU。这里只做「看不看得见」的粗判：5 个点全都落在别的窗口上
/// 才算盖住；有一个点还是自己 → 当作可见（宁可多画，不要漏画）。
fn card_fully_covered(hwnd: HWND, w: f32, h: f32) -> bool {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::Graphics::Gdi::ClientToScreen;
    use windows::Win32::UI::WindowsAndMessaging::{IsChild, WindowFromPoint};
    let w = w.round() as i32;
    let h = h.round() as i32;
    if w <= 2 || h <= 2 {
        return false;
    }
    let inset = 6.min(w / 4).max(1);
    let inset_y = 6.min(h / 4).max(1);
    let pts = [
        (w / 2, h / 2),
        (inset, inset_y),
        (w - inset, inset_y),
        (inset, h - inset_y),
        (w - inset, h - inset_y),
    ];
    for (x, y) in pts {
        let mut p = POINT { x, y };
        if !unsafe { ClientToScreen(hwnd, &mut p) }.as_bool() {
            return false;
        }
        let under = unsafe { WindowFromPoint(p) };
        if under == hwnd || unsafe { IsChild(hwnd, under) }.as_bool() {
            return false;
        }
    }
    true
}

/// 动画重绘间隔（显示同步后）：短动效每个显示帧一步，连续补间按整数帧抽帧。
///
/// 短动效（待办勾选/FLIP、时钟翻位、日历翻月）只有 200~420ms，60fps 在 200Hz 屏上会明显跳，
/// 所以按显示器刷新率每帧走；连续补间（性能/网络条、音乐播放、番茄钟进度等）是常驻的，
/// 按 `max(1, floor(hz/60))` 个显示帧抽一帧，保证 ≥60fps 又不让常驻卡白烧 CPU。
/// 静置仍用卡片自己的 `refresh`（数据刷新节拍）。
fn anim_step(
    kind: CardKind,
    animating: bool,
    refresh: Duration,
    timing: &DisplayTiming,
) -> Duration {
    if !animating {
        return refresh;
    }
    match kind {
        // 时钟：每秒翻一次位，是**周期性**短动效 —— 200Hz 屏上每帧一步等于每秒白画
        // ~40 帧。收到 60fps 足够（一次翻位 200~420ms，仍有 12~25 帧）。
        CardKind::Clock => timing.frame_step().max(Duration::from_nanos(16_666_666)),
        // 通知卡的到达/删除/hover/滚动/清除/状态淡入都是短交互（120~180ms）：
        // 与翻月/勾选同一档，每显示帧一步（B 案 2026-11-09）
        CardKind::Todo | CardKind::Calendar | CardKind::Notify => timing.frame_step(),
        _ => timing.ambient_step(),
    }
}

/// Dock 交互动画的一步：默认每个显示帧；`MC_DOCK_HZ` 只给台架覆盖（30..=1000）。
fn dock_step_of(timing: &DisplayTiming) -> Duration {
    static OVERRIDE: OnceLock<Option<u64>> = OnceLock::new();
    let hz = OVERRIDE.get_or_init(|| {
        std::env::var("MC_DOCK_HZ")
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
    });
    match *hz {
        Some(hz) => Duration::from_micros(1_000_000 / hz.clamp(30, 1000)),
        None => timing.frame_step(),
    }
}

/// 启动日志去重：同一档刷新率只打一行（多屏不同档才各打一行）。
/// 行文格式见 `DisplayTiming::summary`，复核脚本按 `显示同步：` 开头抓。
fn log_timing_once(t: &DisplayTiming, seen: &mut Vec<(u32, &'static str)>) {
    let key = (t.hz, t.source.as_str());
    if seen.contains(&key) {
        return;
    }
    seen.push(key);
    minicard_shell::mlog!("{}", t.summary());
}

/// 卡片跨屏 / 显示器变化后重探刷新率；只有真的变了才打日志（避免拖动时刷屏）。
fn retime_card(win: &mut CardWin) {
    let t = crate::display::detect(win.hwnd);
    if t.hz != win.timing.hz || t.source != win.timing.source {
        win.timing = t;
        minicard_shell::mlog!("卡 {:?}：{}", win.kind, t.summary());
    }
}

/// Dock 跨屏 / 显示器变化后重探刷新率。
fn retime_dock(dw: &mut DockWin) {
    let t = crate::display::detect(dw.hwnd);
    if t.hz != dw.timing.hz || t.source != dw.timing.source {
        dw.timing = t;
        minicard_shell::mlog!("Dock：{}", t.summary());
    }
}

/// `MC_FPS_LOG=1` 的按卡动画帧间隔采样。
///
/// 只在动画连续绘制期间采样：静置那一拍之后的第一帧、或间隔 >500ms 的断档不计入；
/// 攒够 60 帧打一次 p50/p95/max + 期望帧长（台架验证「200Hz → 5ms 档」用）。
/// 阈值不能太高：300~420ms 的短动效在 200Hz 只有 64~84 帧，按 120 帧永远打不出来。
struct FpsProbe {
    last: Option<Instant>,
    dts: Vec<f32>,
    expected: Duration,
    skipped: u32,
}

impl FpsProbe {
    fn enabled() -> bool {
        std::env::var_os("MC_FPS_LOG").is_some()
    }

    fn new() -> Self {
        Self { last: None, dts: Vec::new(), expected: Duration::from_millis(16), skipped: 0 }
    }

    /// 动画停下（或这一拍是静置帧）：结束当前采样窗口，下一段动画重新起算
    fn reset(&mut self) {
        self.last = None;
    }

    fn note(&mut self, now: Instant, expected: Duration, skipped: u32) {
        self.expected = expected;
        if let Some(last) = self.last {
            let dt = now.duration_since(last).as_secs_f32() * 1000.0;
            // >500ms = 中间是静置节拍（不是动画帧），不混进统计
            if dt > 0.0 && dt < 500.0 {
                self.dts.push(dt);
                self.skipped += skipped;
            }
        }
        self.last = Some(now);
    }

    fn log_if_due(&mut self, kind: CardKind, hz: u32) {
        if self.dts.len() < 60 {
            return;
        }
        let mut dts = std::mem::take(&mut self.dts);
        let skipped = std::mem::take(&mut self.skipped);
        dts.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let p = |q: f32| dts[((dts.len() - 1) as f32 * q) as usize];
        let expected_ms = self.expected.as_secs_f32() * 1000.0;
        minicard_shell::mlog!(
            "卡 {kind:?} 动画帧间隔：{} 帧 p50 {:.2} / p95 {:.2} / max {:.2} ms（期望 {:.2} ms ≈ {:.1} fps，{hz}Hz 屏，跳帧 {}）",
            dts.len(),
            p(0.50),
            p(0.95),
            dts[dts.len() - 1],
            expected_ms,
            if expected_ms > 0.0 { 1000.0 / expected_ms } else { 0.0 },
            skipped
        );
    }
}

/// 台架钩子：`MC_CAL_DEMO_MS=800` → 日历卡每 800ms 自翻一页（默认关；只用于测短动效帧率）。
fn cal_demo_period() -> Option<Duration> {
    static V: OnceLock<Option<Duration>> = OnceLock::new();
    *V.get_or_init(|| {
        std::env::var("MC_CAL_DEMO_MS")
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .filter(|ms| *ms >= 50)
            .map(Duration::from_millis)
    })
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
        .with_theme(dark_theme)
        .with_accent(cfg.settings.accent, dark_theme);
    minicard_shell::mlog!(
        "live：强调色 rgb{:?} / 玻璃 {}/100 / 模糊 {}px / 主题 {:?}",
        cfg.settings.accent, cfg.settings.glass_alpha, cfg.settings.glass_blur, cfg.settings.theme
    );
    run(cfg, te, pal, blur, topmost, exit_after)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 0.1.3：显示器增删 / DPI 变化后，窗口位置必须夹回可见工作区（不跑丢）
    #[test]
    fn 显示器变化后窗口夹回工作区() {
        use crate::guide::R4;
        let work: R4 = (0, 0, 1920, 1040);
        // 完全在右侧屏外 → 贴右缘
        assert_eq!(clamp_pos((2500, 300), (240, 120), work), (1680, 300));
        // 负坐标（副屏被拔掉）→ 贴左/上缘
        assert_eq!(clamp_pos((-500, -200), (240, 120), work), (0, 0));
        // 正常位置不动
        assert_eq!(clamp_pos((100, 100), (240, 120), work), (100, 100));
        // 比工作区还大的窗口：对齐左上（不产生小于 min 的值）
        assert_eq!(clamp_pos((500, 500), (3000, 2000), work), (0, 0));
    }

    /// 0.1.3 番茄钟持久化：run 态按绝对结束时刻补偿；paused 保剩余；end 已过 = 完成
    #[test]
    fn pomo_restore_from_config() {
        let now = Instant::now();
        let running = config::PomoState {
            mode: "run".into(),
            dur: 25,
            remain_ms: 25.0 * 60000.0,
            end_unix_ms: Some(unix_now_ms() + 120_000),
        };
        let p = pomo_from_state(Some(&running), now);
        assert_eq!(p.mode, pomo::PomoMode::Run);
        assert!((p.remain(now) - 120_000.0).abs() < 1_500.0, "remain = {}", p.remain(now));
        assert_eq!(p.mmss(now), [0, 2, 0, 0], "恢复后应立刻是 02:00 那一档，不是默认 25:00");

        let paused = config::PomoState {
            mode: "paused".into(),
            dur: 30,
            remain_ms: 42_000.0,
            end_unix_ms: None,
        };
        let p2 = pomo_from_state(Some(&paused), now);
        assert_eq!(p2.mode, pomo::PomoMode::Paused);
        assert_eq!(p2.total_ms(), 30.0 * 60000.0);
        assert!((p2.remain(now) - 42_000.0).abs() < 1.0);

        // end 已过：仍记 run，但有效模式是「本轮完成」
        let expired = config::PomoState {
            mode: "run".into(),
            dur: 25,
            remain_ms: 0.0,
            end_unix_ms: Some(unix_now_ms() - 1_000),
        };
        let p3 = pomo_from_state(Some(&expired), now);
        assert_eq!(p3.eff_mode(now), pomo::PomoMode::Done);

        // 没有持久化字段 = 全新状态
        let fresh = pomo_from_state(None, now);
        assert_eq!(fresh.mode, pomo::PomoMode::Set);
        assert_eq!(fresh.dur, 25);
    }

    #[test]
    fn short_animations_run_every_display_frame_continuous_ones_are_decimated() {
        let idle = Duration::from_millis(1000);
        let t200 = DisplayTiming::new(200, crate::display::TimingSource::Dwm);
        let t60 = DisplayTiming::new(60, crate::display::TimingSource::Dwm);
        // 200Hz 屏：短动效每显示帧 5ms
        assert_eq!(
            anim_step(CardKind::Todo, true, idle, &t200),
            Duration::from_micros(5_000)
        );
        // 时钟是周期性翻位：200Hz 屏也收到 60fps 上限（别每秒白画 40 帧）
        assert_eq!(
            anim_step(CardKind::Clock, true, idle, &t200),
            Duration::from_nanos(16_666_666)
        );
        assert_eq!(
            anim_step(CardKind::Calendar, true, idle, &t200),
            Duration::from_micros(5_000)
        );
        // 通知卡短交互（到达/删除/hover/滚动/清除/状态）也走每显示帧（B 案 2026-11-09）
        assert_eq!(
            anim_step(CardKind::Notify, true, idle, &t200),
            Duration::from_micros(5_000)
        );
        // 连续补间 3 帧抽 1 → 15ms
        assert_eq!(
            anim_step(CardKind::Performance, true, idle, &t200),
            Duration::from_micros(15_000)
        );
        assert_eq!(
            anim_step(CardKind::Music, true, idle, &t200),
            Duration::from_micros(15_000)
        );
        // 60Hz 屏：短动效 16.6667ms；补间不抽帧也是 16.6667ms
        assert_eq!(
            anim_step(CardKind::Todo, true, idle, &t60),
            Duration::from_nanos(16_666_666)
        );
        // 时钟在 60Hz 屏上本来就只有 60fps，不再额外降档
        assert_eq!(
            anim_step(CardKind::Clock, true, idle, &t60),
            Duration::from_nanos(16_666_666)
        );
        assert_eq!(
            anim_step(CardKind::Performance, true, idle, &t60),
            Duration::from_nanos(16_666_666)
        );
        // 144Hz 屏：短动效 6.9444ms；补间 2 帧抽 1 → 13.8889ms
        let t144 = DisplayTiming::new(144, crate::display::TimingSource::Dwm);
        assert_eq!(
            anim_step(CardKind::Calendar, true, idle, &t144),
            Duration::from_nanos(6_944_444)
        );
        assert_eq!(
            anim_step(CardKind::NetRate, true, idle, &t144),
            Duration::from_nanos(13_888_888)
        );
    }

    #[test]
    fn idle_cards_use_their_refresh_interval() {
        let refresh = Duration::from_millis(1000);
        let t200 = DisplayTiming::new(200, crate::display::TimingSource::Dwm);
        assert_eq!(anim_step(CardKind::Todo, false, refresh, &t200), refresh);
        assert_eq!(anim_step(CardKind::Performance, false, refresh, &t200), refresh);
    }

    /// MC_DOCK_HZ 覆盖只影响 Dock；默认跟显示器走（200Hz → 5ms）
    #[test]
    fn dock_step_follows_display_unless_overridden() {
        let t200 = DisplayTiming::new(200, crate::display::TimingSource::Dwm);
        // 测试进程里没设 MC_DOCK_HZ → 走显示帧
        if std::env::var("MC_DOCK_HZ").is_err() {
            assert_eq!(dock_step_of(&t200), Duration::from_micros(5_000));
            let t60 = DisplayTiming::new(60, crate::display::TimingSource::Fallback);
            assert_eq!(dock_step_of(&t60), Duration::from_nanos(16_666_666));
        }
    }

    /// 三态判据与浏览器同一条（card.html::computeState）：
    /// 没采到 → 空态；3.5s 没新值 → 异常；其余正常。
    #[test]
    fn data_health_matches_browser_thresholds() {
        assert_eq!(DataHealth::of(0, 1_000_000), DataHealth::Empty, "at=0 = 还没采到");
        assert_eq!(DataHealth::of(1_000, 1_000), DataHealth::Fresh);
        assert_eq!(DataHealth::of(1_000, 1_000 + 3_500), DataHealth::Fresh, "整 3500ms 还算新鲜");
        assert_eq!(DataHealth::of(1_000, 1_001 + 3_500), DataHealth::Stale);
        // 系统时钟回拨（at 在未来）：不能误报异常
        assert_eq!(DataHealth::of(2_000, 1_000), DataHealth::Fresh);
        // 文案就是浏览器那两句
        assert_eq!(DataHealth::Empty.text(), Some("等待数据…"));
        assert_eq!(DataHealth::Stale.text(), Some("数据异常"));
        assert_eq!(DataHealth::Fresh.text(), None);
    }

    /// 磁盘卡高度按盘数派生（浏览器/外壳同口径），且只在「数据新鲜 + 不是拖动中 + 真的变了」时动手。
    #[test]
    fn disk_card_height_follows_disk_count() {
        // 1 盘 60 / 2–3 盘 120 / 4–5 盘 180
        // 起点故意用「另一个档」，否则目标 == 当前高度时会按幂等规则返回 None
        for (n, cur, want) in [
            (1usize, 180.0f32, 60.0f32),
            (2, 60.0, 120.0),
            (3, 180.0, 120.0),
            (4, 120.0, 180.0),
            (5, 60.0, 180.0),
        ] {
            assert_eq!(
                disk_height_for(CardKind::Disks, cur, n, DataHealth::Fresh, false),
                Some(want),
                "{n} 块盘应给 {want}px"
            );
            assert_eq!(
                config::disk_height_px(n),
                want,
                "{n} 块盘的高度必须与 config::disk_height_px 同源"
            );
        }
        // 采样还没到（0 块盘只是「未知」）：保持默认档 180，别先缩成空态
        assert_eq!(config::disk_height_px(0), 180.0);
        assert_eq!(
            disk_height_for(CardKind::Disks, 180.0, 0, DataHealth::Fresh, false),
            None,
            "默认档本身就是 180，不该动"
        );
        // 已经是目标高度：不动（否则每秒 SetWindowPos 会让窗口抖）
        assert_eq!(
            disk_height_for(CardKind::Disks, 120.0, 3, DataHealth::Fresh, false),
            None
        );
        // 拖动中：不动（改尺寸会跟磁吸打架）
        assert_eq!(
            disk_height_for(CardKind::Disks, 180.0, 1, DataHealth::Fresh, true),
            None
        );
        // 采样没到 / 断了：保持当前高度
        assert_eq!(
            disk_height_for(CardKind::Disks, 180.0, 1, DataHealth::Empty, false),
            None
        );
        assert_eq!(
            disk_height_for(CardKind::Disks, 180.0, 1, DataHealth::Stale, false),
            None
        );
        // 其它卡尺寸由 config 定死，绝不跟着盘数变
        assert_eq!(
            disk_height_for(CardKind::Performance, 120.0, 5, DataHealth::Fresh, false),
            None
        );
    }

    /// 空态/异常态那行字的盒子照 `card.html` 的 `.empty`：
    /// 字号 0.78125rem、居中盒 = 各卡自己的 padding box（`.card-*` 原文）。
    #[test]
    fn status_tokens_match_browser_empty_box() {
        let cases = [
            // (卡, 宽, 高, 圆角, 字号, 左右内边距, 上下内边距)
            (CardKind::Performance, 240.0, 120.0, 10.0, 7.8125, 10.0, 4.375),
            (CardKind::Disks, 240.0, 60.0, 15.0, 11.71875, 11.25, 7.5),
            (CardKind::NetRate, 120.0, 120.0, 16.0, 12.5, 8.0, 0.0),
        ];
        for (kind, w, h, radius, fs, pad_lr, pad_tb) in cases {
            let t = status_tokens(kind, w, h);
            assert!((t.radius - radius).abs() < 1e-4, "{kind:?} 圆角 {:.4}", t.radius);
            assert!((t.fs - fs).abs() < 1e-4, "{kind:?} 字号 {:.4}", t.fs);
            assert!((t.pad_lr - pad_lr).abs() < 1e-4, "{kind:?} 左右 {:.4}", t.pad_lr);
            assert!((t.pad_tb - pad_tb).abs() < 1e-4, "{kind:?} 上下 {:.4}", t.pad_tb);
            // body { line-height:1.55 }
            assert!((t.line_h - fs * 1.55).abs() < 1e-4);
            // 圆角必须与正常态那张卡自己的一致，否则空态会露出硬边
            assert!((t.radius - radius_of(kind, w, h)).abs() < 1e-4);
        }
    }
}

/// 真窗口回归：纯函数测不出的两件事在这里验 ——
/// ① 磁盘卡换高度真的改了窗口物理尺寸、并换了一份能画的 Gfx；
/// ② 空态/异常态那张「整卡一行灰字」真的画出来了（灰度/居中都能查）。
///
/// 没有桌面 / D3D 时整条跳过（`eprintln!` 说明原因），不让 CI/无头环境红。
#[cfg(test)]
mod gfx_tests {
    use super::*;
    use std::sync::Once;

    static REG: Once = Once::new();

    fn hidden_window() -> Option<HWND> {
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(
                None,
                windows::Win32::System::Com::COINIT_APARTMENTTHREADED,
            );
        }
        let hinst = unsafe { GetModuleHandleW(None).ok()? };
        let cls = w!("MiniCardLiveTest");
        REG.call_once(|| unsafe {
            let wc = WNDCLASSW {
                lpfnWndProc: Some(wndproc),
                hInstance: hinst.into(),
                lpszClassName: cls,
                hbrBackground: HBRUSH::default(),
                ..Default::default()
            };
            RegisterClassW(&wc);
        });
        // 摆在所有显示器外面，别打扰正在用的桌面
        unsafe {
            CreateWindowExW(
                WS_EX_NOREDIRECTIONBITMAP | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
                cls,
                w!("mc-live-test"),
                WS_POPUP,
                -4000,
                -4000,
                240,
                180,
                None,
                None,
                Some(hinst.into()),
                None,
            )
            .ok()
        }
    }

    fn test_win(hwnd: HWND, kind: CardKind, w: f32, h: f32) -> Option<CardWin> {
        let dpi = {
            let d = unsafe { GetDpiForWindow(hwnd) };
            if d == 0 {
                96
            } else {
                d
            }
        } as f32;
        let w_px = (w * dpi / 96.0).round() as u32;
        let h_px = (h * dpi / 96.0).round() as u32;
        let mut gfx = Gfx::new_scaled(hwnd, w_px, h_px, dpi, radius_of(kind, w, h), ss_factor()).ok()?;
        gfx.text_dx = 0.0;
        let state = match kind {
            CardKind::Weather => State::Weather { seen_at: i64::MIN },
            _ => State::Disks { warn90: true },
        };
        Some(CardWin {
            hwnd,
            gfx: Some(gfx),
            kind,
            w,
            h,
            state,
            glass: None,
            next_draw: Instant::now(),
            refresh: Duration::from_millis(5000),
            timing: DisplayTiming::new(60, crate::display::TimingSource::Fallback),
            fps: None,
            cfg_idx: 0,
            dpi,
            slice_origin: (0, 0),
            glass_off: (0.0, 0.0),
            cover_at: Instant::now(),
            covered: false,
        })
    }

    fn test_engine() -> Option<TextEngine> {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../assets/fonts");
        let files: Vec<std::path::PathBuf> = ["MiSans-Regular.ttf", "MiSans-Medium.ttf"]
            .iter()
            .map(|n| dir.join(n))
            .filter(|p| p.exists())
            .collect();
        if files.is_empty() {
            return None;
        }
        let refs: Vec<&std::path::Path> = files.iter().map(|p| p.as_path()).collect();
        TextEngine::new_weights(&refs, "MiSans").ok()
    }

    /// 磁盘卡：1 / 3 / 5 盘 → 窗口 60 / 120 / 180（逻辑点），且每次都重造了 Gfx。
    #[test]
    fn 磁盘卡换高度真的改窗口尺寸并重建绘图() {
        let Some(hwnd) = hidden_window() else {
            eprintln!("跳过：建不了窗口");
            return;
        };
        let Some(mut win) = test_win(hwnd, CardKind::Disks, 240.0, 180.0) else {
            eprintln!("跳过：D3D/DComp 不可用");
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            return;
        };
        for (n, want) in [(1usize, 60.0f32), (3, 120.0), (5, 180.0)] {
            let target = disk_height_for(win.kind, win.h, n, DataHealth::Fresh, false)
                .unwrap_or_else(|| panic!("{n} 盘应该换高度"));
            assert_eq!(target, want, "{n} 盘的目标高度");
            set_card_height(&mut win, target, &None, 30.0).expect("换高度");
            let mut r = RECT::default();
            unsafe { GetWindowRect(hwnd, &mut r) }.expect("GetWindowRect");
            let got = (r.bottom - r.top) as f32 * 96.0 / win.dpi;
            eprintln!(
                "{n} 盘 → 窗口 {}×{} 物理 px = {got:.1} 逻辑点（目标 {want:.0}）",
                r.right - r.left,
                r.bottom - r.top
            );
            assert!(
                (got - want).abs() <= 1.0,
                "{n} 盘：窗口 {got} 逻辑点，应为 {want}"
            );
            assert!((win.h - want).abs() < 0.5, "win.h 应同步成 {want}");
            assert!(win.gfx.is_some(), "换完高度必须还有一份能画的 Gfx");
        }
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
    }

    /// 空态 / 异常态：整卡一行字，且**居中**（取字墨迹包围盒的中心）。
    /// dump 出来的 BGRA 留在 `MC_EVIDIR`（默认临时目录），便于人眼复核。
    #[test]
    fn 空态与异常态各画出一行居中的字() {
        let Some(hwnd) = hidden_window() else {
            eprintln!("跳过：建不了窗口");
            return;
        };
        let Some(mut win) = test_win(hwnd, CardKind::Disks, 240.0, 60.0) else {
            eprintln!("跳过：D3D/DComp 不可用");
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            return;
        };
        let Some(te) = test_engine() else {
            eprintln!("跳过：没有字体");
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            return;
        };
        let pal = Palette::new(30.0, false);
        let dir = std::env::var("MC_EVIDIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| std::env::temp_dir());
        let tk = status_tokens(win.kind, win.w, win.h);
        for (txt, name) in [("等待数据…", "empty"), ("数据异常", "error")] {
            let path = dir.join(format!("live-status-{name}.bgra"));
            win.gfx.as_mut().unwrap().dump = Some(path.clone());
            paint_status(
                win.gfx.as_ref().unwrap(),
                &te,
                &pal,
                &tk,
                win.w,
                win.h,
                txt,
                None,
            )
            .expect("画空态");
            win.gfx.as_mut().unwrap().dump = None;

            let pix = std::fs::read(&path).expect("dump 文件");
            let dims = std::fs::read_to_string(path.with_extension("txt")).expect("dump 尺寸");
            let (w, h) = {
                let mut it = dims.split('x').filter_map(|v| v.parse::<usize>().ok());
                (it.next().expect("w"), it.next().expect("h"))
            };
            assert_eq!(pix.len(), w * h * 4, "{name}: 读回的像素数与尺寸不符");

            // ① 卡片铺满（白纱 alpha 很低但不是 0）：四个角之外都该有像素
            let a = |x: usize, y: usize| pix[(y * w + x) * 4 + 3];
            assert!(a(w / 2, h / 2) > 0 || a(w / 2, 4) > 0, "{name}: 卡片没画出来");

            // ② 中间那一带必须有「字」（muted 是不透明的，白纱只有 ~9% alpha）
            let mut ink = 0usize;
            let (mut x0, mut x1, mut y0, mut y1) = (usize::MAX, 0usize, usize::MAX, 0usize);
            for yy in (h / 4)..(h * 3 / 4) {
                for xx in 0..w {
                    if a(xx, yy) > 150 {
                        ink += 1;
                        x0 = x0.min(xx);
                        x1 = x1.max(xx);
                        y0 = y0.min(yy);
                        y1 = y1.max(yy);
                    }
                }
            }
            assert!(ink > 40, "{name}: 中间带只有 {ink} 个字墨迹像素");
            let cx = (x0 + x1) as f32 / 2.0;
            let cy = (y0 + y1) as f32 / 2.0;
            assert!(
                (cx - w as f32 / 2.0).abs() <= 4.0,
                "{name}: 字没水平居中（墨迹中心 {cx:.1}，卡片中心 {:.1}）",
                w as f32 / 2.0
            );
            assert!(
                (cy - h as f32 / 2.0).abs() <= 4.0,
                "{name}: 字没垂直居中（墨迹中心 {cy:.1}，卡片中心 {:.1}）",
                h as f32 / 2.0
            );
            eprintln!("{name}: {w}×{h}, 墨迹 {ink}px 中心 ({cx:.1},{cy:.1}) → {}", path.display());
        }
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
    }

    /// 天气刷新线程只写共享值，重绘得靠这条链路：draw_one 记下画的是哪一版 `at`，
    /// 主循环发现 `at` 变了就把 `next_draw` 提到当下（不然最多等 60s 的 refresh 节拍）。
    #[test]
    fn 天气换版后记下新的_at() {
        let Some(hwnd) = hidden_window() else {
            eprintln!("跳过：建不了窗口");
            return;
        };
        let Some(mut win) = test_win(hwnd, CardKind::Weather, 240.0, 180.0) else {
            eprintln!("跳过：D3D/DComp 不可用");
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            return;
        };
        let Some(te) = test_engine() else {
            eprintln!("跳过：没有字体");
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            return;
        };
        win.state = State::Weather { seen_at: 100 };
        let wsnap = wx::WeatherSnap {
            at: 200,
            city: "北京".into(),
            temp: 21.0,
            code: 0,
            humidity: 40,
            air: "优".into(),
            ..Default::default()
        };
        let ws: wx::SharedWeather = Arc::new(std::sync::RwLock::new(Some(wsnap)));
        let snap = data::spawn_sampler();
        let pal = Palette::new(30.0, false);
        draw_one(
            &mut win,
            &snap,
            &ws,
            &te,
            &pal,
            DataHealth::Fresh,
            Instant::now(),
            (2026, 11, 4, 10, 30),
            "2026年11月4日 周三",
            "",
        )
        .expect("画天气卡");
        match win.state {
            State::Weather { seen_at } => assert_eq!(seen_at, 200, "应记下画上去的那一版 at"),
            _ => panic!("状态变了"),
        }
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
    }

    /// D5：只有「总开关开 + 至少一张启用中的通知卡」才允许轮询通知。
    #[test]
    fn notify_wanted_requires_switch_and_enabled_card() {
        let mut cfg = Config::default();
        cfg.cards.clear();
        cfg.settings.notify_card = true;
        assert!(!notify_wanted(&cfg), "没有通知卡时不应轮询");

        let mut card = config::CardInstance::default();
        card.kind = CardKind::Notify;
        card.enabled = true;
        cfg.cards.push(card);
        assert!(notify_wanted(&cfg));

        cfg.cards[0].enabled = false;
        assert!(!notify_wanted(&cfg), "卡片在桌面关掉时不应轮询");
        cfg.cards[0].enabled = true;
        cfg.settings.notify_card = false;
        assert!(!notify_wanted(&cfg), "总开关关掉时不应轮询");
    }
}
