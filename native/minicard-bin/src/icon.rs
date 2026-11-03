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

/// 确保有条目图标；返回可直接加载的 PNG 路径。
pub fn ensure(path: &str) -> Option<PathBuf> {
    if path.trim().is_empty() || path.starts_with("http") {
        return None;
    }
    let out = cache_path(path);
    if out.exists() {
        return Some(out);
    }
    match extract(path, &out) {
        Ok(()) => Some(out),
        Err(e) => {
            minicard_shell::mlog!("图标提取失败（{path}）：{e}");
            None
        }
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn extract(path: &str, out: &Path) -> Result<(), String> {
    use windows::Win32::UI::Shell::{SHGetFileInfoW, SHFILEINFOW, SHGFI_SYSICONINDEX};
    use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_NORMAL;
    if let Some(d) = out.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    unsafe {
        let _ = windows::Win32::System::Com::CoInitializeEx(
            None,
            windows::Win32::System::Com::COINIT_APARTMENTTHREADED,
        );
        let p = wide(&normalize(path));
        let mut sfi = SHFILEINFOW::default();
        let r = SHGetFileInfoW(
            windows::core::PCWSTR(p.as_ptr()),
            FILE_ATTRIBUTE_NORMAL,
            Some(&mut sfi),
            std::mem::size_of::<SHFILEINFOW>() as u32,
            SHGFI_SYSICONINDEX,
        );
        if r == 0 {
            return Err("SHGetFileInfoW 返回 0".into());
        }
        // 三档里挑覆盖率最高的一档（jumbo 常常是空壳）
        let mut best: Option<(u32, f32, Vec<u8>)> = None;
        for shil in [
            windows::Win32::UI::Shell::SHIL_JUMBO,
            windows::Win32::UI::Shell::SHIL_EXTRALARGE,
            windows::Win32::UI::Shell::SHIL_LARGE,
        ] {
            if let Ok((size, cov, buf)) = grab(sfi.iIcon, shil) {
                if best.as_ref().map(|(_, c, _)| cov > *c).unwrap_or(true) {
                    best = Some((size, cov, buf));
                }
            }
        }
        let Some((size, _, buf)) = best else {
            return Err("三档图标全部提取失败".into());
        };
        save_png(out, size, size, &buf)
    }
}

/// 取一档系统图标画进 32bpp DIB，返回 (边长, 非透明覆盖率%, 预乘 BGRA)
unsafe fn grab(idx: i32, shil: u32) -> Result<(u32, f32, Vec<u8>), String> {
    use windows::Win32::Graphics::Gdi::{
        CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetObjectW, SelectObject,
        BITMAP, BITMAPINFO, BITMAPV5HEADER, BI_BITFIELDS, DIB_RGB_COLORS,
    };
    use windows::Win32::UI::Controls::IImageList;
    use windows::Win32::UI::Shell::SHGetImageList;
    use windows::Win32::UI::WindowsAndMessaging::{
        DestroyIcon, DrawIconEx, GetIconInfo, DI_NORMAL, ICONINFO,
    };

    let il: IImageList = SHGetImageList(shil as i32).map_err(|e| e.to_string())?;
    let hicon = il
        .GetIcon(idx, windows::Win32::UI::Controls::ILD_TRANSPARENT.0)
        .map_err(|e| e.to_string())?;

    let mut ii = ICONINFO::default();
    GetIconInfo(hicon, &mut ii).map_err(|e| e.to_string())?;
    let mut bm = BITMAP::default();
    GetObjectW(
        ii.hbmColor.into(),
        std::mem::size_of::<BITMAP>() as i32,
        Some(&mut bm as *mut _ as *mut std::ffi::c_void),
    );
    let size = (bm.bmWidth.max(bm.bmHeight)).clamp(16, 512) as u32;

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
    let hbmp = CreateDIBSection(
        None,
        &bmi as *const _ as *const BITMAPINFO,
        DIB_RGB_COLORS,
        &mut bits,
        None,
        0,
    )
    .map_err(|e| e.to_string())?;
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
    let _ = DestroyIcon(hicon);
    draw.map_err(|e| e.to_string())?;
    let cov = if buf.is_empty() {
        0.0
    } else {
        let opaque = buf.chunks_exact(4).filter(|px| px[3] > 8).count();
        opaque as f32 * 100.0 / (size * size) as f32
    };
    Ok((size, cov, buf))
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
