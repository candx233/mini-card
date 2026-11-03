//! Dock 的 OLE 拖放目标（把桌面/资源管理器里的图标拖进 Dock 就加进来）。
//!
//! 结构照 WebView 版 `dock.html` 的 `dragover/drop`：拖到条上 → 画落点竖线，
//! 松手 → 按落点插进 `cfg.dock.items` 并落盘，然后重建 Dock 窗（条宽会变）。
//!
//! 口径：
//! - 只认文件系统对象（`CF_HDROP`）；`.exe/.lnk/.url/.bat/.cmd` 与文件夹收，其它忽略
//! - 已在 Dock 里的路径不重复加（大小写不敏感）
//! - 图标就地提取（`crate::icon::ensure`），提不到就交给兜底色块（dock.rs 已实现）
//! - 拖放回调在 OLE 的主线程上跑，与帧循环同线程 → 只动 `Mutex` 状态，不做重活

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use minicard_core::config::{Config, Dock, DockItem};
use windows::core::{implement, Ref};
use windows::Win32::Foundation::{HWND, POINTL};
use windows::Win32::System::Com::{IDataObject, DVASPECT_CONTENT, FORMATETC, TYMED_HGLOBAL};
use windows::Win32::System::Ole::{
    IDropTarget, IDropTarget_Impl, RegisterDragDrop, ReleaseStgMedium, RevokeDragDrop, DROPEFFECT,
    DROPEFFECT_COPY, DROPEFFECT_NONE,
};
use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};

const CF_HDROP: u16 = 15;

/// 画落点竖线要的信息（窗内逻辑 px + 插入位次）
#[derive(Clone, Copy)]
pub struct Ui {
    pub insert: usize,
    pub x: f32,
}

/// 拖放回调要的 Dock 现场（条目 / 窗口原点 / dpi）
#[derive(Clone)]
struct Snap {
    dock: Dock,
    origin: (i32, i32),
    dpi: f32,
}

static SNAP: Mutex<Option<Snap>> = Mutex::new(None);
static UI: Mutex<Option<Ui>> = Mutex::new(None);
/// 松手后置位：主循环看到就重建 Dock 窗
static DROPPED: AtomicBool = AtomicBool::new(false);

/// 注册拖放目标（建窗/重建窗后调）。`origin`/`dpi` 用来把屏幕点换回窗内坐标。
pub fn install(hwnd: HWND, dock: Dock, origin: (i32, i32), dpi: f32) {
    *SNAP.lock().unwrap() = Some(Snap { dock, origin, dpi });
    *UI.lock().unwrap() = None;
    unsafe {
        // OLE 必须在注册拖放目标的线程上初始化过（重复调用返回 S_FALSE，无害）
        let _ = windows::Win32::System::Ole::OleInitialize(None);
        let target: IDropTarget = Target.into();
        if let Err(e) = RegisterDragDrop(hwnd, &target) {
            minicard_shell::mlog!("RegisterDragDrop 失败：{e}");
        } else {
            minicard_shell::mlog!("dock 拖放已启用");
        }
    }
}

/// 卸掉拖放目标（重建窗前调，避免同一个 HWND 重复注册）
pub fn uninstall(hwnd: HWND) {
    unsafe {
        let _ = RevokeDragDrop(hwnd);
    }
    *UI.lock().unwrap() = None;
}

/// 帧循环取「正在拖放」的落点状态
pub fn ui() -> Option<Ui> {
    *UI.lock().unwrap()
}

/// 松手后取一次（取到就清）
pub fn take_dropped() -> bool {
    DROPPED.swap(false, Ordering::Relaxed)
}

/// 屏幕物理点 → 窗内逻辑 x
fn to_dip_x(pt: &POINTL, s: &Snap) -> f32 {
    let k = if s.dpi > 0.0 { s.dpi / 96.0 } else { 1.0 };
    (pt.x - s.origin.0) as f32 / k
}

/// 窗内 x → 插入位次（用静止态几何，拖放时条不放大）
fn insert_at(x: f32, dock: &Dock) -> usize {
    let t = minicard_cards::dock::Tokens::dock(dock);
    let g = t.geo(dock, None, 0.0);
    g.cx.iter().filter(|&&c| c < x).count()
}

/// 指针是否真的落在玻璃条上。
///
/// Dock 窗比条高得多（上方 `TOP_RESERVE` 给气泡/菜单，左右有悬停外扩预留），
/// 那些区域是透明的，但 OLE 的 `WindowFromPoint` 照样命中整个窗口矩形。
/// 不挡的话，把文件拖到条上方 100px 的桌面上也会被当成「拖进 Dock」。
fn in_bar(pt: &POINTL, s: &Snap) -> bool {
    let k = if s.dpi > 0.0 { s.dpi / 96.0 } else { 1.0 };
    let x = (pt.x - s.origin.0) as f32 / k;
    let y = (pt.y - s.origin.1) as f32 / k;
    let t = minicard_cards::dock::Tokens::dock(&s.dock);
    let bar = t.geo(&s.dock, None, 0.0).bar;
    const SLOP: f32 = 2.0;
    x >= bar.left - SLOP && x <= bar.right + SLOP && y >= bar.top - SLOP && y <= bar.bottom + SLOP
}

fn has_files(obj: &IDataObject) -> bool {
    let fmt = FORMATETC {
        cfFormat: CF_HDROP,
        ptd: std::ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT.0,
        lindex: -1,
        tymed: TYMED_HGLOBAL.0 as u32,
    };
    unsafe { obj.QueryGetData(&fmt).is_ok() }
}

/// 从 `IDataObject` 里取所有拖进来的路径
fn paths_of(obj: &IDataObject) -> Vec<String> {
    let fmt = FORMATETC {
        cfFormat: CF_HDROP,
        ptd: std::ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT.0,
        lindex: -1,
        tymed: TYMED_HGLOBAL.0 as u32,
    };
    unsafe {
        let Ok(mut stg) = obj.GetData(&fmt) else {
            return Vec::new();
        };
        let h = HDROP(stg.u.hGlobal.0);
        let n = DragQueryFileW(h, u32::MAX, None);
        let mut out = Vec::new();
        for i in 0..n {
            let len = DragQueryFileW(h, i, None) as usize;
            if len == 0 {
                continue;
            }
            let mut buf = vec![0u16; len + 1];
            let got = DragQueryFileW(h, i, Some(&mut buf));
            if got > 0 {
                out.push(String::from_utf16_lossy(&buf[..got as usize]));
            }
        }
        ReleaseStgMedium(&mut stg);
        out
    }
}

fn ext_ok(p: &str) -> bool {
    let path = Path::new(p);
    if path.is_dir() {
        return true;
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    matches!(ext.as_str(), "exe" | "lnk" | "url" | "bat" | "cmd")
}

/// 把路径插进 config（返回真的加了几项）
pub fn add_paths(paths: &[String], insert: usize) -> usize {
    let mut cfg = Config::load();
    let mut at = insert.min(cfg.dock.items.len());
    let mut added = 0;
    for p in paths {
        if !ext_ok(p) {
            continue;
        }
        if cfg
            .dock
            .items
            .iter()
            .any(|it| it.path.eq_ignore_ascii_case(p))
        {
            continue;
        }
        let name = Path::new(p)
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| p.clone());
        crate::icon::ensure(p);
        cfg.dock.items.insert(
            at,
            DockItem {
                name,
                path: p.clone(),
                ..Default::default()
            },
        );
        at += 1;
        added += 1;
    }
    if added > 0 {
        match cfg.save() {
            Ok(()) => minicard_shell::mlog!("dock 拖入：加了 {added} 项（插在第 {} 位）", insert),
            Err(e) => minicard_shell::mlog!("dock 拖入保存失败：{e}"),
        }
    }
    added
}

#[implement(IDropTarget)]
struct Target;

impl IDropTarget_Impl for Target_Impl {
    fn DragEnter(
        &self,
        pdataobj: Ref<'_, IDataObject>,
        _keys: MODIFIERKEYS_FLAGS,
        pt: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        let ok = pdataobj
            .as_ref()
            .map(|o| has_files(o))
            .unwrap_or(false);
        let snap = SNAP.lock().unwrap().clone();
        let ok = ok && snap.as_ref().map(|s| in_bar(pt, s)).unwrap_or(false);
        unsafe {
            if !effect.is_null() {
                *effect = if ok { DROPEFFECT_COPY } else { DROPEFFECT_NONE };
            }
        }
        if ok {
            if let Some(s) = snap {
                let x = to_dip_x(pt, &s);
                *UI.lock().unwrap() = Some(Ui {
                    insert: insert_at(x, &s.dock),
                    x,
                });
            }
        } else {
            // 非文件，或落在窗口透明区：把上一次的落点线擦掉
            *UI.lock().unwrap() = None;
        }
        Ok(())
    }

    fn DragOver(
        &self,
        _keys: MODIFIERKEYS_FLAGS,
        pt: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        let snap = SNAP.lock().unwrap().clone();
        let inside = snap.as_ref().map(|s| in_bar(pt, s)).unwrap_or(false);
        if let Some(s) = snap {
            let x = to_dip_x(pt, &s);
            *UI.lock().unwrap() = if inside {
                Some(Ui {
                    insert: insert_at(x, &s.dock),
                    x,
                })
            } else {
                None
            };
        }
        unsafe {
            if !effect.is_null() {
                *effect = if inside { DROPEFFECT_COPY } else { DROPEFFECT_NONE };
            }
        }
        Ok(())
    }

    fn DragLeave(&self) -> windows::core::Result<()> {
        *UI.lock().unwrap() = None;
        Ok(())
    }

    fn Drop(
        &self,
        pdataobj: Ref<'_, IDataObject>,
        _keys: MODIFIERKEYS_FLAGS,
        pt: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        let mut added = 0;
        if let (Some(obj), Some(s)) = (pdataobj.as_ref(), SNAP.lock().unwrap().clone()) {
            if in_bar(pt, &s) {
                let paths = paths_of(obj);
                let insert = insert_at(to_dip_x(pt, &s), &s.dock);
                added = add_paths(&paths, insert);
            }
        }
        *UI.lock().unwrap() = None;
        unsafe {
            if !effect.is_null() {
                *effect = if added > 0 { DROPEFFECT_COPY } else { DROPEFFECT_NONE };
            }
        }
        if added > 0 {
            DROPPED.store(true, Ordering::Relaxed);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dock(n: usize) -> Dock {
        let mut d = Dock::default();
        d.items = (0..n)
            .map(|i| DockItem {
                name: format!("应用{i}"),
                path: format!("C:/x/app{i}.exe"),
                ..Default::default()
            })
            .collect();
        d
    }

    /// 落点：按「图标中心在指针左边」计数（与拖拽排序同一口径）
    #[test]
    fn 插入位次按图标中心判() {
        let d = dock(4);
        let t = minicard_cards::dock::Tokens::dock(&d);
        let g = t.geo(&d, None, 0.0);
        assert_eq!(insert_at(0.0, &d), 0);
        assert_eq!(insert_at(g.cx[0] + 0.5, &d), 1);
        assert_eq!(insert_at(g.cx[3] - 0.5, &d), 3);
        assert_eq!(insert_at(t.w + 100.0, &d), 4);
    }

    /// 只收可执行/快捷方式/文件夹
    #[test]
    fn 认得的扩展名() {
        assert!(ext_ok("C:/Windows/notepad.exe"));
        assert!(ext_ok("C:/x/微信.LNK")); // 大小写不敏感
        assert!(ext_ok("C:/x/a.bat"));
        assert!(ext_ok("C:/x/a.url"));
        assert!(!ext_ok("C:/x/readme.txt"));
        assert!(!ext_ok("C:/x/pic.png"));
    }
}
