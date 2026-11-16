//! 小工具（自绘卡进程与 Tauri 外壳共用）。

use std::sync::atomic::{AtomicU64, Ordering};

use crate::config::CardKind;

/// 卡片「可见位图」：bit n = `CardKind` 判别值为 n 的卡当前露在屏幕上。
///
/// 由自绘卡进程的帧循环每拍写一次（遮挡闸门的判定结果）；采样/轮询线程读它决定
/// 要不要降频 —— 卡片被窗口盖住时，500ms 一次的 WinRT/PDH 采样纯属白烧。
///
/// 默认 `u64::MAX`（全可见）：Tauri 外壳进程不写这张表，行为与以前完全一致。
static VISIBLE_MASK: AtomicU64 = AtomicU64::new(u64::MAX);

pub fn set_visible_mask(mask: u64) {
    VISIBLE_MASK.store(mask, Ordering::Relaxed);
}

pub fn visible_mask() -> u64 {
    VISIBLE_MASK.load(Ordering::Relaxed)
}

/// 这种卡现在是不是可见（没出现过的卡 = 不可见）。
pub fn kind_visible(kind: CardKind) -> bool {
    visible_mask() & (1u64 << (kind as u64)) != 0
}

/// 标准 base64（音乐封面 dataURL 用）。手写是为了不引 crate —— 这段以前在 `src-tauri/src/main.rs`。
pub fn base64_encode(data: &[u8]) -> String {
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
