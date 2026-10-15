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

/// 有可见顶层窗口的进程 exe 路径（小写、去重、排序）—— 运行指示点的数据源
#[allow(dead_code)] // D 段运行态轮询线程接线
pub fn running_map() -> Vec<String> {
    let mut rows: Vec<String> = Vec::new();
    unsafe {
        let ptr = &mut rows as *mut Vec<String>;
        ws::UI::WindowsAndMessaging::EnumWindows(Some(enum_cb), ptr as isize);
    }
    rows.sort();
    rows.dedup();
    rows
}

#[allow(dead_code)]
unsafe extern "system" fn enum_cb(
    hwnd: ws::Foundation::HWND,
    lparam: ws::Foundation::LPARAM,
) -> windows_sys::core::BOOL {
    use ws::UI::WindowsAndMessaging as wam;
    let rows = &mut *(lparam as *mut Vec<String>);
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
        rows.push(exe.to_lowercase());
    }
    1
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
}
