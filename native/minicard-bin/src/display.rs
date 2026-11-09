//! 显示同步：运行时探测每张卡所在显示器的真实刷新率，并把「动画节拍」变成纯函数。
//!
//! 背景：卡片窗口走 `WS_EX_NOREDIRECTIONBITMAP` + DComp，`Present(1)` **不等 vblank**，
//! 所以帧率完全由自己按 deadline 控制。以前短动效写死 5ms（假定 200Hz 屏）、连续补间写死
//! 16.667ms（假定 60Hz 屏），换台 60Hz/144Hz 机器就一头快一头慢。
//!
//! 检测顺序（`detect`）：
//! 1. `MC_REFRESH_HZ` 环境变量覆盖（钳 24..=1000，台架/复核用）；
//! 2. `DwmGetCompositionTimingInfo(hwnd).rateRefresh`（U32 比，最接近真实 vblank）；
//! 3. 同一结构里的 `qpcRefreshPeriod`（QPC 10MHz 口径，rateRefresh 不可用时兜底）；
//! 3b. 卡片窗是 `WS_EX_NOREDIRECTIONBITMAP` + DComp 直接合成，不在 DWM 重定向层里，
//!     `hwind` 版实测恒 E_INVALIDARG；主屏上再试一次桌面整体时序
//!     （`DwmGetCompositionTimingInfo(NULL)`，DWM 只有一个合成时钟、跟主屏走）；
//! 4. `MonitorFromWindow` + `GetMonitorInfoW(szDevice)` + `EnumDisplaySettingsW` 的
//!    `dmDisplayFrequency`（逐屏，多屏混合刷新率靠这条）；
//! 5. 60Hz 兜底。
//!
//! 节拍口径（`DisplayTiming`）：
//! - 短动效（待办勾选/FLIP、时钟翻位、日历翻月）→ **每个显示帧**一步；
//! - 连续补间（性能/网络条、音乐、番茄钟进度等）→ 显示帧整数抽帧，
//!   `ambient_frames = max(1, floor(hz / 60))`，保证不低于 60fps：
//!   60→1、90→1、120→2、144→2、165→2、200→3、240→4；
//! - 静置仍走卡片自己的 `refresh_ms`（数据刷新节拍，与刷新率无关）。
//!
//! 所有周期用整数纳秒（1e9/hz），配合 `advance_deadline` 的绝对整步推进就不会有
//! 浮点累积误差；落后超过一步时直接跳到下一个整帧（跳帧而非追帧）。

use std::time::{Duration, Instant};

use windows::Win32::Foundation::HWND;

/// 允许的刷新率下限：低于它视为检测失败 / 环境变量非法
pub const MIN_HZ: u32 = 24;
/// 允许的刷新率上限：高于它视为检测失败 / 环境变量非法
pub const MAX_HZ: u32 = 1000;
/// 什么都探测不到时的兜底刷新率
pub const FALLBACK_HZ: u32 = 60;
/// 连续补间的目标下限（抽帧后 ne 不低于这个值）
pub const AMBIENT_FPS: u32 = 60;
/// QPC 频率按 10 MHz 算：Windows 上 QPC 的名义频率就是 10MHz，这里只给
/// `qpcRefreshPeriod` 兜底用，不值得为它引入 `Win32_System_Performance` feature。
const QPC_HZ: u64 = 10_000_000;

/// 刷新率的来源（启动日志里要能看出「200 Hz 是怎么来的」，复核脚本也认这个）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimingSource {
    /// `MC_REFRESH_HZ` 环境变量（台架覆盖）
    Env,
    /// DWM 合成时序的 `rateRefresh`
    Dwm,
    /// DWM 合成时序的 `qpcRefreshPeriod`
    Qpc,
    /// `EnumDisplaySettingsW` 的 `dmDisplayFrequency`
    EnumDisplay,
    /// 全都拿不到 → 60Hz
    Fallback,
}

impl TimingSource {
    pub fn as_str(self) -> &'static str {
        match self {
            TimingSource::Env => "MC_REFRESH_HZ",
            TimingSource::Dwm => "dwm",
            TimingSource::Qpc => "dwm-qpc",
            TimingSource::EnumDisplay => "enum",
            TimingSource::Fallback => "兜底",
        }
    }
}

/// 一个「显示节拍」：刷新率 + 整数纳秒周期 + 来源。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DisplayTiming {
    /// 刷新率（已钳到 24..=1000）
    pub hz: u32,
    /// 一个显示帧的周期（整数纳秒，1e9/hz 截断）
    pub period: Duration,
    pub source: TimingSource,
}

impl DisplayTiming {
    /// 由刷新率构造；hz 会自动钳到 `MIN_HZ..=MAX_HZ`。
    pub fn new(hz: u32, source: TimingSource) -> Self {
        let hz = hz.clamp(MIN_HZ, MAX_HZ);
        Self { hz, period: period_of(hz), source }
    }

    /// 兜底档（60Hz）
    pub fn fallback() -> Self {
        Self::new(FALLBACK_HZ, TimingSource::Fallback)
    }

    /// 短动效一步 = 一个显示帧
    pub fn frame_step(&self) -> Duration {
        self.period
    }

    /// 连续补间抽帧数：`max(1, floor(hz/60))`
    pub fn ambient_frames(&self) -> u32 {
        ambient_frames_of(self.hz)
    }

    /// 连续补间一步 = 整数个显示帧
    pub fn ambient_step(&self) -> Duration {
        self.period * self.ambient_frames()
    }

    /// 连续补间实际 fps（200Hz → 66.67）。生产路径只打 `ambient_ms`，这里留给单测/复核。
    #[cfg(test)]
    pub fn ambient_fps(&self) -> f64 {
        self.hz as f64 / self.ambient_frames() as f64
    }

    /// 帧长（毫秒，日志口径）
    pub fn frame_ms(&self) -> f64 {
        self.period.as_secs_f64() * 1000.0
    }

    /// 连续补间步长（毫秒，日志口径）
    pub fn ambient_ms(&self) -> f64 {
        self.ambient_step().as_secs_f64() * 1000.0
    }

    /// 启动日志（任务书要求逐字可见的那一行）：
    /// `显示同步：200 Hz（dwm）→ 帧 5.00ms / 连续补间 15.00ms（3 帧）`
    pub fn summary(&self) -> String {
        format!(
            "显示同步：{} Hz（{}）→ 帧 {:.2}ms / 连续补间 {:.2}ms（{} 帧）",
            self.hz,
            self.source.as_str(),
            self.frame_ms(),
            self.ambient_ms(),
            self.ambient_frames()
        )
    }
}

/// 一个显示帧的周期：整数纳秒，44.1 之类的非整数刷新率也能落到纳秒。
fn period_of(hz: u32) -> Duration {
    debug_assert!((MIN_HZ..=MAX_HZ).contains(&hz));
    Duration::from_nanos(1_000_000_000 / hz as u64)
}

/// 连续补间的抽帧数：`max(1, floor(hz / 60))`。
///
/// 取 floor 而不是 round：宁可补间快一点（82.5fps @165Hz）也不掉到 60fps 以下。
pub fn ambient_frames_of(hz: u32) -> u32 {
    (hz / AMBIENT_FPS).max(1)
}

/// 解析 `MC_REFRESH_HZ`：非法（空/非数字/≤0/NaN）→ `None`（当没设，继续走系统探测）；
/// 合法则四舍五入并钳到 24..=1000。
pub fn parse_refresh_override(raw: Option<&str>) -> Option<u32> {
    let s = raw?.trim();
    if s.is_empty() {
        return None;
    }
    let hz: f64 = s.parse().ok()?;
    if !hz.is_finite() || hz <= 0.0 {
        return None;
    }
    Some((hz.round() as u32).clamp(MIN_HZ, MAX_HZ))
}

/// 绝对节拍：从上一拍的截止时刻按**整步**推进，返回 `(下一拍截止, 跳过的整帧槽数)`。
///
/// 不变式：返回的下一拍是 `prev + k*step` 中**严格大于 `now`** 的最小值（k ≥ 1）。
///
/// - 没落后（`prev + step > now`）：下一拍就是 `prev + step`，**不是** `now + step` ——
///   每一拍都落在同一条整数网格上，长期不会累积漂移；
/// - 已过 `prev + step` 不到一整步：这次绘制吃掉迟到量，下一拍仍对齐到网格上的下一帧；
/// - 已过 `prev + step` 一整步以上：说明欠了整帧，直接跳到 `now` 之后的第一个整帧
///   （跳帧而不是连补一串过期帧，避免动画「追帧」越追越卡）。
///
/// `prev` 远在未来（例如刚从 1s 静置节拍切到 5ms 动画节拍）时按 `now` 重新对齐。
pub fn advance_deadline(prev: Instant, now: Instant, step: Duration) -> (Instant, u32) {
    let step = if step.is_zero() { Duration::from_nanos(1) } else { step };
    // 防御：截止时刻远在未来（不该在「画完」之后出现）→ 以当下为基准重新对齐
    let prev = if prev > now + step { now } else { prev };
    let mut next = prev + step;
    if next > now {
        return (next, 0);
    }
    let lag = now.duration_since(next).as_nanos();
    let skipped = (lag / step.as_nanos()) as u32;
    next += step * (skipped + 1);
    (next, skipped)
}

/// 探测某个窗口所在显示器的刷新率。
///
/// 必须在窗口建好（有 HWND）之后调用；窗口跨屏移动 / 显示器增删 / DPI 变化后要重新探测。
pub fn detect(hwnd: HWND) -> DisplayTiming {
    // 1) 环境变量覆盖（台架/复核；生产不设）
    if let Some(hz) = parse_refresh_override(std::env::var("MC_REFRESH_HZ").ok().as_deref()) {
        return DisplayTiming::new(hz, TimingSource::Env);
    }
    // 2) DWM 合成时序（按窗口）
    if let Some((hz, source)) = dwm_hz(hwnd) {
        return DisplayTiming::new(hz, source);
    }
    // 2b) 卡片窗是 `WS_EX_NOREDIRECTIONBITMAP` + DComp 直接合成、不在 DWM 的重定向层里，
    //     实测 `DwmGetCompositionTimingInfo(hwnd)` 恒回 E_INVALIDARG（0x80070057）；
    //     桌面的合成时序（hwnd = NULL）仍然有效，所以主屏上再试一次。
    //     但 DWM 只有**一个**合成时钟（跟主屏的刷新率走），混合刷新率多屏时副屏必须问
    //     EnumDisplaySettingsW，否则副屏会被按主屏的 Hz 跑 —— 因此这里只在主屏上用。
    if is_on_primary_monitor(hwnd) {
        if let Some((hz, source)) = dwm_hz(HWND::default()) {
            return DisplayTiming::new(hz, source);
        }
    }
    // 3) 显示模式（逐屏，多屏混合刷新率靠这条）
    if let Some(hz) = enum_hz(hwnd) {
        return DisplayTiming::new(hz, TimingSource::EnumDisplay);
    }
    // 4) 兜底
    DisplayTiming::fallback()
}

/// 窗口所在显示器是不是主屏（主屏左上角恒为虚拟屏原点 0,0）。
fn is_on_primary_monitor(hwnd: HWND) -> bool {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::Graphics::Gdi::{
        MonitorFromPoint, MonitorFromWindow, MONITOR_DEFAULTTOPRIMARY, MONITOR_DEFAULTTONEAREST,
    };
    unsafe {
        let mon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        if mon.is_invalid() {
            return false;
        }
        mon == MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY)
    }
}

/// DWM 合成时序：优先 `rateRefresh`（U32 比），退化用 `qpcRefreshPeriod`。
/// `hwnd` 传窗口句柄取该窗的时序；传空句柄（`HWND::default()`）取桌面整体的时序。
fn dwm_hz(hwnd: HWND) -> Option<(u32, TimingSource)> {
    use windows::Win32::Graphics::Dwm::{DwmGetCompositionTimingInfo, DWM_TIMING_INFO};

    let mut info = DWM_TIMING_INFO {
        cbSize: std::mem::size_of::<DWM_TIMING_INFO>() as u32,
        ..Default::default()
    };
    unsafe { DwmGetCompositionTimingInfo(hwnd, &mut info) }.ok()?;

    let num = info.rateRefresh.uiNumerator;
    let den = info.rateRefresh.uiDenominator;
    if num > 0 && den > 0 {
        if let Some(hz) = sane_hz_f64(num as f64 / den as f64) {
            return Some((hz, TimingSource::Dwm));
        }
    }
    // qpcRefreshPeriod 单位是 QPC tick（QPC 名义 10MHz），不是 100ns
    if info.qpcRefreshPeriod > 0 {
        if let Some(hz) = sane_hz_f64(QPC_HZ as f64 / info.qpcRefreshPeriod as f64) {
            return Some((hz, TimingSource::Qpc));
        }
    }
    None
}

/// 显示模式刷新率：`MonitorFromWindow` → `GetMonitorInfoW(szDevice)` → `EnumDisplaySettingsW`。
fn enum_hz(hwnd: HWND) -> Option<u32> {
    use windows::Win32::Graphics::Gdi::{
        EnumDisplaySettingsW, GetMonitorInfoW, MonitorFromWindow, DEVMODEW, ENUM_CURRENT_SETTINGS,
        MONITORINFO, MONITORINFOEXW, MONITOR_DEFAULTTONEAREST,
    };
    use windows::core::PCWSTR;

    unsafe {
        let mon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        if mon.is_invalid() {
            return None;
        }
        // MONITORINFOEXW 的前半段就是 MONITORINFO（cbSize 必须按 EX 结构报）
        let mut mi = MONITORINFOEXW::default();
        mi.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        if !GetMonitorInfoW(mon, &mut mi as *mut MONITORINFOEXW as *mut MONITORINFO).as_bool() {
            return None;
        }
        let mut dm = DEVMODEW::default();
        dm.dmSize = std::mem::size_of::<DEVMODEW>() as u16;
        if !EnumDisplaySettingsW(PCWSTR(mi.szDevice.as_ptr()), ENUM_CURRENT_SETTINGS, &mut dm)
            .as_bool()
        {
            return None;
        }
        // dmDisplayFrequency = 0 表示「硬件默认」，当没拿到
        sane_hz_f64(dm.dmDisplayFrequency as f64)
    }
}

/// 合法刷新率：四舍五入后落在 24..=1000；否则 `None`（换下一种探测方式）
fn sane_hz_f64(hz: f64) -> Option<u32> {
    if !hz.is_finite() {
        return None;
    }
    let hz = hz.round();
    if (MIN_HZ as f64..=MAX_HZ as f64).contains(&hz) {
        Some(hz as u32)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(t: Duration) -> f64 {
        t.as_secs_f64() * 1000.0
    }

    /// 60/90/120/144/165/200/240 的帧长、抽帧数与连续补间步长
    #[test]
    fn periods_and_ambient_frames_for_common_refresh_rates() {
        // (hz, 帧长 ms, 抽帧, 连续补间步长 ms)
        let cases = [
            (60u32, 16.666_666, 1u32, 16.666_666),
            (90, 11.111_111, 1, 11.111_111),
            (120, 8.333_333, 2, 16.666_666),
            (144, 6.944_444, 2, 13.888_888),
            (165, 6.060_606, 2, 12.121_212),
            (200, 5.0, 3, 15.0),
            (240, 4.166_666, 4, 16.666_664),
        ];
        for (hz, frame_ms, frames, ambient_ms) in cases {
            let t = DisplayTiming::new(hz, TimingSource::Dwm);
            assert_eq!(t.hz, hz);
            assert!(
                (ms(t.frame_step()) - frame_ms).abs() < 1e-6,
                "{hz}Hz 帧长 {:.6}ms ≠ {frame_ms}ms",
                ms(t.frame_step())
            );
            assert_eq!(t.ambient_frames(), frames, "{hz}Hz 抽帧数");
            assert!(
                (ms(t.ambient_step()) - ambient_ms).abs() < 1e-6,
                "{hz}Hz 连续补间 {:.6}ms ≠ {ambient_ms}ms",
                ms(t.ambient_step())
            );
            // 60Hz 及以上的屏，抽帧后补间 fps 不能掉到 60 以下（24–59Hz 的屏物理上做不到）
            if hz >= AMBIENT_FPS {
                assert!(t.ambient_fps() >= 60.0 - 1e-9, "{hz}Hz 补间 fps 不应低于 60");
            }
        }
        // 200Hz：短动效 5.00ms 每帧，补间 3 帧抽 1 → 15.00ms ≈ 66.7fps
        let t200 = DisplayTiming::new(200, TimingSource::Dwm);
        assert_eq!(t200.frame_step(), Duration::from_micros(5_000));
        assert_eq!(t200.ambient_step(), Duration::from_micros(15_000));
        assert!((t200.ambient_fps() - 200.0 / 3.0).abs() < 1e-9);
    }

    /// 极端值：抽帧至少 1 帧；hz 钳到 24..=1000
    #[test]
    fn hz_is_clamped_and_ambient_frames_at_least_one() {
        for hz in [MIN_HZ, 30, 59, 60, 61, 119, 1000] {
            assert!(ambient_frames_of(hz) >= 1, "{hz}Hz 抽帧不能是 0");
            let t = DisplayTiming::new(hz, TimingSource::EnumDisplay);
            let frames = t.ambient_frames();
            assert!(
                t.ambient_step() >= t.frame_step() * frames,
                "{hz}Hz 补间步长必须是整帧的倍数"
            );
            if hz >= AMBIENT_FPS {
                assert!(t.ambient_fps() >= 60.0 - 1e-9, "{hz}Hz 补间 fps < 60");
            }
        }
        // 低于 24 钳到 24；高于 1000 钳到 1000；0 也不会除零
        assert_eq!(DisplayTiming::new(1, TimingSource::Fallback).hz, MIN_HZ);
        assert_eq!(DisplayTiming::new(0, TimingSource::Fallback).hz, MIN_HZ);
        assert_eq!(DisplayTiming::new(5000, TimingSource::Fallback).hz, MAX_HZ);
        assert_eq!(DisplayTiming::new(MAX_HZ, TimingSource::Fallback).period, Duration::from_millis(1));
    }

    /// `MC_REFRESH_HZ` 解析：非法当没设；合法四舍五入并钳制
    #[test]
    fn refresh_env_override_parsing_and_clamping() {
        assert_eq!(parse_refresh_override(Some("200")), Some(200));
        assert_eq!(parse_refresh_override(Some(" 144 ")), Some(144));
        assert_eq!(parse_refresh_override(Some("59.94")), Some(60));
        assert_eq!(parse_refresh_override(Some("144.4")), Some(144));
        // 钳制
        assert_eq!(parse_refresh_override(Some("1")), Some(MIN_HZ));
        assert_eq!(parse_refresh_override(Some("5")), Some(MIN_HZ));
        assert_eq!(parse_refresh_override(Some("23.9")), Some(MIN_HZ));
        assert_eq!(parse_refresh_override(Some("5000")), Some(MAX_HZ));
        // 非法：当没设（继续走系统探测）
        assert_eq!(parse_refresh_override(None), None);
        assert_eq!(parse_refresh_override(Some("")), None);
        assert_eq!(parse_refresh_override(Some("   ")), None);
        assert_eq!(parse_refresh_override(Some("abc")), None);
        assert_eq!(parse_refresh_override(Some("0")), None);
        assert_eq!(parse_refresh_override(Some("-60")), None);
        assert_eq!(parse_refresh_override(Some("NaN")), None);
        assert_eq!(parse_refresh_override(Some("inf")), None);
    }

    /// 绝对节拍：正常推进 / 迟到不足一步 / 落后整帧跳帧 / 长期不漂移
    #[test]
    fn advance_deadline_keeps_integer_grid_and_skips_late_frames() {
        let base = Instant::now();
        let step = Duration::from_micros(5_000); // 200Hz

        // 正好画在截止时刻：下一拍 = prev + step
        assert_eq!(advance_deadline(base, base, step), (base + step, 0));
        // 提前（理论上不该发生）也按 prev + step
        assert_eq!(advance_deadline(base, base - Duration::from_millis(1), step), (base + step, 0));
        // 迟到 2ms（不足一整步）：这次绘制吃掉迟到量，下一拍仍落在整数网格 base+5
        assert_eq!(
            advance_deadline(base, base + Duration::from_millis(2), step),
            (base + step, 0)
        );
        // 画在 base+7（已错过 base+5 这一拍，但只迟到 2ms < 5ms）：下一拍对齐 base+10
        let (next, skipped) = advance_deadline(base, base + Duration::from_millis(7), step);
        assert_eq!((next, skipped), (base + Duration::from_millis(10), 0));
        assert!(next > base + Duration::from_millis(7), "下一拍必须落在当下之后");
        // 迟到 7ms（相对 base+5 已落后超过一整步）：跳帧，对齐到 base+15
        let (next, skipped) = advance_deadline(base, base + Duration::from_millis(12), step);
        assert_eq!(next, base + Duration::from_millis(15));
        assert_eq!(skipped, 1, "应记录跳过的整帧数");
        assert!(next > base + Duration::from_millis(12), "下一拍必须落在当下之后");
        // 迟到整整一倍：跳过 1 帧（base+5），画在 base+10 这拍，下一拍 base+15
        let (next, skipped) = advance_deadline(base, base + Duration::from_millis(10), step);
        assert_eq!((next, skipped), (base + Duration::from_millis(15), 1));
        // 落后 10s：一跳到底，不补一串过期帧
        let (next, skipped) = advance_deadline(base, base + Duration::from_secs(10), step);
        assert_eq!(skipped, 1999);
        assert_eq!(next, base + Duration::from_micros(5_000 * 2001));

        // 连续 1000 拍、每拍都迟到 0.3ms：截止时刻仍严格落在 5ms 整数网格上
        let mut prev = base;
        let late = Duration::from_micros(300);
        for i in 0..1000u32 {
            let now = prev + late;
            let (next, skipped) = advance_deadline(prev, now, step);
            assert_eq!(skipped, 0, "每拍只迟到 0.3ms，不该跳帧");
            assert_eq!(next, base + step * (i + 1), "第 {i} 拍后应无漂移");
            prev = next;
        }
    }

    /// 从静置（1s）切到动画（5ms）时，未来很远的截止时刻按当下重新对齐
    #[test]
    fn advance_deadline_resyncs_when_prev_is_far_future() {
        let base = Instant::now();
        let step = Duration::from_micros(5_000);
        let (next, skipped) =
            advance_deadline(base + Duration::from_secs(1), base, step);
        assert_eq!(next, base + step);
        assert_eq!(skipped, 0);
    }

    /// 启动日志逐字要求：200Hz(dwm) → 帧 5.00ms / 连续补间 15.00ms（3 帧）
    #[test]
    fn startup_log_line_is_exact() {
        let t = DisplayTiming::new(200, TimingSource::Dwm);
        assert_eq!(
            t.summary(),
            "显示同步：200 Hz（dwm）→ 帧 5.00ms / 连续补间 15.00ms（3 帧）"
        );
        let t60 = DisplayTiming::new(60, TimingSource::Env);
        assert_eq!(
            t60.summary(),
            "显示同步：60 Hz（MC_REFRESH_HZ）→ 帧 16.67ms / 连续补间 16.67ms（1 帧）"
        );
        let t144 = DisplayTiming::new(144, TimingSource::EnumDisplay);
        assert_eq!(
            t144.summary(),
            "显示同步：144 Hz（enum）→ 帧 6.94ms / 连续补间 13.89ms（2 帧）"
        );
        assert_eq!(DisplayTiming::fallback().hz, FALLBACK_HZ);
    }
}
