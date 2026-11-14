//! 点通知 = 复刻系统 toast 自己的激活方式（2026-11-14）
//!
//! 为什么不再猜窗口：QQ 的「邮件提醒」是 `activationType="protocol"` 的 toast，
//! 系统点它时只做一件事 —— `ShellExecute(launch)`，交给 QQ 注册的
//! `ntqq-notification:` 协议处理器（实测落到 `timwp.exe` → 浏览器打开那封邮件）。
//! 我们按窗标题/exe 去猜「哪个 hwnd 是主窗」，猜错就是把 QQ 内部藏着的空壳窗
//! `SW_SHOW` 出来（720×640、PrintWindow 只有 1 种颜色）—— 用户看到一块黑屏。
//!
//! 数据来源：`%LOCALAPPDATA%\Microsoft\Windows\Notifications\wpndatabase.db`
//! （WAL 模式；必须把 `-wal`/`-shm` 一起拷到临时目录再只读打开，否则刚落的新通知
//! 还在 WAL 里查不到）。SQLite 用系统自带的 `winsqlite3.dll`（动态加载），不加 crate。
//!
//! 与 WinRT 的对应关系：`UserNotification.Id` == 通知库 `Notification.Id`
//! （本机 2026-11-14 逐条比对过：28380/28385/… 完全一致）。
//!
//! 隐私：只取 `<toast>` 上的 `launch` / `activationType` 两个属性；通知正文、
//! URI 里的邮箱地址与 mailid **一律不落日志**，临时库读完即删。

use std::ffi::{c_char, c_void, CString};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// toast 的激活方式（系统点这条通知时会做的事）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivationKind {
    /// `activationType="protocol"`：系统会 `ShellExecute(launch)`；QQ 邮件提醒就是这种
    Protocol,
    /// 默认：把应用带到前台（Windows 走应用注册的 COM 激活器，本进程复刻不了）
    Foreground,
    Background,
    /// 认不出的取值：不按 protocol 处理，退回窗口激活
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Activation {
    pub kind: ActivationKind,
    /// `protocol` 时是**要交给 shell 的 URI**；其他类型只是应用自己的参数，不外传
    pub launch: String,
}

/// 按通知 id 读出这条 toast 的激活方式。任何一步失败都返回 `None`
/// （调用方退回「激活已有窗口」），不 panic、不打印内容。
pub fn for_notification(id: u32) -> Option<Activation> {
    let dir = copy_database().ok()?;
    let out = query_payload(&dir.join("wpndatabase.db"), id).and_then(|xml| parse_activation(&xml));
    let _ = std::fs::remove_dir_all(&dir);
    out
}

// ───────────────────────── 系统库拷贝 ─────────────────────────

fn notifications_dir() -> Option<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")?;
    Some(
        PathBuf::from(base)
            .join("Microsoft")
            .join("Windows")
            .join("Notifications"),
    )
}

/// 把通知库（含 WAL/SHM）拷到独立临时目录：服务进程一直开着它，原地只读打开不可靠；
/// 拷贝大约 5MB，只在「点通知」这一次做。
fn copy_database() -> Result<PathBuf, String> {
    let src = notifications_dir().ok_or("没有 LOCALAPPDATA")?;
    let main = src.join("wpndatabase.db");
    if !main.is_file() {
        return Err("系统通知库不存在".into());
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("minicard-wpn-{}-{stamp}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

    for name in ["wpndatabase.db", "wpndatabase.db-wal", "wpndatabase.db-shm"] {
        let from = src.join(name);
        if !from.is_file() {
            continue;
        }
        if let Err(e) = std::fs::copy(&from, dir.join(name)) {
            // 主库必须成功；WAL/SHM 拷不到只意味着「可能查不到刚落的通知」
            if name == "wpndatabase.db" {
                let _ = std::fs::remove_dir_all(&dir);
                return Err(e.to_string());
            }
        }
    }
    Ok(dir)
}

// ───────────────────────── winsqlite3 动态绑定 ─────────────────────────

const SQLITE_OK: i32 = 0;
const SQLITE_ROW: i32 = 100;
const SQLITE_OPEN_READONLY: i32 = 0x0000_0001;

#[link(name = "kernel32")]
extern "system" {
    fn LoadLibraryW(name: *const u16) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
}

/// `winsqlite3.dll` 句柄（Win10 1607+ 系统自带；加载失败 = 无此能力，静默降级）
fn sqlite_lib() -> Option<*mut c_void> {
    static LIB: OnceLock<usize> = OnceLock::new();
    let h = *LIB.get_or_init(|| {
        let name: Vec<u16> = "winsqlite3.dll\0".encode_utf16().collect();
        unsafe { LoadLibraryW(name.as_ptr()) as usize }
    });
    (h != 0).then_some(h as *mut c_void)
}

unsafe fn sym<T: Copy>(lib: *mut c_void, name: &str) -> Option<T> {
    let c = CString::new(name).ok()?;
    let p = GetProcAddress(lib, c.as_ptr() as *const u8);
    if p.is_null() {
        None
    } else {
        // FARPROC 与目标 fn 指针同为函数指针大小
        Some(std::mem::transmute_copy(&p))
    }
}

type OpenV2 = unsafe extern "system" fn(*const c_char, *mut *mut c_void, i32, *const c_char) -> i32;
type PrepareV2 =
    unsafe extern "system" fn(*mut c_void, *const c_char, i32, *mut *mut c_void, *mut *const c_char) -> i32;
type BindInt64 = unsafe extern "system" fn(*mut c_void, i32, i64) -> i32;
type Step = unsafe extern "system" fn(*mut c_void) -> i32;
type ColumnBlob = unsafe extern "system" fn(*mut c_void, i32) -> *const c_void;
type ColumnBytes = unsafe extern "system" fn(*mut c_void, i32) -> i32;
type Finalize = unsafe extern "system" fn(*mut c_void) -> i32;
type Close = unsafe extern "system" fn(*mut c_void) -> i32;

/// 读 `Notification.Id = id` 那行的 Payload（toast XML）。查不到/打不开都返回 None。
fn query_payload(db: &Path, id: u32) -> Option<String> {
    unsafe {
        let lib = sqlite_lib()?;
        let open: OpenV2 = sym(lib, "sqlite3_open_v2")?;
        let prepare: PrepareV2 = sym(lib, "sqlite3_prepare_v2")?;
        let bind: BindInt64 = sym(lib, "sqlite3_bind_int64")?;
        let step: Step = sym(lib, "sqlite3_step")?;
        let blob: ColumnBlob = sym(lib, "sqlite3_column_blob")?;
        let bytes_of: ColumnBytes = sym(lib, "sqlite3_column_bytes")?;
        let finalize: Finalize = sym(lib, "sqlite3_finalize")?;
        let close: Close = sym(lib, "sqlite3_close_v2")?;

        let path = CString::new(db.to_string_lossy().as_bytes()).ok()?;
        let mut handle: *mut c_void = std::ptr::null_mut();
        if open(path.as_ptr(), &mut handle, SQLITE_OPEN_READONLY, std::ptr::null()) != SQLITE_OK {
            if !handle.is_null() {
                close(handle);
            }
            return None;
        }
        let out = (|| -> Option<String> {
            let sql = CString::new("SELECT Payload FROM Notification WHERE Id = ?1 LIMIT 1").ok()?;
            let mut stmt: *mut c_void = std::ptr::null_mut();
            if prepare(handle, sql.as_ptr(), -1, &mut stmt, std::ptr::null_mut()) != SQLITE_OK {
                return None;
            }
            let mut out = None;
            if bind(stmt, 1, id as i64) == SQLITE_OK && step(stmt) == SQLITE_ROW {
                let p = blob(stmt, 0);
                let n = bytes_of(stmt, 0);
                if !p.is_null() && n > 0 && n < 4 * 1024 * 1024 {
                    out = Some(String::from_utf8_lossy(std::slice::from_raw_parts(
                        p as *const u8,
                        n as usize,
                    ))
                    .into_owned());
                }
            }
            finalize(stmt);
            out
        })();
        close(handle);
        out
    }
}

// ───────────────────────── toast XML ─────────────────────────

/// 从 toast XML 里取激活方式（纯函数，单测覆盖）。
pub fn parse_activation(xml: &str) -> Option<Activation> {
    let tag = toast_open_tag(xml)?;
    let launch = attr(tag, "launch").unwrap_or_default();
    let raw = attr(tag, "activationType").unwrap_or_default();
    let kind = match raw.trim().to_ascii_lowercase().as_str() {
        "protocol" => ActivationKind::Protocol,
        "background" => ActivationKind::Background,
        "foreground" => ActivationKind::Foreground,
        "" => ActivationKind::Foreground,
        _ => ActivationKind::Unknown,
    };
    // protocol 却没有 URI = 复刻不了，交给上层退回窗口激活
    if kind == ActivationKind::Protocol && launch.trim().is_empty() {
        return None;
    }
    Some(Activation { kind, launch })
}

/// `<toast …>` 开标签（含尖括号）。属性值里可能出现 `>`，按引号配对扫。
fn toast_open_tag(xml: &str) -> Option<&str> {
    let lower = xml.to_ascii_lowercase();
    let start = lower.find("<toast")?;
    let bytes = xml.as_bytes();
    let after = start + "<toast".len();
    match bytes.get(after) {
        Some(b) if b.is_ascii_whitespace() || *b == b'>' || *b == b'/' => {}
        _ => return None,
    }
    let mut quote: Option<u8> = None;
    let mut i = after;
    while i < bytes.len() {
        let b = bytes[i];
        match quote {
            Some(q) if b == q => quote = None,
            Some(_) => {}
            None => match b {
                b'"' | b'\'' => quote = Some(b),
                b'>' => return Some(&xml[start..=i]),
                _ => {}
            },
        }
        i += 1;
    }
    None
}

/// 取开标签里的属性值（去 XML 实体）。属性名大小写不敏感。
fn attr(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let needle = name.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mut from = 0usize;
    while let Some(pos) = lower[from..].find(&needle) {
        let at = from + pos;
        from = at + needle.len();
        // 名字前必须是空白，避免 `xtoastlaunch=` 这类误命中
        if at != 0 && !bytes[at - 1].is_ascii_whitespace() {
            continue;
        }
        let mut j = at + needle.len();
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        if bytes.get(j) != Some(&b'=') {
            continue;
        }
        j += 1;
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        let quote = *bytes.get(j)?;
        if quote != b'"' && quote != b'\'' {
            continue;
        }
        let val_start = j + 1;
        let mut k = val_start;
        while k < bytes.len() && bytes[k] != quote {
            k += 1;
        }
        if k >= bytes.len() {
            return None;
        }
        return Some(unescape_xml(&tag[val_start..k]));
    }
    None
}

/// 只处理 XML 里会出现的几个实体（`&amp;` 在 URI 里最常见）
fn unescape_xml(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        match entity(&rest[i..]) {
            Some((c, len)) => {
                out.push(c);
                rest = &rest[i + len..];
            }
            None => {
                out.push('&');
                rest = &rest[i + 1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn entity(s: &str) -> Option<(char, usize)> {
    for (pat, c) in [
        ("&amp;", '&'),
        ("&lt;", '<'),
        ("&gt;", '>'),
        ("&quot;", '"'),
        ("&apos;", '\''),
    ] {
        if s.starts_with(pat) {
            return Some((c, pat.len()));
        }
    }
    // 数字实体：&#38; / &#x26;
    let body = s.strip_prefix("&#")?;
    let (digits, radix, prefix) = match body.strip_prefix('x').or_else(|| body.strip_prefix('X')) {
        Some(hex) => (hex, 16, 3),
        None => (body, 10, 2),
    };
    let end = digits.find(';')?;
    let value = u32::from_str_radix(&digits[..end], radix).ok()?;
    Some((char::from_u32(value)?, prefix + end + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 真机 2026-11-14 的 QQ 邮件提醒（截断，只保留结构）
    const QQ_MAIL: &str = r#"<toast launch="ntqq-notification://externalCommand?subcmd=openEmail;url=https%3A%2F%2Fwx.mail.qq.com%2Flogin%3F_a%3D1&amp;b%3D2;fuin=10001;" activationType="protocol"><visual><binding template="ToastGeneric"><text>你有新的邮件</text></binding></visual></toast>"#;

    #[test]
    fn qq_邮件提醒是协议激活() {
        let a = parse_activation(QQ_MAIL).expect("应识别 protocol");
        assert_eq!(a.kind, ActivationKind::Protocol);
        assert!(a.launch.starts_with("ntqq-notification://"), "{}", a.launch);
        // 实体要还原，否则 shell 会拿到 `&amp;`
        assert!(a.launch.contains("&b%3D2"), "{}", a.launch);
        assert!(!a.launch.contains("&amp;"));
    }

    #[test]
    fn 属性顺序与大小写无关() {
        let xml = r#"<toast activationtype = 'protocol' Launch = "app://x?a=1&b=2">"#;
        let a = parse_activation(xml).unwrap();
        assert_eq!(a.kind, ActivationKind::Protocol);
        assert_eq!(a.launch, "app://x?a=1&b=2");
    }

    #[test]
    fn 没有_activationType_默认前台() {
        let xml = r#"<toast launch="x"><visual/></toast>"#;
        let a = parse_activation(xml).unwrap();
        assert_eq!(a.kind, ActivationKind::Foreground);
    }

    #[test]
    fn protocol_但没_launch_视为不可复刻() {
        assert!(parse_activation(r#"<toast activationType="protocol"/>"#).is_none());
        assert!(parse_activation(r#"<toast activationType="protocol" launch="  "/>"#).is_none());
    }

    #[test]
    fn 不是_toast_或没有开标签() {
        assert!(parse_activation("<notification/>").is_none());
        assert!(parse_activation("<toast launch=\"x\"").is_none());
        // <toast2> 不算
        assert!(parse_activation("<toast2 launch=\"x\"/>").is_none());
    }

    #[test]
    fn 属性值里的尖括号不截断标签() {
        let xml = r#"<toast launch="a>b" activationType="protocol"><visual/></toast>"#;
        let a = parse_activation(xml).unwrap();
        assert_eq!(a.launch, "a>b");
    }

    #[test]
    fn 数字实体也还原() {
        assert_eq!(unescape_xml("a&#38;b&#x26;c"), "a&b&c");
        assert_eq!(unescape_xml("0 &lt; 1 &amp;&amp; 2 &gt; 1"), "0 < 1 && 2 > 1");
    }

    #[test]
    fn 未知实体原样保留() {
        assert_eq!(unescape_xml("a &unknown; b"), "a &unknown; b");
    }
}
