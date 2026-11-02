//! 桌面窗口/进程的一点点 Win32 胶水（Dock 的「运行中」与点击行为用）。
//!
//! 口径与 `src-tauri/src/dock.rs` 的 `running_set / same_app` **保持一致**：
//! 只看**可见顶层窗口**（比枚举进程准：有 UI 才算「在跑」），exe 路径小写比较，
//! 同一安装树也算同一应用（Steam 这类 launcher：条目是 Steam.exe，
//! 真正开窗的是 bin\cef\…\steamwebhelper.exe）。

use std::collections::HashSet;

use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowThreadProcessId, IsWindowVisible, SetForegroundWindow, ShowWindow,
    SW_MINIMIZE, SW_RESTORE,
};

fn base(p: &str) -> &str {
    p.rsplit('\\').next().unwrap_or(p)
}

/// 同一应用判定（与外壳 `same_app` 同规则）
pub fn same_app(item_key: &str, exe: &str) -> bool {
    if item_key == exe {
        return true;
    }
    let sys_like = |p: &str| p.starts_with("c:\\windows\\system32\\") || p.contains("\\windowsapps\\");
    if sys_like(item_key) && base(item_key) == base(exe) {
        return true;
    }
    if !sys_like(item_key) {
        if let Some((dir, _)) = item_key.rsplit_once('\\') {
            if dir.len() > 6
                && exe.len() > dir.len() + 1
                && exe.starts_with(dir)
                && exe.as_bytes().get(dir.len()) == Some(&b'\\')
            {
                return true;
            }
        }
    }
    false
}


/// `.lnk` → 目标路径（IShellLinkW；拿不到 = None）。带进程内缓存。
/// Dock 里 14/16 个条目是桌面快捷方式，不解析就等于「明明开着却永远不亮运行点」。
pub fn lnk_target(path: &str) -> Option<String> {
    use std::collections::HashMap;
    use std::sync::Mutex;
    use windows::core::{Interface, HSTRING};
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
        IPersistFile, STGM_READ,
    };
    use windows::Win32::UI::Shell::{IShellLinkW, ShellLink, SLGP_RAWPATH};

    static CACHE: Mutex<Option<HashMap<String, Option<String>>>> = Mutex::new(None);
    let key = path.to_lowercase();
    {
        let mut c = CACHE.lock().unwrap();
        let map = c.get_or_insert_with(HashMap::new);
        if let Some(v) = map.get(&key) {
            return v.clone();
        }
    }
    if !key.ends_with(".lnk") {
        return None;
    }
    let out = (|| -> Option<String> {
        unsafe {
            // 已有别的套间模型就继续用（RPC_E_CHANGED_MODE 不影响读 lnk）
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let link: IShellLinkW =
                CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
            let pf: IPersistFile = link.cast().ok()?;
            pf.Load(&HSTRING::from(path), STGM_READ).ok()?;
            let mut buf = [0u16; 1024];
            link.GetPath(&mut buf, std::ptr::null_mut(), SLGP_RAWPATH.0 as u32).ok()?;
            let n = buf.iter().position(|c| *c == 0).unwrap_or(0);
            if n == 0 {
                return None;
            }
            Some(String::from_utf16_lossy(&buf[..n]))
        }
    })();
    CACHE.lock().unwrap().get_or_insert_with(Default::default).insert(key, out.clone());
    out
}

/// 运行态比对用的键：`.lnk` 先解析到目标 exe，再统一成反斜杠 + 小写（与外壳 `match_key` 同口径）
pub fn match_key(path: &str) -> String {
    let p = path.trim();
    if p.is_empty() {
        return String::new();
    }
    if p.starts_with("http") || p.contains("://") {
        return p.to_lowercase();
    }
    let target = lnk_target(p).unwrap_or_else(|| p.to_string());
    target.replace('/', "\\").trim_end_matches('\\').to_lowercase()
}

pub fn item_running(item_key: &str, running: &HashSet<String>) -> bool {
    !item_key.is_empty() && running.iter().any(|exe| exe == item_key || same_app(item_key, exe))
}

fn exe_of(pid: u32) -> Option<String> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut n = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(
            h,
            PROCESS_NAME_WIN32,
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut n,
        )
        .is_ok();
        let _ = CloseHandle(h);
        if !ok {
            return None;
        }
        Some(String::from_utf16_lossy(&buf[..n as usize]).to_lowercase())
    }
}

unsafe extern "system" fn enum_cb(hwnd: HWND, lparam: LPARAM) -> windows::core::BOOL {
    let rows = &mut *(lparam.0 as *mut Vec<(isize, String)>);
    if !IsWindowVisible(hwnd).as_bool() {
        return true.into();
    }
    let mut pid = 0u32;
    GetWindowThreadProcessId(hwnd, Some(&mut pid));
    if pid != 0 {
        if let Some(exe) = exe_of(pid) {
            rows.push((hwnd.0 as isize, exe));
        }
    }
    true.into()
}

/// 可见顶层窗口 → (hwnd, exe 小写)
pub fn visible_windows() -> Vec<(isize, String)> {
    let mut rows: Vec<(isize, String)> = Vec::new();
    unsafe {
        let p = &mut rows as *mut Vec<(isize, String)>;
        let _ = EnumWindows(Some(enum_cb), LPARAM(p as isize));
    }
    rows
}

/// 「运行中」集合（Dock 的运行点用；2 秒一轮足够）
pub fn running_set() -> HashSet<String> {
    visible_windows().into_iter().map(|(_, e)| e).collect()
}

/// 条目当前是否在跑（单条便利版；批量请用 `running_set` + `item_running`）
#[allow(dead_code)]
pub fn is_running(item_path: &str) -> bool {
    item_running(&match_key(item_path), &running_set())
}

/// 点图标：**已在跑就切到前台**（没有前台窗口时先 restore），否则 ShellExecute 启动。
/// 返回给日志用的一句话。
pub fn click(item_path: &str, args: &str) -> String {
    let key = match_key(item_path);
    let wins = visible_windows();
    let mine: Vec<isize> = wins
        .iter()
        .filter(|(_, exe)| same_app(&key, exe))
        .map(|(h, _)| *h)
        .collect();
    if !mine.is_empty() {
        unsafe {
            let fg = windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow();
            let target = *mine.first().unwrap();
            if fg.0 as isize == target {
                let _ = ShowWindow(HWND(target as *mut _), SW_MINIMIZE);
                return "minimized".into();
            }
            let _ = ShowWindow(HWND(target as *mut _), SW_RESTORE);
            let _ = SetForegroundWindow(HWND(target as *mut _));
        }
        return "activated".into();
    }
    match launch(item_path, args) {
        Ok(()) => "launched".into(),
        Err(e) => format!("launch-failed: {e}"),
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 与外壳 `dock::launch` 同一条路径（ShellExecuteW "open"）
pub fn launch(path: &str, args: &str) -> Result<(), String> {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    if path.trim().is_empty() {
        return Err("路径为空".into());
    }
    let op = wide("open");
    let file = wide(path);
    let params = if args.trim().is_empty() { Vec::new() } else { wide(args) };
    let r = unsafe {
        ShellExecuteW(
            None,
            windows::core::PCWSTR(op.as_ptr()),
            windows::core::PCWSTR(file.as_ptr()),
            if params.is_empty() {
                windows::core::PCWSTR::null()
            } else {
                windows::core::PCWSTR(params.as_ptr())
            },
            windows::core::PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    let code = r.0 as isize;
    if code > 32 {
        Ok(())
    } else {
        Err(format!("ShellExecuteW 返回 {code}（≤32 = 失败）"))
    }
}


/// 在资源管理器里选中该文件（右键菜单「打开所在文件夹」）
pub fn reveal_in_explorer(path: &str) -> Result<(), String> {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let p = path.replace('/', "\\");
    if p.trim().is_empty() || p.starts_with("http") || p.contains("://") {
        return Err("网址没有所在文件夹".into());
    }
    // .lnk 先解析到真实目标：直接 select 一个快捷方式会让资源管理器跳到桌面
    let target = lnk_target(&p).unwrap_or(p);
    let args = format!("/select,\"{target}\"");
    let op = wide("open");
    let file = wide("explorer.exe");
    let params = wide(&args);
    let r = unsafe {
        ShellExecuteW(
            None,
            windows::core::PCWSTR(op.as_ptr()),
            windows::core::PCWSTR(file.as_ptr()),
            windows::core::PCWSTR(params.as_ptr()),
            windows::core::PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    let code = r.0 as isize;
    if code > 32 {
        Ok(())
    } else {
        Err(format!("ShellExecuteW 返回 {code}"))
    }
}

/// Dock 右键菜单：返回用户选了哪一项（0 = 取消/关掉）。
/// `items` = (命令 id, 文案, 是否禁用)，id 由调用方分配。
pub fn popup_menu(
    owner: isize,
    screen: (i32, i32),
    items: &[(u32, String, bool)],
) -> u32 {
    use windows::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CreatePopupMenu, DestroyMenu, SetForegroundWindow, TrackPopupMenu,
        MF_GRAYED, MF_SEPARATOR, MF_STRING, TPM_RETURNCMD, TPM_RIGHTBUTTON,
    };
    unsafe {
        let Ok(menu) = CreatePopupMenu() else {
            return 0;
        };
        for (id, label, disabled) in items {
            let w = wide(label);
            let flags = if *disabled {
                MF_STRING | MF_GRAYED
            } else {
                MF_STRING
            };
            let _ = AppendMenuW(menu, flags, *id as usize, windows::core::PCWSTR(w.as_ptr()));
        }
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, windows::core::PCWSTR::null());
        let cancel = wide("取消");
        let _ = AppendMenuW(menu, MF_STRING, 0usize, windows::core::PCWSTR(cancel.as_ptr()));
        let hwnd = windows::Win32::Foundation::HWND(owner as *mut _);
        // 不抢前台的话菜单点外面不会消失（经典 Win32 坑）
        let _ = SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON,
            screen.0,
            screen.1,
            Some(0),
            hwnd,
            None,
        );
        let _ = DestroyMenu(menu);
        cmd.0 as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 同一安装树算同一应用() {
        // Steam：条目是 Steam.exe，开窗的是 bin\cef\...\steamwebhelper.exe
        assert!(same_app(
            "g:\\steam\\steam.exe",
            "g:\\steam\\bin\\cef\\cef.win7\\steamwebhelper.exe"
        ));
        // 系统目录只认文件名（Win11 记事本在 WindowsApps 里）
        assert!(same_app(
            "c:\\windows\\system32\\notepad.exe",
            "c:\\program files\\windowsapps\\notepad\\notepad.exe"
        ));
        // 不同目录的同名 exe 不算
        assert!(!same_app("c:\\a\\notepad.exe", "c:\\b\\notepad.exe"));
    }

    /// 真机：桌面一定有可见顶层窗口（explorer 等），集合不该是空的
    #[test]
    fn 真机可见窗口集合非空() {
        let set = running_set();
        assert!(!set.is_empty(), "可见窗口集合为空 → 运行点永远点不亮");
        assert!(set.iter().any(|e| e.ends_with("explorer.exe")), "连 explorer 都没有？");
    }

    /// 真机：桌面快捷方式能解析出目标 exe（拿不到 → 运行点永远是灭的）
    #[test]
    fn 快捷方式解析到_exe() {
        let lnk = "C:\\Users\\Public\\Desktop\\微信.lnk";
        if !std::path::Path::new(lnk).exists() {
            return; // 机器上没有就跳过
        }
        let t = lnk_target(lnk).expect("IShellLinkW 解析失败");
        assert!(t.to_lowercase().ends_with(".exe"), "{t}");
        assert!(std::path::Path::new(&t).exists(), "目标不存在：{t}");
    }

    #[test]
    fn 运行点判定() {
        let mut set = HashSet::new();
        set.insert("c:\\program files\\wechat\\wechat.exe".to_string());
        assert!(item_running("c:\\program files\\wechat\\wechat.exe", &set));
        assert!(!item_running("c:\\other\\wechat.exe", &set));
        assert!(!item_running("", &set));
    }
}
