//! 顶层窗口「开/关/显/隐」的事件钩子（给 Dock 的运行点用）。
//!
//! 之前只靠 2s 轮询，用户点了应用要等最多 2 秒小点才亮。这里挂 `SetWinEventHook`
//! （OUTOFCONTEXT 回调进本线程的消息队列），窗口一有变化就把 `DIRTY` 置位，
//! 监视线程立刻补扫一次 —— 实测把延迟从「≤2s」压到「≤100ms」。

use std::sync::atomic::{AtomicBool, Ordering};

pub static DIRTY: AtomicBool = AtomicBool::new(false);

unsafe extern "system" fn on_event(
    _hook: windows::Win32::UI::Accessibility::HWINEVENTHOOK,
    _event: u32,
    hwnd: windows::Win32::Foundation::HWND,
    idobject: i32,
    idchild: i32,
    _thread: u32,
    _time: u32,
) {
    // 只关心窗口对象本身（子对象/光标之类的事件量太大）
    if idobject == 0 && idchild == 0 && !hwnd.0.is_null() {
        DIRTY.store(true, Ordering::Relaxed);
    }
}

/// 在当前线程装钩子（OUTOFCONTEXT 回调需要本线程有消息循环，见 `pump`）
pub fn install() -> bool {
    use windows::Win32::UI::Accessibility::{SetWinEventHook, HWINEVENTHOOK};
    use windows::Win32::UI::WindowsAndMessaging::{
        EVENT_OBJECT_CREATE, EVENT_OBJECT_HIDE, WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS,
    };
    unsafe {
        let h: HWINEVENTHOOK = SetWinEventHook(
            EVENT_OBJECT_CREATE,
            EVENT_OBJECT_HIDE,
            None,
            Some(on_event),
            0,
            0,
            WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
        );
        let ok = !h.is_invalid();
        if ok {
            minicard_shell::mlog!("窗口事件钩子已挂上（EVENT_OBJECT_CREATE..HIDE）");
        } else {
            minicard_shell::mlog!("SetWinEventHook 失败，运行点退回 2s 轮询");
        }
        ok
    }
}

/// 抽消息 100ms（同时让钩子回调有机会跑），返回这段时间有没有事件
pub fn pump(ms: u64) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, PeekMessageW, MSG, PM_REMOVE,
    };
    let t0 = std::time::Instant::now();
    let mut got = false;
    unsafe {
        let mut msg = MSG::default();
        while t0.elapsed() < std::time::Duration::from_millis(ms) {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = DispatchMessageW(&msg);
                got = true;
            }
            std::thread::sleep(std::time::Duration::from_millis(8));
        }
    }
    got && DIRTY.swap(false, Ordering::Relaxed) || DIRTY.swap(false, Ordering::Relaxed)
}
