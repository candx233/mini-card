//! 诊断探针：完整 dump 当前播放会话的时间轴字段（Position / Start / End / Min / Max Seek / LastUpdated / Rate）
//! 用途：确认网易云 SMTC 到底报不报时间轴（0.1.1 排查「网易云进度不显示」）。
//! 运行：cargo run --example smtc_dump_probe
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession as Session,
    GlobalSystemMediaTransportControlsSessionManager as SessionManager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as PlayStatus,
};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

fn now_filetime_100ns() -> i64 {
    let unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap();
    (unix.as_nanos() / 100) as i64 + 116_444_736_000_000_000
}

fn main() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let mgr = SessionManager::RequestAsync()
        .expect("RequestAsync")
        .join()
        .expect("join");
    let sessions = mgr.GetSessions().unwrap();
    let n = sessions.Size().unwrap();
    let mut pick: Option<Session> = None;
    for i in 0..n {
        let s = sessions.GetAt(i).unwrap();
        let app = s.SourceAppUserModelId().map(|h| h.to_string()).unwrap_or_default();
        let st = s.GetPlaybackInfo().ok().and_then(|i| i.PlaybackStatus().ok());
        let title = s
            .TryGetMediaPropertiesAsync()
            .ok()
            .and_then(|op| op.join().ok())
            .and_then(|p| p.Title().ok())
            .map(|h| h.to_string())
            .unwrap_or_default();
        println!("session[{}] app={} status={:?} title={}", i, app, st, title);
        if st == Some(PlayStatus::Playing) && pick.is_none() {
            pick = Some(s);
        }
    }
    let s = match pick {
        Some(s) => s,
        None => {
            println!("没有正在播放的会话");
            return;
        }
    };
    println!("--- 采样 30 次 x 1s（完整时间轴字段）---");
    println!("t_s | status | pos_ms | d_pos | start_ms | end_ms | minseek | maxseek | L_fresh_ms | rate");
    let mut last_p = f64::NAN;
    for k in 0..30 {
        let st = s
            .GetPlaybackInfo()
            .ok()
            .and_then(|i| i.PlaybackStatus().ok())
            .map(|st| format!("{:?}", st))
            .unwrap_or_default();
        let rate = s
            .GetPlaybackInfo()
            .ok()
            .and_then(|i| i.PlaybackRate().ok())
            .and_then(|r| r.Value().ok())
            .unwrap_or(f64::NAN);
        let (p, st_ms, e, min_s, max_s, fresh) = match s.GetTimelineProperties() {
            Ok(tl) => {
                let g = |d: windows::Foundation::TimeSpan| d.Duration as f64 / 1e7;
                (
                    tl.Position().map(g).unwrap_or(-1.0),
                    tl.StartTime().map(g).unwrap_or(-1.0),
                    tl.EndTime().map(g).unwrap_or(-1.0),
                    tl.MinSeekTime().map(g).unwrap_or(-1.0),
                    tl.MaxSeekTime().map(g).unwrap_or(-1.0),
                    tl.LastUpdatedTime()
                        .map(|dt| (now_filetime_100ns() - dt.UniversalTime) as f64 / 1e4)
                        .unwrap_or(-1.0),
                )
            }
            Err(_) => (-2.0, -2.0, -2.0, -2.0, -2.0, -2.0),
        };
        let d = if last_p.is_nan() { f64::NAN } else { p - last_p };
        last_p = p;
        println!(
            "{:>3} | {:>6} | {:>8.0} | {:>6.0} | {:>8.0} | {:>8.0} | {:>7.0} | {:>7.0} | {:>10.0} | {:.2}",
            k, st, p, d, st_ms, e, min_s, max_s, fresh, rate
        );
        std::thread::sleep(std::time::Duration::from_millis(1000));
    }
}
