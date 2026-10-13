//! 诊断探针：看「切歌」时 标题 与 缩略图 谁先更新（网易云「有时候封面不切」的时序取证）。
//! 每 500ms dump 一行：状态 / 标题 / 缩略图字节数 + 哈希；每 3s 调一次 SkipNext。
//! 运行：cargo run --example smtc_thumb_probe
use sha2::{Digest, Sha256};
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession as Session,
    GlobalSystemMediaTransportControlsSessionManager as SessionManager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as PlayStatus,
};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

fn main() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let mgr = SessionManager::RequestAsync().expect("RequestAsync").join().expect("join");
    let sessions = mgr.GetSessions().unwrap();
    let n = sessions.Size().unwrap();
    let mut pick: Option<Session> = None;
    for i in 0..n {
        let s = sessions.GetAt(i).unwrap();
        let app = s.SourceAppUserModelId().map(|h| h.to_string()).unwrap_or_default();
        if app.to_lowercase().contains("cloudmusic") {
            pick = Some(s);
        }
    }
    let s = match pick {
        Some(s) => s,
        None => {
            println!("没有 cloudmusic 会话");
            return;
        }
    };
    // 暂停态也能改会话，但没在播时封面多半也是整张专辑缓存好的 —— 先确保在播
    let playing = |s: &Session| {
        s.GetPlaybackInfo()
            .ok()
            .and_then(|i| i.PlaybackStatus().ok())
            .map(|st| st == PlayStatus::Playing)
            .unwrap_or(false)
    };
    if !playing(&s) {
        println!("（当前不是播放态 → 先切回播放）");
        let _ = s.TryTogglePlayPauseAsync().map(|op| op.join());
        std::thread::sleep(std::time::Duration::from_millis(1500));
    }
    println!("t(s) | 事件 | status | title | thumb_len | thumb_hash8");
    for k in 0..80 {
        let st = s
            .GetPlaybackInfo()
            .ok()
            .and_then(|i| i.PlaybackStatus().ok())
            .map(|st| format!("{:?}", st).replace("GlobalSystemMediaTransportControlsSessionPlaybackStatus(", "").replace(")", ""))
            .unwrap_or_default();
        let (title, tl, th) = match s.TryGetMediaPropertiesAsync().ok().and_then(|op| op.join().ok()) {
            Some(p) => {
                let title = p.Title().map(|h| h.to_string()).unwrap_or_default();
                let (len, hash) = match p.Thumbnail() {
                    Ok(t) => match t.OpenReadAsync().ok().and_then(|o| o.join().ok()) {
                        Some(stream) => {
                            let size = stream.Size().ok().unwrap_or(0) as u32;
                            match windows::Storage::Streams::DataReader::CreateDataReader(&stream) {
                                Ok(reader) => {
                                    let _ = reader.LoadAsync(size).map(|o| o.join());
                                    let mut bytes = vec![0u8; size as usize];
                                    let _ = reader.ReadBytes(&mut bytes);
                                    let h = Sha256::digest(&bytes);
                                    (size, hex8(&h))
                                }
                                Err(_) => (0, "readerr".into()),
                            }
                        }
                        None => (0, "noopen".into()),
                    },
                    Err(_) => (0, "nothumb".into()),
                };
                (title, len, hash)
            }
            None => ("<props err>".into(), 0, "-".into()),
        };
        let skip = k > 0 && k % 6 == 0;
        println!(
            "{:>4} | {:>4} | {:>3} | {:>28} | {:>7} | {}",
            k as f64 * 0.5,
            if skip { "SKIP" } else { "" },
            st,
            title.chars().take(20).collect::<String>(),
            tl,
            th
        );
        if skip {
            let _ = s.TrySkipNextAsync().map(|op| op.join());
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}

fn hex8(b: &[u8]) -> String {
    b.iter().take(4).map(|x| format!("{:02x}", x)).collect()
}
