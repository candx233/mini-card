//! 从文件路径提取 32bpp 图标 → PNG，缓存到 `%APPDATA%\mini-card\icons\<fnv1a>.png`。
//!
//! 与外壳 `src-tauri/src/dock.rs::ensure_icon` 用**同一套缓存命名**，两边共用一份图标；
//! 自绘进程自己也能生成（拖入新条目时不必等外壳）。

use std::path::{Path, PathBuf};

/// 统一路径口径：反斜杠 + 小写（SHGetFileInfoW 认反斜杠，正斜杠返回 0）
fn normalize(p: &str) -> String {
    p.replace('/', "\\").trim_end_matches('\\').to_lowercase()
}

fn fnv1a_hex(s: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

pub fn icons_dir() -> PathBuf {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("mini-card")
        .join("icons")
}

pub fn cache_path(path: &str) -> PathBuf {
    icons_dir().join(format!("{}.png", fnv1a_hex(&normalize(path))))
}

/// 目标边长（逻辑 48px 图标 × 2 倍屏也够；运行时还会再降到 96 画）
pub const TARGET: u32 = 256;

/// 确保有条目图标；返回可直接加载的 PNG 路径。
/// 缓存文件太小（<96，早先是按 32 档写进去的）会重新提取。
pub fn ensure(path: &str) -> Option<PathBuf> {
    if path.trim().is_empty() || path.starts_with("http") {
        return None;
    }
    let out = cache_path(path);
    if let Some(px) = png_side(&out) {
        if px >= 96 && out.metadata().map(|m| m.len() > 4096).unwrap_or(false) {
            return Some(out);
        }
    }
    match extract(path, &out) {
        Ok(()) => Some(out),
        Err(e) => {
            minicard_shell::mlog!("图标提取失败（{path}）：{e}");
            None
        }
    }
}

/// 读 PNG 头里的宽（只认 PNG，别的格式当没有）
fn png_side(p: &Path) -> Option<u32> {
    let b = std::fs::read(p).ok()?;
    if b.len() < 24 || &b[12..16] != b"IHDR" {
        return None;
    }
    Some(u32::from_be_bytes([b[16], b[17], b[18], b[19]]))
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 提取：`PrivateExtractIconsW` 直接要 256 档。
///
/// 早先用 `SHGetImageList` + `IImageList::GetIcon` 三档（JUMBO/EXTRALARGE/LARGE）
/// 挑覆盖率最高的一张，结果对「图标只有 32 档」的应用反而挑到的是放大件（又糊又不准）；
/// 而真带 256 帧的应用（例如 DeepSeek Harness）也会被取成 32 —— 实测同一个 exe
/// 32 档放大到 256 的清晰度只有真 256 的 43%。`PrivateExtractIconsW` 让系统自己挑帧，
/// 要 256 有 256、没有就退到能给的最高档，不会自作主张选低档。
fn extract(path: &str, out: &Path) -> Result<(), String> {
    use windows::Win32::UI::WindowsAndMessaging::PrivateExtractIconsW;
    if let Some(d) = out.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    unsafe {
        let _ = windows::Win32::System::Com::CoInitializeEx(
            None,
            windows::Win32::System::Com::COINIT_APARTMENTTHREADED,
        );
        // `PrivateExtractIconsW` 直接喂 .lnk 一律取不到（实测三个快捷方式全返回 0），
        // 先按 IShellLinkW 解析到目标 exe 再取；缓存键仍是原路径。
        let src = if path.to_lowercase().ends_with(".lnk") {
            crate::appwin::lnk_target(path).unwrap_or_else(|| path.to_string())
        } else {
            path.to_string()
        };
        let w = wide(&normalize(&src));
        let mut name = [0u16; 260];
        let n_chars = w.len().min(259);
        name[..n_chars].copy_from_slice(&w[..n_chars]);
        let mut icons = [windows::Win32::UI::WindowsAndMessaging::HICON::default(); 1];
        let n = PrivateExtractIconsW(
            &name,
            0,
            TARGET as i32,
            TARGET as i32,
            Some(&mut icons),
            None,
            0,
        );
        if n == 0 || icons[0].is_invalid() {
            return Err("PrivateExtractIconsW 没取到图标".into());
        }
        let (size, buf) = icon_to_bgra(icons[0]);
        let _ = windows::Win32::UI::WindowsAndMessaging::DestroyIcon(icons[0]);
        if size == 0 || buf.is_empty() {
            return Err("图标位图取不到像素".into());
        }
        save_png(out, size, size, &buf)
    }
}

/// HICON → (边长, 预乘 BGRA)
unsafe fn icon_to_bgra(
    hicon: windows::Win32::UI::WindowsAndMessaging::HICON,
) -> (u32, Vec<u8>) {
    use windows::Win32::Graphics::Gdi::{
        CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetObjectW, SelectObject,
        BITMAP, BITMAPINFO, BITMAPV5HEADER, BI_BITFIELDS, DIB_RGB_COLORS,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        DrawIconEx, GetIconInfo, DI_NORMAL, ICONINFO,
    };

    let mut ii = ICONINFO::default();
    if GetIconInfo(hicon, &mut ii).is_err() {
        return (0, Vec::new());
    }
    let mut bm = BITMAP::default();
    GetObjectW(
        ii.hbmColor.into(),
        std::mem::size_of::<BITMAP>() as i32,
        Some(&mut bm as *mut _ as *mut std::ffi::c_void),
    );
    let size = bm.bmWidth.max(bm.bmHeight).clamp(16, 512) as u32;
    if size == 0 {
        return (0, Vec::new());
    }

    let mut bmi = BITMAPV5HEADER::default();
    bmi.bV5Size = std::mem::size_of::<BITMAPV5HEADER>() as u32;
    bmi.bV5Width = size as i32;
    bmi.bV5Height = -(size as i32);
    bmi.bV5Planes = 1;
    bmi.bV5BitCount = 32;
    bmi.bV5Compression = BI_BITFIELDS;
    bmi.bV5RedMask = 0x00FF_0000;
    bmi.bV5GreenMask = 0x0000_FF00;
    bmi.bV5BlueMask = 0x0000_00FF;
    bmi.bV5AlphaMask = 0xFF00_0000;

    let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
    let Ok(hbmp) = CreateDIBSection(
        None,
        &bmi as *const _ as *const BITMAPINFO,
        DIB_RGB_COLORS,
        &mut bits,
        None,
        0,
    ) else {
        return (0, Vec::new());
    };
    let hdc = CreateCompatibleDC(None);
    let old = SelectObject(hdc, hbmp.into());
    let draw = DrawIconEx(hdc, 0, 0, hicon, size as i32, size as i32, 0, None, DI_NORMAL);
    let n = (size * size * 4) as usize;
    let buf = if draw.is_ok() && !bits.is_null() {
        std::slice::from_raw_parts(bits as *const u8, n).to_vec()
    } else {
        Vec::new()
    };
    SelectObject(hdc, old);
    let _ = DeleteObject(hbmp.into());
    let _ = DeleteDC(hdc);
    if !ii.hbmColor.0.is_null() {
        let _ = DeleteObject(ii.hbmColor.into());
    }
    if !ii.hbmMask.0.is_null() {
        let _ = DeleteObject(ii.hbmMask.into());
    }
    (size, buf)
}

/// DIB 是**预乘** BGRA → PNG 要直通 alpha（按 a 反预乘）
fn save_png(path: &Path, w: u32, h: u32, bgra: &[u8]) -> Result<(), String> {
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
    let img = ::image::RgbaImage::from_raw(w, h, rgba).ok_or("图标缓冲尺寸不符")?;
    img.save_with_format(path, ::image::ImageFormat::Png)
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 缓存名与外壳同口径() {
        // 反斜杠/小写/去尾斜杠之后哈希，正斜杠与大小写不能各算一份
        assert_eq!(normalize("C:/Windows/System32/notepad.exe"), "c:\\windows\\system32\\notepad.exe");
        assert_eq!(normalize("C:\\Windows\\"), "c:\\windows");
        assert_eq!(cache_path("C:/Windows/System32/notepad.exe"), cache_path("c:\\windows\\system32\\NOTEPAD.exe"));
        assert!(cache_path("a.exe").file_name().unwrap().to_string_lossy().ends_with(".png"));
    }
}

#[cfg(test)]
mod probe_tests {
    use super::*;

    /// 系统自带的 exe 一定带 256 帧；取到的 PNG 不能是小图（否则 Dock 里会被放大糊掉）
    #[test]
    fn exe取到高分辨率图标() {
        let exe = "C:\\Windows\\System32\\notepad.exe";
        if !Path::new(exe).exists() {
            return;
        }
        let out = std::env::temp_dir().join("mc_icon_exe_test.png");
        let _ = std::fs::remove_file(&out);
        extract(exe, &out).expect("notepad 图标提取失败");
        let px = png_side(&out).expect("不是 PNG");
        let _ = std::fs::remove_file(&out);
        assert!(px >= 128, "取到的图标只有 {px}px，Dock 里放大必糊");
    }

    /// 快捷方式要先解析到目标 exe（直接喂 .lnk 系统返回 0）
    #[test]
    fn 快捷方式解析后取图标() {
        let lnk = "C:\\Users\\Public\\Desktop\\DaVinci Resolve.lnk";
        if !Path::new(lnk).exists() {
            return;
        }
        let out = std::env::temp_dir().join("mc_icon_lnk_test.png");
        let _ = std::fs::remove_file(&out);
        extract(lnk, &out).expect("快捷方式图标提取失败");
        let px = png_side(&out).expect("不是 PNG");
        let _ = std::fs::remove_file(&out);
        assert!(px >= 128, "快捷方式只取到 {px}px");
    }
}
