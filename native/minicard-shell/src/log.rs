//! 极简日志：GUI 子系统下没有 stderr（eprintln! 会直接 panic），一律落 %TEMP% 文件。

use std::io::Write;
use std::sync::{Mutex, OnceLock};

static SINK: OnceLock<Mutex<Option<std::fs::File>>> = OnceLock::new();

pub fn init(path: &std::path::Path) {
    let f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .ok();
    let _ = SINK.set(Mutex::new(f));
    log(&format!("── run ── {:?}", path));
}

pub fn log(msg: &str) {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    if let Some(m) = SINK.get() {
        if let Ok(mut g) = m.lock() {
            if let Some(f) = g.as_mut() {
                let _ = writeln!(f, "[{ms}] {msg}");
                let _ = f.flush();
            }
        }
    }
    if cfg!(debug_assertions) {
        println!("{msg}");
    }
}

#[macro_export]
macro_rules! mlog {
    ($($a:tt)*) => { $crate::log::log(&format!($($a)*)) };
}
