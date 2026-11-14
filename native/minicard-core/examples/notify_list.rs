//! 只读：列出通知中心当前的 toast（id / 应用 / AUMID），不打印正文、不清空。
//!
//! 用法：`cargo run --example notify_list`
//!
//! 用途：确认 WinRT 的 `UserNotification.Id` 与系统通知库
//! `%LOCALAPPDATA%\Microsoft\Windows\Notifications\wpndatabase.db` 里的 `Notification.Id`
//! 是同一个编号（点通知复刻激活要按这个 id 查 launch）。

use windows::UI::Notifications::Management::UserNotificationListener;
use windows::UI::Notifications::NotificationKinds;
use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};

fn main() -> windows::core::Result<()> {
    unsafe { RoInitialize(RO_INIT_MULTITHREADED)? };
    let listener = UserNotificationListener::Current()?;
    println!("access = {:?}", listener.GetAccessStatus()?);
    let list = listener.GetNotificationsAsync(NotificationKinds::Toast)?.join()?;
    let n = list.Size()?;
    println!("count = {n}");
    for i in 0..n {
        let u = list.GetAt(i)?;
        let id = u.Id()?;
        let app = u.AppInfo()?;
        let name = app
            .DisplayInfo()
            .and_then(|d| d.DisplayName())
            .map(|s| s.to_string())
            .unwrap_or_default();
        let aumid = app
            .AppUserModelId()
            .map(|s| s.to_string())
            .unwrap_or_default();
        println!("  id={id} app={name:?} aumid={aumid:?}");
    }
    Ok(())
}
