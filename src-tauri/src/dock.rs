//! Dock 后端（0.1.2）：图标提取缓存 / 启动 / 运行态。
//!
//! 数据链路全部在 `docs/dock-spike.md`（真机 spike，一键复跑 `docs/verify/mc_dock_spike.py`）里验过：
//! · 图标：SHGetFileInfoW → SHGetImageList(JUMBO/EXTRALARGE/LARGE) → IImageList::GetIcon
//!   → DrawIconEx 到 32bpp DIB（BI_BITFIELDS + alpha 掩码）→ 反预乘 → PNG。
//!   ⚠️ 路径**必须反斜杠**（正斜杠一律返回 0）；DIB 是**预乘 alpha**；
//!   个别应用的 256 帧是坏的（实测 FinalShell 只有左上角一小块）→ 覆盖 <8% 逐档退化。
//! · 启动：ShellExecuteW（返回值 >32 = 成功）。
//! · 运行态：EnumWindows 可见顶层窗口（过滤 tool window / DWM cloaked；最小化仍算 —— 与任务栏同口径）。

use std::ffi::{c_void, OsStr};
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;

use crate::config::DockItem;
use windows::Win32 as w;
use windows_sys::Win32 as ws;

/// 逐档退化阈值：非透明像素占比 < 8% 视为「这档是坏的」
const MIN_COVER: f32 = 8.0;

fn wide(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
}

/// `C:/a/b` → `C:\a\b`（外壳 API 只吃反斜杠）
pub fn win_path(p: &str) -> String {
    p.replace('/', "\\")
}

/// 比较用规范化路径：反斜杠 + 小写（.lnk 解析到目标 exe 的活留到运行态联调时补）
pub fn normalize_path(p: &str) -> String {
    win_path(p).trim_end_matches('\\').to_lowercase()
}

pub fn is_url(p: &str) -> bool {
    let l = p.trim().to_lowercase();
    l.starts_with("http://") || l.starts_with("https://")
}

/// 图标缓存目录：%APPDATA%\mini-card\icons\
pub fn icons_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("mini-card").join("icons")
}

fn fnv1a_hex(s: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// 条目图标（PNG 路径）。
/// · `item.icon` 非空且文件存在 → 直接用（用户自定义）
/// · 缓存命中（缓存文件比源文件新）→ 直接用
/// · 否则现提一次；URL / 不存在的路径 → None（前端落首字母色块）
pub fn ensure_icon(item: &DockItem) -> Option<PathBuf> {
    if !item.icon.is_empty() {
        let p = PathBuf::from(&item.icon);
        if p.exists() {
            return Some(p);
        }
    }
    if is_url(&item.path) || item.path.trim().is_empty() {
        return None;
    }
    let src = PathBuf::from(win_path(&item.path));
    let cache = icons_dir().join(format!("{}.png", fnv1a_hex(&normalize_path(&item.path))));
    let fresh = match (std::fs::metadata(&src), std::fs::metadata(&cache)) {
        (Ok(s), Ok(c)) => match (s.modified(), c.modified()) {
            (Ok(sm), Ok(cm)) => cm >= sm,
            _ => true,
        },
        (Ok(_), Err(_)) => false,
        _ => cache.exists(),
    };
    if fresh && cache.exists() {
        return Some(cache);
    }
    match extract_icon_png(&item.path, &cache) {
        Ok(()) => Some(cache),
        Err(_) => None,
    }
}

/// 提取图标 → PNG（三档择优；见文件头注释）
fn extract_icon_png(path: &str, out: &std::path::Path) -> Result<(), String> {
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    unsafe {
        // 外壳图像列表是 COM 对象；已初始化过（S_FALSE）或换过套间都无害
        let _ = w::System::Com::CoInitializeEx(None, w::System::Com::COINIT_APARTMENTTHREADED);

        let idx = sysicon_index(path)?;
        let mut got: Vec<(PathBuf, f32, u32)> = Vec::new();
        let tmp_dir = std::env::temp_dir();
        for (tag, shil) in [
            ("256", w::UI::Shell::SHIL_JUMBO),
            ("48", w::UI::Shell::SHIL_EXTRALARGE),
            ("32", w::UI::Shell::SHIL_LARGE),
        ] {
            let tmp = tmp_dir.join(format!("mc_icon_{}_{}.png", fnv1a_hex(path), tag));
            match grab_one(idx, shil, &tmp) {
                Ok((size, cov)) => got.push((tmp, cov, size)),
                Err(_) => continue,
            }
        }
        let pick = got
            .iter()
            .find(|(_, cov, _)| *cov >= MIN_COVER)
            .or_else(|| got.iter().max_by(|a, b| a.1.partial_cmp(&b.1).unwrap()));
        let Some((src, _, _)) = pick else {
            return Err("三档图标全部提取失败".into());
        };
        std::fs::copy(src, out).map_err(|e| e.to_string())?;
        // 清掉临时件
        for (p, _, _) in &got {
            let _ = std::fs::remove_file(p);
        }
        Ok(())
    }
}

/// SHGetFileInfoW 取系统图像列表索引。⚠️ 路径必须反斜杠，正斜杠会返回 0。
unsafe fn sysicon_index(path: &str) -> Result<i32, String> {
    let mut sfi = w::UI::Shell::SHFILEINFOW::default();
    let p = wide(&win_path(path));
    let r = w::UI::Shell::SHGetFileInfoW(
        windows::core::PCWSTR(p.as_ptr()),
        w::Storage::FileSystem::FILE_ATTRIBUTE_NORMAL,
        Some(&mut sfi),
        std::mem::size_of::<w::UI::Shell::SHFILEINFOW>() as u32,
        w::UI::Shell::SHGFI_SYSICONINDEX,
    );
    if r == 0 {
        return Err(format!("SHGetFileInfoW 返回 0（err={}）", ws::Foundation::GetLastError()));
    }
    Ok(sfi.iIcon)
}

/// 取一档图标画到 DIB 并写 PNG，返回 (源边长, 非透明覆盖率%)
unsafe fn grab_one(idx: i32, shil: u32, out: &std::path::Path) -> Result<(u32, f32), String> {
    use w::Graphics::Gdi as gdi;
    use w::UI::Controls as ctl;
    use w::UI::WindowsAndMessaging as wam;

    let il: ctl::IImageList = w::UI::Shell::SHGetImageList(shil as i32).map_err(|e| e.to_string())?;
    let hicon = il.GetIcon(idx, ctl::ILD_TRANSPARENT.0).map_err(|e| e.to_string())?;

    let mut ii = wam::ICONINFO::default();
    wam::GetIconInfo(hicon, &mut ii).map_err(|e| e.to_string())?;
    let mut bm = gdi::BITMAP::default();
    gdi::GetObjectW(
        ii.hbmColor.into(),
        std::mem::size_of::<gdi::BITMAP>() as i32,
        Some(&mut bm as *mut _ as *mut c_void),
    );
    let (src_w, src_h) = (bm.bmWidth.max(0) as u32, bm.bmHeight.max(0) as u32);
    let size = src_w.max(src_h).clamp(16, 512);

    let mut bmi = gdi::BITMAPV5HEADER::default();
    bmi.bV5Size = std::mem::size_of::<gdi::BITMAPV5HEADER>() as u32;
    bmi.bV5Width = size as i32;
    bmi.bV5Height = -(size as i32); // top-down
    bmi.bV5Planes = 1;
    bmi.bV5BitCount = 32;
    bmi.bV5Compression = gdi::BI_BITFIELDS;
    bmi.bV5RedMask = 0x00FF_0000;
    bmi.bV5GreenMask = 0x0000_FF00;
    bmi.bV5BlueMask = 0x0000_00FF;
    bmi.bV5AlphaMask = 0xFF00_0000;

    let mut bits: *mut c_void = std::ptr::null_mut();
    let hbmp = gdi::CreateDIBSection(
        None,
        &bmi as *const _ as *const gdi::BITMAPINFO,
        gdi::DIB_RGB_COLORS,
        &mut bits,
        None,
        0,
    )
    .map_err(|e| e.to_string())?;
    let hdc = gdi::CreateCompatibleDC(None);
    let old = gdi::SelectObject(hdc, hbmp.into());
    let draw = wam::DrawIconEx(hdc, 0, 0, hicon, size as i32, size as i32, 0, None, wam::DI_NORMAL);
    let n = (size * size * 4) as usize;
    let buf = if draw.is_ok() {
        std::slice::from_raw_parts(bits as *const u8, n).to_vec()
    } else {
        Vec::new()
    };
    gdi::SelectObject(hdc, old);
    let _ = gdi::DeleteObject(hbmp.into());
    let _ = gdi::DeleteDC(hdc);
    if !ii.hbmColor.0.is_null() {
        let _ = gdi::DeleteObject(ii.hbmColor.into());
    }
    if !ii.hbmMask.0.is_null() {
        let _ = gdi::DeleteObject(ii.hbmMask.into());
    }
    let _ = wam::DestroyIcon(hicon);
    draw.map_err(|e| e.to_string())?;

    let cov = if buf.is_empty() {
        0.0
    } else {
        let opaque = buf.chunks_exact(4).filter(|px| px[3] > 8).count();
        opaque as f32 * 100.0 / (size * size) as f32
    };
    save_png(out, size, size, &buf)?;
    Ok((size, cov))
}

/// BGRA(DIB，**预乘** alpha) → PNG（直通 alpha：按 a 反预乘）
fn save_png(path: &std::path::Path, w: u32, h: u32, bgra: &[u8]) -> Result<(), String> {
    let mut rgba = Vec::with_capacity(bgra.len());
    for px in bgra.chunks_exact(4) {
        let a = px[3] as u32;
        let un = |c: u8| -> u8 {
            if a > 0 && a < 255 {
                (((c as u32) * 255 + a / 2) / a).min(255) as u8
            } else {
                c
            }
        };
        rgba.extend_from_slice(&[un(px[2]), un(px[1]), un(px[0]), px[3]]);
    }
    let img = image::RgbaImage::from_raw(w, h, rgba).ok_or("图标缓冲尺寸不符")?;
    img.save_with_format(path, image::ImageFormat::Png).map_err(|e| e.to_string())
}

/// ShellExecuteW 启动条目（exe / lnk / 文件夹 / URL）
#[allow(dead_code)] // C 段 dock_action 命令接线
pub fn launch(item: &DockItem) -> Result<(), String> {
    if item.path.trim().is_empty() {
        return Err("路径为空".into());
    }
    let op = wide("open");
    let file = wide(&item.path);
    let params: Vec<u16> = if item.args.trim().is_empty() {
        Vec::new()
    } else {
        wide(&item.args)
    };
    let r = unsafe {
        ws::UI::Shell::ShellExecuteW(
            std::ptr::null_mut(),
            op.as_ptr(),
            file.as_ptr(),
            if params.is_empty() { std::ptr::null() } else { params.as_ptr() },
            std::ptr::null(),
            ws::UI::WindowsAndMessaging::SW_SHOWNORMAL,
        )
    };
    let code = r as isize;
    if code > 32 {
        Ok(())
    } else {
        Err(format!("ShellExecuteW 返回 {code}（≤32 = 失败）"))
    }
}

#[allow(dead_code)]
unsafe fn proc_exe(pid: u32) -> String {
    use ws::System::Threading as th;
    let h = th::OpenProcess(th::PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
    if h.is_null() {
        return String::new();
    }
    let mut buf = vec![0u16; 1024];
    let mut sz = buf.len() as u32;
    let ok = th::QueryFullProcessImageNameW(h, th::PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut sz);
    ws::Foundation::CloseHandle(h);
    if ok == 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..sz as usize])
}

// ─── C 段：条目标识 / 显示名 / 打开所在文件夹 / 桌面图标显隐 ───

/// 8 位短 id（时间 + 计数器混合；不引 uuid 依赖）。dock_add 里会再查一次重
pub fn short_id() -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static SEQ: AtomicU32 = AtomicU32::new(0);
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed) as u64;
    let x = t ^ n.wrapping_mul(0x9e37_79b9_7f4a_7c15);
    format!("{:08x}", (x ^ (x >> 32)) as u32)
}

/// 显示名：`.exe/.lnk` 取主文件名；文件夹取末级目录；URL 取主机名（去 www.）；
/// 取不出来（空 / 盘根）就原样返回，绝不留空串
pub fn display_name(path: &str) -> String {
    let p = path.trim();
    if p.is_empty() {
        return String::new();
    }
    if is_url(p) {
        let rest = p.split("://").nth(1).unwrap_or(p);
        let host = rest.split(['/', '?', '#']).next().unwrap_or(rest);
        let host = host.trim_start_matches("www.");
        return if host.is_empty() { p.to_string() } else { host.to_string() };
    }
    let base = win_path(p);
    let trimmed = base.trim_end_matches('\\');
    if trimmed.is_empty() {
        return base;
    }
    let name = trimmed.rsplit('\\').next().unwrap_or(trimmed).to_string();
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && ext.len() <= 4 => stem.to_string(),
        _ => name,
    }
}

/// 「打开所在文件夹」：文件 → `explorer /select` 定位到它；文件夹 → 直接开。
/// 走 `raw_arg` 是为了让 `/select,"D:\带 空格\a.exe"` 原样进命令行（std 的自动加引号会把整串包起来，explorer 不认）
pub fn reveal(path: &str) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    use std::process::Command;
    let p = win_path(path.trim());
    if p.is_empty() {
        return Err("路径为空".into());
    }
    if is_url(&p) {
        return Err("网址没有「所在文件夹」".into());
    }
    let raw = if PathBuf::from(&p).is_dir() {
        format!("\"{p}\"")
    } else {
        format!("/select,\"{p}\"")
    };
    Command::new("explorer.exe")
        .raw_arg(raw)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("打开资源管理器失败：{e}"))
}

const EXPLORER_ADV: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced";
const HIDE_ICONS: &str = "HideIcons";

/// 读注册表真值：`Some(true)` = 桌面图标被隐藏。桌面右键「查看→显示桌面图标」改过之后，
/// 这里读到的才是实情（界面开关按它显示，别按我们自己的 config）
pub fn desktop_icons_hidden() -> Option<bool> {
    use ws::System::Registry::{
        RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE,
    };
    unsafe {
        let mut hkey: HKEY = std::ptr::null_mut();
        let st = RegOpenKeyExW(
            HKEY_CURRENT_USER,
            wide(EXPLORER_ADV).as_ptr(),
            0,
            KEY_QUERY_VALUE,
            &mut hkey,
        );
        if st != 0 {
            return None;
        }
        let name = wide(HIDE_ICONS);
        let mut ty: u32 = 0;
        let mut data: u32 = 0;
        let mut len: u32 = 4;
        let st = RegQueryValueExW(
            hkey,
            name.as_ptr(),
            std::ptr::null(),
            &mut ty,
            &mut data as *mut u32 as *mut u8,
            &mut len,
        );
        RegCloseKey(hkey);
        if st != 0 {
            return Some(false); // 值不在 = 显示桌面图标（系统默认）
        }
        Some(data != 0)
    }
}

/// 设桌面图标显隐：写 `HKCU\...\Explorer\Advanced\HideIcons` + 通知外壳刷新。
/// ⚠️ 两个实测坑：① 只写注册表不生效，必须补发 `WM_COMMAND 0x7402`；
/// ② 这条消息是**开关语义**（真机上 0x7402 发给 DefView 本体才动，发给父窗 Progman/WorkerW 没反应），
/// 所以先读当前真值，状态真变了才发一次 —— 否则会把用户现有状态翻个面
pub fn set_desktop_icons(hide: bool) -> Result<(), String> {
    use ws::System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE,
        REG_DWORD, REG_OPTION_NON_VOLATILE,
    };
    let cur = desktop_icons_hidden().unwrap_or(false);
    unsafe {
        let mut hkey: HKEY = std::ptr::null_mut();
        let mut disp: u32 = 0;
        let st = RegCreateKeyExW(
            HKEY_CURRENT_USER,
            wide(EXPLORER_ADV).as_ptr(),
            0,
            std::ptr::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            std::ptr::null(),
            &mut hkey,
            &mut disp,
        );
        if st != 0 {
            return Err(format!("打开注册表失败（错误码 {st}）"));
        }
        let name = wide(HIDE_ICONS);
        let val: u32 = if hide { 1 } else { 0 };
        let bytes = val.to_le_bytes();
        let st = RegSetValueExW(hkey, name.as_ptr(), 0, REG_DWORD, bytes.as_ptr(), 4);
        RegCloseKey(hkey);
        if st != 0 {
            return Err(format!("写入 HideIcons 失败（错误码 {st}）"));
        }
    }
    if cur != hide {
        refresh_desktop_icons();
    }
    Ok(())
}

/// 通知外壳刷新桌面层：把 `WM_COMMAND 0x7402` 发给 `SHELLDLL_DefView` **本体**
/// （Progman 下没有就翻 WorkerW：壁纸幻灯片模式会把 DefView 挪走）。
/// 实测：发给父窗 Progman / WorkerW 不动，发给 DefView 才切换
fn refresh_desktop_icons() {
    use ws::UI::WindowsAndMessaging as wam;
    const WM_COMMAND: usize = 0x0111;
    const TOGGLE_DESKTOP_ICONS: usize = 0x7402;
    unsafe {
        let progman = wam::FindWindowW(wide("Progman").as_ptr(), std::ptr::null());
        let mut view: ws::Foundation::HWND = std::ptr::null_mut();
        if !progman.is_null() {
            view = wam::FindWindowExW(
                progman,
                std::ptr::null_mut(),
                wide("SHELLDLL_DefView").as_ptr(),
                std::ptr::null(),
            );
        }
        if view.is_null() {
            for top in top_level_of_class("WorkerW") {
                let v = wam::FindWindowExW(
                    top,
                    std::ptr::null_mut(),
                    wide("SHELLDLL_DefView").as_ptr(),
                    std::ptr::null(),
                );
                if !v.is_null() {
                    view = v;
                    break;
                }
            }
        }
        if view.is_null() {
            return;
        }
        let mut out: usize = 0;
        wam::SendMessageTimeoutW(
            view,
            WM_COMMAND as u32,
            TOGGLE_DESKTOP_ICONS,
            0,
            wam::SMTO_ABORTIFHUNG,
            1500,
            &mut out,
        );
    }
}

/// 顶层窗口里指定类名的句柄（Progman / WorkerW 都要挑，DefView 的位置随系统版本与壁纸模式变）
fn top_level_of_class(class: &str) -> Vec<ws::Foundation::HWND> {
    let mut out: Vec<ws::Foundation::HWND> = Vec::new();
    unsafe {
        let ptr = &mut out as *mut Vec<ws::Foundation::HWND>;
        ws::UI::WindowsAndMessaging::EnumWindows(Some(class_enum_cb), ptr as isize);
    }
    let want = wide(class);
    out.into_iter()
        .filter(|h| {
            let mut buf = [0u16; 64];
            let n = unsafe {
                ws::UI::WindowsAndMessaging::GetClassNameW(*h, buf.as_mut_ptr(), buf.len() as i32)
            };
            n > 0 && buf[..n as usize] == want[..n as usize]
        })
        .collect()
}

unsafe extern "system" fn class_enum_cb(
    hwnd: ws::Foundation::HWND,
    lparam: ws::Foundation::LPARAM,
) -> windows_sys::core::BOOL {
    let out = &mut *(lparam as *mut Vec<ws::Foundation::HWND>);
    out.push(hwnd);
    1
}

// ─── D 段：运行态（EnumWindows）+ 激活/最小化 ───

/// 当前线程初始化过 COM 吗（ShellLink 是进程内组件，没初始化会 CO_E_NOTINITIALIZED；COM 是**按线程**的）
fn ensure_com() {
    use std::cell::Cell;
    thread_local! { static DONE: Cell<bool> = const { Cell::new(false) }; }
    DONE.with(|d| {
        if !d.get() {
            use w::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
            unsafe {
                let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED); // 已经是别的模式就算了（不影响读 lnk）
            }
            d.set(true);
        }
    });
}

/// `.lnk` → 目标路径（IShellLinkW；拿不到 = None）。带进程内缓存，非 lnk 直接 None
pub fn lnk_target(path: &str) -> Option<String> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<HashMap<String, Option<String>>>> = OnceLock::new();
    let p = win_path(path.trim());
    if !p.to_lowercase().ends_with(".lnk") {
        return None;
    }
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let key = p.to_lowercase();
    if let Some(v) = cache.lock().unwrap().get(&key) {
        return v.clone();
    }
    ensure_com();
    let resolved = unsafe {
        use windows::core::{Interface, PCWSTR};
        use w::System::Com::{CoCreateInstance, IPersistFile, CLSCTX_INPROC_SERVER, STGM_READ};
        use w::UI::Shell::{IShellLinkW, ShellLink, SLGP_RAWPATH};
        let link: Option<IShellLinkW> = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok();
        link.and_then(|l| {
            let pf: Option<IPersistFile> = l.cast().ok();
            pf.and_then(|pf| {
                pf.Load(PCWSTR(wide(&p).as_ptr()), STGM_READ).ok()?;
                let mut buf = [0u16; 1024];
                l.GetPath(&mut buf, std::ptr::null_mut(), SLGP_RAWPATH.0 as u32).ok()?;
                let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
                if n == 0 {
                    None
                } else {
                    Some(String::from_utf16_lossy(&buf[..n]))
                }
            })
        })
    };
    cache.lock().unwrap().insert(key, resolved.clone());
    resolved
}

/// 运行态比对用的键：`.lnk` 先解析到目标 exe，再统一成反斜杠 + 小写。
/// （`normalize_path` 是纯路径归一，不动 lnk —— 图标缓存名依赖它的稳定性）
pub fn match_key(path: &str) -> String {
    let p = path.trim();
    if p.is_empty() {
        return String::new();
    }
    if is_url(p) {
        return p.to_lowercase();
    }
    let target = lnk_target(p).unwrap_or_else(|| p.to_string());
    normalize_path(&target)
}

/// 运行态比对：普通应用按**完整路径**比；**系统应用别名**按 exe 文件名比。
/// 为什么需要后者：Win11 记事本这类「app alias」在 System32 里只有个转发器，
/// 真实进程是 `C:\Program Files\WindowsApps\…\Notepad.exe`（实测：条目 System32\notepad.exe
/// ↔ 进程 WindowsApps\Microsoft.WindowsNotepad_11.2607…\Notepad.exe），只比全路径会
/// 「明明开着却永远显示未运行、点了再开一个」。放宽只针对**条目侧**是系统别名的情况，
/// 普通目录下的同名 exe 一律不算（别把 D:\tools\b\app.exe 当成自己的）
fn same_app(item_key: &str, exe: &str) -> bool {
    fn base(p: &str) -> &str {
        p.rsplit('\\').next().unwrap_or(p)
    }
    if item_key == exe {
        return true;
    }
    let sys_like = |p: &str| p.starts_with("c:\\windows\\system32\\") || p.contains("\\windowsapps\\");
    if !sys_like(item_key) {
        return false;
    }
    base(item_key) == base(exe)
}

/// 该条目现在算「运行中」吗（拿一份 running 集合问，省得每条都重扫窗口）
pub fn item_running(item_key: &str, running: &std::collections::HashSet<String>) -> bool {
    !item_key.is_empty()
        && running
            .iter()
            .any(|exe| exe != item_key && same_app(item_key, exe) || exe == item_key)
}

/// 可见顶层窗口 → (hwnd, exe 小写)。D1 的「运行中」与 D2 的「找目标窗口」共用同一份枚举
fn visible_windows() -> Vec<(isize, String)> {
    let mut rows: Vec<(isize, String)> = Vec::new();
    unsafe {
        let ptr = &mut rows as *mut Vec<(isize, String)>;
        ws::UI::WindowsAndMessaging::EnumWindows(Some(enum_win_cb), ptr as isize);
    }
    rows
}

unsafe extern "system" fn enum_win_cb(
    hwnd: ws::Foundation::HWND,
    lparam: ws::Foundation::LPARAM,
) -> windows_sys::core::BOOL {
    use ws::UI::WindowsAndMessaging as wam;
    let rows = &mut *(lparam as *mut Vec<(isize, String)>);
    if wam::IsWindowVisible(hwnd) == 0 {
        return 1;
    }
    let ex = wam::GetWindowLongW(hwnd, wam::GWL_EXSTYLE) as u32;
    if ex & wam::WS_EX_TOOLWINDOW != 0 {
        return 1;
    }
    let mut cloaked: u32 = 0;
    let hr = ws::Graphics::Dwm::DwmGetWindowAttribute(
        hwnd,
        ws::Graphics::Dwm::DWMWA_CLOAKED as u32,
        &mut cloaked as *mut u32 as *mut c_void,
        4,
    );
    if hr >= 0 && cloaked != 0 {
        return 1;
    }
    let mut pid = 0u32;
    wam::GetWindowThreadProcessId(hwnd, &mut pid);
    let exe = proc_exe(pid);
    if !exe.is_empty() {
        rows.push((hwnd as isize, exe.to_lowercase()));
    }
    1
}

/// 运行中应用集合（exe 小写路径）—— D1 轮询的数据源
pub fn running_set() -> std::collections::HashSet<String> {
    visible_windows().into_iter().map(|(_, exe)| exe).collect()
}

/// 目标应用的可见顶层窗口，**前台那个排最前**（匹配规则见 `same_app`）；
/// 带标题、有实际面积、非 IME 之类的辅助窗才算候选
fn windows_of(item_key: &str) -> Vec<isize> {
    use ws::UI::WindowsAndMessaging as wam;
    let fg = unsafe { wam::GetForegroundWindow() } as isize;
    let mut hits: Vec<isize> = visible_windows()
        .into_iter()
        .filter(|(h, exe)| same_app(item_key, exe) && is_main_window(*h))
        .map(|(h, _)| h)
        .collect();
    hits.sort_by_key(|h| if *h == fg { 0 } else { 1 });
    hits
}

/// 像「主窗」吗：有标题 + 面积够（`无标题 - Notepad` 这类打包应用的多顶层窗都算，
/// IME / GDI+ 钩子那类 0×0 无名窗被挡在外面）
fn is_main_window(h: isize) -> bool {
    use ws::UI::WindowsAndMessaging as wam;
    unsafe {
        let hwnd = h as ws::Foundation::HWND;
        if wam::GetWindowTextLengthW(hwnd) <= 0 {
            return false;
        }
        let mut r = ws::Foundation::RECT::default();
        if wam::GetWindowRect(hwnd, &mut r) == 0 {
            return false;
        }
        (r.right - r.left) >= 160 && (r.bottom - r.top) >= 120
    }
}

/// 点已运行的图标：**有窗口在前台 → 全部最小化**（与任务栏一致：Win11 记事本这种一个应用
/// 多顶层窗的情况，一次收干净）；否则恢复 + 抢前台，抢不到就闪任务栏。
/// 返回 `minimized` / `activated` / `flash`
pub fn activate_or_minimize(item_key: &str) -> Result<&'static str, String> {
    use ws::System::Threading as th;
    use ws::UI::WindowsAndMessaging as wam;
    let wins = windows_of(item_key);
    let Some(&first) = wins.first() else {
        return Err("没找到该应用的窗口".into());
    };
    unsafe {
        let fg = wam::GetForegroundWindow();
        if wins.iter().any(|x| *x as ws::Foundation::HWND == fg) {
            for h in &wins {
                wam::ShowWindow(*h as ws::Foundation::HWND, wam::SW_MINIMIZE);
            }
            return Ok("minimized");
        }
        let h = first as ws::Foundation::HWND;
        if wam::IsIconic(h) != 0 {
            wam::ShowWindow(h, wam::SW_RESTORE);
        } else {
            wam::ShowWindow(h, wam::SW_SHOW);
        }
        let fg_tid = wam::GetWindowThreadProcessId(fg, std::ptr::null_mut());
        let me = th::GetCurrentThreadId();
        let mut at = 0;
        let sf1 = wam::SetForegroundWindow(h);
        if sf1 != 0 {
            return Ok("activated");
        }
        // 抢前台被拒（Windows 只放行「刚有输入的那个进程」之外的前台切换）
        // ① 临时把输入队列挂到前台线程上再抢
        if fg_tid != 0 && fg_tid != me {
            th::AttachThreadInput(me, fg_tid, 1);
            at = wam::SetForegroundWindow(h);
            th::AttachThreadInput(me, fg_tid, 0);
            if at != 0 {
                return Ok("activated");
            }
        }
        // ② 合成一次 ALT：把「最后输入事件」抢到本进程，前台锁就放行了（经典手法）
        {
            use ws::UI::Input::KeyboardAndMouse as kbd;
            const VK_MENU: u8 = 0x12;
            kbd::keybd_event(VK_MENU, 0, 0, 0);                          // ALT 按下
            kbd::keybd_event(VK_MENU, 0, kbd::KEYEVENTF_KEYUP, 0);       // ALT 抬起
        }
        let sf2 = wam::SetForegroundWindow(h);
        if sf2 != 0 {
            return Ok("activated");
        }
        // ③ SwitchToThisWindow（未公开但任务栏那套一直在用）
        wam::SwitchToThisWindow(h, 1);
        if wam::GetForegroundWindow() == h {
            return Ok("activated");
        }
        crate::mlog(&format!(
            "dock activate 全败：sf1={sf1} at={at} sf2={sf2} fg_tid={fg_tid} me={me}"
        ));
        // 兜底：闪任务栏（至少让用户看到它在哪）
        let fwi = wam::FLASHWINFO {
            cbSize: std::mem::size_of::<wam::FLASHWINFO>() as u32,
            hwnd: h,
            dwFlags: wam::FLASHW_ALL | wam::FLASHW_TIMERNOFG,
            uCount: 3,
            dwTimeout: 0,
        };
        wam::FlashWindowEx(&fwi);
        Ok("flash")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_normalize_and_url() {
        assert_eq!(win_path("C:/a/b.exe"), "C:\\a\\b.exe");
        assert_eq!(normalize_path("C:/A/B.exe"), "c:\\a\\b.exe");
        assert!(is_url("https://example.com"));
        assert!(is_url("HTTP://x"));
        assert!(!is_url("C:\\x.exe"));
        assert_eq!(fnv1a_hex("a"), fnv1a_hex("a"));
        assert_ne!(fnv1a_hex("a"), fnv1a_hex("b"));
    }

    #[test]
    fn app_match_rules() {
        // 普通应用：全路径相等才算运行中
        assert!(same_app("c:\\a\\b.exe", "c:\\a\\b.exe"));
        assert!(!same_app("c:\\a\\b.exe", "d:\\a\\b.exe"));
        // 系统应用别名（Win11 记事本：System32 转发器 vs WindowsApps 真身）按文件名认
        assert!(same_app(
            "c:\\windows\\system32\\notepad.exe",
            "c:\\program files\\windowsapps\\microsoft.windowsnotepad_x\\notepad\\notepad.exe"
        ));
        assert!(same_app(
            "c:\\program files\\windowsapps\\x\\app.exe",
            "c:\\program files\\windowsapps\\y\\app.exe"
        ));
        // 但普通目录下同名不算（别把别的 chrome 当自己）
        assert!(!same_app("d:\\tools\\a\\app.exe", "d:\\tools\\b\\app.exe"));
    }

    #[test]
    fn item_running_uses_match_rules() {
        let mut set = std::collections::HashSet::new();
        set.insert("c:\\program files\\windowsapps\\x\\notepad\\notepad.exe".to_string());
        assert!(item_running("c:\\windows\\system32\\notepad.exe", &set));
        assert!(!item_running("c:\\other\\notepad.exe", &set));
        assert!(!item_running("", &set));
    }
}
