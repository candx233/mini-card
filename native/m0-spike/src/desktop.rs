//! 桌面层：把窗口插到「桌面锚点（Progman/WorkerW，含 SHELLDLL_DefView）」正上方
//! —— 即壁纸之上、所有普通窗口之下。与主程序 `apply_desktop_zorder` 同一套做法（不 SetParent）。
//!
//! 另外提供一个取证用的 DWM 缩略图宿主窗：把我们自己的窗口画面投到一个置顶小窗里，
//! 这样即使卡片被别的窗口压着（桌面层本来就该被压着），也能拍到它到底合成了什么。

use windows::core::{w, Result, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmRegisterThumbnail, DwmUpdateThumbnailProperties, DWM_THUMBNAIL_PROPERTIES,
    DWM_TNP_RECTDESTINATION, DWM_TNP_VISIBLE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, EnumChildWindows, EnumWindows, FindWindowExW, GetClassNameW,
    PostQuitMessage, RegisterClassW, SetWindowPos, ShowWindow, GW_HWNDNEXT, SWP_NOACTIVATE,
    SWP_NOMOVE, SWP_NOSIZE, WNDCLASSW, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
};

fn class_of(h: HWND) -> String {
    let mut buf = [0u16; 64];
    let n = unsafe { GetClassNameW(h, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

fn has_defview(h: HWND) -> bool {
    struct S {
        found: bool,
    }
    unsafe extern "system" fn cb(h: HWND, l: LPARAM) -> windows::core::BOOL {
        let s = &mut *(l.0 as *mut S);
        if class_of(h) == "SHELLDLL_DefView" {
            s.found = true;
            return false.into();
        }
        true.into()
    }
    let mut s = S { found: false };
    let _ = unsafe { EnumChildWindows(Some(h), Some(cb), LPARAM(&mut s as *mut S as isize)) };
    s.found
}

/// 桌面锚点 = 类名 Progman/WorkerW 且子窗口含 SHELLDLL_DefView 的顶层窗
pub fn anchor() -> HWND {
    struct S {
        hwnd: HWND,
    }
    unsafe extern "system" fn cb(h: HWND, l: LPARAM) -> windows::core::BOOL {
        let s = &mut *(l.0 as *mut S);
        let c = class_of(h);
        if c == "Progman" || c == "WorkerW" {
            if has_defview(h) {
                s.hwnd = h;
                return false.into();
            }
        }
        true.into()
    }
    let mut s = S { hwnd: HWND::default() };
    let _ = unsafe { EnumWindows(Some(cb), LPARAM(&mut s as *mut S as isize)) };
    s.hwnd
}

/// 插到锚点正上方（壁纸之上、普通窗口之下）
pub fn insert_above_desktop(hwnd: HWND) -> Result<HWND> {
    let a = anchor();
    if a.0.is_null() {
        return Err(windows::core::Error::from_thread());
    }
    unsafe {
        SetWindowPos(hwnd, Some(a), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE)?;
        Ok(a)
    }
}

/// 当前窗口在 z 序里的上下邻居（取证用）
pub fn neighbors(hwnd: HWND) -> (String, String) {
    unsafe {
        let prev = FindWindowExW(None, Some(hwnd), PCWSTR::null(), PCWSTR::null())
            .map(|h| class_of(h))
            .unwrap_or_default();
        // 自身之后的那个（z 序更下）
        let next = windows::Win32::UI::WindowsAndMessaging::GetWindow(hwnd, GW_HWNDNEXT).ok();
        let nextc = match next {
            Some(h) if !h.0.is_null() => class_of(h),
            _ => String::new(),
        };
        (prev, nextc)
    }
}

/// 取证宿主：一个置顶小窗，里面用 DWM 缩略图显示 `target` 的合成结果
pub fn spawn_viewer(target: HWND, w: i32, h: i32, x: i32, y: i32) -> Result<HWND> {
    unsafe {
        let hinst = windows::Win32::System::LibraryLoader::GetModuleHandleW(None)?;
        let cls = w!("MiniCardM0Viewer");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(viewer_proc),
            hInstance: hinst.into(),
            lpszClassName: cls,
            ..Default::default()
        };
        RegisterClassW(&wc);
        let v = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            cls,
            w!("MiniCard M0 viewer"),
            WS_POPUP | WS_VISIBLE,
            x,
            y,
            w,
            h,
            None,
            None,
            Some(hinst.into()),
            None,
        )?;
        let thumb: isize = DwmRegisterThumbnail(v, target)?;
        let props = DWM_THUMBNAIL_PROPERTIES {
            dwFlags: DWM_TNP_VISIBLE | DWM_TNP_RECTDESTINATION,
            rcDestination: RECT { left: 0, top: 0, right: w, bottom: h },
            rcSource: RECT::default(),
            fVisible: true.into(),
            fSourceClientAreaOnly: false.into(),
            opacity: 255,
        };
        DwmUpdateThumbnailProperties(thumb, &props)?;
        let _ = ShowWindow(v, windows::Win32::UI::WindowsAndMessaging::SW_SHOWNOACTIVATE);
        Ok(v)
    }
}

unsafe extern "system" fn viewer_proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        windows::Win32::UI::WindowsAndMessaging::WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(h, msg, wp, lp),
    }
}
