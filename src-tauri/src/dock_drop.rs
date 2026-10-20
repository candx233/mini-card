//! Dock 的文件拖放接收（自注册 OLE IDropTarget）。
//!
//! 为什么不靠 wry：wry 0.55 / 0.57 的 `webview2/drag_drop.rs::inject_in_hwnd` 只在
//! 「创建时**已经注册过**拖放目标」的子窗上挂 IDropTarget（先 `RevokeDragDrop`，成功才
//! `RegisterDragDrop`），而本机 WebView2 真正的渲染子窗 `Chrome_RenderWidgetHostHWND`
//! 上没有目标 → OLE 找不到接收方，光标显示「禁止」，Tauri 的 `WindowEvent::DragDrop`
//! 一次都不来（实测：主窗 / 卡片窗同样收不到，是 wry 这条链在 WebView2 上的缺口）。
//!
//! 这里在窗口建好后把**顶层窗 + 所有 WebView2 子窗**都注册成自己的目标，语义与 wry 一致：
//! Enter / Over 推插入标线（页面 `__setDrop`），Drop 才真插（走 `dock_add`）。
//! ⚠️ 注册必须发生在**拥有该窗口的线程**（UI 线程），否则 `RegisterDragDrop` 返
//! `RPC_E_WRONG_THREAD` —— 所以 installer 用 `AppHandle::run_on_main_thread` 回主线程做。

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};

use tauri::{AppHandle, Manager};
use windows::core::{implement, Ref, Result as WinResult};
use windows::Win32::Foundation::{HWND, LPARAM, POINT, POINTL};
use windows::Win32::System::Com::{
    IDataObject, DVASPECT_CONTENT, FORMATETC, STGMEDIUM, TYMED_HGLOBAL,
};
use windows::Win32::System::Ole::{
    IDropTarget, IDropTarget_Impl, RegisterDragDrop, ReleaseStgMedium, RevokeDragDrop, CF_HDROP,
    DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_NONE,
};
use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};
use windows::Win32::Graphics::Gdi::ScreenToClient;
use windows::Win32::UI::WindowsAndMessaging::{EnumChildWindows, GetClassNameW};

/// 拖放回调（都在 UI 线程上调用）：坐标是**窗内物理 px**（与 wry 的 DragDropEvent 同口径）。
pub struct Hooks {
    pub enter: Box<dyn Fn(i32, i32, Vec<String>)>,
    pub over: Box<dyn Fn(i32, i32)>,
    pub leave: Box<dyn Fn()>,
    pub drop: Box<dyn Fn(i32, i32, Vec<String>)>,
}

// 已注册的目标必须活着：IDropTarget 一旦释放，OLE 那边的注册就变成悬空。
// 它 !Send（COM 指针），而且只属于 UI 线程 → 用 thread_local 存。
thread_local! {
    static LIVE: RefCell<Vec<IDropTarget>> = const { RefCell::new(Vec::new()) };
}
/// 装成功一次就别再重复装（重复装会先 Revoke 自己的目标、把拖动中的状态打断）。
static DONE: AtomicBool = AtomicBool::new(false);

#[implement(IDropTarget)]
struct Target {
    hwnd: HWND,
    hooks: Rc<Hooks>,
    valid: RefCell<bool>,
    effect: RefCell<DROPEFFECT>,
}

impl Target {
    fn new(hwnd: HWND, hooks: Rc<Hooks>) -> Self {
        Self {
            hwnd,
            hooks,
            valid: RefCell::new(false),
            effect: RefCell::new(DROPEFFECT_NONE),
        }
    }
}

/// 从 IDataObject 取 CF_HDROP 里的文件路径（不是文件就返回空 → 目标不接）。
fn hdrop_paths(data: &IDataObject) -> Vec<String> {
    let fmt = FORMATETC {
        cfFormat: CF_HDROP.0,
        ptd: std::ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT.0,
        lindex: -1,
        tymed: TYMED_HGLOBAL.0 as u32,
    };
    unsafe {
        let Ok(medium) = data.GetData(&fmt) else {
            return Vec::new();
        };
        let hdrop = HDROP(medium.u.hGlobal.0);
        let count = DragQueryFileW(hdrop, u32::MAX, None);
        let mut out = Vec::with_capacity(count as usize);
        for i in 0..count {
            let len = DragQueryFileW(hdrop, i, None) as usize;
            let mut buf = vec![0u16; len + 1];
            DragQueryFileW(hdrop, i, Some(&mut buf));
            out.push(String::from_utf16_lossy(&buf[..len]));
        }
        // medium 归我们了（GetData 成功即所有权转移），读完全部路径再释放
        let mut m = medium;
        ReleaseStgMedium(&mut m as *mut STGMEDIUM);
        out
    }
}

fn client_xy(hwnd: HWND, pt: &POINTL) -> (i32, i32) {
    let mut p = POINT { x: pt.x, y: pt.y };
    unsafe {
        let _ = ScreenToClient(hwnd, &mut p);
    }
    (p.x, p.y)
}

#[allow(non_snake_case)]
impl IDropTarget_Impl for Target_Impl {
    fn DragEnter(
        &self,
        data: Ref<'_, IDataObject>,
        _keys: MODIFIERKEYS_FLAGS,
        pt: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> WinResult<()> {
        let paths = data.as_ref().map(hdrop_paths).unwrap_or_default();
        crate::mlog(&format!("dock drop(own): DragEnter hwnd={:?} paths={}", self.hwnd, paths.len()));
        let ok = !paths.is_empty();
        *self.valid.borrow_mut() = ok;
        let eff = if ok { DROPEFFECT_COPY } else { DROPEFFECT_NONE };
        *self.effect.borrow_mut() = eff;
        unsafe {
            if !effect.is_null() {
                *effect = eff;
            }
        }
        if ok {
            let (x, y) = client_xy(self.hwnd, pt);
            (self.hooks.enter)(x, y, paths);
        }
        Ok(())
    }

    fn DragOver(
        &self,
        _keys: MODIFIERKEYS_FLAGS,
        pt: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> WinResult<()> {
        let on = *self.valid.borrow();
        unsafe {
            if !effect.is_null() {
                *effect = *self.effect.borrow();
            }
        }
        if on {
            let (x, y) = client_xy(self.hwnd, pt);
            (self.hooks.over)(x, y);
        }
        Ok(())
    }

    fn DragLeave(&self) -> WinResult<()> {
        if *self.valid.borrow() {
            *self.valid.borrow_mut() = false;
            (self.hooks.leave)();
        }
        Ok(())
    }

    fn Drop(
        &self,
        data: Ref<'_, IDataObject>,
        _keys: MODIFIERKEYS_FLAGS,
        pt: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> WinResult<()> {
        let paths = data.as_ref().map(hdrop_paths).unwrap_or_default();
        let was = *self.valid.borrow();
        *self.valid.borrow_mut() = false;
        unsafe {
            if !effect.is_null() {
                *effect = if paths.is_empty() { DROPEFFECT_NONE } else { DROPEFFECT_COPY };
            }
        }
        if was && !paths.is_empty() {
            let (x, y) = client_xy(self.hwnd, pt);
            (self.hooks.drop)(x, y, paths);
        }
        Ok(())
    }
}

fn class_of(h: HWND) -> String {
    let mut buf = [0u16; 128];
    let n = unsafe { GetClassNameW(h, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

/// 把 parent 及其所有后代子窗注册成拖放目标。返回成功数。
fn register_all(parent: HWND, hooks: &Rc<Hooks>) -> usize {
    let mut list: Vec<HWND> = vec![parent];
    unsafe extern "system" fn cb(h: HWND, lp: LPARAM) -> windows::core::BOOL {
        let v = unsafe { &mut *(lp.0 as *mut Vec<HWND>) };
        v.push(h);
        windows::core::BOOL(1)
    }
    let mut kids: Vec<HWND> = Vec::new();
    unsafe {
        let _ = EnumChildWindows(Some(parent), Some(cb), LPARAM(&mut kids as *mut _ as isize));
    }
    list.extend(kids);

    let mut n = 0usize;
    let mut live: Vec<IDropTarget> = Vec::new();
    for h in list {
        // D3D 合成窗不接窗口消息，注册上去只会白占一个 COM 对象
        if class_of(h) == "Intermediate D3D Window" {
            continue;
        }
        let t: IDropTarget = Target::new(h, hooks.clone()).into();
        unsafe {
            let _ = RevokeDragDrop(h);
            match RegisterDragDrop(h, &t) {
                Ok(()) => {
                    live.push(t);
                    n += 1;
                }
                Err(e) => crate::mlog(&format!("dock drop: RegisterDragDrop({h:?}) ERR {e}")),
            }
        }
    }
    if n > 0 {
        LIVE.with(|v| v.borrow_mut().extend(live));
    }
    n
}

/// 异步安装（建窗时调用）：WebView2 的子窗是异步出生的，装不上就每 250ms 重试一次，
/// 最多 6s；成功后不再重复装。`hooks_of` 在主线程上被调用（构造 Rc，不能跨线程）。
pub fn install_async(app: AppHandle, label: &'static str, hooks_of: fn(tauri::WebviewWindow) -> Hooks) {
    std::thread::Builder::new()
        .name("mc-dock-drop".into())
        .spawn(move || {
            for _ in 0..24 {
                std::thread::sleep(std::time::Duration::from_millis(250));
                if DONE.load(Ordering::Relaxed) {
                    return;
                }
                let Some(win) = app.get_webview_window(label) else {
                    return; // 窗没了（关掉 Dock / 退出），不用再装
                };
                let Some(hwnd) = crate::hwnd_of(&win) else {
                    continue;
                };
                let hwnd_val = hwnd as isize;   // 裸指针 !Send：转成整数带进主线程闭包
                let (tx, rx) = std::sync::mpsc::channel::<usize>();
                let win2 = win.clone();
                let app2 = app.clone();
                let _ = app.run_on_main_thread(move || {
                    let hwnd = HWND(hwnd_val as *mut core::ffi::c_void);
                    let hooks = Rc::new(hooks_of(win2));
                    let n = register_all(hwnd, &hooks);
                    let _ = tx.send(n);
                });
                if let Ok(n) = rx.recv_timeout(std::time::Duration::from_secs(3)) {
                    if n > 0 {
                        DONE.store(true, Ordering::Relaxed);
                        crate::mlog(&format!("dock drop: IDropTarget 已注册（{n} 个窗口）"));
                        return;
                    }
                }
                let _ = app2; // 保持 handle 存活到本轮结束
            }
            crate::mlog("dock drop: IDropTarget 注册超时（6s）");
        })
        .ok();
}
