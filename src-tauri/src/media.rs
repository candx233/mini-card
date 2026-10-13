//! Windows SMTC 媒体会话（音乐卡数据源 · 2026-10-08 · 2026-10-11 改成只认音乐应用）
//! 结构：
//!   - 500ms 轮询线程：读当前**音乐**会话（曲目 / 状态 / 进度 / 封面）→ emit("music") 给所有窗口；
//!   - 进度带 LastUpdatedTime 补偿（播放器 timeline 写入时刻 → 现在 的间隔补上），配合前端走表消滞后；
//!   - 播放器**不给时间轴**时（实测网易云 3.1.41 原生 SMTC 恒为 0）走本地计时兜底：
//!     位置按本地时钟累计（暂停冻结、恢复续走），时长从网易云客户端自己的队列文件里按歌名+歌手取。
//!   - 封面 = Rust 侧缓存 dataURL（key = app|title|artist，换曲才重读缩略图；与 Spike 结论一致）；
//!   - 命令：music_state（卡片首帧拉取 / 预览轮询）、music_action（上一首 / 播放暂停 / 下一首）。
//! API 写法照 examples/smtc_probe.rs（windows 0.62 实机验证过）：
//!   异步用 .join()（不是 .get()）；GetCurrentSession()/Thumbnail() 缺失返回 Err。
//! 注意：命令是 async + spawn_blocking —— 同步命令跑主线程，COM 等锁会把整个 App 冻住。

use serde::Serialize;
use std::sync::Mutex;
use std::time::Instant;
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
/// 封面缓存：键 + dataURL + 换曲观察窗（2026-10-13）。
/// 切歌那一拍读到的缩略图常常还是上一首的（实测约半数切歌撞上），只读一次会把旧图缓存住；
/// 观察窗内每拍重读，读到不同图就换上，窗口过完仍没读到本首的图才清成占位。
struct CoverState {
    key: Option<String>,
    data: String,
    settle_until: Option<Instant>,
    /// 本曲是否读到过图（窗口结束时判「这歌确实没封面」用）
    got: bool,
}
static COVER: Mutex<CoverState> = Mutex::new(CoverState {
    key: None,
    data: String::new(),
    settle_until: None,
    got: false,
});

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

/// 本地计时兜底（2026-10-13）：部分播放器（实测网易云 3.1.41 的原生 SMTC）只报歌名/歌手/封面，
/// TimelineProperties 恒为 0 —— 卡片进度条与时间就永远停在 0:00。这类「无时间轴」会话改用本地计时：
///   换曲（key 变）归零重计；只在 playing 时走表，暂停冻结、恢复续走（拖进度条感知不到，换首歌就好）。
struct LocalClock {
    key: String,
    /// 已结清的历史播放毫秒（不含正在走的这一段）
    acc_ms: f64,
    /// 本段走表起点（仅 playing 时有值）
    tick_at: Option<Instant>,
}
static CLOCK: Mutex<Option<LocalClock>> = Mutex::new(None);

/// 计时状态落盘（%APPDATA%\mini-card\music_clock.json，同 weather.json 的位置）。
/// 目的：重启卡片（含自更新后自动重启）接着算，别让正在放的那首从 0:00 重来 ——
/// 只在「同一首歌 + 距上次落盘 < 15 分钟」时采用，其它情况按新曲从 0 起算。
#[derive(serde::Serialize, serde::Deserialize)]
struct ClockState {
    key: String,
    acc_ms: f64,
    playing: bool,
    /// 落盘时刻（unix 毫秒）
    t: i64,
}
static SEED: Mutex<Option<ClockState>> = Mutex::new(None);

fn clock_path() -> std::path::PathBuf {
    crate::config::path().with_file_name("music_clock.json")
}

fn now_unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 落盘当前计时（轮询线程每 10s 调一次；没有会话时不写）
fn save_clock(present: bool) {
    if !present {
        return;
    }
    let now = Instant::now();
    let st = {
        let g = CLOCK.lock().unwrap();
        match g.as_ref() {
            Some(c) => {
                let live = c
                    .tick_at
                    .map(|t| c.acc_ms + now.saturating_duration_since(t).as_secs_f64() * 1000.0)
                    .unwrap_or(c.acc_ms);
                Some(ClockState {
                    key: c.key.clone(),
                    acc_ms: live,
                    playing: c.tick_at.is_some(),
                    t: now_unix_ms(),
                })
            }
            None => None,
        }
    };
    if let Some(st) = st {
        if let Ok(txt) = serde_json::to_string(&st) {
            let _ = std::fs::write(clock_path(), txt);
        }
    }
}

fn local_position_ms(key: &str, playing: bool) -> f64 {
    let now = Instant::now();
    let mut g = CLOCK.lock().unwrap();
    if g.as_ref().map(|c| c.key.as_str()) != Some(key) {
        // 换曲：先看有没有上次落盘的同一首歌可续（重启场景）
        let mut acc = 0.0;
        if let Some(st) = SEED.lock().unwrap().take() {
            let gap = now_unix_ms() - st.t;
            if st.key == key && (0..15 * 60 * 1000).contains(&gap) {
                acc = st.acc_ms.max(0.0) + if st.playing { gap as f64 } else { 0.0 };
            }
        }
        *g = Some(LocalClock {
            key: key.to_string(),
            acc_ms: acc,
            tick_at: if playing { Some(now) } else { None },
        });
    }
    let c = g.as_mut().unwrap();
    if playing {
        let t = *c.tick_at.get_or_insert(now);
        c.acc_ms + now.saturating_duration_since(t).as_secs_f64() * 1000.0
    } else {
        if let Some(t) = c.tick_at.take() {
            c.acc_ms += now.saturating_duration_since(t).as_secs_f64() * 1000.0;
        }
        c.acc_ms
    }
}

/// 时长兜底：网易云客户端把当前队列写在 webdata\file 下
/// （playingList = 歌单/专辑队列；fmPlay = 私人FM，queue+currentIndex），每首带 duration（毫秒）。
/// 按「歌名 + 歌手」匹配，只读本地文件、不联网；取不到 = 0（卡片就不画进度占比，时间仍走表）。
static DUR_CACHE: Mutex<Option<(String, f64, Instant)>> = Mutex::new(None);

fn netease_ish(app: &str) -> bool {
    let a = app.to_lowercase();
    a.contains("cloudmusic") || a.contains("netease") || a.contains("orpheus")
}

fn netease_duration_ms(app: &str, title: &str, artist: &str) -> f64 {
    if !netease_ish(app) || title.is_empty() {
        return 0.0;
    }
    let ck = format!("{title}|{artist}");
    {
        let g = DUR_CACHE.lock().unwrap();
        if let Some((k, d, at)) = g.as_ref() {
            // 命中就复用；没命中的最多 5s 重扫一次（队列文件可能比会话晚写）
            if *k == ck && (*d > 0.0 || at.elapsed().as_secs() < 5) {
                return *d;
            }
        }
    }
    let d = scan_netease_queue(title, artist);
    *DUR_CACHE.lock().unwrap() = Some((ck, d, Instant::now()));
    d
}

fn scan_netease_queue(title: &str, artist: &str) -> f64 {
    let base = match std::env::var("LOCALAPPDATA") {
        Ok(v) => v,
        Err(_) => return 0.0,
    };
    let dir = std::path::PathBuf::from(base)
        .join("NetEase")
        .join("CloudMusic")
        .join("webdata")
        .join("file");
    for f in ["playingList", "fmPlay"] {
        let txt = match std::fs::read_to_string(dir.join(f)) {
            Ok(t) => t,
            Err(_) => continue,
        };
        let d = pick_duration_json(&txt, title, artist);
        if d > 0.0 {
            return d;
        }
    }
    0.0
}

/// 从网易云队列 JSON 里挑当前曲目的时长（毫秒）。两种结构都认：
/// playingList = `{list:[{track:{…}}]}`；fmPlay = `{queue:[{…}],currentIndex}`。
/// 先按「歌名 + 歌手」精确匹配；只对上歌名时退而用第一首同名（同曲不同版本也至少能画条）。
fn pick_duration_json(txt: &str, title: &str, artist: &str) -> f64 {
    let v: serde_json::Value = match serde_json::from_str(txt) {
        Ok(v) => v,
        Err(_) => return 0.0,
    };
    let arr = match v
        .get("list")
        .or_else(|| v.get("queue"))
        .and_then(|a| a.as_array())
    {
        Some(a) => a,
        None => return 0.0,
    };
    let mut loose: f64 = 0.0; // 只对上歌名、没对上歌手的备选
    for it in arr {
        let tr = it.get("track").unwrap_or(it);
        if tr.get("name").and_then(|n| n.as_str()).map(|n| n.trim()) != Some(title.trim()) {
            continue;
        }
        let dur = tr.get("duration").and_then(|d| d.as_f64()).unwrap_or(0.0);
        if dur <= 0.0 {
            continue;
        }
        let hit_artist = tr
            .get("artists")
            .and_then(|a| a.as_array())
            .map(|a| {
                a.iter().any(|x| {
                    x.get("name")
                        .and_then(|n| n.as_str())
                        .map(|an| !an.is_empty() && (artist.contains(an) || an.contains(artist)))
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false);
        if artist.is_empty() || hit_artist {
            return dur;
        }
        if loose == 0.0 {
            loose = dur;
        }
    }
    loose
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

/// 只认音乐应用的会话（2026-10-11 用户「音乐卡只接入音乐」）——
/// 起因：网页 B 站看视频时，浏览器也会挂 SMTC 会话，且会被 GetCurrentSession() 顶到「当前」位，
/// 卡片就把视频标题当歌播。这里按 AUMID 小写子串过滤：只认音乐播放器，浏览器/视频类一律不算。
/// 加新播放器 = 在表里补一个子串（用进程名/代号即可，不要求精确 AUMID）。
const MUSIC_APP_HINTS: &[&str] = &[
    "qqmusic",    // QQ 音乐（本机实测 AUMID = QQMusic.exe）
    "cloudmusic", // 网易云音乐（本机进程 cloudmusic.exe，占位会话未播放时不挂 SMTC）
    "netease",    // 网易系包名兜底（如 Netease.CloudMusic）
    "orpheus",    // 网易云内部代号兜底
    "spotify",
    "kugou",      // 酷狗
    "kuwo",       // 酷我
    "kwmusic",
    "migu",       // 咪咕音乐
    "foobar",     // foobar2000
    "aimp",
    "musicbee",
    "itunes",     // iTunes
    "applemusic", // Apple Music
    "zunemusic",  // Groove 音乐
    "wmplayer",   // Windows Media Player
];

fn is_music_app(aumid: &str) -> bool {
    let a = aumid.to_lowercase();
    MUSIC_APP_HINTS.iter().any(|h| a.contains(h))
}

/// 挑会话：只在音乐应用里挑 —— 正在播放的最优先；其次系统认定的「当前」会话（若也是音乐应用）；
/// 再次其它音乐会话（已加载未播 / 暂停）。没有音乐会话 → None（卡片显示占位样张）。
fn current_session(mgr: &SessionManager) -> Option<Session> {
    let cur_app = mgr
        .GetCurrentSession()
        .ok()
        .and_then(|s| s.SourceAppUserModelId().ok())
        .map(|h| h.to_string())
        .unwrap_or_default();
    let sessions = mgr.GetSessions().ok()?;
    let n = sessions.Size().ok()?;
    let mut best: Option<(u8, Session)> = None;
    for i in 0..n {
        let s = match sessions.GetAt(i) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let app = s
            .SourceAppUserModelId()
            .map(|h| h.to_string())
            .unwrap_or_default();
        if !is_music_app(&app) {
            continue;
        }
        let st = s.GetPlaybackInfo().ok().and_then(|i| i.PlaybackStatus().ok());
        let mut score: u8 = 1;
        if st == Some(PlayStatus::Playing) {
            score = 3;
        } else if !cur_app.is_empty() && app == cur_app {
            score = 2;
        }
        if st == Some(PlayStatus::Closed) {
            score = 0; // 关掉的残留会话：仅当没有别的音乐会话时兜底
        }
        if best.as_ref().map(|(b, _)| score > *b).unwrap_or(true) {
            best = Some((score, s));
        }
    }
    best.map(|(_, s)| s)
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

    // 时间轴：正常播放器直接给 Position/EndTime（带写入时刻补偿）；
    // 给不出（EndTime=0，实测网易云原生 SMTC 恒如此）→ 本地计时兜底 + 客户端队列取时长。
    let key = format!("{app}|{title}|{artist}");
    let (position_ms, duration_ms) = match s.GetTimelineProperties() {
        Ok(tl) if tl.EndTime().map(|d| d.Duration).unwrap_or(0) > 0 => {
            let p = tl.Position().map(|d| d.Duration as f64 / 1e7 * 1000.0).unwrap_or(0.0);
            let end = tl.EndTime().map(|d| d.Duration as f64 / 1e7 * 1000.0).unwrap_or(0.0);
            // 位置补偿（2026-10-09：「条和播放时间都比声音慢一秒」）：
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
                pos = pos.min(end);
            }
            (pos, end)
        }
        _ => {
            let playing = status == "playing";
            let dur = netease_duration_ms(&app, &title, &artist);
            let mut pos = local_position_ms(&key, playing);
            if dur > 0.0 {
                // 单曲循环：本地计时越过总长就取模从头（顺序播放换曲会重置，用不到这条）
                pos = if playing { pos % dur } else { pos.min(dur) };
            }
            (pos, dur)
        }
    };

    // 封面：换曲读一次 + 观察窗内每拍重读（缩略图常比元数据晚到、或读到上一首的图）；
    // **读空也要重试**——QQ 音乐等刚启动时缩略图比元数据晚到 1~2 秒（2026-10-09）。
    let cover = {
        let now = Instant::now();
        let mut st = COVER.lock().unwrap();
        let changed_now = st.key.as_deref() != Some(key.as_str());
        if changed_now {
            st.key = Some(key.clone());
            st.settle_until = Some(now + std::time::Duration::from_millis(3000));
            st.got = false;
        }
        let settling = st.settle_until.map(|t| now < t).unwrap_or(false);
        if !settling && !st.got && !st.data.is_empty() {
            // 观察窗过完仍没读到本曲的图（这歌确实没封面）→ 清成占位，别把上一首的图一直挂着
            st.data.clear();
        }
        if changed_now || st.data.is_empty() || settling {
            let mut cov = String::new();
            if let Ok(thumb) = props.Thumbnail() {
                if let Ok(bytes) = read_thumb(&thumb) {
                    if !bytes.is_empty() {
                        cov = format!("data:image/png;base64,{}", crate::base64_encode(&bytes));
                    }
                }
            }
            if !cov.is_empty() {
                if cov != st.data {
                    st.data = cov;
                }
                st.got = true;
            }
        }
        st.data.clone()
    };

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
        // 上次退出时落的计时（同一首歌且 15 分钟内 → 接着算，见 local_position_ms）
        {
            let seed = std::fs::read_to_string(clock_path())
                .ok()
                .and_then(|t| serde_json::from_str::<ClockState>(&t).ok());
            *SEED.lock().unwrap() = seed;
        }
        let mut mgr: Option<SessionManager> = None;
        let mut ticks: u64 = 0;
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
            if !snap.present {
                // 会话消失 → 本地计时一起清掉：下次从 0 起算，别把上一轮的账续上
                *CLOCK.lock().unwrap() = None;
            }
            {
                *SNAP.lock().unwrap() = Some(snap.clone());
            }
            // 每 20 拍（10s）落一次盘，供重启续算；写文件不在热路径上
            ticks += 1;
            if ticks % 20 == 0 {
                save_clock(snap.present);
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
        let s = current_session(&mgr).ok_or("没有正在播放的音乐会话")?;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// CLOCK / SEED 是进程级单例，测试并行跑会互相重置 → 这几条串起来跑
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    /// 网易云 playingList 的真实结构（list[].track，时长毫秒）
    const PL: &str = r#"{"list":[
        {"track":{"name":"想说","duration":200689,"artists":[{"name":"颜人中"}]}},
        {"track":{"name":"海的那边","duration":258235,"artists":[{"name":"法老"},{"name":"欧阳娜娜"},{"name":"青苹果学园幼儿园"}]}},
        {"track":{"name":"有些","duration":229523,"artists":[{"name":"颜人中"}]}}
    ]}"#;

    #[test]
    fn pick_duration_from_playlist() {
        assert_eq!(pick_duration_json(PL, "海的那边", "法老"), 258235.0);
        assert_eq!(pick_duration_json(PL, "有些", "颜人中"), 229523.0);
        assert_eq!(pick_duration_json(PL, "没这首歌", "谁"), 0.0);
    }

    #[test]
    fn pick_duration_fm_queue_and_loose_match() {
        let fm = r#"{"currentIndex":0,"queue":[{"name":"New Beginning","duration":249918,"artists":[{"name":"Maduk"},{"name":"Lexurus"}]}]}"#;
        assert_eq!(pick_duration_json(fm, "New Beginning", ""), 249918.0);
        // 歌手对不上 → 退到同名备选（同曲不同版本至少还能画条）
        assert_eq!(pick_duration_json(PL, "想说", "别人唱的版本"), 200689.0);
        assert_eq!(pick_duration_json("不是 json", "想说", "颜人中"), 0.0);
    }

    /// 本地计时：播放走表、暂停冻结、换曲归零 + 落盘续算（一个测试里顺序跑，避免共用单例打架）
    #[test]
    fn local_clock_ticks_freezes_resets_and_resumes() {
        let _g = TEST_LOCK.lock().unwrap();
        *SEED.lock().unwrap() = None;
        let k = "unit|clock|songA";
        assert!(local_position_ms(k, true) < 5.0);
        std::thread::sleep(std::time::Duration::from_millis(80));
        let p1 = local_position_ms(k, true);
        assert!(p1 >= 60.0, "播放中应当走表，实际 {p1}");
        let f1 = local_position_ms(k, false);
        std::thread::sleep(std::time::Duration::from_millis(60));
        assert_eq!(f1, local_position_ms(k, false), "暂停时必须冻结");
        assert!(local_position_ms("unit|clock|songB", true) < 5.0, "换曲必须归零");

        // 重启续算：同一首歌 + 10s 前落盘 60s → 接着从 ~70s 算
        *SEED.lock().unwrap() = Some(ClockState {
            key: "unit|seed|songC".into(),
            acc_ms: 60_000.0,
            playing: true,
            t: now_unix_ms() - 10_000,
        });
        let r = local_position_ms("unit|seed|songC", true);
        assert!((r - 70_000.0).abs() < 2_000.0, "应当接着上次的 60s 算，实际 {r}");

        // 不是同一首歌 / 太久以前 → 不采用
        *SEED.lock().unwrap() = Some(ClockState {
            key: "别的歌".into(),
            acc_ms: 60_000.0,
            playing: true,
            t: now_unix_ms(),
        });
        assert!(local_position_ms("unit|seed|songD", true) < 5.0, "换了歌必须归零");
        *SEED.lock().unwrap() = Some(ClockState {
            key: "unit|seed|songE".into(),
            acc_ms: 60_000.0,
            playing: true,
            t: now_unix_ms() - 20 * 60 * 1000,
        });
        assert!(local_position_ms("unit|seed|songE", true) < 5.0, "超过 15 分钟不采用");
        *SEED.lock().unwrap() = None;
    }
}
