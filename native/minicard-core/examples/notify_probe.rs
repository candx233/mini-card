//! D1 通知读取 spike（0.1.3 通知卡）
//!
//! 用法：
//!   cargo run --example notify_probe -- [--seconds N] [--dump-text] [--filter 子串] [--remove ID]
//!
//! 口径：
//! - 默认只打印条数与各字段长度（隐私，不落正文）；`--dump-text` 才打印正文，
//!   配合 `--filter` 只对自己发的测试 toast 打印，避免把用户真实通知带进日志。
//! - 记录 GetAccessStatus / RequestAccessAsync / ClearNotifications / NotificationChanged 的 HRESULT。
//! - `RemoveNotification(id)` 用来做「删除后通知中心消失」的眼验。

use std::collections::HashSet;
use std::time::{Duration, Instant};

use windows::Foundation::TypedEventHandler;
use windows::UI::Notifications::Management::UserNotificationListener;
use windows::UI::Notifications::{KnownNotificationBindings, NotificationKinds};
use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};

fn main() {
    if let Err(e) = run() {
        eprintln!("probe failed: {e}");
        std::process::exit(1);
    }
}

fn run() -> windows::core::Result<()> {
    let mut seconds = 12u64;
    let mut dump_text = false;
    let mut filter: Option<String> = None;
    let mut remove_id: Option<u32> = None;

    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--seconds" => {
                i += 1;
                seconds = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(seconds);
            }
            "--dump-text" => dump_text = true,
            "--filter" => {
                i += 1;
                filter = args.get(i).cloned();
            }
            "--remove" => {
                i += 1;
                remove_id = args.get(i).and_then(|s| s.parse().ok());
            }
            "-h" | "--help" => {
                println!("usage: notify_probe [--seconds N] [--dump-text] [--filter 子串] [--remove ID]");
                return Ok(());
            }
            other => {
                eprintln!("unknown arg: {other}");
                std::process::exit(2);
            }
        }
        i += 1;
    }

    // 未打包进程也能用 WinRT 的前提：本线程初始化 MTA。
    unsafe { RoInitialize(RO_INIT_MULTITHREADED)? };

    let listener = UserNotificationListener::Current()?;
    let access = listener.GetAccessStatus()?;
    println!("access_status = {access:?}");

    // 官方文档要求 UI 线程；未打包进程的实测见 docs/research-013/notify-card-eval.md §2.2。
    let requested = listener.RequestAccessAsync()?.join()?;
    println!("request_access = {requested:?}");

    // 预期失败（0x80070490）：未打包进程不能清空/订阅。
    match listener.ClearNotifications() {
        Ok(()) => println!("clear_notifications = OK"),
        Err(e) => println!("clear_notifications = 0x{:08X}", e.code().0 as u32),
    }

    let handler = TypedEventHandler::new(|_sender, _args| Ok(()));
    match listener.NotificationChanged(&handler) {
        Ok(token) => {
            println!("notification_changed = OK token={token}");
            let _ = listener.RemoveNotificationChanged(token);
        }
        Err(e) => println!("notification_changed = 0x{:08X}", e.code().0 as u32),
    }

    if let Some(id) = remove_id {
        match listener.RemoveNotification(id) {
            Ok(()) => println!("remove_notification({id}) = OK"),
            Err(e) => println!("remove_notification({id}) = 0x{:08X}", e.code().0 as u32),
        }
    }

    let deadline = Instant::now() + Duration::from_secs(seconds);
    let t0 = Instant::now();
    let mut seen: HashSet<u32> = HashSet::new();
    let mut first_round = true;
    while Instant::now() < deadline {
        let list = listener.GetNotificationsAsync(NotificationKinds::Toast)?.join()?;
        let count = list.Size()?;
        println!("poll t_ms={} count = {count}", t0.elapsed().as_millis());
        for idx in 0..count {
            let n = list.GetAt(idx)?;
            let id = n.Id()?;
            let is_new = seen.insert(id);
            if !first_round && !is_new {
                continue;
            }

            let app = n.AppInfo()?;
            let app_name = app.DisplayInfo()?.DisplayName()?.to_string();
            let aumid = app.AppUserModelId()?.to_string();
            let secs_since_epoch = (n.CreationTime()?.UniversalTime - 116_444_736_000_000_000) / 10_000_000;

            let binding = n
                .Notification()?
                .Visual()?
                .GetBinding(&KnownNotificationBindings::ToastGeneric()?)?;
            let texts = binding.GetTextElements()?;
            let text_count = texts.Size()?;
            let mut lines: Vec<String> = Vec::new();
            for t in 0..text_count {
                lines.push(texts.GetAt(t)?.Text()?.to_string());
            }

            let selected = filter
                .as_deref()
                .map(|f| app_name.contains(f) || aumid.contains(f))
                .unwrap_or(true);

            if dump_text && selected {
                println!(
                    "  id={id} app={app_name:?} aumid={aumid:?} at={secs_since_epoch} texts={lines:?}"
                );
            } else {
                let lens: Vec<usize> = lines.iter().map(|s| s.chars().count()).collect();
                println!(
                    "  id={id} app_len={} aumid_len={} at={secs_since_epoch} text_count={text_count} text_lens={lens:?}",
                    app_name.chars().count(),
                    aumid.chars().count()
                );
            }
        }
        first_round = false;
        std::thread::sleep(Duration::from_millis(500));
    }

    Ok(())
}
