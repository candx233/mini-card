//! 桌面窗口/进程的一点点 Win32 胶水（Dock 的「运行中」与点击行为用）。
//!
//! 口径与 `src-tauri/src/dock.rs` 的 `running_set / same_app` **保持一致**：
//! 只看**可见顶层窗口**（比枚举进程准：有 UI 才算「在跑」），exe 路径小写比较，
//! 同一安装树也算同一应用（Steam 这类 launcher：条目是 Steam.exe，
//! 真正开窗的是 bin\cef\…\steamwebhelper.exe）。

use std::collections::{HashMap, HashSet};

use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, RECT};
use windows::Win32::System::Threading::{
    AttachThreadInput, GetCurrentThreadId, OpenProcess, QueryFullProcessImageNameW,
    PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    keybd_event, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, FlashWindowEx, GetClassNameW, GetForegroundWindow, GetWindow, GetWindowLongPtrW,
    GetWindowRect, GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindowVisible,
    SetForegroundWindow, SetWindowPos, ShowWindow, SwitchToThisWindow, FLASHWINFO, FLASHW_ALL,
    FLASHW_TIMERNOFG, GWL_EXSTYLE, GW_OWNER, HWND_TOP, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    SW_MINIMIZE, SW_RESTORE, SW_SHOW, WS_EX_TOOLWINDOW,
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

/// 一个顶层窗口的快照（**含隐藏窗**）。QQ 关到托盘时主窗 `IsWindowVisible=false`，
/// 上一版只枚举可见窗 → 匹配不到 → 回退 ShellExecute，表现为「点通知又开一个新 QQ」。
#[derive(Debug, Clone)]
struct WinRow {
    hwnd: isize,
    exe: String,
    title: String,
    class: String,
    rect: RECT,
    visible: bool,
    iconic: bool,
    owner: isize,
    tool: bool,
}

unsafe extern "system" fn enum_all_cb(hwnd: HWND, lparam: LPARAM) -> windows::core::BOOL {
    let rows = &mut *(lparam.0 as *mut Vec<(isize, u32)>);
    let mut pid = 0u32;
    GetWindowThreadProcessId(hwnd, Some(&mut pid));
    if pid != 0 {
        rows.push((hwnd.0 as isize, pid));
    }
    true.into()
}

/// 全部顶层窗（含隐藏 / 最小化）。与 `visible_windows()` 分开：Dock 的「运行中」
/// 仍按可见口径，这里只给「点通知激活发送方」用。
fn all_windows() -> Vec<WinRow> {
    let mut raw: Vec<(isize, u32)> = Vec::new();
    unsafe {
        let p = &mut raw as *mut Vec<(isize, u32)>;
        let _ = EnumWindows(Some(enum_all_cb), LPARAM(p as isize));
    }
    let mut exe_cache: HashMap<u32, Option<String>> = HashMap::new();
    let mut out = Vec::with_capacity(raw.len());
    for (h, pid) in raw {
        let exe = exe_cache.entry(pid).or_insert_with(|| exe_of(pid)).clone();
        let Some(exe) = exe else { continue };
        let hwnd = HWND(h as *mut _);
        unsafe {
            let mut title = [0u16; 256];
            let n = GetWindowTextW(hwnd, &mut title) as usize;
            let mut class = [0u16; 256];
            let m = GetClassNameW(hwnd, &mut class) as usize;
            let mut r = RECT::default();
            let _ = GetWindowRect(hwnd, &mut r);
            let owner = GetWindow(hwnd, GW_OWNER).map(|x| x.0 as isize).unwrap_or(0);
            let ex_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            out.push(WinRow {
                hwnd: h,
                exe,
                title: String::from_utf16_lossy(&title[..n]),
                class: String::from_utf16_lossy(&class[..m]),
                rect: r,
                visible: IsWindowVisible(hwnd).as_bool(),
                iconic: IsIconic(hwnd).as_bool(),
                owner,
                tool: ex_style & WS_EX_TOOLWINDOW.0 as isize != 0,
            });
        }
    }
    out
}

/// 像不像「可直接激活的主窗」：挡掉 IME/辅助窗、0×0 与小工具条（与 Dock 的 120×90 同口径）
fn usable(w: &WinRow) -> bool {
    let ww = w.rect.right - w.rect.left;
    let hh = w.rect.bottom - w.rect.top;
    if ww < 120 || hh < 90 || w.tool {
        return false;
    }
    let c = w.class.as_str();
    !(c == "IME" || c == "MSCTFIME UI" || c.starts_with("GDI+"))
}

fn hit_keys(w: &WinRow, keys: &[String]) -> bool {
    let stem = base(&w.exe).trim_end_matches(".exe");
    keys.iter().any(|k| {
        k.len() >= 2
            && (stem == k.as_str()
                || (k.len() >= 4 && stem.contains(k.as_str()))
                || (stem.len() >= 4 && k.contains(stem)))
    })
}

fn title_hit(w: &WinRow, keys: &[String]) -> bool {
    let t = w.title.to_lowercase();
    keys.iter().any(|k| k.chars().count() >= 2 && t.contains(k.as_str()))
}

/// 在「可见 / 隐藏」这一档里挑最像主窗的：标题命中 > 无 owner > 非最小化 > 面积大。
/// QQ 现场（5 个隐藏窗）会选中 1054×695 的主面板，而不是会话窗/小工具窗。
fn pick_main<'a>(wins: &'a [WinRow], keys: &[String], want_visible: bool) -> Option<&'a WinRow> {
    let mut best: Option<(&WinRow, i64)> = None;
    for w in wins {
        if w.visible != want_visible || !usable(w) || !hit_keys(w, keys) {
            continue;
        }
        let area = (w.rect.right - w.rect.left).max(0) as i64
            * (w.rect.bottom - w.rect.top).max(0) as i64;
        let score = if title_hit(w, keys) { 1_000_000_000i64 } else { 0 }
            + if w.owner == 0 { 100_000_000i64 } else { 0 }
            + if !w.iconic { 10_000_000i64 } else { 0 }
            + area;
        if best.map_or(true, |(_, s)| score > s) {
            best = Some((w, score));
        }
    }
    best.map(|(w, _)| w)
}

/// 抢前台四连兜底（与 `src-tauri/src/dock.rs:784-851` 同口径）：
/// SetForegroundWindow → AttachThreadInput → 合成 ALT → SwitchToThisWindow；
/// 全败抬到 z 顶 + 闪任务栏。返回值 = 是否真拿到前台。
fn raise_window(hwnd: isize) -> bool {
    let h = HWND(hwnd as *mut _);
    unsafe {
        if GetForegroundWindow() == h {
            return true;
        }
        if SetForegroundWindow(h).as_bool() {
            return true;
        }
        // ① 临时把输入队列挂到前台线程上再抢
        let fg = GetForegroundWindow();
        let fg_tid = GetWindowThreadProcessId(fg, None);
        let me = GetCurrentThreadId();
        if fg_tid != 0 && fg_tid != me {
            let _ = AttachThreadInput(me, fg_tid, true);
            let ok = SetForegroundWindow(h).as_bool();
            let _ = AttachThreadInput(me, fg_tid, false);
            if ok {
                return true;
            }
        }
        // ② 合成一次 ALT：把「最后输入事件」抢到本进程，前台锁就放行
        const VK_MENU: u8 = 0x12;
        keybd_event(VK_MENU, 0, KEYBD_EVENT_FLAGS(0), 0);
        keybd_event(VK_MENU, 0, KEYEVENTF_KEYUP, 0);
        if SetForegroundWindow(h).as_bool() {
            return true;
        }
        // ③ 未公开但任务栏那套一直在用
        SwitchToThisWindow(h, true);
        if GetForegroundWindow() == h {
            return true;
        }
        let _ = SetWindowPos(
            h,
            Some(HWND_TOP),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
        let fwi = FLASHWINFO {
            cbSize: std::mem::size_of::<FLASHWINFO>() as u32,
            hwnd: h,
            dwFlags: FLASHW_ALL | FLASHW_TIMERNOFG,
            uCount: 3,
            dwTimeout: 0,
        };
        let _ = FlashWindowEx(&fwi);
        false
    }
}

/// 匹配键：应用名 / 去空格名 / AUMID（包名!AppId 拆开）/ 常见别名表
fn keys_of(app: &str, aumid: &str) -> Vec<String> {
    fn push(keys: &mut Vec<String>, s: &str) {
        let k = s.trim().to_lowercase();
        if k.chars().count() >= 2 && !keys.iter().any(|x| x == &k) {
            keys.push(k);
        }
    }
    let mut keys: Vec<String> = Vec::new();
    push(&mut keys, app);
    push(&mut keys, &app.replace(' ', ""));
    if let Some((pkg, appid)) = aumid.split_once('!') {
        push(&mut keys, appid);
        push(&mut keys, pkg.split('_').next().unwrap_or(pkg));
    } else {
        push(&mut keys, aumid);
    }
    let a = app.to_lowercase();
    for (needle, exe) in [
        ("qq", "qq"),
        ("微信", "wechat"),
        ("wechat", "wechat"),
        ("邮件", "outlook"),
        ("mail", "outlook"),
        ("outlook", "outlook"),
        ("edge", "msedge"),
        ("浏览器", "msedge"),
        ("画图", "mspaint"),
        ("paint", "mspaint"),
        ("记事本", "notepad"),
        ("notepad", "notepad"),
        ("计算器", "calculator"),
        ("calculator", "calculator"),
        ("截图", "snippingtool"),
        ("snipping", "snippingtool"),
        ("终端", "windowsterminal"),
        ("terminal", "windowsterminal"),
    ] {
        if a.contains(needle) {
            push(&mut keys, exe);
        }
    }
    keys
}

/// 通知等场景：按应用名/AUMID 找到**已在运行**的窗口并切到前台。
/// 先找可见窗（保持旧行为），没有再找**隐藏窗**（QQ 关到托盘 = 5 个 Chrome_WidgetWin_1
/// 全 IsWindowVisible=false）；只要认领到窗口就返回 true，调用方绝不能再 ShellExecute。
pub fn activate_app(app: &str, aumid: &str) -> bool {
    let keys = keys_of(app, aumid);
    if keys.is_empty() {
        return false;
    }
    let wins = all_windows();

    // ① 可见窗：保持旧行为；只对**最小化**的做 SW_RESTORE（别把最大化窗口还原成小窗）
    if let Some(w) = pick_main(&wins, &keys, true) {
        unsafe {
            if w.iconic {
                let _ = ShowWindow(HWND(w.hwnd as *mut _), SW_RESTORE);
            }
        }
        let fg = raise_window(w.hwnd);
        minicard_shell::mlog!("通知激活：{app} 可见窗 hwnd={} 到前台={fg}", w.hwnd);
        return true;
    }

    // ② 隐藏窗：QQ 关到托盘走这条（ShowWindow(SW_SHOW) 实测可拉起 Electron 主窗）
    if let Some(w) = pick_main(&wins, &keys, false) {
        unsafe {
            let cmd = if w.iconic { SW_RESTORE } else { SW_SHOW };
            let _ = ShowWindow(HWND(w.hwnd as *mut _), cmd);
        }
        let visible = unsafe { IsWindowVisible(HWND(w.hwnd as *mut _)).as_bool() };
        let fg = raise_window(w.hwnd);
        minicard_shell::mlog!(
            "通知激活：{app} 隐藏窗 hwnd={} 已显示={visible} 到前台={fg}",
            w.hwnd
        );
        // 窗口已认领：即使显示/抢前台失败，也不能再启动，避免双开
        return true;
    }
    false
}

/// 点图标：**已在跑就切到前台**（没有前台窗口时先 restore），否则 ShellExecute 启动。
/// 返回给日志用的一句话。
pub fn click(item_path: &str, args: &str) -> String {
    // 验收钩子：模拟「打开要很久」的条目（不真启动应用），用来量点击期间帧是否还在走
    if let Ok(ms) = std::env::var("MC_DOCK_SLOW") {
        if let Ok(ms) = ms.parse::<u64>() {
            std::thread::sleep(std::time::Duration::from_millis(ms));
            return "launched".into();
        }
    }
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


/// 「添加应用」用的系统文件选择框（comdlg32 的 GetOpenFileNameW，同步阻塞）
pub fn pick_exe() -> Option<String> {
    use windows::Win32::UI::Controls::Dialogs::{
        GetOpenFileNameW, OFN_FILEMUSTEXIST, OFN_PATHMUSTEXIST, OPENFILENAMEW,
    };
    let mut buf = [0u16; 1024];
    let filter: Vec<u16> = "程序 (*.exe;*.lnk)\0*.exe;*.lnk\0所有文件 (*.*)\0*.*\0\0"
        .encode_utf16()
        .collect();
    let title: Vec<u16> = "选择要添加到 Dock 的程序\0".encode_utf16().collect();
    let mut ofn = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        lpstrFilter: windows::core::PCWSTR(filter.as_ptr()),
        lpstrFile: windows::core::PWSTR(buf.as_mut_ptr()),
        nMaxFile: buf.len() as u32,
        lpstrTitle: windows::core::PCWSTR(title.as_ptr()),
        Flags: OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST,
        ..Default::default()
    };
    let ok = unsafe { GetOpenFileNameW(&mut ofn) };
    if !ok.as_bool() {
        return None;
    }
    let n = buf.iter().position(|c| *c == 0).unwrap_or(0);
    if n == 0 {
        None
    } else {
        Some(String::from_utf16_lossy(&buf[..n]))
    }
}

/// 唤出迷你卡片主窗（Dock 右键「Dock 设置」走这条，不需要跟外壳加 IPC）
pub fn raise_main_window() -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{
        FindWindowW, IsIconic, SetForegroundWindow, ShowWindow, SW_RESTORE, SW_SHOW,
    };
    unsafe {
        let Ok(h) = FindWindowW(
            windows::core::w!("Tauri Window"),
            windows::core::w!("Mini Card"),
        ) else {
            return false;
        };
        let _ = ShowWindow(h, if IsIconic(h).as_bool() { SW_RESTORE } else { SW_SHOW });
        let _ = SetForegroundWindow(h);
    }
    true
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

    /// 真机手测：QQ 关到托盘（窗口全隐藏）时，点通知应把已有主窗拉起来而不是再开一个。
    /// 默认 ignore（会改变前台窗口，需要人手把 QQ 藏回托盘）。
    #[test]
    #[ignore = "会真的把 QQ 拉到前台"]
    fn qq已有窗口激活() {
        assert!(activate_app("QQ", "QQ"), "没找到运行中的 QQ 窗口");
    }

    fn row(
        hwnd: isize,
        exe: &str,
        title: &str,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        visible: bool,
        iconic: bool,
        owner: isize,
    ) -> WinRow {
        WinRow {
            hwnd,
            exe: exe.to_string(),
            title: title.to_string(),
            class: "Chrome_WidgetWin_1".to_string(),
            rect: RECT {
                left: x,
                top: y,
                right: x + w,
                bottom: y + h,
            },
            visible,
            iconic,
            owner,
            tool: false,
        }
    }

    /// 2026-11-14 现场：QQ 关托盘时 5 个 Chrome_WidgetWin_1 全隐藏；主面板 1054×695。
    /// 修复前只枚举可见窗 → 找不到 → ShellExecute 再开一个 QQ；修复后应选中隐藏主窗。
    #[test]
    fn 隐藏主窗按标题与面积选中() {
        let keys = keys_of("QQ", "QQ");
        let wins = vec![
            row(100, r"e:\qq\qq.exe", "QQ", 560, 256, 800, 600, false, false, 0),
            row(101, r"e:\qq\qq.exe", "QQ", 434, 199, 1054, 695, false, false, 0),
            row(102, r"e:\qq\qq.exe", "QQ", 959, 555, 48, 39, false, false, 0),
            row(103, r"e:\other\wechat.exe", "微信", 0, 0, 1920, 1080, false, false, 0),
        ];
        let hit = pick_main(&wins, &keys, false).expect("应找到隐藏主窗");
        assert_eq!(hit.hwnd, 101, "应选面积最大的 1054×695 主面板");
        // 可见档里没有 → 旧逻辑确实会漏（这正是 bug 的入口）
        assert!(pick_main(&wins, &keys, true).is_none());
    }

    /// 标题命中优先于纯面积（避免把标题为空的窗口/会话窗当主窗）
    #[test]
    fn 标题命中优先于面积() {
        let keys = keys_of("QQ", "QQ");
        let wins = vec![
            row(200, r"e:\qq\qq.exe", "", 0, 0, 1600, 900, false, false, 0),
            row(201, r"e:\qq\qq.exe", "QQ", 0, 0, 300, 200, false, false, 0),
        ];
        assert_eq!(pick_main(&wins, &keys, false).unwrap().hwnd, 201);
    }

    /// 可见窗与隐藏窗分两档选：可见档不得把隐藏窗选进来（保持旧行为）
    #[test]
    fn 可见与隐藏分档() {
        let keys = keys_of("QQ", "QQ");
        let wins = vec![
            row(300, r"e:\qq\qq.exe", "QQ", 0, 0, 500, 400, true, false, 0),
            row(301, r"e:\qq\qq.exe", "QQ", 0, 0, 1054, 695, false, false, 0),
        ];
        assert_eq!(pick_main(&wins, &keys, true).unwrap().hwnd, 300);
        assert_eq!(pick_main(&wins, &keys, false).unwrap().hwnd, 301);
    }

    /// 应用名 / AUMID / 别名表都能产出匹配键（QQ 的 AUMID 实测就是 "QQ"）
    #[test]
    fn 匹配键覆盖应用名与_aumid() {
        let k = keys_of("QQ", "QQ");
        assert!(k.iter().any(|x| x == "qq"), "{k:?}");
        let k = keys_of("WeChat", "Tencent.WeChat_8wekyb3d8bbwe!App");
        assert!(k.iter().any(|x| x == "wechat"), "{k:?}");
        assert!(k.iter().any(|x| x == "tencent.wechat"), "{k:?}");
        assert!(!keys_of("", "").iter().any(|x| x.is_empty()));
    }
}
