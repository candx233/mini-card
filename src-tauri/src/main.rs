#![windows_subsystem = "windows"]

//! mini-card 主界面 + 桌面卡片窗口（Tauri 2 迁移版）
//! 配置与 egui 版共用同一份 config.toml；卡片 = 无边框透明小窗，
//! 玻璃靠 WCA Acrylic（失焦不掉，DWM backdrop 失焦会回白——实测），
//! 数据靠 1s 采样线程 emit 推送。

mod config;
mod data;
mod display;
mod guide;
mod media;
mod update;
mod weather;

use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU64, AtomicU8, AtomicUsize, Ordering};
use std::sync::Mutex;

use config::{CardInstance, CardKind, Config, ZOrder};
use data::{DataSnapshot, SharedSnap};
use raw_window_handle::HasWindowHandle;
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindow,
            WebviewWindowBuilder};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

/// 单实例锁：与 egui 版同一个锁名，迁移期两个版本互斥。
fn ensure_single_instance() {
    use windows_sys::Win32::Foundation::GetLastError;
    use windows_sys::Win32::System::Threading::CreateMutexW;

    let name: Vec<u16> = "Local\\mini-card-instance"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    // 自更新拉起的新进程：老进程正在退出，锁要先等一下再判定
    let after_update = std::env::args().any(|a| a == update::ARG_AFTER_UPDATE);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        unsafe {
            let h = CreateMutexW(std::ptr::null(), 0, name.as_ptr());
            if h.is_null() || GetLastError() != 183 {
                return; // 拿到锁
            }
        }
        if !after_update || std::time::Instant::now() >= deadline {
            std::process::exit(0);
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

fn card_label(id: &str) -> String {
    format!("card-{id}")
}

/// 最近一次采样到的磁盘数 + 是否还没有数据（采样未到 / 真的 0 块盘）。
/// 快照线程每秒更新；建窗 / 参数面板 / 自动高度三处都读它算磁盘卡的实际窗口高。
static DISK_N: AtomicUsize = AtomicUsize::new(0);
static DISK_STALE: AtomicBool = AtomicBool::new(true);

/// 该卡此刻的**实际窗口尺寸**。
/// 除磁盘卡外都等于 config 里的固定尺寸；磁盘卡的高度由盘数决定（见 config::disk_height_px），
/// config 里的 size 只当「默认档」——算出来的高度是**派生值，绝不写回 config**
/// （写回去就会变成「3 盘机器上跑一次，5 盘机器就按 3 盘的高度显示」）。
/// 采样未到（stale）时回默认档，避免卡片启动瞬间先缩成空态再弹回来。
fn effective_size(kind: CardKind, cfg_size: [f32; 2], disk_count: usize, disk_stale: bool) -> [f32; 2] {
    if !matches!(kind, CardKind::Disks) || disk_stale {
        return cfg_size;
    }
    [cfg_size[0], config::disk_height_px(disk_count)]
}

/// 磁盘卡高度随盘数自动调整（快照线程每秒调一次）。
/// **幂等**：只有盘数真的变了（或首次拿到数据）才动手，否则每秒 set_size 会让窗口抖。
/// 拖动中跳过 —— 拖动循环里改窗口尺寸会跟 guide.rs 的磁吸 / 落点吸附互相打架。
fn sync_disk_height(app: &AppHandle, count: usize, stale: bool) {
    let prev = DISK_N.swap(count, Ordering::Relaxed);
    let was_stale = DISK_STALE.swap(stale, Ordering::Relaxed);
    if stale || (prev == count && !was_stale) {
        return;
    }
    if guide::is_moving() {
        return; // 这一轮的盘数已经存下，等拖动结束后的下一轮再改高度
    }
    let cfg = Config::load();
    for card in cfg.cards.iter().filter(|c| matches!(c.kind, CardKind::Disks)) {
        let want = config::disk_height_px(count);
        let Some(w) = app.get_webview_window(&card_label(&card.id)) else {
            continue;
        };
        let sf = w.scale_factor().unwrap_or(1.0);
        if let Ok(sz) = w.inner_size() {
            let cur = sz.to_logical::<f32>(sf).height;
            if (cur - want).abs() < 1.0 {
                continue; // 已经是目标高度，不动
            }
        }
        let _ = w.set_size(tauri::Size::Logical(tauri::LogicalSize::new(
            card.size[0] as f64,
            want as f64,
        )));
        // 尺寸变了：磨砂层是按「卡片 X/Y 裁壁纸切片」定位的，必须让它按新尺寸重画一次，
        // 否则卡内平均亮度会和卡外对不上（磨砂切片错位）。
        let _ = w.eval("window.__onResize && window.__onResize();");
    }
}

/// 窗口当前外框（物理像素 → 逻辑点）写进 cfg.main_window
fn overlay_main_bounds(w: &WebviewWindow, cfg: &mut Config) {
    let (Ok(pos), Ok(size)) = (w.outer_position(), w.outer_size()) else {
        return;
    };
    let s = w.scale_factor().unwrap_or(1.0);
    cfg.main_window = [
        (pos.x as f64 / s) as f32,
        (pos.y as f64 / s) as f32,
        (size.width as f64 / s) as f32,
        (size.height as f64 / s) as f32,
    ];
}

/// 活着的卡片窗口位置盖进 cfg（窗口没开的卡保持原值）
fn overlay_card_positions(app: &AppHandle, cfg: &mut Config) {
    for card in cfg.cards.iter_mut() {
        if let Some(w) = app.get_webview_window(&card_label(&card.id)) {
            if let Ok(pos) = w.outer_position() {
                let s = w.scale_factor().unwrap_or(1.0);
                card.pos = [(pos.x as f64 / s) as f32, (pos.y as f64 / s) as f32];
            }
        }
    }
}

fn persist_bounds(w: &WebviewWindow) {
    let mut c = Config::load();
    overlay_main_bounds(w, &mut c);
    let _ = c.save();
}

/// 屏幕物理像素尺寸
fn screen_size() -> (i32, i32) {
    use windows_sys::Win32::UI::WindowsAndMessaging::GetSystemMetrics;
    unsafe { (GetSystemMetrics(0), GetSystemMetrics(1)) }
}

/// 极简 base64（只为把壁纸塞进 data URL，避免引 crate）
pub(crate) fn base64_encode(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for c in data.chunks(3) {
        let n = ((c[0] as u32) << 16)
            | ((*c.get(1).unwrap_or(&0) as u32) << 8)
            | (*c.get(2).unwrap_or(&0) as u32);
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push(if c.len() > 1 { T[((n >> 6) & 63) as usize] as char } else { '=' });
        out.push(if c.len() > 2 { T[(n & 63) as usize] as char } else { '=' });
    }
    out
}

/// 当前壁纸文件路径（SPI_GETDESKWALLPAPER）。
/// 磨砂自绘的底图：系统那条 backdrop-filter / WCA 在 Win11 22H2+ 已失效，
/// 只能「取壁纸按填充等比缩放、裁出卡片所在显示器区域、CSS blur」。
fn wallpaper_path() -> Option<String> {
    use windows_sys::Win32::UI::WindowsAndMessaging::SystemParametersInfoW;
    let mut buf = vec![0u16; 32768];
    let ok = unsafe {
        SystemParametersInfoW(
            0x0073, // SPI_GETDESKWALLPAPER
            buf.len() as u32,
            buf.as_mut_ptr() as *mut core::ffi::c_void,
            0,
        )
    };
    if ok == 0 {
        return None;
    }
    let n = buf.iter().position(|&c| c == 0).unwrap_or(0);
    if n == 0 {
        return None;
    }
    Some(String::from_utf16_lossy(&buf[..n]))
}

/// 壁纸 data URL 缓存：(路径, mtime 秒, dataURL)。
/// 每张卡页面 onload 都会 invoke 一次 get_glass，6 张卡就是 6 次读盘 + base64；
/// 缓存后只在「换壁纸」（路径或 mtime 变）时重读。data URL 140KB+，也不适合频繁重发。
static WALL_CACHE: Mutex<Option<(String, u64, String)>> = Mutex::new(None);

fn wallpaper_data_url_cached() -> Option<String> {
    let path = wallpaper_path()?;
    let mtime = std::fs::metadata(&path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if let Ok(g) = WALL_CACHE.lock() {
        if let Some((p, t, u)) = g.as_ref() {
            if *p == path && *t == mtime {
                return Some(u.clone());
            }
        }
    }
    let bytes = std::fs::read(&path).ok()?;
    let ext = path.rsplit('.').next().unwrap_or("jpg").to_ascii_lowercase();
    let mime = match ext.as_str() {
        "png" => "image/png",
        "bmp" => "image/bmp",
        "gif" => "image/gif",
        _ => "image/jpeg",
    };
    let url = format!("data:{mime};base64,{}", base64_encode(&bytes));
    if let Ok(mut g) = WALL_CACHE.lock() {
        *g = Some((path, mtime, url.clone()));
    }
    Some(url)
}

/// 卡片磨砂层的全量数据（**逐卡**）。
///
/// ⚠️ 2026-10-07 多屏修：旧版是「主屏尺寸 + 全卡绝对坐标 + 循环里最后一张卡的 scale」三者
/// 拼一份 payload —— 副屏上的卡会裁到错位的壁纸块，混合 DPI 下连主屏的卡都可能被带歪。
/// 现在全部用**这张卡自己显示器**的坐标系：
///   · `mon` = 该显示器逻辑尺寸（物理 / 该卡 scale），
///   · `pos` = 卡在显示器内的相对位置（同尺，CSS px，与卡片页同一坐标系）。
/// Windows「填充」= 每台显示器各自 cover 壁纸，所以按显示器算才是对的。
fn card_glass_payload(app: &AppHandle, id: &str) -> Option<serde_json::Value> {
    let w = app.get_webview_window(&card_label(id))?;
    let s = w.scale_factor().unwrap_or(1.0);
    let wpos = w.outer_position().ok();
    let mon = hwnd_of(&w).and_then(display::window_monitor);
    let (mon_size, rel) = match (mon, wpos) {
        (Some((hmon, m)), Some(p)) => {
            // 记下「这张卡最近一次全量推送时的显示器」：拖动换屏时要重推全量（见 push_glass_pos）
            if let Ok(mut map) = last_mon().lock() {
                map.insert(id.to_string(), hmon);
            }
            (
                [(m.2 - m.0) as f64 / s, (m.3 - m.1) as f64 / s],
                [(p.x as f64 - m.0 as f64) / s, (p.y as f64 - m.1 as f64) / s],
            )
        }
        // 兜底（拿不到显示器句柄的罕见场景）：回退主屏口径 —— 单屏下与正式路径等价
        (_, Some(p)) => {
            let (sw, sh) = screen_size();
            ([(sw as f64) / s, (sh as f64) / s], [(p.x as f64) / s, (p.y as f64) / s])
        }
        _ => return None,
    };
    let cfg = Config::load();
    Some(serde_json::json!({
        "wall": wallpaper_data_url_cached(),
        "mon": mon_size,
        "pos": rel,
        "blur": cfg.settings.glass_blur,
        "alpha": cfg.settings.glass_alpha,
    }))
}

/// 每张卡最近一次全量推送时的显示器句柄（换屏检测用）
static LAST_MON: std::sync::OnceLock<Mutex<std::collections::HashMap<String, isize>>> =
    std::sync::OnceLock::new();

fn last_mon() -> &'static Mutex<std::collections::HashMap<String, isize>> {
    LAST_MON.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

#[tauri::command]
fn get_glass(app: AppHandle, id: Option<String>) -> serde_json::Value {
    match id.as_deref().and_then(|id| card_glass_payload(&app, id)) {
        Some(p) => p,
        None => serde_json::json!({ "wall": null }),
    }
}

#[tauri::command]
fn get_config() -> Config {
    Config::load()
}

#[tauri::command]
fn get_snapshot(snap: tauri::State<SharedSnap>) -> DataSnapshot {
    snap.read().map(|s| s.clone()).unwrap_or_default()
}

/// 保存配置（写盘 + 同步卡片窗口）。
///
/// **必须 async**：同步命令跑在主线程上，而主线程要处理窗口创建/销毁的消息；
/// 在同步命令里 create/close 卡片窗口会互相等待 → 死锁，整个 App 的 IPC 全卡住
/// （实测：在同步 save_config 里加一张卡，App 表面还在但所有 invoke 永不返回）。
#[tauri::command]
async fn save_config(app: AppHandle, cfg: Config) -> Result<(), String> {
    let mut cfg = cfg;
    // 页面持有的是旧值：以活窗口为准盖回去，防止拖动/移动被旧数据覆盖
    overlay_card_positions(&app, &mut cfg);
    if let Some(w) = app.get_webview_window("main") {
        overlay_main_bounds(&w, &mut cfg);
    }
    cfg.save()?;
    sync_cards(&app, &cfg);
    // 层级档切了就对所有活卡重放（设置页那个选项以前只存 config、不生效）
    let mode = z_mode_of(cfg.settings.z_order);
    if mode != Z_MODE.load(Ordering::Relaxed) {
        apply_z_mode(&app, mode);
    }
    // 玻璃厚度/模糊强度实时推给所有卡片（设置页那两个滑块以前只喂给已失效的 WCA，等于没用）；主题同理。
    // 强调色**不推**：主界面的强调色只改主界面，卡片正常态保持白色（用户 2026-09-24 要求）。
    let a = cfg.settings.glass_alpha;
    let b = cfg.settings.glass_blur;
    let dark = !matches!(cfg.settings.theme, config::Theme::Light);
    for (label, w) in app.webview_windows() {
        if label.starts_with("card-") {
            let _ = w.eval(&format!(
                "window.__setGlass && window.__setGlass({a}); window.__setBlur && window.__setBlur({b}); \
                 window.__setTheme && window.__setTheme({});",
                dark
            ));
        }
    }
    Ok(())
}

/// 卡片隐藏 = enabled=false + 关窗口（同样必须 async，见 save_config 注释）。
/// 注意：卡片侧已经没有「双击隐藏」手势了（用户 2026-09-28 要求），
/// 显隐统一只在主界面「桌面卡片」页的开关里做 → 这条命令已删除。
/// 参数面板：改单张卡的尺寸 / 位置 / 显隐 —— 写 config + 立刻应用到活窗口。
/// 不走 save_config 是因为那边会用「活窗口位置」盖回 cfg（保护拖动结果），
/// 会把面板里手填的 X/Y 顶掉。
/// 参数面板单卡改动（可能触发建窗/关窗 → async，见 save_config 注释）
#[tauri::command]
async fn card_update(
    app: AppHandle,
    id: String,
    size: Option<[f32; 2]>,
    pos: Option<[f32; 2]>,
    enabled: Option<bool>,
    refresh_ms: Option<u64>,
    show_gpu: Option<bool>,
    warn90: Option<bool>,
) -> Result<Config, String> {
    let mut cfg = Config::load();
    {
        let Some(card) = cfg.cards.iter_mut().find(|c| c.id == id) else {
            return Err("card not found".into());
        };
        if let Some(s) = size {
            // 尺寸制：不接受任意尺寸，一律吸附到整格（60px 的整数倍），Rust 是最终闸门。
            // 磁盘卡例外：它的高度由盘数决定（派生值），不接受外部指定 —— 面板那行 2026-10-02
            // 已删，现在只有脚本/config 能碰到这里，但闸门得留着，否则一调就把自动高度顶掉。
            if !matches!(card.kind, CardKind::Disks) {
                card.size = card.kind.snap_size(s[0], s[1]);
            }
        }
        if let Some(p) = pos {
            // 位置自由（2026-09-28 用户：先不做吸到格点），面板/脚本给什么就存什么
            card.pos = p;
        }
        if let Some(e) = enabled {
            card.enabled = e;
        }
        if let Some(r) = refresh_ms {
            card.refresh_ms = r.max(200);
        }
        if let Some(g) = show_gpu {
            card.show_gpu = g;
        }
        if let Some(x) = warn90 {
            card.warn90 = x;
        }
    }
    cfg.save()?;
    if let Some(card) = cfg.cards.iter().find(|c| c.id == id) {
        if let Some(w) = app.get_webview_window(&card_label(&id)) {
            use tauri::{LogicalPosition, LogicalSize};
            // 磁盘卡的高度是派生值（盘数决定），别用 config 里的默认档盖回去
            let sz = effective_size(
                card.kind,
                card.size,
                DISK_N.load(Ordering::Relaxed),
                DISK_STALE.load(Ordering::Relaxed),
            );
            let _ = w.set_size(tauri::Size::Logical(LogicalSize::new(
                sz[0] as f64,
                sz[1] as f64,
            )));
            let _ = w.set_position(tauri::Position::Logical(LogicalPosition::new(
                card.pos[0] as f64,
                card.pos[1] as f64,
            )));
            // 刷新间隔 / 卡片选项实时生效（卡片端 __setRate 立刻重画一次并改节流）
            let _ = w.eval(&format!(
                "window.__setRate && window.__setRate({});                  window.__setOpts && window.__setOpts({{gpu:{}, warn:{}}});",
                card.refresh_ms,
                if card.show_gpu { 1 } else { 0 },
                if card.warn90 { 1 } else { 0 }
            ));
        }
    }
    sync_cards(&app, &cfg);
    Ok(cfg)
}

/// 主界面「桌面卡片」页的图钉：固定/取消固定位置。
/// 不做建窗/关窗，同步命令即可（只有建/删窗口才会跟主线程互相等）。
#[tauri::command]
fn card_pin(app: AppHandle, id: String, pinned: bool) -> Result<Config, String> {
    let mut cfg = Config::load();
    {
        let Some(card) = cfg.cards.iter_mut().find(|c| c.id == id) else {
            return Err("card not found".into());
        };
        card.pinned = pinned;
    }
    cfg.save()?;
    if let Some(w) = app.get_webview_window(&card_label(&id)) {
        let _ = w.eval(&format!(
            "window.__setPinned && window.__setPinned({});",
            if pinned { 1 } else { 0 }
        ));
    }
    Ok(cfg)
}

/// 主界面「定位」：卡片跑出屏幕就搬回可见区 → 闪一圈高亮 → 短暂置顶，1.8s 后归位
#[tauri::command]
fn card_locate(app: AppHandle, id: String) -> bool {
    let Some(w) = app.get_webview_window(&card_label(&id)) else {
        return false;
    };
    if let (Ok(pos), Ok(size)) = (w.outer_position(), w.outer_size()) {
        let rect = (pos.x, pos.y, pos.x + size.width as i32, pos.y + size.height as i32);
        let full: Vec<display::R4> = display::monitors().into_iter().map(|(m, _)| m).collect();
        // 判据 = 「任意显示器上至少露得出 32×32」：副屏上摆得好好的卡不再被误判成越屏
        //（旧版按主屏尺寸判，会把副屏的卡拽回主屏）。落点 = 最近显示器工作区里夹回、
        // 保留尽量多原位置（旧版固定挪到主屏右上角）。越屏回收（rescue_offscreen）共用同一套。
        if !full.is_empty() && !display::visible_enough(rect, &full, 32) {
            if let Some(work) = display::nearest_work(rect) {
                let (nx, ny) = display::clamp_into(rect, work, 40);
                let _ = w.set_position(tauri::Position::Physical(tauri::PhysicalPosition::new(nx, ny)));
                let s = w.scale_factor().unwrap_or(1.0);
                let mut cfg = Config::load();
                if let Some(c) = cfg.cards.iter_mut().find(|c| c.id == id) {
                    c.pos = [nx as f32 / s as f32, ny as f32 / s as f32];
                    let _ = cfg.save();
                }
            }
        }
    }
    let _ = w.eval("window.__flash && window.__flash();");
    // 唯一允许临时置顶的例外：定位高亮。放行 1.8s，超时自动恢复（见窗口子类的 z 序总闸）
    suspend_z_lock(1800);
    if let Some(h) = hwnd_of(&w) {
        unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::SetWindowPos(
                h,
                -1isize as *mut core::ffi::c_void, // HWND_TOPMOST（不带 SWP_NOZORDER）
                0,
                0,
                0,
                0,
                0x0001 | 0x0002 | 0x0010, // NOSIZE | NOMOVE | NOACTIVATE
            );
        }
    }
    let w2 = w.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(1800));
        let cfg = Config::load();
        if let Some(h) = hwnd_of(&w2) {
            match cfg.settings.z_order {
                ZOrder::Desktop => apply_desktop_zorder(h),
                ZOrder::Normal => {
                    let _ = w2.set_always_on_top(false);
                }
                ZOrder::Top => {
                    let _ = w2.set_always_on_top(true);
                }
            }
        }
        let _ = w2.eval("window.__unflash && window.__unflash();");
        resume_z_lock(); // 放行结束：巡检/子类重新接管 z 序
    });
    true
}

/* 位置不再有「对齐桌面」命令（2026-09-28 用户：先不做吸到格点，位置自由拖动）。
   拖动对齐改成拖动时的辅助线 + 磁吸边，见 src/guide.rs。 */

/// 待办卡：勾选/取消一条（卡片内点击，`data-idx` = config.todo 原数组下标）。
/// 落盘后回传最新列表（卡片以响应为准重绘）。
#[tauri::command]
fn todo_toggle(id: String, index: usize) -> Result<Vec<config::TodoItem>, String> {
    let mut cfg = Config::load();
    let Some(card) = cfg.cards.iter_mut().find(|c| c.id == id) else {
        return Err("card not found".into());
    };
    if index >= card.todo.len() {
        return Err("index out of range".into());
    }
    card.todo[index].done = !card.todo[index].done;
    let items = card.todo.clone();
    cfg.save()?;
    Ok(items)
}

/// 参数面板编辑待办：整表替换（空文本剔除、上限 20 条）。
/// 落盘后卡片窗口开着的话让它立即刷新（__todoRefresh 钩子）。
#[tauri::command]
fn todo_set(
    app: AppHandle,
    id: String,
    items: Vec<config::TodoItem>,
) -> Result<Vec<config::TodoItem>, String> {
    let mut cfg = Config::load();
    let Some(card) = cfg.cards.iter_mut().find(|c| c.id == id) else {
        return Err("card not found".into());
    };
    card.todo = items
        .into_iter()
        .filter(|t| !t.text.trim().is_empty())
        .take(20)
        .collect();
    let out = card.todo.clone();
    cfg.save()?;
    if let Some(w) = app.get_webview_window(&card_label(&id)) {
        let _ = w.eval("window.__todoRefresh && window.__todoRefresh();");
    }
    Ok(out)
}

/* ───────────── 天气（Open-Meteo，免 key）─────────────
   注意：Tauri 的**同步**命令跑在主线程上 → 网络请求必须走 async + spawn_blocking，
   否则一次卡住的 DNS/TLS 会把整个 App（界面、卡片、热键）冻住（实测踩过）。 */

/// 当前天气快照（卡片/主界面取数；未取到 = None → 卡片停在「等待数据…」）
#[tauri::command]
fn get_weather(w: tauri::State<weather::SharedWeather>) -> Option<weather::WeatherSnap> {
    w.read().ok().and_then(|g| g.clone())
}

/// 立刻拉一轮（切城市后、或界面上的手动刷新）
#[tauri::command]
async fn weather_refresh(
    app: AppHandle,
    w: tauri::State<'_, weather::SharedWeather>,
) -> Result<weather::WeatherSnap, String> {
    let shared = w.inner().clone();
    tauri::async_runtime::spawn_blocking(move || weather::refresh(&shared, &app))
        .await
        .map_err(|e| e.to_string())?
}

/// 城市搜索（Photon/OSM 为主，Open-Meteo 地理编码兜底）
#[tauri::command]
async fn weather_search(q: String) -> Result<Vec<weather::Place>, String> {
    tauri::async_runtime::spawn_blocking(move || weather::search(&q))
        .await
        .map_err(|e| e.to_string())
}

/// 指定城市 / 切回自动检测。auto=true 时清掉坐标，由 IP 归属地决定。
#[tauri::command]
async fn weather_set_place(
    app: AppHandle,
    w: tauri::State<'_, weather::SharedWeather>,
    city: String,
    // 省（Photon 的 state），拿它把「朝阳」这种重名城市对到正确的中国天气网代码
    region: Option<String>,
    lat: f64,
    lon: f64,
    auto: bool,
) -> Result<Config, String> {
    let mut cfg = Config::load();
    if auto {
        cfg.settings.weather_city = String::new();
        cfg.settings.weather_lat = None;
        cfg.settings.weather_lon = None;
        cfg.settings.weather_wxid = None;
    } else {
        // 实况要走中国天气网就得先有城市代码；查不到存 None，实况自动回退 Open-Meteo
        cfg.settings.weather_wxid =
            weather::wx_resolve(&city, region.as_deref().unwrap_or(""));
        cfg.settings.weather_city = city;
        cfg.settings.weather_lat = Some(lat);
        cfg.settings.weather_lon = Some(lon);
    }
    cfg.save()?;
    // 拉失败不算错（卡片会自己退到异常态，界面照常显示新城市）
    let shared = w.inner().clone();
    let app2 = app.clone();
    let _ = tauri::async_runtime::spawn_blocking(move || weather::refresh(&shared, &app2)).await;
    Ok(cfg)
}

/* ───────────── 全局热键：显示/隐藏全部卡片 ───────────── */

/// 卡片当前是否显示（热键是运行时开关，不改 config 的 enabled）
struct CardVis(AtomicBool);

fn toggle_cards(app: &AppHandle) {
    let vis = app.state::<CardVis>();
    let visible = vis.0.load(Ordering::SeqCst);
    let cfg = Config::load();
    for c in cfg.cards.iter().filter(|c| c.enabled) {
        if let Some(w) = app.get_webview_window(&card_label(&c.id)) {
            if visible {
                let _ = w.hide();
            } else {
                let _ = w.show();
            }
        }
    }
    vis.0.store(!visible, Ordering::SeqCst);
}

/// 注册热键（空串 = 注销）。解析/占用失败把原因回给界面。
fn apply_hotkey(app: &AppHandle, combo: &str) -> Result<(), String> {
    let gs = app.global_shortcut();
    let _ = gs.unregister_all();
    let c = combo.trim();
    if c.is_empty() {
        return Ok(());
    }
    let sc = Shortcut::from_str(c).map_err(|e| format!("无法识别「{c}」：{e}"))?;
    gs.register(sc)
        .map_err(|e| format!("注册失败（可能已被其它程序占用）：{e}"))
}

/// 按 config 里的开关状态同步注册结果：关掉时不注册（键位保留在 config 里）。
/// 启动、录制成功、点开关都走这一个入口，避免三处各写一遍。
fn sync_hotkey(app: &AppHandle) -> Result<(), String> {
    let s = Config::load().settings;
    if s.hotkey_on {
        apply_hotkey(app, &s.hotkey)
    } else {
        apply_hotkey(app, "")
    }
}

/// 界面「重新录制」按下组合键后调这里
#[tauri::command]
fn hotkey_set(app: AppHandle, combo: String) -> Result<(), String> {
    // 录制成功即视为启用（用户按下组合键就是想让它生效）
    let mut cfg = Config::load();
    cfg.settings.hotkey = combo.clone();
    cfg.settings.hotkey_on = true;
    cfg.save()?;
    apply_hotkey(&app, &combo)
}

/// 设置页「全局热键」那个开关
#[tauri::command]
fn hotkey_enable(app: AppHandle, on: bool) -> Result<(), String> {
    let mut cfg = Config::load();
    cfg.settings.hotkey_on = on;
    cfg.save()?;
    sync_hotkey(&app)
}

#[tauri::command]
fn win_min(window: WebviewWindow) {
    let _ = window.minimize();
}

#[tauri::command]
fn win_max(window: WebviewWindow) {
    if matches!(window.is_maximized(), Ok(true)) {
        let _ = window.unmaximize();
    } else {
        let _ = window.maximize();
    }
}

/// 关闭按钮：按 config 决定「隐藏到托盘」或退出；落一次窗口位置。
#[tauri::command]
fn win_close(window: WebviewWindow) {
    persist_bounds(&window);
    if Config::load().settings.close_to_tray {
        let _ = window.hide();
    } else {
        let app = window.app_handle().clone();
        quit(&app);
    }
}

// ─── 开机自启（HKCU\...\Run）：设置页那个开关以前只改 config，这里补上真实现 ───

const AUTOSTART_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const AUTOSTART_VALUE: &str = "Mini Card";

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 注册表里当前的启动命令（没有则 None）
fn autostart_command() -> Option<String> {
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE,
        REG_SZ,
    };
    unsafe {
        let mut hkey: HKEY = std::ptr::null_mut();
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            wide(AUTOSTART_KEY).as_ptr(),
            0,
            KEY_QUERY_VALUE,
            &mut hkey,
        ) != 0
        {
            return None;
        }
        let name = wide(AUTOSTART_VALUE);
        let mut kind: u32 = 0;
        let mut len: u32 = 0;
        let mut st = RegQueryValueExW(
            hkey,
            name.as_ptr(),
            std::ptr::null(),
            &mut kind,
            std::ptr::null_mut(),
            &mut len,
        );
        if st != 0 || kind != REG_SZ || len < 2 {
            RegCloseKey(hkey);
            return None;
        }
        let mut buf = vec![0u8; len as usize + 2];
        st = RegQueryValueExW(
            hkey,
            name.as_ptr(),
            std::ptr::null(),
            &mut kind,
            buf.as_mut_ptr(),
            &mut len,
        );
        RegCloseKey(hkey);
        if st != 0 {
            return None;
        }
        let words: Vec<u16> = buf[..len as usize]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        let n = words.iter().position(|&c| c == 0).unwrap_or(words.len());
        Some(String::from_utf16_lossy(&words[..n]))
    }
}

/// 注册表里的自启项是否指向当前这个 exe（指向旧路径 / 别的版本都算没开）
fn autostart_enabled() -> bool {
    let cur = std::env::current_exe()
        .ok()
        .map(|p| format!("\"{}\"", p.display()));
    match (autostart_command(), cur) {
        (Some(v), Some(c)) => v.trim().eq_ignore_ascii_case(c.trim()),
        _ => false,
    }
}

fn autostart_apply(on: bool) -> Result<(), String> {
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW, RegSetValueExW, HKEY,
        HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ,
    };
    let exe = std::env::current_exe().map_err(|e| format!("取程序路径失败：{e}"))?;
    let cmd = format!("\"{}\"", exe.display()); // 带引号：路径含空格时 Run 不会切碎
    unsafe {
        if on {
            let mut hkey: HKEY = std::ptr::null_mut();
            let mut disp: u32 = 0;
            let st = RegCreateKeyExW(
                HKEY_CURRENT_USER,
                wide(AUTOSTART_KEY).as_ptr(),
                0,
                std::ptr::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_SET_VALUE | KEY_QUERY_VALUE,
                std::ptr::null(),
                &mut hkey,
                &mut disp,
            );
            if st != 0 {
                return Err(format!("打开注册表失败（错误码 {st}）"));
            }
            let name = wide(AUTOSTART_VALUE);
            let data: Vec<u16> = cmd.encode_utf16().chain(std::iter::once(0)).collect();
            let bytes = std::slice::from_raw_parts(data.as_ptr() as *const u8, data.len() * 2);
            let st = RegSetValueExW(
                hkey,
                name.as_ptr(),
                0,
                REG_SZ,
                bytes.as_ptr(),
                bytes.len() as u32,
            );
            RegCloseKey(hkey);
            if st != 0 {
                return Err(format!("写入自启项失败（错误码 {st}）"));
            }
        } else {
            let mut hkey: HKEY = std::ptr::null_mut();
            let st = RegOpenKeyExW(
                HKEY_CURRENT_USER,
                wide(AUTOSTART_KEY).as_ptr(),
                0,
                KEY_SET_VALUE,
                &mut hkey,
            );
            if st == 0 {
                let name = wide(AUTOSTART_VALUE);
                let _ = RegDeleteValueW(hkey, name.as_ptr()); // 值本来就不在也算成功
                RegCloseKey(hkey);
            }
        }
    }
    Ok(())
}

/// 设置页「开机自动启动」开关
#[tauri::command]
fn autostart_set(on: bool) -> Result<bool, String> {
    autostart_apply(on)?;
    let mut cfg = Config::load();
    cfg.settings.autostart = on;
    cfg.save()?;
    Ok(autostart_enabled())
}

/// 回读真实状态（任务管理器里被禁过 / 手动删过注册表项时，界面显示的就是实情）
#[tauri::command]
fn autostart_state() -> bool {
    autostart_enabled()
}

// ─── 软件更新：查 Release / 自更新（实现在 update.rs）───

/// 真查一次：有新版回 has_new=true（界面 toast + 可以自己点开 url）
#[tauri::command]
async fn check_update(app: AppHandle) -> Result<serde_json::Value, String> {
    let cur = app.package_info().version.to_string();
    tauri::async_runtime::spawn_blocking(move || update::check(&cur))
        .await
        .map_err(|e| format!("检查更新失败：{e}"))?
}

/// 自更新：下载 → ed25519 验签 → sha256 校验 → 解压 → 改名替换 → 启动新版 → 本进程退出。
/// 阻塞 IO 全部在 spawn_blocking 里（同步命令里做网络 IO 会占死 tokio worker）。
#[tauri::command]
async fn update_install(app: AppHandle) -> Result<(), String> {
    let handle = app.clone();
    tauri::async_runtime::spawn_blocking(move || update::install(&handle))
        .await
        .map_err(|e| format!("更新失败：{e}"))??;
    // 替换已就位、新版已拉起 → 落盘窗口位置后退出（新版在单实例锁上等着接管）
    let h2 = app.clone();
    let _ = tauri::async_runtime::spawn_blocking(move || quit(&h2)).await;
    Ok(())
}

/// 退出前把 main + 所有卡片窗口位置落盘
fn quit(app: &AppHandle) {
    let mut cfg = Config::load();
    if let Some(w) = app.get_webview_window("main") {
        overlay_main_bounds(&w, &mut cfg);
    }
    overlay_card_positions(app, &mut cfg);
    let _ = cfg.save();
    app.exit(0);
}

// ─── 卡片窗口 chrome：小框 / 玻璃 / 桌面层级 三个修复的落点 ───

/// 被子类化的原窗口过程。全部卡片窗口同属一个窗口类 → 原 proc 唯一，存一份即可。
static ORIG_PROC: Mutex<Option<windows_sys::Win32::UI::WindowsAndMessaging::WNDPROC>> =
    Mutex::new(None);

/// WM_NCCALCSIZE = 0x0083：直接令 NC=0（客户区=窗口区），
/// 消掉 tao 无条件 WS_CAPTION 带来的 8px 非客户区带 + DWM 可见边框线（=「小框」）。
/// 实测仅改 style 位不够（tao window_state.rs:244 每次 flags 同步都会加回 WS_CAPTION）。
///
/// 另：WM_NCACTIVATE(0x0086) 一并吞掉——实测这是「卡片顶上多出一条带窗口标题的浅色
/// 标题栏」的根因：Windows 在激活状态变化时让 DefWindowProc 重画 caption，而我们的
/// NC 区是 0，caption 就画进客户区，透过半透明卡面显出来（先稳定复现：SendMessage
/// WM_NCACTIVATE(1) 必现；隐藏/重显窗口可清掉）。WM_NCPAINT 与主题引擎私有的
/// WM_NCUAHDRAWCAPTION/WM_NCUAHDRAWFRAME 同理一并拒绝绘制。
unsafe extern "system" fn card_wnd_proc(
    hwnd: *mut core::ffi::c_void,
    msg: u32,
    wparam: usize,
    lparam: isize,
) -> isize {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        STYLESTRUCT, WINDOWPOS, GWL_EXSTYLE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
        SWP_NOZORDER, WS_EX_NOACTIVATE,
    };

    match msg {
        // wParam!=0: rgrc[0] 即候选矩形，不改它 → 客户区=窗口矩形，NC=0
        0x0083 => return 0, // WM_NCCALCSIZE
        0x0086 => return 1, // WM_NCACTIVATE：自己处理，不回原 proc → 不重画标题栏
        0x0085 => return 0, // WM_NCPAINT：无 NC 区，无需绘制
        0x00AE | 0x00AF => return 0, // WM_NCUAHDRAWCAPTION / WM_NCUAHDRAWFRAME

        /* ── 桌面层锁（只在 Desktop 档、且没被「定位」临时放行时生效）── */
        // B：点击不激活、也不丢鼠标消息（MA_NOACTIVATE=3）。
        //    微软文档：WS_EX_NOACTIVATE 只挡「前台激活」，点击引起的输入队列激活
        //    必须靠处理 WM_MOUSEACTIVATE 才行。用 4（…ANDEAT）会把点击吃掉。
        0x0021 => {
            if desktop_mode() {
                return 3; // WM_MOUSEACTIVATE → MA_NOACTIVATE
            }
        }
        // C：z 序总闸。任何来源的抬升（别家 BringWindowToTop、显示桌面恢复、Explorer
        //    重排、拖动循环）都在这里被改成「插到桌面锚点正上方」。我们自己带 SWP_NOZORDER
        //    的调用（1px 抖动 / set_size）不动它，所以不会跟 tao 打架。
        0x0046 => {
            // WM_WINDOWPOSCHANGING
            if desktop_mode() && !z_suspended() {
                let wp = &mut *(lparam as *mut WINDOWPOS);
                if wp.flags & SWP_NOZORDER == 0 {
                    let a = anchor_hwnd();
                    if !a.is_null() {
                        wp.hwndInsertAfter = a;
                        wp.flags |= SWP_NOACTIVATE;
                    }
                }
                // Win+D / 点任务栏「显示桌面」会要求把窗口搬到 -32000：跟它耗住
                if wp.x == -32000 && wp.y == -32000 {
                    wp.flags |= SWP_NOMOVE | SWP_NOSIZE;
                }
            }
        }
        // A：tao 每次 flag 同步都用 to_window_styles() 整份重写 exstyle → 这里补回来
        0x007C => {
            // WM_STYLECHANGING
            if desktop_mode() && !z_suspended() && wparam as i32 == GWL_EXSTYLE {
                let ss = &mut *(lparam as *mut STYLESTRUCT);
                ss.styleNew |= WS_EX_NOACTIVATE;
            }
        }
        // 拖动：对齐辅助线 + 磁吸边（guide.rs）。
        // WM_MOVING 是系统拖动循环里「位置还没落地」的那一枪 → 改 RECT 就能无抖动地磁吸；
        // WM_EXITSIZEMOVE 收尾（清线 + 隐藏辅助线窗口 + 把卡钉回桌面层）。
        0x0216 => {
            // WM_MOVING
            let r = &mut *(lparam as *mut windows_sys::Win32::Foundation::RECT);
            guide::moving(hwnd, r);
        }
        // 拖动开始/结束（WM_ENTERSIZEMOVE=0x0231 / WM_EXITSIZEMOVE=0x0232）
        // ⚠️ 2026-09-28 修：这里原先写的是 0x0024（= WM_GETMINMAXINFO，不是 WM_EXITSIZEMOVE！）
        //    → 拖完既不收辅助线也不立刻归位；辅助线一直挂在屏幕上就是这么来的。
        0x0231 => {
            guide::begin_move();
        }
        0x0232 => {
            guide::end_move(hwnd);
            if desktop_mode() {
                apply_desktop_zorder(hwnd);
            }
        }
        // 屏幕拓扑 / 系统设置 / 每窗口 DPI 变了（分辨率、显示器增删、Windows 缩放、壁纸、
        // Explorer 重启）→ 锚点作废；去抖后统一善后（越屏回收 + 磨砂切片重推）。
        // WM_DPICHANGED(0x02E0) 是「缩放变了」最准的一枪（每个窗口各发一次，去抖器合并）。
        0x007E | 0x001A | 0x02E0 => {
            ANCHOR.store(0, Ordering::Relaxed);
            schedule_display_recheck();
        }
        _ => {}
    }
    let orig = ORIG_PROC.lock().ok().and_then(|g| *g);
    match orig {
        Some(p) => windows_sys::Win32::UI::WindowsAndMessaging::CallWindowProcW(
            p, hwnd, msg, wparam, lparam,
        ),
        None => windows_sys::Win32::UI::WindowsAndMessaging::DefWindowProcW(
            hwnd, msg, wparam, lparam,
        ),
    }
}

#[repr(C)]
struct AccentPolicy {
    state: i32,
    flags: i32,
    color: u32, // ABGR
    anim: i32,
}
#[repr(C)]
struct WcaData {
    attrib: u32,
    _pad: u32,
    data: *const AccentPolicy,
    len: usize,
}

/// 未公开 API，user32.lib 里没有 → 动态 GetProcAddress 取。
type WcaFn = unsafe extern "system" fn(
    *mut core::ffi::c_void,
    *mut WcaData,
) -> i32;

fn wca_fn() -> Option<WcaFn> {
    use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, GetModuleHandleW};
    unsafe {
        let name = b"SetWindowCompositionAttribute\0";
        let m = GetModuleHandleW(core::ptr::null());
        if m.is_null() {
            return None;
        }
        let p = GetProcAddress(m, name.as_ptr())?;
        Some(std::mem::transmute::<
            unsafe extern "system" fn() -> isize,
            WcaFn,
        >(p))
    }
}

/// WCA 丙烯酸玻璃：实测聚焦/失焦两张截图逐像素一致（g_wcaB on==off），
/// 替代 DWM backdrop（失焦回退白色，terminal#593 同款系统行为，无解）。
/// glass_alpha(0-100 UI) → 白色染色 alpha。
fn apply_wca_glass(hwnd: *mut core::ffi::c_void, glass_alpha: u8) {
    let Some(f) = wca_fn() else { return };
    let alpha = (glass_alpha as u32).saturating_mul(2).min(220);
    let policy = AccentPolicy {
        state: 3, // ACCENT_ENABLE_ACRYLICBLURBEHIND
        flags: 2,
        color: (alpha << 24) | 0x00FF_FFFF, // ABGR 白
        anim: 0,
    };
    let mut data = WcaData {
        attrib: 19, // WCA_ACCENT_POLICY
        _pad: 0,
        data: &policy,
        len: core::mem::size_of::<AccentPolicy>(),
    };
    unsafe {
        f(hwnd, &mut data);
    }
}

/* ───────────── 桌面层锁：卡片永远待在「壁纸上、普通窗口下」─────────────

   历史坑（实测）：tao 0.35.3 的 `WindowState::set_window_flags()` 在任一 flag 变化时
   用 `to_window_styles()` **整份重写** GWL_STYLE + GWL_EXSTYLE（window_state.rs:437-441），
   而它只在 `!FOCUSABLE` 时才带 WS_EX_NOACTIVATE（window_state.rs:296-298）。
   建卡顺序（chrome → 手加 NOACTIVATE → set_size 1px 抖动 → show()）里，
   set_inner_size / set_visible 都走 set_window_flags → 手加的位必掉
   （实测活卡 exstyle = 0x00040110，没有 0x08000000）→ 点卡片被系统激活 → 抬到普通窗口之上。
   四道锁缺一不可（方案与验收见 docs/desktop-layer-lock.md）：
     A 样式：set_focusable(false)（tao 自己算风格就带 NOACTIVATE）+ WM_STYLECHANGING 兜底
     B 点不激活：WM_MOUSEACTIVATE → MA_NOACTIVATE（微软文档点名的做法）
     C z 序总闸：WM_WINDOWPOSCHANGING 里把 hwndInsertAfter 钉回桌面锚点
     D 重锚：巡检线程 + WM_DISPLAYCHANGE/SETTINGCHANGE 自愈（锚点会死） */

/// 桌面锚点 = 含 SHELLDLL_DefView 的桌面壳窗口（Win11 24H2+ 就是 Progman）；0 = 待解析
static ANCHOR: AtomicIsize = AtomicIsize::new(0);
/// 层级档（0=Desktop / 1=Normal / 2=Top）；子类里要用它决定锁不锁
static Z_MODE: AtomicU8 = AtomicU8::new(0);
/// 桌面层锁的临时放行截止时间（ms；0 = 不放行）。只给主界面「定位」的 1.8s 高亮用。
static Z_SUSPEND_UNTIL: AtomicU64 = AtomicU64::new(0);

fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn z_mode_of(z: ZOrder) -> u8 {
    match z {
        ZOrder::Desktop => 0,
        ZOrder::Normal => 1,
        ZOrder::Top => 2,
    }
}

fn desktop_mode() -> bool {
    Z_MODE.load(Ordering::Relaxed) == 0
}

/// 放行中？（只有「定位」高亮那 1.8s）
fn z_suspended() -> bool {
    let t = Z_SUSPEND_UNTIL.load(Ordering::Relaxed);
    t != 0 && now_ms() < t
}

fn suspend_z_lock(ms: u64) {
    Z_SUSPEND_UNTIL.store(now_ms() + ms, Ordering::Relaxed);
}

fn resume_z_lock() {
    Z_SUSPEND_UNTIL.store(0, Ordering::Relaxed);
}

/// 顶层窗是不是「桌面壳」（Progman / WorkerW）？
/// ⚠️ 必须按窗口类筛：资源管理器的 CabinetWClass 窗口，子窗里**也有** SHELLDLL_DefView
/// （那是资源管理器自己的文件视图），光靠「子窗含 DefView」会把资源管理器窗口当成桌面锚点。
fn is_shell_class_name(name: &str) -> bool {
    matches!(name, "Progman" | "WorkerW")
}

fn is_desktop_shell(hwnd: *mut core::ffi::c_void) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::GetClassNameW;
    let mut buf = [0u16; 64];
    let n = unsafe { GetClassNameW(hwnd, buf.as_mut_ptr(), buf.len() as i32) };
    if n <= 0 {
        return false;
    }
    is_shell_class_name(&String::from_utf16_lossy(&buf[..n as usize]))
}

/// 找桌面锚点：顶层窗里，**窗口类是 Progman/WorkerW 且**子窗口含 SHELLDLL_DefView 的那个
/// （= 桌面图标宿主）。
///
/// 历史坑（2026-10-06 实测）：早期只判「子窗含 SHELLDLL_DefView」，而 EnumWindows 是
/// 按 z 序 top→bottom 枚举的 → 只要资源管理器窗口开着且排在前面，锚点就被解析成资源管理器
/// 的 CabinetWClass 窗口，卡片被插到它正上方 = 卡片「跟着跳到文件管理器前面」
/// （触发条件：ANCHOR 被 WM_SETTINGCHANGE/WM_DISPLAYCHANGE 清掉后的下一轮重解析、
/// 冷启动时资源管理器排在最前、Explorer 重启）。加窗口类判据后锚点恒为 Progman/WorkerW。
fn find_desktop_anchor() -> *mut core::ffi::c_void {
    use windows_sys::Win32::UI::WindowsAndMessaging::{EnumChildWindows, EnumWindows, GetClassNameW};

    struct FindShell {
        hwnd: *mut core::ffi::c_void,
    }
    unsafe extern "system" fn child_cb(h: *mut core::ffi::c_void, lparam: isize) -> i32 {
        let mut buf = [0u16; 64];
        let n = GetClassNameW(h, buf.as_mut_ptr(), buf.len() as i32);
        if n > 0 && String::from_utf16_lossy(&buf[..n as usize]) == "SHELLDLL_DefView" {
            let f = &mut *(lparam as *mut FindShell);
            f.hwnd = h;
            return 0;
        }
        1
    }
    unsafe extern "system" fn top_cb(hwnd: *mut core::ffi::c_void, lparam: isize) -> i32 {
        // 先按窗口类筛掉资源管理器（CabinetWClass）等同样带 SHELLDLL_DefView 子窗的普通窗口
        if !is_desktop_shell(hwnd) {
            return 1;
        }
        let f = &mut *(lparam as *mut FindShell);
        let mut sub = FindShell {
            hwnd: std::ptr::null_mut(),
        };
        EnumChildWindows(hwnd, Some(child_cb), &mut sub as *mut FindShell as isize);
        if !sub.hwnd.is_null() {
            f.hwnd = hwnd;
            return 0;
        }
        1
    }

    let mut shell = FindShell {
        hwnd: std::ptr::null_mut(),
    };
    unsafe {
        EnumWindows(Some(top_cb), &mut shell as *mut FindShell as isize);
    }
    shell.hwnd
}

/// 锚点句柄：缓存着用；失效（Explorer 重启 / 抢 WorkerW）就重解析。
/// ⚠️ 校验必须带「窗口类是 Progman/WorkerW」这一条：只判 `IsWindow` 的话，
///    句柄被系统回收复用（重解析期间那一下）也能通过，卡片就会按错误的锚点排 z 序。
fn anchor_hwnd() -> *mut core::ffi::c_void {
    use windows_sys::Win32::UI::WindowsAndMessaging::IsWindow;
    let a = ANCHOR.load(Ordering::Relaxed);
    if a != 0 {
        let h = a as *mut core::ffi::c_void;
        if unsafe { IsWindow(h) } != 0 && is_desktop_shell(h) {
            return h;
        }
        ANCHOR.store(0, Ordering::Relaxed);
    }
    let f = find_desktop_anchor();
    ANCHOR.store(f as isize, Ordering::Relaxed);
    f
}

/// 补上 WS_EX_NOACTIVATE（tao 每次整份重写 exstyle 后由 WM_STYLECHANGING 兜回）
fn ensure_noactivate(card: *mut core::ffi::c_void) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE, WS_EX_NOACTIVATE,
    };
    unsafe {
        let style = GetWindowLongPtrW(card, GWL_EXSTYLE);
        if style & (WS_EX_NOACTIVATE as isize) == 0 {
            SetWindowLongPtrW(card, GWL_EXSTYLE, style | WS_EX_NOACTIVATE as isize);
        }
    }
}

/// 桌面层级：插到锚点正上方（= 壁纸之上、所有普通窗口之下），并保证 NOACTIVATE
fn apply_desktop_zorder(card: *mut core::ffi::c_void) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SetWindowPos, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    };
    let anchor = anchor_hwnd();
    if anchor.is_null() {
        return;
    }
    ensure_noactivate(card);
    unsafe {
        SetWindowPos(
            card,
            anchor,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
    }
}

/// 把所有卡片钉回桌面层（档位是 Desktop 且没被放行时才动）
fn reassert_desktop_layer(app: &AppHandle) {
    if !desktop_mode() || z_suspended() {
        return;
    }
    for (label, w) in app.webview_windows() {
        if label.starts_with("card-") {
            if let Some(h) = hwnd_of(&w) {
                apply_desktop_zorder(h);
            }
        }
    }
}

/// 巡检线程：2s 一次。锚点会死（Explorer 重启、Wallpaper Engine/Fences 抢 WorkerW、
/// 多屏拓扑变化、壁纸切换），死了就重解析，再把卡片钉回桌面层。
/// 代价 = 每轮两个 API 调用 + 每卡一次 SetWindowPos（z 序本就对时等于空操作）。
///
/// 2026-10-08 起这里还做一件事：**显示器签名巡检**（分辨率/拓扑/缩放变化）。
/// 卡片子类收 WM_DISPLAYCHANGE/SETTINGCHANGE/DPICHANGED 是第一道触发，但那条路
/// 依赖「有卡窗存在」；这轮签名检查是兜底，也覆盖主窗单独在的场景。
fn spawn_z_guard(app: AppHandle) {
    std::thread::spawn(move || {
        let mut sig = display_signature();
        loop {
            std::thread::sleep(std::time::Duration::from_secs(2));
            let cur = display_signature();
            if cur != sig {
                sig = cur;
                ANCHOR.store(0, Ordering::Relaxed);
                schedule_display_recheck();
            }
            if !desktop_mode() {
                continue;
            }
            let _ = anchor_hwnd(); // 失效自动重解析
            reassert_desktop_layer(&app);
        }
    });
}

/// 显示器布局签名（物理 px + 主屏 DPI）：分辨率、显示器增删、主屏缩放任一变都改这个值。
/// 只比整数，2s 一次，开销可忽略。
fn display_signature() -> (i32, i32, i32, i32, i32, i32, i32, i32) {
    use windows_sys::Win32::Graphics::Gdi::{GetDC, GetDeviceCaps, ReleaseDC, LOGPIXELSX};
    use windows_sys::Win32::UI::WindowsAndMessaging::GetSystemMetrics;
    unsafe {
        let dc = GetDC(std::ptr::null_mut());
        let dpi = if dc.is_null() {
            96
        } else {
            let d = GetDeviceCaps(dc, LOGPIXELSX as i32);
            ReleaseDC(std::ptr::null_mut(), dc);
            d
        };
        (
            GetSystemMetrics(0),  // SM_CXSCREEN
            GetSystemMetrics(1),  // SM_CYSCREEN
            GetSystemMetrics(76), // SM_XVIRTUALSCREEN
            GetSystemMetrics(77), // SM_YVIRTUALSCREEN
            GetSystemMetrics(78), // SM_CXVIRTUALSCREEN
            GetSystemMetrics(79), // SM_CYVIRTUALSCREEN
            GetSystemMetrics(80), // SM_CMONITORS
            dpi,
        )
    }
}

/// 全局 AppHandle：卡片子类等 Win32 回调没有 Tauri 上下文，靠这个入口派任务。
static APP: std::sync::OnceLock<AppHandle> = std::sync::OnceLock::new();
/// 上一次显示器善后的时间戳（去抖：一次系统变化会广播给每个窗口，别每张卡各跑一遍）
static RECHECK_AT: AtomicU64 = AtomicU64::new(0);

/// 显示器拓扑/缩放/分辨率变化后的统一善后（去抖 900ms 后跑一次）：
/// 越屏回收 → （①修后还会推磨砂全量）。卡片子类收广播时调，2s 巡检兜底。
fn schedule_display_recheck() {
    let Some(app) = APP.get() else {
        return;
    };
    let now = now_ms();
    if now.saturating_sub(RECHECK_AT.load(Ordering::Relaxed)) < 900 {
        return;
    }
    RECHECK_AT.store(now, Ordering::Relaxed);
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(900)); // 等系统把新布局/新缩放落定
        rescue_offscreen(&app);
        refresh_glass_all(&app);
    });
}

/// 越屏回收：分辨率变小 / 拔显示器 / 改缩放之后，把「整个都露不出来」的卡片和主窗
/// 夹回最近显示器的工作区（保留尽量多原位置）。
/// 不碰：拖动中（跟磁吸打架）、部分越屏（用户可能故意摆出边）、最小化的主窗（-32000 是残影）。
fn rescue_offscreen(app: &AppHandle) {
    if guide::is_moving() {
        return;
    }
    let mons = display::monitors();
    if mons.is_empty() {
        return;
    }
    let full: Vec<display::R4> = mons.iter().map(|(m, _)| *m).collect();
    let mut cfg = Config::load();
    let mut dirty = false;

    for card in cfg.cards.iter_mut().filter(|c| c.enabled) {
        let Some(w) = app.get_webview_window(&card_label(&card.id)) else {
            continue;
        };
        let (Ok(pos), Ok(sz)) = (w.outer_position(), w.outer_size()) else {
            continue;
        };
        let rect = (pos.x, pos.y, pos.x + sz.width as i32, pos.y + sz.height as i32);
        if display::visible_enough(rect, &full, 32) {
            continue;
        }
        let Some(work) = display::nearest_work(rect) else {
            continue;
        };
        let (nx, ny) = display::clamp_into(rect, work, 8);
        let _ = w.set_position(tauri::Position::Physical(tauri::PhysicalPosition::new(nx, ny)));
        let s = w.scale_factor().unwrap_or(1.0);
        card.pos = [nx as f32 / s as f32, ny as f32 / s as f32];
        dirty = true;
    }

    // 主窗：藏到托盘时会保留原位置，一并夹；最小化时不碰（rect 是 -32000 残影）
    if let Some(w) = app.get_webview_window("main") {
        let iconic = hwnd_of(&w)
            .map(|h| unsafe { windows_sys::Win32::UI::WindowsAndMessaging::IsIconic(h) } != 0)
            .unwrap_or(false);
        if !iconic {
            if let (Ok(pos), Ok(sz)) = (w.outer_position(), w.outer_size()) {
                let rect = (pos.x, pos.y, pos.x + sz.width as i32, pos.y + sz.height as i32);
                if !display::visible_enough(rect, &full, 32) {
                    if let Some(work) = display::nearest_work(rect) {
                        let (nx, ny) = display::clamp_into(rect, work, 8);
                        let _ = w.set_position(tauri::Position::Physical(
                            tauri::PhysicalPosition::new(nx, ny),
                        ));
                        overlay_main_bounds(&w, &mut cfg);
                        dirty = true;
                    }
                }
            }
        }
    }

    if dirty {
        let _ = cfg.save();
    }
}

/// 磨砂切片全量重推给所有卡片（显示器变化善后用；卡片页 onload 自己也会拉一次）。
fn refresh_glass_all(app: &AppHandle) {
    let cfg = Config::load();
    for card in cfg.cards.iter().filter(|c| c.enabled) {
        let Some(payload) = card_glass_payload(app, &card.id) else {
            continue;
        };
        if let Some(w) = app.get_webview_window(&card_label(&card.id)) {
            let _ = w.eval(&format!(
                "window.__setWall && window.__setWall({payload});"
            ));
        }
    }
}

/// 卡片移动后推磨砂数据：
/// · 同一显示器 → 只发 id + 相对位置（轻量）：壁纸 data URL 140KB+，不能跟着拖动重发；
/// · 换显示器（跨屏拖动）→ 重推全量 —— 显示器的尺寸/原点/壁纸都变了，只推位置会错位。
/// 同时把未换屏时的绝对位置（pos）照旧推给主界面（参数面板「位置」跟着动）。
fn push_glass_pos(app: &AppHandle, id: &str) {
    let Some(w) = app.get_webview_window(&card_label(id)) else {
        return;
    };
    let Some(h) = hwnd_of(&w) else {
        return;
    };
    let Some((hmon, m)) = display::window_monitor(h) else {
        return;
    };
    let changed = last_mon()
        .lock()
        .map(|mut map| map.insert(id.to_string(), hmon) != Some(hmon))
        .unwrap_or(true);
    if changed {
        if let Some(payload) = card_glass_payload(app, id) {
            let _ = w.eval(&format!("window.__setWall && window.__setWall({payload});"));
        }
        return;
    }
    let Ok(p) = w.outer_position() else {
        return;
    };
    let s = w.scale_factor().unwrap_or(1.0);
    let rel = [(p.x as f64 - m.0 as f64) / s, (p.y as f64 - m.1 as f64) / s];
    let abs = [p.x as f64 / s, p.y as f64 / s];
    let _ = app.emit(
        "glass_pos",
        serde_json::json!({ "id": id, "pos": abs, "wpos": rel }),
    );
}

/// 切层级档：Desktop = 桌面层锁全开；Normal/Top = 退回普通窗口（可激活、可置顶）
fn apply_z_mode(app: &AppHandle, mode: u8) {
    Z_MODE.store(mode, Ordering::Relaxed);
    for (label, w) in app.webview_windows() {
        if !label.starts_with("card-") {
            continue;
        }
        match mode {
            0 => {
                let _ = w.set_focusable(false);
                if let Some(h) = hwnd_of(&w) {
                    apply_desktop_zorder(h);
                }
            }
            1 => {
                let _ = w.set_focusable(true);
                let _ = w.set_always_on_top(false);
            }
            _ => {
                let _ = w.set_focusable(true);
                let _ = w.set_always_on_top(true);
            }
        }
    }
}

fn hwnd_of(w: &WebviewWindow) -> Option<*mut core::ffi::c_void> {
    let handle = w.window_handle().ok()?;
    let raw_window_handle::RawWindowHandle::Win32(h) = handle.as_raw() else {
        return None;
    };
    Some(h.hwnd.get() as *mut core::ffi::c_void)
}

/// 卡片 chrome 三合一：
/// ① DWM 系统圆角；② 剥 WS_CAPTION + 子类化 WM_NCCALCSIZE 消小框，再把尺寸压回设计值；
/// ③ 玻璃 = DWM 系统背景材质（acrylic/mica，真磨砂；本机实测 WCA 那条老路没效果）。
fn apply_card_chrome(w: &WebviewWindow, size: [f32; 2], s: &config::Settings) {
    let Some(hwnd) = hwnd_of(w) else { return };
    guide::register(hwnd); // 拖动对齐的目标白名单：只认登记过的卡窗（主界面同名类，别混进去）
    unsafe {
        // ① 圆角：卡片自己画 16px 圆角（.card 已改成 inset:0 铺满窗口），窗口保持方角，
        //    否则 DWM 的 8px 圆角会把卡片的 16px 圆角切掉一圈。
        let pref: u32 = 1; // DWMWCP_DONOTROUND
        windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute(
            hwnd, 33,
            &pref as *const _ as *const core::ffi::c_void, 4,
        );
        // ③ 玻璃材质：38 = DWMWA_SYSTEMBACKDROP_TYPE（1=NONE / 2=MICA / 3=ACRYLIC），
        // 20 = DWMWA_USE_IMMERSIVE_DARK_MODE（材质的明暗变体）。实测 DWM 材质在「永不聚焦的桌面层卡片」
        // 上稳定渲染，失焦回白的老问题（terminal#593）没有复现；WCA 那条路本机返回成功但无视觉变化。
        let (backdrop, dark): (u32, u32) = match s.glass_mode.as_str() {
            "acrylic" => (3, 1),
            "mica" => (2, 1),
            _ => (1, 0), // wca / off：关掉 DWM 背景，由 WCA（或无玻璃）兜底
        };
        windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute(
            hwnd, 20,
            &dark as *const _ as *const core::ffi::c_void, 4,
        );
        windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute(
            hwnd, 38,
            &backdrop as *const _ as *const core::ffi::c_void, 4,
        );
        // DWM 默认窗口边框会画在窗口最外沿（= 卡面外 6px 处），浅色壁纸下就是「卡片外面那个小框」→ 显式关掉
        let noborder: u32 = 0xFFFF_FFFE; // DWMWA_COLOR_NONE
        windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute(
            hwnd, 34,
            &noborder as *const _ as *const core::ffi::c_void, 4,
        );

        // ② 剥框架样式（tao 会无条件加回，故再叠子类化兜底）
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            GetWindowLongW, SetWindowLongPtrW, SetWindowLongW, SetWindowPos,
            GWL_STYLE, GWLP_WNDPROC, SWP_FRAMECHANGED, SWP_NOACTIVATE,
            SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
        };
        let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
        let stripped = (style
            & !(0x00C0_0000 // WS_CAPTION
                | 0x0004_0000 // WS_THICKFRAME
                | 0x0008_0000 // WS_SYSMENU
                | 0x0002_0000 // WS_MINIMIZEBOX
                | 0x0001_0000)) // WS_MAXIMIZEBOX
            | 0x8000_0000; // WS_POPUP
        SetWindowLongW(hwnd, GWL_STYLE, stripped as i32);

        let mut origs = ORIG_PROC.lock().unwrap();
        let prev = SetWindowLongPtrW(hwnd, GWLP_WNDPROC, card_wnd_proc as *const () as isize);
        if prev != 0 && origs.is_none() {
            *origs = Some(std::mem::transmute::<
                isize,
                windows_sys::Win32::UI::WindowsAndMessaging::WNDPROC,
            >(prev));
        }
        drop(origs);

        SetWindowPos(
            hwnd, std::ptr::null_mut(), 0, 0, 0, 0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        );
    }
    // NC 归零后客户区=旧外框，把尺寸压回设计值（外框=客户区=设计尺寸）
    use tauri::LogicalSize;
    let _ = w.set_size(tauri::Size::Logical(LogicalSize::new(
        size[0] as f64,
        size[1] as f64,
    )));

    // 建窗瞬间 tao 还带着 WS_CAPTION，系统可能已把 caption 画进窗口表面（半透明卡面会透出来）→
    // 强制整窗重绘一次把它擦掉（与「隐藏/重显」等效，实测能清掉 caption 残留）
    unsafe {
        windows_sys::Win32::Graphics::Gdi::RedrawWindow(
            hwnd,
            std::ptr::null(),
            std::ptr::null_mut(),
            0x0001 | 0x0004 | 0x0400 | 0x0080 | 0x0100, // INVALIDATE|ERASE|FRAME|ALLCHILDREN|UPDATENOW
        );
    }

    // ③ 玻璃：dwm 模式已由 DWM 材质提供；只有 wca 模式才需要手工染色兜底
    if s.glass_mode == "wca" {
        apply_wca_glass(hwnd, s.glass_alpha);
    }
}

fn create_card(app: &AppHandle, cfg: &Config, card: &CardInstance) {
    let label = card_label(&card.id);
    if app.get_webview_window(&label).is_some() {
        return;
    }
    // 磁盘卡：按当前盘数决定建窗高度（派生值，见 effective_size）。
    // 必须在 first_paint 之前算好，否则冷启动会先按 config 的 180 建一次再缩，用户看得见闪一下。
    let sz = effective_size(
        card.kind,
        card.size,
        DISK_N.load(Ordering::Relaxed),
        DISK_STALE.load(Ordering::Relaxed),
    );
    let kind = serde_json::to_string(&card.kind)
        .unwrap_or_else(|_| "\"Performance\"".into());
    let url = format!(
        "card.html?kind={}&id={}&glass={}&blur={}&dark={}&pinned={}&gpu={}&warn={}&rate={}",
        kind.trim_matches('"'),
        card.id,
        cfg.settings.glass_alpha,
        cfg.settings.glass_blur,
        if matches!(cfg.settings.theme, config::Theme::Light) { 0 } else { 1 },
        if card.pinned { 1 } else { 0 },
        if card.show_gpu { 1 } else { 0 },
        if card.warn90 { 1 } else { 0 },
        // 刷新间隔必须走 URL：以前只在 card_update 里 eval __setRate，
        // 重启后卡片退回内置默认（实测六卡 __getRate() 全为 0，天气按 30s 而不是配置的 30min）
        card.refresh_ms
    );
    let win = match WebviewWindowBuilder::new(app, &label, WebviewUrl::App(url.into()))
        .title(format!("MiniCard · {}", card.kind.display_name()))
        .inner_size(sz[0] as f64, sz[1] as f64)
        .position(card.pos[0] as f64, card.pos[1] as f64)
        .decorations(false)
        .transparent(true)
        .resizable(false)
        .skip_taskbar(true)
        .focused(false)
        .always_on_top(matches!(cfg.settings.z_order, ZOrder::Top))
        .visible(false)   // 先隐藏：chrome 处理 + 1px 抖动在隐藏时做，用户看不到未处理的裸窗和白条
        .build()
    {
        Ok(w) => w,
        Err(_) => return,
    };
    apply_card_chrome(&win, sz, &cfg.settings);

    // 桌面层级：普通窗口之下 + 点击不激活不抬升（四道锁见 apply_desktop_zorder 上方注释）
    if matches!(cfg.settings.z_order, ZOrder::Desktop) {
        // tao 自己算窗口风格就带 WS_EX_NOACTIVATE —— 比手加持久（手加的会被 tao 重写掉）
        let _ = win.set_focusable(false);
        if let Some(h) = hwnd_of(&win) {
            apply_desktop_zorder(h);
        }
    }

    // WebView2 的表面 bounds 不会跟着 NC 变化重算 → 左侧留 ~6px 未覆盖（WRY_WEBVIEW 白底）
    // = 用户报的「左边白色的一条」。一次 1px 尺寸抖动逼它重算，再 show()。
    {
        use tauri::LogicalSize;
        let _ = win.set_size(tauri::Size::Logical(LogicalSize::new(
            (sz[0] + 1.0) as f64,
            (sz[1] + 1.0) as f64,
        )));
        std::thread::sleep(std::time::Duration::from_millis(40));
        let _ = win.set_size(tauri::Size::Logical(LogicalSize::new(
            sz[0] as f64,
            sz[1] as f64,
        )));
    }
    let _ = win.show();

    // 拖动落盘：位置变了才写，写的是自己这张卡。
    // 位置**不吸附**（2026-09-28 用户：先不做吸到格点，位置自由）；对齐提示走 guide.rs：
    // 拖动中 WM_MOVING 里做磁吸 + 画辅助线，落点就是用户放下的地方。
    let last: Mutex<Option<(i32, i32)>> = Mutex::new(None);
    let id = card.id.clone();
    let app_move = app.clone();
    let win_ev = win.clone();
    win.on_window_event(move |e| {
        if let tauri::WindowEvent::Moved(p) = e {
            // ⚠️ scale 必须**事件到达时现取**（原来建窗时缓存一份）：把卡拖到另一台缩放
            //    不同的显示器后缓存值会过期 → 写盘/推送给主界面的逻辑坐标整体偏。
            //    与 overlay_card_positions 的实时口径保持一致（两处必须同源）。
            let s = win_ev.scale_factor().unwrap_or(1.0) as f64;
            let logical = ((p.x as f64 / s) as i32, (p.y as f64 / s) as i32);
            {
                let mut last = last.lock().unwrap();
                if *last == Some(logical) {
                    return;
                }
                *last = Some(logical);
            }
            let mut cfg = Config::load();
            if let Some(c) = cfg.cards.iter_mut().find(|c| c.id == id) {
                c.pos = [logical.0 as f32, logical.1 as f32];
                let _ = cfg.save();
            }
            // 移动后推磨砂 + 位置（真值）：同屏只发相对位置、换屏重推全量（见 push_glass_pos）；
            // 主界面参数面板的「位置」也吃这条事件，不显示拖动中途的野坐标
            push_glass_pos(&app_move, &id);
        }
    });
}

/// config ↔ 卡片窗口双向同步：enabled 的保证存在，禁用/删掉的关掉
fn sync_cards(app: &AppHandle, cfg: &Config) {
    for card in &cfg.cards {
        let label = card_label(&card.id);
        let exists = app.get_webview_window(&label).is_some();
        if card.enabled && !exists {
            create_card(app, cfg, card);
        } else if !card.enabled && exists {
            if let Some(w) = app.get_webview_window(&label) {
                let _ = w.close();
            }
        }
    }
    for (label, w) in app.webview_windows() {
        if let Some(id) = label.strip_prefix("card-") {
            if !cfg.cards.iter().any(|c| c.id == id) {
                let _ = w.close();
            }
        }
    }
}

/// 托盘：主窗口藏起来之后的回家路
/// 把主窗口叫回来。托盘左键单击与菜单「显示主窗口」共用这一条，
/// 免得两处各写一遍 show/unminimize/set_focus。
fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

fn build_tray(app: &tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    use tauri::menu::{Menu, MenuItem};
    use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

    let show = MenuItem::with_id(app, "show", "显示主窗口", true, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, "quit", "退出 Mini Card", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &quit_item])?;

    let mut builder = TrayIconBuilder::with_id("main-tray")
        .tooltip("Mini Card")
        .menu(&menu)
        // Windows 习惯：左键单击图标 = 直接开主界面，右键才弹菜单（默认左键也弹菜单，要关掉）
        .show_menu_on_left_click(false)
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main(tray.app_handle());
            }
        })
        .on_menu_event(|app, event| {
            match event.id().0.as_str() {
                "show" => show_main(app),
                "quit" => quit(&app.clone()),
                _ => {}
            }
        });

    // 托盘图标用 32px 精确帧（icons/tray.png）：exe 图标资源里 256px 帧被系统缩到托盘 16/20/24
    // 会糊出一圈灰边，直接给一张按像素网格画的 32px 图最干净。
    let icon = tauri::image::Image::from_bytes(include_bytes!("../icons/tray.png"))
        .ok()
        .or_else(|| app.default_window_icon().cloned())
        .or_else(|| tauri::image::Image::from_bytes(include_bytes!("../icons/icon.png")).ok());
    if let Some(ic) = icon {
        builder = builder.icon(ic);
    }
    let _ = builder.build(app);
    Ok(())
}

/// 给窗口设精确尺寸的图标（单帧 ico → HICON）。
/// Tauri 默认只给系统一个 16×16 的小图标（实测 ICON_SMALL=16×16），而任务栏在 100% DPI 下按
/// 24px 绘制，explorer 把 16 拉伸到 24 就会发虚——这就是"任务栏 logo 糊"的根因。
/// 这里按任务栏/Alt+Tab 实际绘制的尺寸各给一个精确 HICON，列表按 1:1 画，不再插值。
/// 让任务栏重建按钮。explorer 不会因为之后的 WM_SETICON 去更新一个已存在的任务栏按钮：
/// 实测把图标从 16px 换成 24px（并且换成平滑圆角的帧）之后，按钮上显示的仍是旧图标，
/// 四角还留着上一版硬边帧的阶梯缺口。把窗口临时标成 WS_EX_TOOLWINDOW（任务栏移除按钮）
/// 再标回来，按钮会重新创建并重取窗口当前的 HICON。
/// 必须在窗口已显示、按钮已建好之后再调用，所以放到线程里延迟执行。
unsafe fn refresh_taskbar_button(hwnd: windows_sys::Win32::Foundation::HWND) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE, WS_EX_TOOLWINDOW,
    };
    let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
    SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex | WS_EX_TOOLWINDOW as isize);
    SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex);
}

unsafe fn set_exact_window_icons(hwnd: windows_sys::Win32::Foundation::HWND) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateIconFromResourceEx, SendMessageW, ICON_BIG, ICON_SMALL, WM_SETICON,
    };
    const VER: u32 = 0x0003_0000; // 3.0 版图标资源
    const LR_DEFAULTCOLOR: u32 = 0x0000_0000;
    // 注意：CreateIconFromResourceEx 吃的是 RT_ICON 资源数据（不含 ICO 文件头），
    // 直接喂整个 .ico 文件字节会返回 NULL（实测）。单帧 ico = 6 字节 ICONDIR + 16 字节 DIRENTRY = 22 字节头。
    // 尺寸要对上 explorer 的绘制尺寸：量任务栏像素反推图标是 24px 画的（外框外沿到蓝块 11~12px、
    // 描边 2px），给 20px 会被放大 1.2 倍反而更糊。所以 SMALL 用 24px，BIG 给 Alt+Tab 用 32px。
    let small = &include_bytes!("../icons/icon24.ico")[22..]; // 任务栏按钮（实测绘制 24px）
    let big = &include_bytes!("../icons/icon32.ico")[22..]; // Alt+Tab / 任务视图
    let h_small = CreateIconFromResourceEx(small.as_ptr(), small.len() as u32, 1, VER, 0, 0, LR_DEFAULTCOLOR);
    let h_big = CreateIconFromResourceEx(big.as_ptr(), big.len() as u32, 1, VER, 0, 0, LR_DEFAULTCOLOR);
    if !h_small.is_null() {
        SendMessageW(hwnd, WM_SETICON, ICON_SMALL as usize, h_small as isize);
    }
    if !h_big.is_null() {
        SendMessageW(hwnd, WM_SETICON, ICON_BIG as usize, h_big as isize);
    }
    // 等窗口显示、任务栏按钮建好之后，强制重建按钮让新图标生效
    // HWND 是裸指针、不实现 Send，转成 isize 传进线程
    let raw = hwnd as isize;
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(1500));
        unsafe { refresh_taskbar_button(raw as windows_sys::Win32::Foundation::HWND) };
    });
}

fn main() {
    ensure_single_instance();
    update::cleanup_old_exes();   // 清掉上一轮自更新留下的 mini-card.old*.exe
    let snap = data::spawn_sampler();
    let weather_snap: weather::SharedWeather =
        std::sync::Arc::new(std::sync::RwLock::new(None));
    let weather_seed = weather_snap.clone();

    tauri::Builder::default()
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, event| {
                    // 按下才切，松开那次忽略
                    if event.state == ShortcutState::Pressed {
                        toggle_cards(app);
                    }
                })
                .build(),
        )
        .manage(snap.clone())
        .manage(weather_snap)
        .manage(CardVis(AtomicBool::new(true)))
        // 系统级关闭（Alt+F4 / 任务栏右键「关闭窗口」/ 窗口系统菜单）也要和界面里的 ✕ 一致。
        // 不拦的话窗口被真销毁，而卡片窗口还开着 → 进程不会退出、留在托盘，
        // 托盘菜单的「显示主窗口」只会去 show 一个已经不存在的主窗 → 主窗口再也调不回来，
        // 用户只能从托盘退出再重开（下载来就双击 exe 的人一定会踩）。
        .on_window_event(|window, event| {
            if window.label() != "main" {
                return;
            }
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if Config::load().settings.close_to_tray {
                    api.prevent_close();
                    if let Some(w) = window.app_handle().get_webview_window("main") {
                        persist_bounds(&w);
                    }
                    let _ = window.hide();
                } else {
                    quit(&window.app_handle().clone());
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_config,
            get_snapshot,
            get_glass,
            save_config,
            card_update,
            card_locate,
            card_pin,
            todo_toggle,
            todo_set,
            media::music_state,
            media::music_action,
            get_weather,
            weather_refresh,
            weather_search,
            weather_set_place,
            hotkey_set,
            hotkey_enable,
            autostart_set,
            autostart_state,
            check_update,
            update_install,
            win_min,
            win_max,
            win_close
        ])
        .setup(move |app| {
            guide::prepare(); // 先备好拖动辅助线窗口（隐藏）
            let handle = app.handle().clone();
            let cfg = Config::load();
            if let Some(w) = app.get_webview_window("main") {
                use tauri::{LogicalPosition, LogicalSize};
                let _ = w.set_position(tauri::Position::Logical(LogicalPosition::new(
                    cfg.main_window[0] as f64,
                    cfg.main_window[1] as f64,
                )));
                let _ = w.set_size(tauri::Size::Logical(LogicalSize::new(
                    cfg.main_window[2] as f64,
                    cfg.main_window[3] as f64,
                )));
                if let Ok(h) = w.window_handle() {
                    if let raw_window_handle::RawWindowHandle::Win32(h) = h.as_raw() {
                        let pref: u32 = 1; // DWMWCP_DONOTROUND：圆角交给 CSS（主窗真透明 + --radius-win 18px），系统 8px 会切掉 CSS 圆角
                        unsafe {
                            windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute(
                                h.hwnd.get() as *mut core::ffi::c_void,
                                33,
                                &pref as *const _ as *const core::ffi::c_void,
                                4,
                            );
                            // 窗口图标：Tauri 只给系统一个 16×16 的小图标（实测 ICON_SMALL=16×16），
                            // 而任务栏在 100% DPI 下按 24px 绘制 → explorer 把 16px 拉伸到 24 → 发虚。
                            // 这里改成按目标尺寸给单帧 ico 造的精确 HICON，让 explorer 1:1 画。
                            let hwnd = h.hwnd.get() as windows_sys::Win32::Foundation::HWND;
                            set_exact_window_icons(hwnd);
                        }
                    }
                }
            }
            // 主窗口在配置里是 visible=false：图标（24px HICON）设好之后再显示，
            // 否则任务栏按钮会在窗口首次显示时就用 Tauri 的 16px 图标建好，之后改图标不会刷新按钮。
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.show();
            }
            build_tray(app)?;

            // 桌面层档位 + 巡检线程：卡片永远待在壁纸上、普通窗口下（见 apply_desktop_zorder 注释）
            Z_MODE.store(z_mode_of(cfg.settings.z_order), Ordering::Relaxed);
            let _ = APP.set(handle.clone()); // 子类/巡检线程要拿 AppHandle 做显示器善后
            spawn_z_guard(handle.clone());

            // 垫一次盘数：采样线程通常已写出首帧，磁盘卡建窗时就能按正确盘数算高度。
            // 万一还没写出（采样线程要先建 PDH 探针），这一垫不生效 → 卡片先按 config 默认档建，
            // 1s 内快照线程的 sync_disk_height 会纠正。盘数 == 默认档的机器（如本机 5 盘）看不出来。
            if let Ok(s) = snap.read() {
                DISK_N.store(s.disks.len(), Ordering::Relaxed);
                DISK_STALE.store(s.disks.is_empty(), Ordering::Relaxed);
            }

            // 按 config 拉起已启用的卡片
            sync_cards(&handle, &cfg);

            // 启动即回收一次越屏卡片（上次退出后拔了显示器/改了分辨率/改了缩放的场景）
            rescue_offscreen(&handle);

            // 1s 快照推给所有窗口（卡片页监听 snapshot 事件）
            let emit_handle = handle.clone();
            std::thread::spawn(move || loop {
                let s = snap.read().map(|s| s.clone()).unwrap_or_default();
                let _ = emit_handle.emit("snapshot", &s);
                // 磁盘卡高度随盘数自动调整（幂等：盘数没变时只是个原子读）
                sync_disk_height(&emit_handle, s.disks.len(), s.disks.is_empty());
                std::thread::sleep(std::time::Duration::from_secs(1));
            });

            // 天气：先贴上次落盘的值，再起 30 分钟轮询
            weather::spawn(weather_seed, handle.clone());

            // 媒体（音乐卡）：1s 轮询 SMTC → emit("music")；封面 Rust 缓存换曲才重读
            media::spawn(handle.clone());

            // 全局热键：注册 config 里存的那一组（失败不拦启动，界面里能重录）
            // 开关关着就不注册；键位仍留在 config 里（设置页那个 switch）
            let _ = sync_hotkey(&handle);

            // 开机自启：以 config 为准回写注册表（exe 换路径 / 注册表被手动删过都能自愈）
            if cfg.settings.autostart != autostart_enabled() {
                let _ = autostart_apply(cfg.settings.autostart);
            }

            // 软件更新：开着「自动检查」就晚一点查一次，有新版推给主界面 toast
            if cfg.settings.update_auto {
                let upd_handle = handle.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_secs(8));
                    let _ = tauri::async_runtime::block_on(async move {
                        match check_update(upd_handle.clone()).await {
                            Ok(info) => {
                                if info.get("has_new").and_then(|x| x.as_bool()).unwrap_or(false) {
                                    let _ = upd_handle.emit("update_available", info);
                                }
                            }
                            Err(_) => {} // 自动检查失败不打扰用户（界面里手动点会给出原因）
                        }
                    });
                });
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running mini-card");
}

#[cfg(test)]
mod tests {
    use super::is_shell_class_name;

    /// 桌面锚点的窗口类判据：只有 Progman / WorkerW 算桌面壳。
    /// 资源管理器的 CabinetWClass 窗口子窗里也有 SHELLDLL_DefView（文件视图），
    /// 漏掉这一条就会把资源管理器当成锚点、卡片被插到它正上方（实测过的 bug）。
    #[test]
    fn desktop_anchor_only_accepts_shell_classes() {
        assert!(is_shell_class_name("Progman"));
        assert!(is_shell_class_name("WorkerW"));
        assert!(!is_shell_class_name("CabinetWClass"));
        assert!(!is_shell_class_name("ExplorerWClass"));
        assert!(!is_shell_class_name("Tauri Window"));
        assert!(!is_shell_class_name(""));
    }
}
