//! 音乐：**外壳适配层**（实现在 `minicard_core::media`，自绘卡进程共用同一份）。
//!
//! 这里只保留 Tauri 特有的东西：`emit("music")` 广播线程 + 两个 `#[tauri::command]` 包装。

pub use minicard_core::media::*;

use tauri::{AppHandle, Emitter};

/// 启动轮询线程（setup 里调用一次）。会话消失 → present:false（保留管理器，下次直接复用）。
pub fn spawn(app: AppHandle) {
    std::thread::spawn(move || {
        let mut sampler = Sampler::new();
        loop {
            let payload = sampler.tick();
            let _ = app.emit("music", &payload);
            std::thread::sleep(std::time::Duration::from_millis(TICK_MS));
        }
    });
}

/// 卡片首帧 / 预览轮询取数
#[tauri::command]
pub fn music_state() -> Option<MusicSnap> {
    snapshot()
}

/// 播放控制：toggle / next / previous（用户自己点卡片上的按钮）
#[tauri::command]
pub async fn music_action(action: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || minicard_core::media::action(&action))
        .await
        .map_err(|e| e.to_string())?
}
