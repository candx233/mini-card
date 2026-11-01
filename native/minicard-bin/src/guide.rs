//! 卡片拖动磁吸 + 对齐辅助线（原生绘制版的 guide.rs）
//!
//! 与 Tauri 那条（`src-tauri/src/guide.rs`）同一套口径：
//! * 位置仍然自由，config 里存的是落点原值；
//! * 拖到与别的卡片（或屏幕工作区）的 左/中/右、上/中/下 对齐时磁吸 + 出一条 1px 辅助线；
//! * **新增「固定间距」**：两张卡之间保持 `gap` 逻辑 px —— 吸的是「邻居边缘 ± gap」，
//!   而不是边缘重合。gap=0 就退化成原来的边缘对齐。
//!
//! 辅助线画在一个整屏、色键透明、鼠标穿透、永不激活、TOPMOST 的独立窗口上：
//! 卡片窗口只有自己那么大，线画不到卡外就看不出「和谁对齐了」。

use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::Mutex;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreatePen, CreateSolidBrush, DeleteObject, EndPaint, FillRect, MoveToEx, SelectObject,
    LineTo, PAINTSTRUCT, PS_SOLID,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetSystemMetrics, RegisterClassW, SetLayeredWindowAttributes,
    ShowWindow, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
    SW_HIDE, SW_SHOWNOACTIVATE, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

/// 矩形元组 (left, top, right, bottom)，屏幕物理 px
pub type R4 = (i32, i32, i32, i32);

/// 磁吸容差（逻辑 px，调用方按 DPI 换成物理 px）
pub const TOL: i32 = 6;

/// 色键：几乎不可能出现在界面上的颜色（COLORREF = BGR）
const KEY: COLORREF = COLORREF(0x0003_0201);
/// 辅助线颜色 = 强调蓝 #4C9AFF
const INK: COLORREF = COLORREF(0x00FF_9A4C);

static OVERLAY: AtomicIsize = AtomicIsize::new(0);
static LINES: Mutex<Vec<[i32; 4]>> = Mutex::new(Vec::new());
static SHOWN: AtomicBool = AtomicBool::new(false);

/// 一次吸附结果
#[derive(Debug, Default, Clone)]
pub struct Snap {
    /// 需要施加的位移（物理 px）
    pub dx: i32,
    pub dy: i32,
    /// 辅助线：[[x1, y1, x2, y2], ...]
    pub lines: Vec<[i32; 4]>,
}

/// 计算吸附：`cur` = 卡片当前矩形，`others` = 其余卡片，`work` = 工作区。
/// `gap` = 固定间距（物理 px，0 = 只做边缘/中线对齐）。
pub fn compute(cur: R4, others: &[R4], work: Option<R4>, tol: i32, gap: i32) -> Snap {
    // ── X 轴候选 ──
    // 「固定间距」候选优先：自己的左边缘贴到邻居右边缘 + gap、右边缘贴到邻居左边缘 − gap
    // 左边缘的候选（邻居在左：左边缘 = 邻居右边缘 + gap）
    let mut gap_l: Vec<(i32, Option<R4>)> = Vec::new();
    // 右边缘的候选（邻居在右：右边缘 = 邻居左边缘 − gap）
    let mut gap_r: Vec<(i32, Option<R4>)> = Vec::new();
    // 上/下边缘同理
    let mut gap_t: Vec<(i32, Option<R4>)> = Vec::new();
    let mut gap_b: Vec<(i32, Option<R4>)> = Vec::new();
    if gap > 0 {
        for o in others {
            gap_l.push((o.2 + gap, Some(*o)));
            gap_r.push((o.0 - gap, Some(*o)));
            gap_t.push((o.3 + gap, Some(*o)));
            gap_b.push((o.1 - gap, Some(*o)));
        }
    }
    // 「对齐」候选：左/中/右，上/中/下
    let mut al_x: Vec<(i32, Option<R4>)> = Vec::new();
    let mut al_y: Vec<(i32, Option<R4>)> = Vec::new();
    for o in others {
        al_x.push((o.0, Some(*o)));
        al_x.push(((o.0 + o.2) / 2, Some(*o)));
        al_x.push((o.2, Some(*o)));
        al_y.push((o.1, Some(*o)));
        al_y.push(((o.1 + o.3) / 2, Some(*o)));
        al_y.push((o.3, Some(*o)));
    }
    if let Some(w) = work {
        al_x.push((w.0, None));
        al_x.push(((w.0 + w.2) / 2, None));
        al_x.push((w.2, None));
        al_y.push((w.1, None));
        al_y.push(((w.1 + w.3) / 2, None));
        al_y.push((w.3, None));
    }

    // 自己的三条边；「固定间距」只认外侧边（左边缘只能贴到「邻居右 + gap」）
    let own_x = [cur.0, (cur.0 + cur.2) / 2, cur.2];
    let own_y = [cur.1, (cur.1 + cur.3) / 2, cur.3];

    let mut lines: Vec<[i32; 4]> = Vec::new();
    let (mut dx, mut dy) = (0, 0);

    // 某条边对同一类候选取最近且在容差内的
    let near = |own: i32, targets: &[(i32, Option<R4>)]| -> Option<(i32, i32, Option<R4>)> {
        let mut best: Option<(i32, i32, Option<R4>)> = None;
        let mut best_abs = i32::MAX;
        for (t, partner) in targets {
            let d = t - own;
            if d.abs() <= tol && d.abs() < best_abs {
                best_abs = d.abs();
                best = Some((d, *t, *partner));
            }
        }
        best
    };

    let gx = near(own_x[0], &gap_l).or_else(|| near(own_x[2], &gap_r));
    if let Some((d, pos, partner)) = gx {
        dx = d;
        let (t, b) = match partner {
            Some(p) => (cur.1.min(p.1), cur.3.max(p.3)),
            None => work.map(|w| (w.1, w.3)).unwrap_or((cur.1, cur.3)),
        };
        lines.push([pos, t, pos, b]);
    } else if let Some((d, pos, partner)) = axis_match(own_x, &al_x, tol) {
        dx = d;
        let (t, b) = match partner {
            Some(p) => (cur.1.min(p.1), cur.3.max(p.3)),
            None => work.map(|w| (w.1, w.3)).unwrap_or((cur.1, cur.3)),
        };
        lines.push([pos, t, pos, b]);
    }

    let gy = near(own_y[0], &gap_t).or_else(|| near(own_y[2], &gap_b));
    if let Some((d, pos, partner)) = gy {
        dy = d;
        let (l, r) = match partner {
            Some(p) => (cur.0.min(p.0), cur.2.max(p.2)),
            None => work.map(|w| (w.0, w.2)).unwrap_or((cur.0, cur.2)),
        };
        lines.push([l, pos, r, pos]);
    } else if let Some((d, pos, partner)) = axis_match(own_y, &al_y, tol) {
        dy = d;
        let (l, r) = match partner {
            Some(p) => (cur.0.min(p.0), cur.2.max(p.2)),
            None => work.map(|w| (w.0, w.2)).unwrap_or((cur.0, cur.2)),
        };
        lines.push([l, pos, r, pos]);
    }

    Snap { dx, dy, lines }
}

/// 三条边对一堆候选里取**最近**且在容差内的那个
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

// ───────────────────────── 辅助线窗口 ─────────────────────────

unsafe extern "system" fn guide_wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    const WM_PAINT: u32 = 0x000F;
    const WM_ERASEBKGND: u32 = 0x0014;
    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let dc = BeginPaint(hwnd, &mut ps);
            if !dc.is_invalid() {
                let (vx, vy) = (
                    GetSystemMetrics(SM_XVIRTUALSCREEN),
                    GetSystemMetrics(SM_YVIRTUALSCREEN),
                );
                let (vw, vh) = (
                    GetSystemMetrics(SM_CXVIRTUALSCREEN),
                    GetSystemMetrics(SM_CYVIRTUALSCREEN),
                );
                let all = RECT { left: vx, top: vy, right: vx + vw, bottom: vy + vh };
                let bg = CreateSolidBrush(KEY);
                FillRect(dc, &all, bg);
                let _ = DeleteObject(bg.into());
                let pen = CreatePen(PS_SOLID, 1, INK);
                let old = SelectObject(dc, pen.into());
                for l in LINES.lock().unwrap().iter() {
                    let _ = MoveToEx(dc, l[0], l[1], None);
                    let _ = LineTo(dc, l[2], l[3]);
                }
                SelectObject(dc, old);
                let _ = DeleteObject(pen.into());
                let _ = EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

/// 建辅助线窗口（整屏、色键透明、鼠标穿透、不激活、TOPMOST），启动时调一次。
pub unsafe fn ensure_overlay() -> Option<HWND> {
    let h = OVERLAY.load(Ordering::Relaxed);
    if h != 0 {
        return Some(HWND(h as *mut core::ffi::c_void));
    }
    let title: Vec<u16> = "minicard-guide\0".encode_utf16().collect();
    let class: Vec<u16> = "MiniCardGuide\0".encode_utf16().collect();
    let hinst = windows::Win32::System::LibraryLoader::GetModuleHandleW(None).ok()?;
    let wc = WNDCLASSW {
        style: windows::Win32::UI::WindowsAndMessaging::WNDCLASS_STYLES(0),
        lpfnWndProc: Some(guide_wndproc),
        hInstance: hinst.into(),
        lpszClassName: PCWSTR(class.as_ptr()),
        ..Default::default()
    };
    let _ = RegisterClassW(&wc);
    let (vx, vy) = (
        GetSystemMetrics(SM_XVIRTUALSCREEN),
        GetSystemMetrics(SM_YVIRTUALSCREEN),
    );
    let (vw, vh) = (
        GetSystemMetrics(SM_CXVIRTUALSCREEN),
        GetSystemMetrics(SM_CYVIRTUALSCREEN),
    );
    let hwnd = CreateWindowExW(
        WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
        PCWSTR(class.as_ptr()),
        PCWSTR(title.as_ptr()),
        WS_POPUP,
        vx,
        vy,
        vw,
        vh,
        None,
        None,
        Some(hinst.into()),
        None,
    )
    .ok()?;
    let _ = SetLayeredWindowAttributes(hwnd, KEY, 0, windows::Win32::UI::WindowsAndMessaging::LWA_COLORKEY);
    OVERLAY.store(hwnd.0 as isize, Ordering::Relaxed);
    Some(hwnd)
}

/// 显示这几条线（拖动中每帧调）
pub unsafe fn show_lines(lines: &[[i32; 4]]) {
    let h = OVERLAY.load(Ordering::Relaxed);
    if h == 0 {
        return;
    }
    let hwnd = HWND(h as *mut core::ffi::c_void);
    *LINES.lock().unwrap() = lines.to_vec();
    let _ = windows::Win32::Graphics::Gdi::InvalidateRect(Some(hwnd), None, false);
    if !SHOWN.swap(true, Ordering::Relaxed) {
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    }
}

/// 拖动结束：隐藏
pub unsafe fn hide() {
    let h = OVERLAY.load(Ordering::Relaxed);
    if h == 0 || !SHOWN.swap(false, Ordering::Relaxed) {
        return;
    }
    let hwnd = HWND(h as *mut core::ffi::c_void);
    let _ = ShowWindow(hwnd, SW_HIDE);
    LINES.lock().unwrap().clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(l: i32, t: i32, w: i32, h: i32) -> R4 {
        (l, t, l + w, t + h)
    }

    #[test]
    fn 边缘对齐_左边贴左边() {
        // 邻居在 (100, 100) 100×100；自己被拖到 103 附近
        let other = r(100, 100, 100, 100);
        let cur = r(103, 300, 100, 100);
        let s = compute(cur, &[other], None, 6, 0);
        assert_eq!(s.dx, -3, "应当左移 3px 贴齐");
        assert!(s.lines.iter().any(|l| l[0] == 100 && l[2] == 100), "竖线应在 x=100");
    }

    #[test]
    fn 固定间距_左侧留_gap() {
        // gap=12：邻居右边缘 200，自己左边缘应吸到 212
        let other = r(100, 100, 100, 100); // 右边缘 200
        let cur = r(209, 300, 100, 100);
        let s = compute(cur, &[other], None, 6, 12);
        assert_eq!(s.dx, 3, "应当右移 3px 到 212");
        // 已经完全贴齐（相距 0）时不需要「推开」：12px 超出容差，不吸
        let cur2 = r(200, 300, 100, 100);
        let s2 = compute(cur2, &[other], None, 6, 12);
        assert_eq!(s2.dx, 0, "超容差不推");
        // 差 5px 到「留 12px」的位置时要吸过去
        let cur3 = r(207, 300, 100, 100);
        let s3 = compute(cur3, &[other], None, 6, 12);
        assert_eq!(s3.dx, 5);
    }

    #[test]
    fn 固定间距_右侧留_gap() {
        // 邻居左边缘 400；自己右边缘应吸到 388
        let other = r(400, 100, 100, 100);
        let cur = r(285, 300, 100, 100); // 右边缘 385
        let s = compute(cur, &[other], None, 6, 12);
        assert_eq!(s.dx, 3, "应当右移到 388（右边缘）");
    }

    #[test]
    fn 工作区边缘仍然贴边() {
        let work = r(0, 0, 1920, 1080);
        let cur = r(3, 400, 100, 100);
        let s = compute(cur, &[], Some(work), 6, 12);
        assert_eq!(s.dx, -3, "屏幕左边仍是贴边，不因为 gap 留空");
        let cur2 = r(1817, 400, 100, 100); // 右边缘 1917
        let s2 = compute(cur2, &[], Some(work), 6, 12);
        assert_eq!(s2.dx, 3);
    }

    #[test]
    fn 中线对齐() {
        let other = r(500, 100, 100, 100); // 中线 550
        let cur = r(497, 400, 100, 100); // 中线 547
        let s = compute(cur, &[other], None, 6, 0);
        assert_eq!(s.dx, 3);
    }

    #[test]
    fn 超出容差不吸() {
        let other = r(100, 100, 100, 100);
        let cur = r(120, 300, 100, 100);
        let s = compute(cur, &[other], None, 6, 0);
        assert_eq!((s.dx, s.dy), (0, 0));
        assert!(s.lines.is_empty());
    }

    #[test]
    fn 纵向固定间距() {
        let other = r(100, 100, 100, 100); // 下边缘 200
        let cur = r(100, 211, 100, 100);
        let s = compute(cur, &[other], None, 6, 12);
        assert_eq!(s.dy, 1, "上边缘应吸到 212");
    }
}
