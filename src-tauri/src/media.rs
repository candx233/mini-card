//! Windows SMTC 媒体会话（音乐卡数据源 · 2026-10-08）
//! 结构：
//!   - 500ms 轮询线程：读当前媒体会话（曲目 / 状态 / 进度 / 封面）→ emit("music") 给所有窗口；
//!   - 进度带 LastUpdatedTime 补偿（播放器 timeline 写入时刻 → 现在 的间隔补上），配合前端走表消滞后；
//!   - 封面 = Rust 侧缓存 dataURL（key = app|title|artist，换曲才重读缩略图；与 Spike 结论一致）；
//!   - 命令：music_state（卡片首帧拉取 / 预览轮询）、music_action（上一首 / 播放暂停 / 下一首）。
//! API 写法照 examples/smtc_probe.rs（windows 0.62 实机验证过）：
//!   异步用 .join()（不是 .get()）；GetCurrentSession()/Thumbnail() 缺失返回 Err。
//! 注意：命令是 async + spawn_blocking —— 同步命令跑主线程，COM 等锁会把整个 App 冻住。

use serde::Serialize;
use std::sync::Mutex;
use tauri::{AppHandle, Emitter};
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession as Session,
    GlobalSystemMediaTransportControlsSessionManager as SessionManager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as PlayStatus,
};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

#[derive(Debug, Clone, Serialize, Default)]
pub struct MusicSnap {
    /// 有没有媒体会话（false = 卡片显示「未在播放」）
    pub present: bool,
    pub app: String,
    pub title: String,
    pub artist: String,
    /// "playing" | "paused" | "stopped" | ...
    pub status: String,
    pub position_ms: f64,
    pub duration_ms: f64,
    /// data:image/png;base64,…（取不到 = 空串，卡片回退音符图标）
    pub cover: String,
}

static SNAP: Mutex<Option<MusicSnap>> = Mutex::new(None);
/// 封面缓存键 + 缓存值（换曲才重读）
static COVER_KEY: Mutex<Option<String>> = Mutex::new(None);
static COVER: Mutex<String> = Mutex::new(String::new());

fn status_name(s: PlayStatus) -> &'static str {
    if s == PlayStatus::Playing {
        "playing"
    } else if s == PlayStatus::Paused {
        "paused"
    } else if s == PlayStatus::Stopped {
        "stopped"
    } else if s == PlayStatus::Closed {
        "closed"
    } else if s == PlayStatus::Opened {
        "opened"
    } else if s == PlayStatus::Changing {
        "changing"
    } else {
        "unknown"
    }
}

/// FILETIME 基准（100ns，1601-01-01 起）的当前时刻 —— SMTC LastUpdatedTime 同基准。
fn now_filetime_100ns() -> i64 {
    let unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap();
    (unix.as_nanos() / 100) as i64 + 116_444_736_000_000_000
}

fn read_thumb(
    thumb: &windows::Storage::Streams::IRandomAccessStreamReference,
) -> windows::core::Result<Vec<u8>> {
    let stream = thumb.OpenReadAsync()?.join()?;
    let size = stream.Size()? as u32;
    let reader = windows::Storage::Streams::DataReader::CreateDataReader(&stream)?;
    reader.LoadAsync(size)?.join()?;
    let mut bytes = vec![0u8; size as usize];
    reader.ReadBytes(&mut bytes)?;
    Ok(bytes)
}

fn current_session(mgr: &SessionManager) -> Option<Session> {
    if let Ok(s) = mgr.GetCurrentSession() {
        return Some(s);
    }
    // 没有「当前」会话时退而取第一个（刚打开、还没被系统认定焦点的播放器）
    let sessions = mgr.GetSessions().ok()?;
    let n = sessions.Size().ok()?;
    if n > 0 {
        sessions.GetAt(0).ok()
    } else {
        None
    }
}

fn read_snap(mgr: &SessionManager) -> Option<MusicSnap> {
    let s = current_session(mgr)?;
    let app = s.SourceAppUserModelId().map(|h| h.to_string()).unwrap_or_default();
    let props = s.TryGetMediaPropertiesAsync().ok()?.join().ok()?;
    let title = props.Title().map(|h| h.to_string()).unwrap_or_default();
    let artist = props.Artist().map(|h| h.to_string()).unwrap_or_default();

    let status = s
        .GetPlaybackInfo()
        .ok()
        .and_then(|i| i.PlaybackStatus().ok())
        .map(status_name)
        .unwrap_or("unknown")
        .to_string();

    let (position_ms, duration_ms) = match s.GetTimelineProperties() {
        Ok(tl) => {
            let p = tl.Position().map(|d| d.Duration as f64 / 1e7 * 1000.0).unwrap_or(0.0);
            let end = tl.EndTime().map(|d| d.Duration as f64 / 1e7 * 1000.0).unwrap_or(0.0);
            // 位置补偿（2026-10-09 用户「条和播放时间都比声音慢一秒」）：
            // P 是播放器上一次写入 timeline 的进度，LastUpdatedTime 是那次写入的时刻；
            // 正在播放时补上「写入时刻 → 现在」的间隔 ≈ 当前真实进度（速率按 1.0）。
            // 超过 30s 的间隔视为时间线异常（个别播放器不更新），退回原始 P。
            let mut pos = p;
            if status == "playing" {
                if let Ok(l) = tl.LastUpdatedTime() {
                    let gap_ms = (now_filetime_100ns() - l.UniversalTime) as f64 / 1e4;
                    if gap_ms > 0.0 && gap_ms < 30_000.0 {
                        pos = p + gap_ms;
                    }
                }
                if end > 0.0 {
                    pos = pos.min(end);
                }
            }
            (pos, end)
        }
        Err(_) => (0.0, 0.0),
    };

    // 封面：键（app|title|artist）变了重读；**读空也要重试**——
    // QQ 音乐等刚启动时缩略图比元数据晚到 1~2 秒，只按 key 变化读一次会一直空到下一首
    // （2026-10-09 用户实测：刚打开 QQ音乐没有封面、切歌才出）。
    let key = format!("{app}|{title}|{artist}");
    let changed = {
        let mut k = COVER_KEY.lock().unwrap();
        if k.as_deref() != Some(key.as_str()) {
            *k = Some(key);
            true
        } else {
            false
        }
    };
    let need_read = changed || COVER.lock().unwrap().is_empty();
    if need_read {
        let mut cov = String::new();
        if let Ok(thumb) = props.Thumbnail() {
            if let Ok(bytes) = read_thumb(&thumb) {
                if !bytes.is_empty() {
                    cov = format!("data:image/png;base64,{}", crate::base64_encode(&bytes));
                }
            }
        }
        *COVER.lock().unwrap() = cov;
    }
    let cover = COVER.lock().unwrap().clone();

    Some(MusicSnap {
        present: true,
        app,
        title,
        artist,
        status,
        position_ms,
        duration_ms,
        cover,
    })
}

/// 启动轮询线程（setup 里调用一次）。会话消失 → present:false（保留管理器，下次直接复用）。
pub fn spawn(app: AppHandle) {
    std::thread::spawn(move || {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        let mut mgr: Option<SessionManager> = None;
        loop {
            if mgr.is_none() {
                mgr = SessionManager::RequestAsync()
                    .ok()
                    .and_then(|op| op.join().ok());
            }
            let snap = match &mgr {
                Some(m) => read_snap(m).unwrap_or_default(),
                None => MusicSnap::default(),
            };
            {
                *SNAP.lock().unwrap() = Some(snap.clone());
            }
            let _ = app.emit("music", &snap);
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    });
}

/// 卡片首帧 / 预览轮询取数
#[tauri::command]
pub fn music_state() -> Option<MusicSnap> {
    SNAP.lock().unwrap().clone()
}

/// 播放控制：toggle / next / previous（用户自己点卡片上的按钮）
#[tauri::command]
pub async fn music_action(action: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || -> Result<(), String> {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        let mgr = SessionManager::RequestAsync()
            .map_err(|e| e.to_string())?
            .join()
            .map_err(|e| e.to_string())?;
        let s = current_session(&mgr).ok_or("没有正在播放的媒体会话")?;
        match action.as_str() {
            "toggle" => s
                .TryTogglePlayPauseAsync()
                .map_err(|e| e.to_string())?
                .join()
                .map_err(|e| e.to_string())?,
            "next" => s
                .TrySkipNextAsync()
                .map_err(|e| e.to_string())?
                .join()
                .map_err(|e| e.to_string())?,
            "previous" => s
                .TrySkipPreviousAsync()
                .map_err(|e| e.to_string())?
                .join()
                .map_err(|e| e.to_string())?,
            _ => return Err("unknown action".into()),
        };
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}
