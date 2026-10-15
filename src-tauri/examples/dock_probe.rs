//! dock 探针：验证 Dock 三条数据链路（0.1.2 A4 spike，见 docs/TODO-012.md）
//!
//! 用法（cwd = src-tauri/）：
//!   cargo run --example dock_probe -- icons <out_dir> <path>...
//!   cargo run --example dock_probe -- running <exe_path>...
//!   cargo run --example dock_probe -- launch <path> [args...]
//!
//! - icons  ：SHGetFileInfoW(SHGFI_SYSICONINDEX) → SHGetImageList(JUMBO=256 / LARGE=32)
//!            → IImageList::GetIcon → DrawIconEx 到 32bpp DIB（BI_BITFIELDS 带 alpha 掩码）→ PNG。
//!            每个图标存两份：`*.raw.png`（DIB 原样）与 `*.unpre.png`（按 alpha 反预乘），视觉判断哪份是真色。
//! - running：EnumWindows 汇总「有可见顶层窗口」的进程（过滤 tool window / DWM cloaked），与任务栏同口径。
//! - launch ：ShellExecuteW（返回值 > 32 为成功）。

use std::ffi::{c_void, OsStr};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

/// 提取策略：从大到小取第一个「非退化」档；都退化就取覆盖最大的（应用侧再落首字母色块兜底）
const MIN_COVER: f32 = 8.0;

use windows::Win32 as w;
use windows_sys::Win32 as ws;

fn wide(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("icons") if args.len() >= 3 => cmd_icons(&args[1], &args[2..]),
        Some("running") => cmd_running(&args[1..]),
        Some("launch") if args.len() >= 2 => cmd_launch(&args[1], &args[2..]),
        Some("dbg") if args.len() >= 2 => cmd_dbg(&args[1]),
        _ => {
            eprintln!("用法：dock_probe icons <out_dir> <path>... | running <exe>... | launch <path> [args...]");
            std::process::exit(2);
        }
    }
}

/// 同一个调用走 windows-sys / windows 两条绑定，对比结果（排查绑定差异用）
fn cmd_dbg(path: &str) {
    let p = wide(path);
    unsafe {
        let mut sfi2: ws::UI::Shell::SHFILEINFOW = std::mem::zeroed();
        let r2 = ws::UI::Shell::SHGetFileInfoW(
            p.as_ptr(),
            ws::Storage::FileSystem::FILE_ATTRIBUTE_NORMAL,
            &mut sfi2,
            std::mem::size_of::<ws::UI::Shell::SHFILEINFOW>() as u32,
            ws::UI::Shell::SHGFI_SYSICONINDEX,
        );
        println!("windows-sys ret={r2} iIcon={} err={}", sfi2.iIcon, ws::Foundation::GetLastError());
        let mut sfi: w::UI::Shell::SHFILEINFOW = std::mem::zeroed();
        let r = w::UI::Shell::SHGetFileInfoW(
            windows::core::PCWSTR(p.as_ptr()),
            w::Storage::FileSystem::FILE_ATTRIBUTE_NORMAL,
            Some(&mut sfi),
            std::mem::size_of::<w::UI::Shell::SHFILEINFOW>() as u32,
            w::UI::Shell::SHGFI_SYSICONINDEX,
        );
        println!("windows-cr  ret={r} iIcon={} err={}", sfi.iIcon, ws::Foundation::GetLastError());
        println!("sizeof SHFILEINFOW: ws={} w={}", std::mem::size_of::<ws::UI::Shell::SHFILEINFOW>(), std::mem::size_of::<w::UI::Shell::SHFILEINFOW>());
    }
}

/* ── icons ─────────────────────────────────────────────────────────── */

fn stem_of(p: &str) -> String {
    let s = p.trim_end_matches(['\\', '/']);
    let name = s.rsplit(['\\', '/']).next().unwrap_or(s);
    let stem = name.split('.').next().unwrap_or(name);
    let stem = stem.trim_matches('.').replace([' ', ':', '*', '?', '"', '<', '>', '|'], "_");
    if stem.is_empty() { "icon".into() } else { stem }
}

fn cmd_icons(out_dir: &str, paths: &[String]) {
    let out = PathBuf::from(out_dir);
    if let Err(e) = std::fs::create_dir_all(&out) {
        eprintln!("mkdir {out_dir} 失败: {e}");
        std::process::exit(1);
    }
    unsafe {
        // 外壳图像列表是 COM 对象；RPC_E_CHANGED_MODE 之类失败不影响后续调用
        let _ = w::System::Com::CoInitializeEx(None, w::System::Com::COINIT_APARTMENTTHREADED);
        for p in paths {
            let stem = stem_of(p);
            match sysicon_index(p) {
                Ok(idx) => {
                    println!("== {p}\n   sysicon index = {idx}");
                    let mut got: Vec<Grab> = Vec::new();
                    for (tag, shil) in [
                        ("256", w::UI::Shell::SHIL_JUMBO),
                        ("48", w::UI::Shell::SHIL_EXTRALARGE),
                        ("32", w::UI::Shell::SHIL_LARGE),
                    ] {
                        match grab_one(idx, shil, &out, &stem, tag) {
                            Ok(r) => {
                                println!("   [{tag}] 源 {}x{}  覆盖 {:.1}%  {}", r.src_w, r.src_h, r.cov, r.png.display());
                                got.push(r);
                            }
                            Err(e) => println!("   [{tag}] FAIL: {e}"),
                        }
                    }
                    let pick = got
                        .iter()
                        .find(|g| g.cov >= MIN_COVER)
                        .or_else(|| got.iter().filter(|g| g.cov > 0.0).max_by(|a, b| a.cov.partial_cmp(&b.cov).unwrap()));
                    if let Some(b) = pick {
                        let best = out.join(format!("{stem}.best.png"));
                        let _ = std::fs::copy(&b.png, &best);
                        println!("   [best] 取 {} 档（覆盖 {:.1}%）-> {}", b.size, b.cov, best.display());
                    }
                }
                Err(e) => println!("== {p}\n   FAIL: {e}"),
            }
        }
    }
}

/// SHGetFileInfoW 取系统图像列表索引（图标只在索引里，图本身按需取）
/// ⚠️ 实测：pszPath 必须用反斜杠——正斜杠路径一律返回 0（GetLastError 还报 1008 误导）
unsafe fn sysicon_index(path: &str) -> Result<i32, String> {
    let mut sfi = w::UI::Shell::SHFILEINFOW::default();
    let win_path = path.replace('/', "\\");
    let p = wide(&win_path);
    let r = w::UI::Shell::SHGetFileInfoW(
        windows::core::PCWSTR(p.as_ptr()),
        w::Storage::FileSystem::FILE_ATTRIBUTE_NORMAL,
        Some(&mut sfi),
        std::mem::size_of::<w::UI::Shell::SHFILEINFOW>() as u32,
        w::UI::Shell::SHGFI_SYSICONINDEX,
    );
    if r == 0 {
        let err = ws::Foundation::GetLastError();
        return Err(format!("SHGetFileInfoW 返回 0（GetLastError={err}）"));
    }
    Ok(sfi.iIcon)
}

struct Grab {
    size: u32,
    src_w: u32,
    src_h: u32,
    cov: f32,
    png: PathBuf,
}

unsafe fn grab_one(idx: i32, shil: u32, out: &Path, stem: &str, tag: &str) -> Result<Grab, String> {
    use w::Graphics::Gdi as gdi;
    use w::UI::Controls as ctl;
    use w::UI::WindowsAndMessaging as wam;

    let il: ctl::IImageList = w::UI::Shell::SHGetImageList(shil as i32).map_err(|e| e.to_string())?;
    let hicon = il.GetIcon(idx, ctl::ILD_TRANSPARENT.0).map_err(|e| e.to_string())?;

    // 源尺寸：image list 不一定给到标称尺寸，要如实报告
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

    // 32bpp DIB：BI_BITFIELDS + alpha 掩码，DrawIconEx 才会把 alpha 写进来
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
    // ⚠️ 实测：DIB 里是预乘 alpha（半透明像素 100% 满足 max(rgb) <= a）→ PNG 必须反预乘成直通 alpha
    let png = out.join(format!("{stem}.{tag}.png"));
    save_png(&png, size, size, &buf, true)?;
    Ok(Grab { size, src_w, src_h, cov, png })
}

/// BGRA(DIB) → PNG；unpre = 是否按 alpha 反预乘（DrawIconEx 混过之后可能是预乘的）
fn save_png(path: &Path, w: u32, h: u32, bgra: &[u8], unpre: bool) -> Result<(), String> {
    let mut rgba = Vec::with_capacity(bgra.len());
    for px in bgra.chunks_exact(4) {
        let a = px[3] as u32;
        let un = |c: u8| -> u8 {
            if unpre && a > 0 && a < 255 {
                (((c as u32) * 255 + a / 2) / a).min(255) as u8
            } else {
                c
            }
        };
        rgba.extend_from_slice(&[un(px[2]), un(px[1]), un(px[0]), px[3]]);
    }
    let img = image::RgbaImage::from_raw(w, h, rgba).ok_or("缓冲尺寸不符")?;
    img.save_with_format(path, image::ImageFormat::Png).map_err(|e| e.to_string())
}

/* ── running ───────────────────────────────────────────────────────── */

struct WinRow {
    exe: String,
    title: String,
    iconic: bool,
}

fn cmd_running(want: &[String]) {
    let mut rows: Vec<WinRow> = Vec::new();
    unsafe {
        let ptr = &mut rows as *mut Vec<WinRow>;
        ws::UI::WindowsAndMessaging::EnumWindows(Some(enum_cb), ptr as isize);
    }
    use std::collections::BTreeMap;
    // exe(小写) -> (可见窗口数, 其中最小化数, 样例标题)
    let mut agg: BTreeMap<String, (usize, usize, String)> = BTreeMap::new();
    for r in &rows {
        if r.exe.is_empty() {
            continue;
        }
        let e = agg.entry(r.exe.to_lowercase()).or_insert((0, 0, String::new()));
        e.0 += 1;
        if r.iconic {
            e.1 += 1;
        }
        if e.2.is_empty() && !r.title.trim().is_empty() {
            e.2 = r.title.clone();
        }
    }
    println!("共 {} 个可见顶层窗口 / {} 个进程：", rows.len(), agg.len());
    for (exe, (n, ic, t)) in &agg {
        println!("  {exe:<72} 窗口x{n} 最小化x{ic}  「{t}」");
    }
    if !want.is_empty() {
        println!("-- 命中检查（按 exe 全路径比对）--");
        for wpath in want {
            let needle = wpath.replace('/', "\\").to_lowercase();
            let hit = agg.keys().any(|k| k == &needle);
            println!("  [{}] {wpath}", if hit { "HIT " } else { "MISS" });
        }
    }
}

unsafe extern "system" fn enum_cb(hwnd: ws::Foundation::HWND, lparam: ws::Foundation::LPARAM) -> windows_sys::core::BOOL {
    use ws::UI::WindowsAndMessaging as wam;
    let rows = &mut *(lparam as *mut Vec<WinRow>);
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
    let len = wam::GetWindowTextLengthW(hwnd);
    let mut buf = vec![0u16; len as usize + 1];
    wam::GetWindowTextW(hwnd, buf.as_mut_ptr(), len + 1);
    let title = String::from_utf16_lossy(&buf[..len as usize]);
    let mut pid = 0u32;
    wam::GetWindowThreadProcessId(hwnd, &mut pid);
    rows.push(WinRow { exe: proc_exe(pid), title, iconic: wam::IsIconic(hwnd) != 0 });
    1
}

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

/* ── launch ────────────────────────────────────────────────────────── */

fn cmd_launch(path: &str, args: &[String]) {
    let op = wide("open");
    let file = wide(path);
    let params = if args.is_empty() { Vec::new() } else { wide(&args.join(" ")) };
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
    println!("ShellExecuteW(\"{path}\") -> {code} ({})", if code > 32 { "OK" } else { "FAIL" });
}
