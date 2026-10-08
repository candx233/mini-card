//! 显示器几何：多屏 / 分辨率 / 缩放变化相关的坐标运算。
//!
//! 2026-10-08 巡检结论：**卡片渲染本身与分辨率无关**（窗口是固定逻辑尺寸、卡面 rem 只看
//! CSS px；已实证六卡在 DSF 1.0/1.25/1.5/2.0 下布局逐元素一致）。所有「换分辨率/换屏
//! 就出问题」的场景都出在这里：
//!   · 磨砂切片按哪台显示器裁（原来是主屏口径）；
//!   · 卡片/主窗是否跑出了所有显示器；
//!   · 跨显示器拖动时坐标怎么换算。
//! 所以把「显示器是谁、工作区在哪、窗口相对显示器在哪」集中到这个模块 ——
//! 纯函数部分（visible_enough / clamp_into）可单测，Win32 只在下半区碰。

use windows_sys::Win32::Foundation::{HWND, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, MonitorFromRect, MonitorFromWindow, HDC, HMONITOR,
    MONITORINFO, MONITOR_DEFAULTTONEAREST,
};

/// 矩形 (left, top, right, bottom)，屏幕物理 px。
/// 进程是 Per-Monitor-V2（tao `become_dpi_aware`），所以 Win32 量到的全是物理值。
pub type R4 = (i32, i32, i32, i32);

fn r4_of(r: &RECT) -> R4 {
    (r.left, r.top, r.right, r.bottom)
}

fn rect_of(r: R4) -> RECT {
    RECT { left: r.0, top: r.1, right: r.2, bottom: r.3 }
}

/* ────────────────────────── 纯几何（可单测） ────────────────────────── */

/// rect 至少要在**某一台**显示器上露出 min × min 才算「看得见」。
/// 口径沿用「定位」按钮原始的 32px 可抓取宽度：卡几乎整张在屏外（只剩一条缝）就判不可见。
pub fn visible_enough(rect: R4, monitors: &[R4], min: i32) -> bool {
    monitors.iter().any(|m| {
        let w = rect.2.min(m.2) - rect.0.max(m.0);
        let h = rect.3.min(m.3) - rect.1.max(m.1);
        w >= min && h >= min
    })
}

/// 把 rect 夹回工作区：保留尽量多的原位置（不硬拽到某个角），四边留 margin。
/// 返回目标左上角。
pub fn clamp_into(rect: R4, work: R4, margin: i32) -> (i32, i32) {
    let w = rect.2 - rect.0;
    let h = rect.3 - rect.1;
    let lo_x = work.0 + margin;
    let hi_x = (work.2 - w - margin).max(lo_x);
    let lo_y = work.1 + margin;
    let hi_y = (work.3 - h - margin).max(lo_y);
    (rect.0.clamp(lo_x, hi_x), rect.1.clamp(lo_y, hi_y))
}

/* ────────────────────────── Win32 封装 ────────────────────────── */

/// 所有显示器：(rcMonitor, rcWork)，物理 px
pub fn monitors() -> Vec<(R4, R4)> {
    unsafe extern "system" fn cb(hmon: HMONITOR, _hdc: HDC, _clip: *mut RECT, data: isize) -> i32 {
        let out = &mut *(data as *mut Vec<(R4, R4)>);
        if let Some(pair) = info(hmon) {
            out.push(pair);
        }
        1
    }
    let mut out: Vec<(R4, R4)> = Vec::new();
    unsafe {
        EnumDisplayMonitors(
            std::ptr::null_mut(),
            std::ptr::null(),
            Some(cb),
            &mut out as *mut Vec<(R4, R4)> as isize,
        );
    }
    out
}

/// 窗口所在显示器：(句柄原始值, rcMonitor)。
/// 句柄值给「换屏检测」当身份比较用（磨砂切片按显示器定位，换屏必须重推全量）。
pub fn window_monitor(hwnd: HWND) -> Option<(isize, R4)> {
    let hmon = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    if hmon.is_null() {
        return None;
    }
    info(hmon).map(|(m, _w)| (hmon as isize, m))
}

/// 离 rect 最近的显示器工作区（完全在屏外也返回最近那台）
pub fn nearest_work(rect: R4) -> Option<R4> {
    let rc = rect_of(rect);
    let hmon = unsafe { MonitorFromRect(&rc, MONITOR_DEFAULTTONEAREST) };
    if hmon.is_null() {
        return None;
    }
    info(hmon).map(|(_m, w)| w)
}

fn info(hmon: HMONITOR) -> Option<(R4, R4)> {
    let mut mi: MONITORINFO = unsafe { std::mem::zeroed() };
    mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
    if unsafe { GetMonitorInfoW(hmon, &mut mi) } == 0 {
        return None;
    }
    Some((r4_of(&mi.rcMonitor), r4_of(&mi.rcWork)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 单屏 1920×1080@100% 的常见判据
    #[test]
    fn visible_cases() {
        let mons = [(0, 0, 1920, 1080)];
        // 屏内
        assert!(visible_enough((100, 100, 340, 220), &mons, 32));
        // 完全在右外
        assert!(!visible_enough((2100, 300, 2340, 420), &mons, 32));
        // 贴着右沿露出一条 20px 的缝 → 不算（抓不到）
        assert!(!visible_enough((1900, 300, 2140, 420), &mons, 32));
        // 露出 40px → 算
        assert!(visible_enough((1880, 300, 2120, 420), &mons, 32));
        // 垂直方向同理：底部只露 20px
        assert!(!visible_enough((100, 1060, 340, 1180), &mons, 32));
    }

    /// 双屏：副屏上摆得好好的卡绝不能被判「不可见」（「定位」曾经把副屏卡拽回主屏的根因）
    #[test]
    fn secondary_monitor_card_is_visible() {
        let mons = [(0, 0, 2560, 1440), (2560, 0, 4480, 1080)];
        assert!(visible_enough((2600, 100, 2840, 220), &mons, 32));
        // 副屏以外（两台屏右边很远）→ 不可见
        assert!(!visible_enough((4600, 100, 4840, 220), &mons, 32));
        // 跨缝：卡宽 ≥64 时两侧必有一侧 ≥32 → 永远算可见（缝隙不可能吞掉一张卡）
        assert!(visible_enough((2530, 100, 2650, 220), &mons, 32));
    }

    /// 回收落点：夹回工作区、保留原位置、四边留边距
    #[test]
    fn clamp_keeps_position_inside_work() {
        let work = (0, 0, 1920, 1040); // 下边被任务栏占掉
        // 右侧屏外：x 夹到右沿内 margin；y 保留
        assert_eq!(clamp_into((2100, 300, 2340, 420), work, 8), (1920 - 240 - 8, 300));
        // 底部屏外：y 夹回；x 保留
        assert_eq!(clamp_into((100, 1100, 340, 1220), work, 8), (100, 1040 - 120 - 8));
        // 左上屏外（负坐标，副屏被拔后常见）：夹到 0+margin
        assert_eq!(clamp_into((-500, -300, -260, -180), work, 8), (8, 8));
        // 已在工作区内：原样
        assert_eq!(clamp_into((500, 300, 740, 420), work, 8), (500, 300));
        // 卡片比工作区还大：压到左边距（不产生 hi<lo 的负宽度）
        assert_eq!(clamp_into((100, 100, 2500, 1300), work, 8), (8, 8));
    }
}
