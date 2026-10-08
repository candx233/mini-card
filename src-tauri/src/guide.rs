//! 拖动对齐辅助线 + 磁吸边（2026-09-28 用户改需求后新增）
//!
//! 用户原话：「先不做吸到格点，我想做的是带对齐的那种，就是可以自由动位置，
//! 再加上卡片如果有线对上可以有交互提示那种」。
//!
//! 于是：
//! * 位置**自由**拖动（不再吸附 60px 格点，config 里存的就是落点原值）；
//! * 拖到与别的卡片（或屏幕工作区）的 左/中/右、上/中/下 对齐时：
//!   ① **磁吸**：差 ≤ TOL 就贴齐 —— 在 WM_MOVING 里直接改 RECT，跟手指不打架、零抖动；
//!   ② **辅助线**：一条 1px 蓝线画在本模块的独立窗口上（拖完立刻隐藏）。
//!
//! 为什么单独开一个窗口画线，而不是让被拖的卡片自己画：
//! 卡片窗口只有自己那么大，线画不到卡外 —— 「和谁对齐」这件事看不出来。
//! 这个窗口是「整屏 + 色键透明 + 鼠标穿透 + 永不激活 + TOPMOST」，只在拖动时显示，
//! 所以不碰桌面层锁，也不拦鼠标。

use std::sync::atomic::{AtomicI32, AtomicIsize, Ordering};
use std::sync::Mutex;

use windows_sys::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, CreatePen, CreateSolidBrush, DeleteObject, EndPaint, FillRect, GetDC,
    GetDeviceCaps, GetMonitorInfoW, InvalidateRect, LineTo, MonitorFromRect, MoveToEx, ReleaseDC,
    SelectObject, LOGPIXELSX, MONITORINFO, MONITOR_DEFAULTTONEAREST, PAINTSTRUCT,
    PS_SOLID,
};
use std::sync::OnceLock;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::GetCurrentProcessId;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, EnumWindows, GetClassNameW, GetClientRect, GetSystemMetrics,
    GetWindowRect, GetWindowThreadProcessId, IsIconic, IsWindow, IsWindowVisible, RegisterClassW,
    SetLayeredWindowAttributes, SetWindowPos, ShowWindow, LWA_COLORKEY, SM_CXVIRTUALSCREEN,
    SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER,
    SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SW_HIDE, SW_SHOWNOACTIVATE,
    WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT,
    WS_POPUP,
};

/// 磁吸容差（逻辑 px；按 DPI 换算成物理 px 后使用）
pub const TOL: i32 = 6;

/// 色键：几乎不可能出现在界面上的颜色（COLORREF = BGR）
const KEY: COLORREF = 0x0003_0201;
/// 辅助线颜色 = 强调蓝 #4C9AFF（COLORREF = BGR）
const INK: COLORREF = 0x00FF_9A4C;

/// 矩形元组 (left, top, right, bottom)，屏幕物理 px
pub type R4 = (i32, i32, i32, i32);

static GUIDE: AtomicIsize = AtomicIsize::new(0);
static ORIG_X: AtomicI32 = AtomicI32::new(0);
static ORIG_Y: AtomicI32 = AtomicI32::new(0);
static LINES: Mutex<Vec<[i32; 4]>> = Mutex::new(Vec::new());
/// 落点待吸附的目标矩形（拖动中算出来，WM_EXITSIZEMOVE 时用一次）
static PENDING: Mutex<Option<[i32; 4]>> = Mutex::new(None);
/// 辅助线窗口当前是否可见（避免每条 WM_MOVING 都 ShowWindow）
static GUIDE_SHOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// 是否正在拖动（WM_ENTERSIZEMOVE..WM_EXITSIZEMOVE）。
/// 磁盘卡的自动高度（main.rs::sync_disk_height）在拖动中要跳过：
/// 拖动循环里改窗口尺寸会跟本模块的磁吸 / 落点吸附互相打架。
static MOVING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 拖动是否进行中（给自动高度用）
pub fn is_moving() -> bool {
    MOVING.load(Ordering::Relaxed)
}

/* ────────────────────────── 纯几何（可单测） ────────────────────────── */

/// 在候选目标里挑最近的：返回 (位移, 线所在坐标, 对齐对象)
fn axis_match(own: [i32; 3], targets: &[(i32, Option<R4>)], tol: i32) -> Option<(i32, i32, Option<R4>)> {
    let mut best: Option<(i32, i32, Option<R4>)> = None;
    let mut best_abs = i32::MAX;
    for o in own {
        for (t, partner) in targets {
            let d = t - o;
            if d.abs() <= tol && d.abs() < best_abs {
                best_abs = d.abs();
                best = Some((d, *t, *partner));
            }
        }
    }
    best
}

/// 算出磁吸位移与要画的辅助线（屏幕物理 px；线跨「被拖卡 ∪ 对齐对象」）。
/// `others` = 其它卡片的窗口矩形；`work` = 被拖卡所在显示器的工作区（对齐屏幕边用）。
pub fn align_offset(cur: R4, others: &[R4], work: Option<R4>, tol: i32) -> (i32, i32, Vec<[i32; 4]>) {
    let mut xt: Vec<(i32, Option<R4>)> = Vec::new();
    let mut yt: Vec<(i32, Option<R4>)> = Vec::new();
    for o in others {
        let cx = (o.0 + o.2) / 2;
        let cy = (o.1 + o.3) / 2;
        xt.push((o.0, Some(*o)));
        xt.push((cx, Some(*o)));
        xt.push((o.2, Some(*o)));
        yt.push((o.1, Some(*o)));
        yt.push((cy, Some(*o)));
        yt.push((o.3, Some(*o)));
    }
    if let Some(w) = work {
        xt.push((w.0, None));
        xt.push(((w.0 + w.2) / 2, None));
        xt.push((w.2, None));
        yt.push((w.1, None));
        yt.push(((w.1 + w.3) / 2, None));
        yt.push((w.3, None));
    }

    let own_x = [cur.0, (cur.0 + cur.2) / 2, cur.2];
    let own_y = [cur.1, (cur.1 + cur.3) / 2, cur.3];
    let mut lines: Vec<[i32; 4]> = Vec::new();
    let (mut dx, mut dy) = (0, 0);

    if let Some((d, pos, partner)) = axis_match(own_x, &xt, tol) {
        dx = d;
        let (t, b) = match partner {
            Some(p) => (cur.1.min(p.1), cur.3.max(p.3)),
            None => work.map(|w| (w.1, w.3)).unwrap_or((cur.1, cur.3)),
        };
        lines.push([pos, t, pos, b]);
    }
    if let Some((d, pos, partner)) = axis_match(own_y, &yt, tol) {
        dy = d;
        let (l, r) = match partner {
            Some(p) => (cur.0.min(p.0), cur.2.max(p.2)),
            None => work.map(|w| (w.0, w.2)).unwrap_or((cur.0, cur.2)),
        };
        lines.push([l, pos, r, pos]);
    }
    (dx, dy, lines)
}

/* ────────────────────────── Win32：窗口与绘制 ────────────────────────── */

/// 当前显示器的 DPI 缩放（磁吸容差按逻辑 px 定义）
fn scale_of(hwnd: HWND) -> f64 {
    unsafe {
        let dc = GetDC(hwnd);
        if dc.is_null() {
            return 1.0;
        }
        let dpi = GetDeviceCaps(dc, LOGPIXELSX as i32);
        ReleaseDC(hwnd, dc);
        if dpi <= 0 { 1.0 } else { dpi as f64 / 96.0 }
    }
}

unsafe extern "system" fn guide_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        0x000F => {
            // WM_PAINT：整窗涂色键（= 透明），再画辅助线
            let mut ps: PAINTSTRUCT = std::mem::zeroed();
            let hdc = BeginPaint(hwnd, &mut ps);
            if !hdc.is_null() {
                let mut rc: RECT = std::mem::zeroed();
                GetClientRect(hwnd, &mut rc);
                let bg = CreateSolidBrush(KEY);
                FillRect(hdc, &rc, bg);
                DeleteObject(bg);
                let (ox, oy) = (
                    ORIG_X.load(Ordering::Relaxed),
                    ORIG_Y.load(Ordering::Relaxed),
                );
                let lines = LINES.lock().map(|g| g.clone()).unwrap_or_default();
                for [x1, y1, x2, y2] in lines {
                    let pen = CreatePen(PS_SOLID, 1, INK);
                    let old = SelectObject(hdc, pen);
                    if MoveToEx(hdc, x1 - ox, y1 - oy, std::ptr::null_mut()) != 0 {
                        LineTo(hdc, x2 - ox, y2 - oy);
                    }
                    SelectObject(hdc, old);
                    DeleteObject(pen);
                }
            }
            EndPaint(hwnd, &ps);
            0
        }
        0x0014 => 1, // WM_ERASEBKGND：自己画，不要系统刷
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// 第一次拖动时才建（平时不占任何资源）
fn ensure_guide() -> HWND {
    let h = GUIDE.load(Ordering::Relaxed) as HWND;
    if h as isize != 0 && unsafe { IsWindow(h) } != 0 {
        return h;
    }
    unsafe {
        let cls: Vec<u16> = "MiniCardGuide\0".encode_utf16().collect();
        let hinst = GetModuleHandleW(std::ptr::null());
        let mut wc: WNDCLASSW = std::mem::zeroed();
        wc.lpfnWndProc = Some(guide_proc);
        wc.hInstance = hinst;
        wc.lpszClassName = cls.as_ptr();
        RegisterClassW(&wc); // 已注册会失败，无所谓

        let (x, y) = (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
        );
        let (w, hh) = (
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        );
        let win = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            cls.as_ptr(),
            cls.as_ptr(),
            WS_POPUP,
            x,
            y,
            w,
            hh,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            hinst,
            std::ptr::null(),
        );
        if win.is_null() {
            return win;
        }
        SetLayeredWindowAttributes(win, KEY, 0, LWA_COLORKEY);
        ORIG_X.store(x, Ordering::Relaxed);
        ORIG_Y.store(y, Ordering::Relaxed);
        GUIDE.store(win as isize, Ordering::Relaxed);
        win
    }
}

fn show_guide(visible: bool) {
    let h = ensure_guide();
    if h.is_null() {
        return;
    }
    if GUIDE_SHOWN.load(Ordering::Relaxed) == visible {
        return; // 状态没变就别再 ShowWindow（反复显示会打断拖动循环的跟踪，见 moving() 注释）
    }
    unsafe {
        ShowWindow(h, if visible { SW_SHOWNOACTIVATE } else { SW_HIDE });
    }
    GUIDE_SHOWN.store(visible, Ordering::Relaxed);
}

fn set_lines(lines: Vec<[i32; 4]>) {
    let h = GUIDE.load(Ordering::Relaxed) as HWND;
    if h as isize == 0 {
        return;
    }
    if let Ok(mut g) = LINES.lock() {
        *g = lines;
    }
    unsafe {
        // 只要异步重画：UpdateWindow 会在 WM_MOVING 里同步跑一次整屏 FillRect（2560×1440），
        // 每条鼠标消息都搭上一次十几毫秒的活，拖动循环会被拖垮。
        InvalidateRect(h, std::ptr::null(), 0);
    }
}

/// 别的卡片窗口矩形（本进程、可见、没最小化、类名 Tauri Window、排除自己）。
/// 桌面层锁的「最小化窗 rect=-32000」「DWM 隐形窗」都用可见性/最小化过滤掉了。
fn sibling_rects(self_hwnd: HWND) -> Vec<R4> {
    struct Ctx {
        self_hwnd: HWND,
        pid: u32,
        out: Vec<R4>,
    }
    unsafe extern "system" fn cb(hwnd: HWND, lparam: LPARAM) -> i32 {
        let c = &mut *(lparam as *mut Ctx);
        if hwnd == c.self_hwnd || IsWindowVisible(hwnd) == 0 || IsIconic(hwnd) != 0 {
            return 1;
        }
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, &mut pid);
        if pid != c.pid {
            return 1;
        }
        let mut buf = [0u16; 64];
        let n = GetClassNameW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
        if n <= 0 || String::from_utf16_lossy(&buf[..n as usize]) != "Tauri Window" {
            return 1;
        }
        // 只认登记过的卡窗；主界面同为 "Tauri Window"，绝不能当对齐目标。
        if !cards().lock().unwrap().contains(&(hwnd as isize)) {
            return 1;
        }

        let mut r: RECT = std::mem::zeroed();
        if GetWindowRect(hwnd, &mut r) != 0 && r.right > r.left && r.bottom > r.top {
            c.out.push((r.left, r.top, r.right, r.bottom));
        }
        1
    }
    let mut ctx = Ctx {
        self_hwnd,
        pid: unsafe { GetCurrentProcessId() },
        out: Vec::new(),
    };
    unsafe {
        EnumWindows(Some(cb), &mut ctx as *mut Ctx as LPARAM);
    }
    ctx.out
}

/// 卡片窗登记表：对齐目标只认这里登记过的窗口。
/// （为什么不用「类名 + 标题前缀」猜：主界面同为 `Tauri Window`、标题是 `Mini Card`，
///   标题判据在真机日志里漏过一次，附近的卡被主界面边缘莫名吸走。登记表不存在这种歧义。）
fn cards() -> &'static Mutex<Vec<isize>> {
    static C: OnceLock<Mutex<Vec<isize>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(Vec::new()))
}

/// 建卡时登记（`main.rs::apply_card_chrome` 里调）。
pub fn register(hwnd: HWND) {
    let mut v = cards().lock().unwrap();
    let k = hwnd as isize;
    if !v.contains(&k) {
        v.push(k);
    }
    drop(v);
    // 一次性自检（debug）：登记进来的必须是卡窗（标题 `MiniCard · …`）。
    // 真出问题时白名单会默默多一个大窗，拖动时卡片会被它的边缘莫名吸走，所以留个哨兵。
    #[cfg(debug_assertions)]
    {
        use std::io::Write;
        let ttl = unsafe {
            let mut tb = [0u16; 128];
            // 全路径调用：这个 API 只在 debug 自检里用，别进 release 的 import 表（否则告警）
            let n = windows_sys::Win32::UI::WindowsAndMessaging::GetWindowTextW(
                hwnd, tb.as_mut_ptr(), tb.len() as i32,
            );
            if n > 0 { String::from_utf16_lossy(&tb[..n as usize]) } else { String::new() }
        };
        if !ttl.starts_with("MiniCard") {
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(std::env::temp_dir().join("mc_guide.log"))
            {
                let _ = writeln!(f, "⚠️ 非卡窗被登记进对齐白名单：hwnd={:?} 标题={:?}", hwnd, ttl);
            }
        }
    }
}

/// 卡窗销毁时注销。目前卡片只 hide 不 destroy（全应用退出才一起销毁），所以还没有调用点；
/// 留着是为了将来真加 destroy 路径时能防句柄复用。
#[allow(dead_code)]
pub fn forget(hwnd: HWND) {
    cards().lock().unwrap().retain(|x| *x != hwnd as isize);
}

/// 被拖卡所在显示器的工作区
fn work_area(cur: R4) -> Option<R4> {
    unsafe {
        let rc = RECT {
            left: cur.0,
            top: cur.1,
            right: cur.2,
            bottom: cur.3,
        };
        let mon = MonitorFromRect(&rc, MONITOR_DEFAULTTONEAREST);
        if mon.is_null() {
            return None;
        }
        let mut mi: MONITORINFO = std::mem::zeroed();
        mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        if GetMonitorInfoW(mon, &mut mi) == 0 {
            return None;
        }
        Some((mi.rcWork.left, mi.rcWork.top, mi.rcWork.right, mi.rcWork.bottom))
    }
}

/// WM_MOVING：算对齐 + 更新辅助线；**不在这里动窗口位置**。
///
/// ⚠️ 2026-09-30 真机踩坑（用户报「拖不动、不能想拖到哪拖到哪」）：原来在这里直接改 RECT 做磁吸，
/// 而系统的拖动循环会把「上一次落地的位置」当成下一次的基准 —— 每修一次就累积一点偏移，
/// 慢拖（WM_MOVING 多）时卡片严重跟不上手：实测光标走 360px，卡只走 150px（91 次 WM_MOVING × 平均 2.3px）。
/// 正解：拖动中只画辅助线（自由跟手），落点 `end_move()` 再一次性吸上去。
pub fn moving(hwnd: HWND, rect: &mut RECT) -> bool {
    let cur = (rect.left, rect.top, rect.right, rect.bottom);
    let others = sibling_rects(hwnd);
    let work = work_area(cur);
    let tol = ((TOL as f64) * scale_of(hwnd)).round() as i32;
    let (dx, dy, lines) = align_offset(cur, &others, work, tol.max(2));
    #[cfg(debug_assertions)]
    {
        // 拖动对齐的现场取证（只在 debug 构建里写文件；release 不带）
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(std::env::temp_dir().join("mc_guide.log"))
        {
            let _ = writeln!(
                f,
                "self={:?} tol={} others={:?} work={:?} -> dx={} dy={} lines={:?}",
                cur, tol, others, work, dx, dy, lines
            );
        }
    }
    let hit = (dx != 0 || dy != 0) && cur.0 != -32000;
    if let Ok(mut g) = PENDING.lock() {
        *g = if hit {
            Some([cur.0 + dx, cur.1 + dy, cur.2 + dx, cur.3 + dy])
        } else {
            None
        };
    }
    let _ = rect; // 位置一个像素都不改：改了就累积
    // ⚠️ 2026-09-30 二次踩坑：不要在每一条 WM_MOVING 里反复 ShowWindow —— 拖动循环期间反复显示
    // 顶层窗口会让循环的跟踪基准复位，慢拖时卡片几乎不动（用户看到的「拖不动」）。
    // 现在只在拖动开始亮一次，拖动中只更新线。
    if !GUIDE_SHOWN.load(Ordering::Relaxed) {
        show_guide(true);
        GUIDE_SHOWN.store(true, Ordering::Relaxed);
    }
    set_lines(lines);
    hit
}

/// 启动时先备好辅助线窗口（隐藏）：别在拖动循环里现建窗口，那一枪容易踩到系统的坑
pub fn prepare() {
    ensure_guide();
    show_guide(false);
}

/// 拖动开始（WM_ENTERSIZEMOVE）：先把辅助线窗口亮出来，第一枪 WM_MOVING 就能画线
pub fn begin_move() {
    MOVING.store(true, Ordering::Relaxed);
    if let Ok(mut g) = PENDING.lock() {
        *g = None;
    }
    show_guide(true);
}

/// 拖动结束（WM_EXITSIZEMOVE）：清线 + 隐藏 + **把落点一次性吸到对齐位置**。
/// 吸附只做这一次，不做逐帧修正（逐帧修正会被拖动循环累积成卡跟不上手，见 moving() 的注释）。
pub fn end_move(hwnd: HWND) {
    MOVING.store(false, Ordering::Relaxed);
    let snap = PENDING.lock().ok().and_then(|mut g| g.take());
    set_lines(Vec::new());
    show_guide(false);
    if let Some([x, y, ..]) = snap {
        unsafe {
            SetWindowPos(
                hwnd,
                std::ptr::null_mut(),
                x,
                y,
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 右边缘贴齐：被拖卡 (100,100,400,300) 与另一卡 (600,50,900,250)
    /// → 被拖卡右 400 距另一卡左 600 太远（>tol）不吸；但 300 高度区间外的中心/边另算。
    #[test]
    fn snaps_to_neighbour_left_edge() {
        let other = (398, 50, 700, 250);
        let (dx, dy, lines) = align_offset((100, 100, 400, 300), &[other], None, 6);
        assert_eq!(dx, -2, "右边缘应吸到邻居左边缘（400→398，位移 -2）");
        assert_eq!(dy, 0);
        assert_eq!(lines.len(), 1, "只有一条竖线");
        assert_eq!(lines[0][0], 398, "线画在对齐的 x 上");
        assert_eq!((lines[0][1], lines[0][3]), (50, 300), "线跨两张卡的并集高度");
    }

    /// 超容差不动
    #[test]
    fn no_snap_beyond_tolerance() {
        let (dx, dy, lines) = align_offset((100, 100, 400, 300), &[(420, 50, 700, 250)], None, 6);
        assert_eq!((dx, dy), (0, 0));
        assert!(lines.is_empty());
    }

    /// 屏幕工作区左边缘：容差内吸 + 竖线贯穿工作区高度
    #[test]
    fn snaps_to_work_area_edge() {
        let work = (0, 0, 1920, 1040);
        let (dx, dy, lines) = align_offset((3, 200, 303, 380), &[], Some(work), 6);
        assert_eq!((dx, dy), (-3, 0));
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], [0, 0, 0, 1040]);
    }

    /// 中轴对齐（上/下边不沾）也要吸 + 画横线
    #[test]
    fn snaps_to_center_axis() {
        let other = (0, 0, 300, 180);
        // 被拖卡高 120，中心 240 → 与 other 中心 90 差 150，超容差；横向中心 550 vs 150 也超
        let (dx, dy, _) = align_offset((400, 180, 700, 300), &[other], None, 6);
        assert_eq!((dx, dy), (0, 0));
        // 把被拖卡挪到中心差 4px 的位置：竖向中心 90±4
        let (dx, dy, lines) = align_offset((400, 26, 700, 146), &[other], None, 6);
        assert_eq!(dy, 4, "竖向中心吸齐（cur 中心 86 → other 中心 90）");
        assert_eq!(dx, 0);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0][1], 90);
    }

    /// 同时命中两根轴：两条线
    #[test]
    fn both_axes_can_guide() {
        let work = (0, 0, 1920, 1040);
        let other = (0, 0, 300, 180);
        let (dx, dy, lines) = align_offset((3, 2, 303, 182), &[other], Some(work), 6);
        assert_eq!((dx, dy), (-3, -2));
        assert_eq!(lines.len(), 2);
    }
}
