//! 托盘右键菜单（0.1.2 G 段）：自绘菜单窗口。
//!
//! 为什么自绘：Windows 原生托盘菜单（muda → `TrackPopupMenu`）的文字左侧有 **46px 固定空槽**
//! （给勾选/图标预留的列）+ 行高/高亮几何全是系统度量，没有 CSS 可写 —— 悬停时高亮框左半截是空的。
//! 方案与规格正本 = `docs/tray-menu-plan.md`（形态 §三 / 定位 §四 / 交互 §五 / 坑 §七）。
//!
//! 形态：label `tray-menu` 的独立透明小窗。窗口 = 菜单盒（168 宽）**四周加投影余量**
//! （上/左右 12、下 8）；菜单盒里就是一列菜单项（行高 32 / 圆角 8 / 项内左右 10）。
//! 生命周期「用时建、闲时回收」：第一次右键现建（WebView2 冷启动），隐藏 30s 后销毁
//! （隐藏常驻 = 多养一个渲染进程）。
//!
//! ⚠️ 两个不能照抄卡片/Dock 的地方：
//! ① **不能用 `apply_glass_chrome`**：它的子类还带桌面层锁（NOACTIVATE / 钉桌面锚点 / WM_MOUSEACTIVATE=3），
//!    而菜单必须能激活（ESC 与失焦关闭都要焦点）、必须盖在别的窗口上面 → 自己写一份轻 chrome（见 `apply_menu_chrome`）。
//! ② **菜单项不能沿用 `.card * { pointer-events: none }`**（tray-menu-plan 坑 #2）。
//!
//! 定位：托盘图标上方 8px、右沿对齐图标中心；越界回退到下方 / 夹回工作区内（`place_menu`）。
//! 托盘事件 rect 是**物理像素**（`Shell_NotifyIconGetRect` 口径）→ 按图标所在屏的 scale 换成逻辑 px。

use std::sync::Mutex;
use std::time::{Duration, Instant};

use tauri::{
    AppHandle, Manager, Position, Rect, Size, WebviewUrl, WebviewWindow, WebviewWindowBuilder,
};

pub const LABEL: &str = "tray-menu";

/// 菜单项（id, 文案）。**加项要连 `web/menu.html` 的静态 DOM 一起改**（窗口高度按条数算；
/// 单测 `menu_html_matches_items` 会盯着两边一致）。
pub const ITEMS: &[(&str, &str)] = &[("show", "显示主窗口"), ("quit", "退出 Mini Card")];

/// 菜单盒宽（固定；最长项「退出 Mini Card」+ 左右内边距，还留得下「开机自启 / 检查更新」这类新项）
pub const MENU_W: f64 = 168.0;
/// 行高 = 悬停高亮的整行高度
pub const ITEM_H: f64 = 32.0;
/// 菜单盒上下内边距
pub const MENU_PAD: f64 = 6.0;
/// 菜单项间距 + 菜单盒描边宽度（menu.html 的 `gap: 2px` / `border: 1px`）
pub const ITEM_GAP: f64 = 2.0;
pub const BORDER: f64 = 1.0;
/// 菜单盒与图标之间留的间隙
pub const GAP: f64 = 8.0;
/// 窗口比菜单盒多出的投影余量：上/左右 12、下 8。
/// 下方只给 8 = GAP：透明区**也吃鼠标** —— 窗口一旦压到图标上，右键就点不到图标了
/// （投影在 8px 处正好收尾：`0 3px 10px` 的下沿 = 3 + 10/2 = 8）。
pub const PAD_TOP: f64 = 12.0;
pub const PAD_X: f64 = 12.0;
pub const PAD_BOTTOM: f64 = 8.0;
/// 隐藏后多久销毁窗口（隐藏常驻 = 一个 WebView2 渲染进程；销毁后下次右键现建）
const IDLE_DESTROY: Duration = Duration::from_secs(30);

/// 菜单盒高度 = n × 行高 + 上下内边距 + 项间距 ×(n−1) + 上下描边。
/// ⚠️ 必须与 `web/menu.html` 的 `.menu` 实测渲染盒一致（80px for n=2）——差一像素就是
/// 「菜单与图标之间那 8px 间隙」对不上（`menu_h_equals_html_box` 单测盯着）。
pub fn menu_h() -> f64 {
    let n = ITEMS.len() as f64;
    n * ITEM_H + MENU_PAD * 2.0 + (n - 1.0) * ITEM_GAP + 2.0 * BORDER
}

/// 窗口尺寸（逻辑 px）= 菜单盒 + 投影余量
pub fn window_size() -> (f64, f64) {
    (MENU_W + PAD_X * 2.0, menu_h() + PAD_TOP + PAD_BOTTOM)
}

/// 隐藏时刻（收起后 30s 没人再开 → 回收线程销毁窗口）
static HIDDEN_AT: Mutex<Option<Instant>> = Mutex::new(None);

/// 原窗口过程（同卡片窗口：tao 的窗口类唯一，原 proc 只存一份）
static MENU_ORIG: Mutex<Option<windows_sys::Win32::UI::WindowsAndMessaging::WNDPROC>> =
    Mutex::new(None);

/* ───────────── 定位（纯函数 + 单测） ───────────── */

/// 菜单落点（逻辑 px）：输入 = 图标矩形 `(x,y,w,h)` 与工作区矩形（同一坐标系、逻辑 px），
/// 输出 = 菜单**盒**左上角。规则（tray-menu-plan §四）：
/// 默认在图标上方 8px；横向右沿对齐图标中心、越出工作区右沿就整体左移（距边 ≥8px）；
/// 上方放不下 → 改到下方 8px（下方也放不下就夹回工作区内）。
pub fn place_menu(
    icon: (f64, f64, f64, f64),
    work: (f64, f64, f64, f64),
    mw: f64,
    mh: f64,
) -> (f64, f64) {
    let (ix, iy, iw, ih) = icon;
    let (wx, wy, ww, wh) = work;
    let x = (ix + iw / 2.0 - mw)
        .min(wx + ww - GAP - mw)
        .max(wx + GAP);
    let mut y = iy - GAP - mh;
    if y < wy + GAP {
        y = (iy + ih + GAP).min(wy + wh - GAP - mh).max(wy + GAP);
    }
    (x, y)
}

/// 托盘事件 rect → 物理 px。
/// Windows 上 `Shell_NotifyIconGetRect` 恒给物理值（tray-menu-plan 坑 #6）；Logical 分支只是兜底。
fn rect_phys(rect: Rect) -> (f64, f64, f64, f64) {
    let (x, y) = match rect.position {
        Position::Physical(p) => (p.x as f64, p.y as f64),
        Position::Logical(p) => (p.x, p.y),
    };
    let (w, h) = match rect.size {
        Size::Physical(s) => (s.width as f64, s.height as f64),
        Size::Logical(s) => (s.width, s.height),
    };
    (x, y, w, h)
}

/// 图标所在显示器的 `(工作区逻辑矩形, scale)`。取不到显示器信息时退回主屏 / 兜底值。
fn work_area_of(app: &AppHandle, px: f64, py: f64) -> ((f64, f64, f64, f64), f64) {
    let mon = app
        .monitor_from_point(px, py)
        .ok()
        .flatten()
        .or_else(|| app.primary_monitor().ok().flatten());
    match mon {
        Some(m) => {
            let s = m.scale_factor();
            let w = m.work_area();
            (
                (
                    w.position.x as f64 / s,
                    w.position.y as f64 / s,
                    w.size.width as f64 / s,
                    w.size.height as f64 / s,
                ),
                s,
            )
        }
        None => ((0.0, 0.0, 1920.0, 1040.0), 1.0),
    }
}

/* ───────────── 显隐 ───────────── */

/// 右键托盘：可见 → 收起；不可见 → 定位后弹出（rect 是托盘事件给的图标物理矩形）
pub fn toggle(app: &AppHandle, rect: Rect) {
    let visible = app
        .get_webview_window(LABEL)
        .map(|w| w.is_visible().unwrap_or(false))
        .unwrap_or(false);
    if visible {
        hide(app);
    } else {
        show_at(app, rect);
    }
}

/// 打开菜单：算落点 → 建/摆窗口 → 显示 → 抢焦点（ESC / 失焦关闭都要焦点）
pub fn show_at(app: &AppHandle, rect: Rect) {
    let (px, py, pw, ph) = rect_phys(rect);
    let ((wx, wy, ww, wh), s) = work_area_of(app, px, py);
    let (mx, my) = place_menu(
        (px / s, py / s, pw / s, ph / s),
        (wx, wy, ww, wh),
        MENU_W,
        menu_h(),
    );
    let pos = (mx - PAD_X, my - PAD_TOP);
    let Some(w) = ensure(app, pos) else { return };
    // 复用时也要摆一次（上一次可能在另一块屏 / 屏幕拓扑变过）
    // ⚠️ 逻辑坐标按「窗口当前所在屏」的 scale 换算，跨屏不同缩放不精确 —— 与 Dock 同一口径，
    //    双屏/混合 DPI 是既定后置项（见 skill「跨分辨率」节），不在本轮范围。
    let _ = w.set_position(Position::Logical(tauri::LogicalPosition::new(pos.0, pos.1)));
    let _ = w.show();
    if let Some(h) = crate::hwnd_of(&w) {
        focus_menu(h);
    }
    set_hidden(None);
}

/// 收起菜单（点击项 / ESC / 失焦 / 左键托盘），并起算回收倒计时
pub fn hide(app: &AppHandle) {
    if let Some(w) = app.get_webview_window(LABEL) {
        if w.is_visible().unwrap_or(false) {
            let _ = w.hide();
            set_hidden(Some(Instant::now()));
        }
    }
}

fn set_hidden(v: Option<Instant>) {
    if let Ok(mut g) = HIDDEN_AT.lock() {
        *g = v;
    }
}

/// 30s 空闲回收线程（启动时拉起一次）：隐藏超过 `IDLE_DESTROY` 没人再开 → 销毁窗口。
/// 隐藏常驻 ≈ 一个 WebView2 渲染进程的内存（tray-menu-plan §八 判据 5）；
/// 销毁后下次右键现建，代价是一次 WebView2 冷启动。
pub fn spawn_reaper(app: AppHandle) {
    let _ = std::thread::Builder::new()
        .name("mini-card-menu-reaper".into())
        .spawn(move || loop {
            std::thread::sleep(Duration::from_secs(5));
            let due = HIDDEN_AT
                .lock()
                .map(|g| g.map(|t| t.elapsed() >= IDLE_DESTROY).unwrap_or(false))
                .unwrap_or(false);
            if !due {
                continue;
            }
            match app.get_webview_window(LABEL) {
                Some(w) if !w.is_visible().unwrap_or(false) => {
                    set_hidden(None);
                    if let Err(e) = w.destroy() {
                        crate::mlog(&format!("tray menu reaper: destroy ERR {e}"));
                    }
                }
                None => set_hidden(None),
                _ => {} // 又被打开了：等下次隐藏
            }
        });
}

/* ───────────── 建窗 / chrome ───────────── */

/// 建窗（不存在才建）。建成即隐藏 —— 显示时机由 `show_at` 说了算。
fn ensure(app: &AppHandle, pos: (f64, f64)) -> Option<WebviewWindow> {
    if let Some(w) = app.get_webview_window(LABEL) {
        return Some(w);
    }
    let (ww, wh) = window_size();
    let s = crate::Config::load().settings;
    let dark = if matches!(s.theme, crate::config::Theme::Light) { 0 } else { 1 };
    let url = format!("menu.html?dark={dark}");
    let win = match WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App(url.into()))
        .title("MiniCard · Menu")
        .inner_size(ww, wh)
        .position(pos.0, pos.1)
        .decorations(false)
        .transparent(true)
        .resizable(false)
        .skip_taskbar(true)
        .always_on_top(true)
        .focused(false)
        .shadow(false)
        .visible(false)
        .build()
    {
        Ok(w) => w,
        Err(e) => {
            crate::mlog(&format!("tray menu: build ERR {e}"));
            return None;
        }
    };
    apply_menu_chrome(&win);
    // 1px 尺寸抖动逼 WebView2 重算表面 bounds（卡片 / Dock 同款）；菜单窗口是「用时建」的，
    // 这一拍必须在 —— 少了它首帧可能是没画完的空白
    use tauri::LogicalSize;
    let _ = win.set_size(Size::Logical(LogicalSize::new(ww + 1.0, wh + 1.0)));
    std::thread::sleep(Duration::from_millis(40));
    let _ = win.set_size(Size::Logical(LogicalSize::new(ww, wh)));
    // 失焦关闭（点项 / ESC 那两条各自显式关）。菜单窗**可激活**（与 Dock 相反），
    // 所以这条能收到；不做「鼠标移出就关」（与原生菜单一致，避免误关 —— tray-menu-plan §五）
    let app_ev = app.clone();
    win.on_window_event(move |e| {
        if matches!(e, tauri::WindowEvent::Focused(false)) {
            if let Some(w) = app_ev.get_webview_window(LABEL) {
                if w.is_visible().unwrap_or(false) {
                    let _ = w.hide();
                    set_hidden(Some(Instant::now()));
                }
            }
        }
    });
    crate::mlog("tray menu: created");
    Some(win)
}

/// 菜单窗的轻 chrome：剥 `WS_CAPTION`（tao 每次 flag 同步都会整份写回来 → 子类里再剥一次）
/// + 子类 `WM_NCCALCSIZE=0`（客户区 = 窗口矩形，否则 webview 会被标题栏挤小一截）+ DWM 三个属性。
/// ⚠️ 与 `apply_glass_chrome` 的唯一区别 = **不注册桌面层锁**（菜单要能激活、要能压在别家窗口上）。
fn apply_menu_chrome(w: &WebviewWindow) {
    let Some(hwnd) = crate::hwnd_of(w) else { return };
    unsafe {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            GetWindowLongW, SetWindowLongPtrW, SetWindowLongW, SetWindowPos, GWLP_WNDPROC,
            GWL_STYLE, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
        };
        let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
        let stripped = (style
            & !(0x00C0_0000 // WS_CAPTION
                | 0x0004_0000 // WS_THICKFRAME
                | 0x0008_0000 // WS_SYSMENU
                | 0x0002_0000 // WS_MINIMIZEBOX
                | 0x0001_0000)) // WS_MAXIMIZEBOX
            | 0x8000_0000; // WS_POPUP
        SetWindowLongW(hwnd, GWL_STYLE, stripped as i32);

        let mut origs = MENU_ORIG.lock().unwrap();
        let prev = SetWindowLongPtrW(hwnd, GWLP_WNDPROC, menu_wnd_proc as *const () as isize);
        if prev != 0 && origs.is_none() {
            *origs = Some(std::mem::transmute::<
                isize,
                windows_sys::Win32::UI::WindowsAndMessaging::WNDPROC,
            >(prev));
        }
        drop(origs);

        SetWindowPos(
            hwnd,
            std::ptr::null_mut(),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        );

        use windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute;
        // 33 = DWMWA_WINDOW_CORNER_PREFERENCE：圆角交给 CSS（12px），系统 8px 会切掉
        let pref: u32 = 1; // DWMWCP_DONOTROUND
        DwmSetWindowAttribute(hwnd, 33, &pref as *const _ as *const core::ffi::c_void, 4);
        // 34 = DWMWA_BORDER_COLOR：无 DWM 描边（否则窗口最外沿会有一圈细线）
        let noborder: u32 = 0xFFFF_FFFE; // DWMWA_COLOR_NONE
        DwmSetWindowAttribute(
            hwnd,
            34,
            &noborder as *const _ as *const core::ffi::c_void,
            4,
        );
        // 38 = DWMWA_SYSTEMBACKDROP_TYPE：不做真磨砂（菜单是纯 CSS 面板 + 投影，tray-menu-plan §六）
        let backdrop: u32 = 1; // DWMSBT_NONE
        DwmSetWindowAttribute(
            hwnd,
            38,
            &backdrop as *const _ as *const core::ffi::c_void,
            4,
        );
    }
}

/// 菜单窗的子类窗口过程：只干「消标题栏 / 消非客户区」这一件事，
/// 桌面层锁那套（NOACTIVATE / 钉 z 序 / MA_NOACTIVATE）一概不碰。
unsafe extern "system" fn menu_wnd_proc(
    hwnd: *mut core::ffi::c_void,
    msg: u32,
    wparam: usize,
    lparam: isize,
) -> isize {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CallWindowProcW, DefWindowProcW, STYLESTRUCT, GWL_STYLE,
    };
    match msg {
        0x0083 => return 0,          // WM_NCCALCSIZE：客户区 = 窗口矩形
        0x0086 => return 1,          // WM_NCACTIVATE：自己处理，不回原 proc → 不重画标题栏
        0x0085 => return 0,          // WM_NCPAINT：无 NC 区，无需绘制
        0x00AE | 0x00AF => return 0, // WM_NCUAHDRAWCAPTION / WM_NCUAHDRAWFRAME
        // tao 每次窗口 flag 同步都用 to_window_styles() 整份重写 GWL_STYLE（show/hide 都会触发）
        // → 这里把 WS_CAPTION 那一组再剥掉、并补回 WS_POPUP
        0x007C => {
            if wparam as i32 == GWL_STYLE {
                let ss = &mut *(lparam as *mut STYLESTRUCT);
                ss.styleNew = (ss.styleNew
                    & !(0x00C0_0000 | 0x0004_0000 | 0x0008_0000 | 0x0002_0000 | 0x0001_0000))
                    | 0x8000_0000;
            }
        }
        _ => {}
    }
    let orig = MENU_ORIG.lock().ok().and_then(|g| *g);
    match orig {
        Some(p) => CallWindowProcW(p, hwnd, msg, wparam, lparam),
        None => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// 抢前台四连兜底（同 `dock::activate_or_minimize` 的梯子）：
/// 点托盘图标**不给本进程「最后输入事件」**，所以 `SetForegroundWindow` 常被拒 ——
/// 挂输入队列 / 合成 ALT / `SwitchToThisWindow` 逐个试。全败只记日志（ESC 会失灵，但点项仍可用）。
fn focus_menu(hwnd: *mut core::ffi::c_void) {
    use windows_sys::Win32::System::Threading as th;
    use windows_sys::Win32::UI::WindowsAndMessaging as wam;
    unsafe {
        let h = hwnd as windows_sys::Win32::Foundation::HWND;
        if wam::GetForegroundWindow() == h {
            return;
        }
        if wam::SetForegroundWindow(h) != 0 {
            return;
        }
        let fg = wam::GetForegroundWindow();
        let fg_tid = wam::GetWindowThreadProcessId(fg, std::ptr::null_mut());
        let me = th::GetCurrentThreadId();
        if fg_tid != 0 && fg_tid != me {
            th::AttachThreadInput(me, fg_tid, 1);
            let r = wam::SetForegroundWindow(h);
            th::AttachThreadInput(me, fg_tid, 0);
            if r != 0 {
                return;
            }
        }
        {
            use windows_sys::Win32::UI::Input::KeyboardAndMouse as kbd;
            const VK_MENU: u8 = 0x12;
            kbd::keybd_event(VK_MENU, 0, 0, 0);
            kbd::keybd_event(VK_MENU, 0, kbd::KEYEVENTF_KEYUP, 0);
        }
        if wam::SetForegroundWindow(h) != 0 {
            return;
        }
        wam::SwitchToThisWindow(h, 1);
        if wam::GetForegroundWindow() != h {
            crate::mlog("tray menu: 抢前台全败（ESC / 失焦关闭可能不生效）");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_geometry_matches_spec() {
        assert_eq!(menu_h(), 80.0); // 2 项 × 32 + 上下 6 + 项间距 2 + 描边 2（= menu.html 实测盒高）
        assert_eq!(window_size(), (192.0, 100.0)); // + 投影余量（上/左右 12、下 8）
    }

    /// menu.html 的 CSS 数值必须与上面的常量一致 —— 改了一边漏另一边，菜单盒与窗口就对不上，
    /// 表现是「菜单与图标那 8px 间隙」和「菜单盒底边」一起漂（数值不报错，只是位置悄悄歪）
    #[test]
    fn menu_html_css_matches_geometry() {
        let html = include_str!("../../web/menu.html");
        let css = html.split("<style>").nth(1).expect("menu.html 没找到 <style>");
        let css = css.split("</style>").next().unwrap();
        assert!(css.contains(&format!("width: {}px", MENU_W as i64)), "菜单盒宽");
        assert!(css.contains(&format!("padding: {}px", MENU_PAD as i64)), "菜单盒内边距");
        assert!(css.contains(&format!("gap: {}px", ITEM_GAP as i64)), "项间距");
        assert!(css.contains("border: 1px solid"), "菜单盒描边");
        assert!(css.contains(&format!("height: {}px", ITEM_H as i64)), "行高");
        assert!(css.contains("border-radius: 12px"), "菜单圆角 12");
        assert!(css.contains("border-radius: 8px"), "高亮圆角 8");
        assert!(css.contains("padding: 0 10px"), "项内左右 10");
    }

    #[test]
    fn place_menu_above_icon_by_default() {
        // 任务栏在屏幕底部：图标在 1920×1080 屏右下角、工作区到 1032
        let icon = (1800.0, 1040.0, 24.0, 24.0);
        let work = (0.0, 0.0, 1920.0, 1032.0);
        let (x, y) = place_menu(icon, work, MENU_W, menu_h());
        assert_eq!(x, 1800.0 + 12.0 - MENU_W); // 右沿 = 图标中心
        assert_eq!(y, 1040.0 - GAP - menu_h()); // 图标上方 8px
        assert!(y + menu_h() <= 1032.0, "完全在工作区内");
        assert!(y + menu_h() <= icon.1 - GAP, "与图标不相交（还隔 8px）");
    }

    #[test]
    fn place_menu_shifts_left_at_right_edge() {
        let icon = (1908.0, 1040.0, 24.0, 24.0); // 图标贴工作区右沿
        let work = (0.0, 0.0, 1920.0, 1032.0);
        let (x, _) = place_menu(icon, work, MENU_W, menu_h());
        assert_eq!(x, 1920.0 - GAP - MENU_W); // 整体左移，距右沿 8px
        assert!(x + MENU_W <= 1920.0 - GAP);
    }

    #[test]
    fn place_menu_falls_below_when_no_room_above() {
        // 任务栏在屏幕顶部：图标的 rect 在工作区上方（y=10 < 工作区上沿 48）
        let work = (0.0, 48.0, 1920.0, 1032.0);
        let (_, y) = place_menu((900.0, 10.0, 24.0, 24.0), work, MENU_W, menu_h());
        assert_eq!(y, 56.0, "下方 8px 也放不下 → 夹回工作区上沿 + 8（仍与图标不相交：图标底 34）");
        assert!(y >= 48.0 + GAP && y + menu_h() <= 48.0 + 1032.0 - GAP);
        assert!(y >= 34.0, "与图标不相交");

        // 下方够放：图标底 34 + 8 = 42（工作区上沿 30，放得下）
        let (_, y2) = place_menu(
            (900.0, 10.0, 24.0, 24.0),
            (0.0, 30.0, 1920.0, 1000.0),
            MENU_W,
            menu_h(),
        );
        assert_eq!(y2, 42.0);
    }

    /// Rust 的条目表与 menu.html 的静态 DOM 必须一致（漏改一边 = 高度错 / 点不动）
    #[test]
    fn menu_html_matches_items() {
        let html = include_str!("../../web/menu.html");
        assert_eq!(html.matches("data-id=\"").count(), ITEMS.len());
        for (id, text) in ITEMS {
            assert!(html.contains(&format!("data-id=\"{id}\"")), "menu.html 缺 data-id={id}");
            assert!(html.contains(text), "menu.html 缺文案「{text}」");
        }
    }
}
