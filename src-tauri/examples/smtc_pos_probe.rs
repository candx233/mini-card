//! 诊断探针：SMTC timeline 新鲜度（P / LastUpdatedTime / End），~400ms × 12 次
//! 用途：确认「进度条比声音慢一秒」的来源 —— QQ 音乐 timeline 是每秒更新还是逐帧更新。
//! 运行：cargo run --example smtc_pos_probe

use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession as Session,
    GlobalSystemMediaTransportControlsSessionManager as SessionManager,
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
        let status = s
            .GetPlaybackInfo()
            .ok()
            .and_then(|pi| pi.PlaybackStatus().ok())
            .map(|st| format!("{:?}", st))
            .unwrap_or_default();
        let title = s
            .TryGetMediaPropertiesAsync()
            .ok()
            .and_then(|op| op.join().ok())
            .and_then(|p| p.Title().ok())
            .map(|h| h.to_string())
            .unwrap_or_default();
        println!("session[{}] app={} status={} title={}", i, app, status, title);
        if status.contains("Playing") && pick.is_none() {
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
    println!("--- 锁定播放中会话，开始采样 ---");
    let t0 = std::time::Instant::now();
    println!("t_ms | status | P_ms | P_delta | L_fresh_ms | End_ms");
    let mut last_p = f64::NAN;
    for _ in 0..12 {
        let status = s
            .GetPlaybackInfo()
            .ok()
            .and_then(|i| i.PlaybackStatus().ok())
            .map(|st| format!("{:?}", st))
            .unwrap_or_default();
        let (p, end, fresh) = match s.GetTimelineProperties() {
            Ok(tl) => {
                let p = tl.Position().map(|d| d.Duration as f64 / 1e7).unwrap_or(-1.0); // ms
                let e = tl.EndTime().map(|d| d.Duration as f64 / 1e7).unwrap_or(-1.0);
                let f = tl
                    .LastUpdatedTime()
                    .map(|dt| (now_filetime_100ns() - dt.UniversalTime) as f64 / 1e4)
                    .unwrap_or(-1.0);
                (p, e, f)
            }
            Err(_) => (-2.0, -2.0, -2.0),
        };
        let d = if last_p.is_nan() { f64::NAN } else { p - last_p };
        last_p = p;
        println!(
            "{:>5.0} | {:>8} | {:>8.1} | {:>+7.1} | {:>10.1} | {:>8.1}",
            t0.elapsed().as_millis(),
            status,
            p,
            d,
            fresh,
            end
        );
        std::thread::sleep(std::time::Duration::from_millis(400));
    }
}
