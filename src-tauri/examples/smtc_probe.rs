// SMTC spike 探针（windows crate 版）：列会话 → 读元数据/封面 → 播放控制试用。
// 用法（src-tauri 目录）：cargo run --example smtc_probe -- [封面输出.png]
// 安全约定：只对 app 名含 "msedge" 的会话做 toggle 试用（避免误控用户自己的播放器）。
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSessionManager as SessionManager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as PlayStatus,
};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

fn status_name(s: PlayStatus) -> &'static str {
    if s == PlayStatus::Playing {
        "Playing"
    } else if s == PlayStatus::Paused {
        "Paused"
    } else if s == PlayStatus::Stopped {
        "Stopped"
    } else if s == PlayStatus::Closed {
        "Closed"
    } else if s == PlayStatus::Opened {
        "Opened"
    } else if s == PlayStatus::Changing {
        "Changing"
    } else {
        "Unknown"
    }
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

fn main() -> windows::core::Result<()> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let cover_out = std::env::args().nth(1);

    let mgr = SessionManager::RequestAsync()?.join()?;
    println!("manager_ok=true");
    let sessions = mgr.GetSessions()?;
    let count = sessions.Size()?;
    println!("sessions={count}");

    match mgr.GetCurrentSession() {
        Ok(cur) => println!("current_app={}", cur.SourceAppUserModelId()?.to_string()),
        Err(_) => println!("current_app=none"),
    }

    for i in 0..count {
        let s = sessions.GetAt(i)?;
        let app = s.SourceAppUserModelId()?.to_string();
        let status = s.GetPlaybackInfo()?.PlaybackStatus()?;
        let props = s.TryGetMediaPropertiesAsync()?.join()?;
        let title = props.Title()?.to_string();
        let artist = props.Artist()?.to_string();
        let tl = s.GetTimelineProperties()?;
        let pos_s = tl.Position()?.Duration as f64 / 1e7;
        let dur_s = tl.EndTime()?.Duration as f64 / 1e7;

        let mut cover_note = String::from("cover=None");
        if let Ok(thumb) = props.Thumbnail() {
            match read_thumb(&thumb) {
                Ok(bytes) => {
                    if let Some(p) = &cover_out {
                        std::fs::write(p, &bytes).ok();
                    }
                    let sig = &bytes[..bytes.len().min(8)];
                    cover_note = format!("cover=Some({}B sig={:02x?})", bytes.len(), sig);
                }
                Err(e) => cover_note = format!("cover=ERR({e:?})"),
            }
        }
        println!(
            "app={app} status={} title={title} artist={artist} pos={pos_s:.1} dur={dur_s:.1} {cover_note}",
            status_name(status)
        );

        if app.to_lowercase().contains("msedge") {
            let t1 = s.TryTogglePlayPauseAsync()?.join()?;
            std::thread::sleep(std::time::Duration::from_millis(1500));
            let after1 = status_name(s.GetPlaybackInfo()?.PlaybackStatus()?);
            let t2 = s.TryTogglePlayPauseAsync()?.join()?;
            std::thread::sleep(std::time::Duration::from_millis(1500));
            let after2 = status_name(s.GetPlaybackInfo()?.PlaybackStatus()?);
            println!("toggle_ok: t1={t1} after1={after1} t2={t2} after2={after2}");
        }
    }
    println!("probe_done");
    Ok(())
}
