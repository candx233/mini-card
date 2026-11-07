//! 系统通知读取（0.1.3 通知卡数据层 · D2）
//!
//! 口径与证据：`docs/research-013/notify-card-eval.md`；D1 spike 探针
//! `examples/notify_probe.rs`（本机 Win11 26H2 26300.9711 实测可用）。
//!
//! 设计要点：
//! - **只轮询**：未打包进程订阅 `NotificationChanged` / 调 `ClearNotifications` 都返回
//!   `0x80070490`，只能 500ms 轮询 `GetNotificationsAsync(Toast)` + 逐条 `RemoveNotification(id)`；
//! - **权限**：`GetAccessStatus` 每拍检查；被撤销后 API 会**静默返回空列表**，所以不能
//!   用「空列表」判空态，必须用 access 区分「未授权 / 无通知 / 接口不可用」；
//! - **隐私**：正文只在内存 ring buffer（≤20 条）里，不落盘、不写日志；Debug 只打印长度；
//! - **启动快照去重**：第一次成功轮询里的旧通知一律视为已读，之后新出现的 id 才标未读；
//! - 线程模型：独立工作线程里 `RoInitialize(MTA)` + 持有 listener；删除/标已读通过
//!   mpsc 命令投递，绘制线程只读 `Arc<RwLock<NotifySnapshot>>`。

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use windows::UI::Notifications::Management::{
    UserNotificationListener, UserNotificationListenerAccessStatus,
};
use windows::UI::Notifications::{KnownNotificationBindings, NotificationKinds, UserNotification};
use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};

/// ring buffer 上限；卡面最多只画 3 组 / 4 条，20 条给后续「全部」视图留余量。
pub const MAX_ITEMS: usize = 20;
/// 轮询节拍（未打包进程实测 500ms 延迟 131–504ms；D1 探针 500ms 足够）。
pub const POLL_INTERVAL: Duration = Duration::from_millis(500);
/// 会话正文单行上限（防超长 toast 把内存/绘制拖垮；卡面单行省略，不需要全文）。
const MAX_TITLE_CHARS: usize = 200;
const MAX_BODY_CHARS: usize = 400;

/// 通知读取权限/可用性。`Unspecified` = 还没请求过；`Unavailable` = 系统不支持 / 接口失败。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NotifyAccess {
    #[default]
    Unspecified,
    Allowed,
    Denied,
    Unavailable,
}

/// 一条通知。字段只活在内存里；**不要**给这个结构实现 `Serialize`。
#[derive(Clone, PartialEq, Eq)]
pub struct NotifyItem {
    /// Windows 通知 id（u32；RemoveNotification 用）
    pub id: u32,
    /// 应用显示名（如「微信」；取不到时回退 aumid）
    pub app: String,
    /// 发送方 AUMID（`shell:AppsFolder\<AUMID>` 可打开）
    pub aumid: String,
    /// 第一行文本（通常标题 / 发送者）
    pub title: String,
    /// 其余文本行合并成一行
    pub body: String,
    /// 创建时间，unix 毫秒；0 = 取不到
    pub created_ms: u64,
    /// 启动快照之后的**新**通知 = true；启动时已有的旧通知 = false
    pub unread: bool,
}

impl fmt::Debug for NotifyItem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NotifyItem")
            .field("id", &self.id)
            .field("app", &self.app)
            .field("created_ms", &self.created_ms)
            .field("unread", &self.unread)
            .field("title_len", &self.title.chars().count())
            .field("body_len", &self.body.chars().count())
            .finish()
    }
}

/// 绘制侧读到的整份快照。`at_ms` = 最近一次成功轮询的时刻（判断接口是否还活着）。
#[derive(Debug, Clone, Default)]
pub struct NotifySnapshot {
    pub access: NotifyAccess,
    pub items: Vec<NotifyItem>,
    pub at_ms: u64,
}

impl NotifySnapshot {
    /// 未读数（卡面抬头「N 条未读」用）
    pub fn unread_count(&self) -> usize {
        self.items.iter().filter(|i| i.unread).count()
    }
}

enum Cmd {
    Remove(u32),
    MarkRead(u32),
    /// 停掉工作线程（D5 关闭通知卡时用；重新开启就再 spawn 一个）。
    Stop,
}

/// 通知读取服务：进程内单例即可；`spawn` 起工作线程，`snapshot` 给绘制侧读。
pub struct NotifyService {
    shared: Arc<RwLock<NotifySnapshot>>,
    cmd: Sender<Cmd>,
}

impl NotifyService {
    pub fn spawn() -> Arc<Self> {
        let shared = Arc::new(RwLock::new(NotifySnapshot::default()));
        let (tx, rx) = mpsc::channel();
        let worker_shared = Arc::clone(&shared);
        let spawned = thread::Builder::new()
            .name("mini-card-notify".into())
            .spawn(move || worker(worker_shared, rx));
        if spawned.is_err() {
            if let Ok(mut s) = shared.write() {
                s.access = NotifyAccess::Unavailable;
            }
        }
        Arc::new(Self { shared, cmd: tx })
    }

    pub fn snapshot(&self) -> NotifySnapshot {
        self.shared.read().map(|s| s.clone()).unwrap_or_default()
    }

    /// 从 Windows 通知中心删除一条（逐条删；`ClearNotifications` 不可用）。
    pub fn remove(&self, id: u32) {
        let _ = self.cmd.send(Cmd::Remove(id));
    }

    /// 把一条标成已读（卡面点击后调用；后续轮询不会重新变未读）。
    pub fn mark_read(&self, id: u32) {
        let _ = self.cmd.send(Cmd::MarkRead(id));
    }

    /// 停止工作线程（关闭通知卡时调用；线程会在 ≤500ms 内退出，快照清空）。
    pub fn stop(&self) {
        let _ = self.cmd.send(Cmd::Stop);
    }
}

impl fmt::Debug for NotifyService {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NotifyService").finish_non_exhaustive()
    }
}

fn worker(shared: Arc<RwLock<NotifySnapshot>>, rx: Receiver<Cmd>) {
    // 未打包进程用 WinRT 的前提：本线程初始化 MTA（D1 探针同款）。
    let _ = unsafe { RoInitialize(RO_INIT_MULTITHREADED) };

    let mut tracker = Tracker::new();
    let mut listener: Option<UserNotificationListener> = None;
    let mut last_access = NotifyAccess::Unspecified;
    let mut access_requested = false;

    loop {
        while let Ok(cmd) = rx.try_recv() {
            match cmd {
                Cmd::Remove(id) => {
                    if let Some(l) = listener.as_ref() {
                        let _ = l.RemoveNotification(id);
                    }
                    tracker.remove(id);
                    publish(&shared, last_access, &tracker);
                }
                Cmd::MarkRead(id) => {
                    tracker.mark_read(id);
                    publish(&shared, last_access, &tracker);
                }
                Cmd::Stop => {
                    tracker.reset_baseline();
                    publish(&shared, NotifyAccess::Unspecified, &tracker);
                    return;
                }
            }
        }

        if listener.is_none() {
            match UserNotificationListener::Current() {
                Ok(l) => listener = Some(l),
                Err(_) => {
                    last_access = NotifyAccess::Unavailable;
                    tracker.reset_baseline();
                    publish(&shared, last_access, &tracker);
                    thread::sleep(Duration::from_secs(2));
                    continue;
                }
            }
        }
        let Some(l) = listener.as_ref() else {
            thread::sleep(POLL_INTERVAL);
            continue;
        };

        let mut access = match l.GetAccessStatus() {
            Ok(a) => map_access(a),
            Err(_) => NotifyAccess::Unavailable,
        };

        // 只请求一次；被拒绝后不反复弹窗，用户改系统设置后下一拍会自动变 Allowed。
        if access == NotifyAccess::Unspecified && !access_requested {
            access_requested = true;
            access = match l.RequestAccessAsync() {
                Ok(op) => match op.join() {
                    Ok(a) => map_access(a),
                    Err(_) => NotifyAccess::Unavailable,
                },
                Err(_) => NotifyAccess::Unavailable,
            };
        }

        if access == NotifyAccess::Allowed {
            let polled = match l.GetNotificationsAsync(NotificationKinds::Toast) {
                Ok(op) => match op.join() {
                    Ok(list) => {
                        let mut items = Vec::new();
                        let count = list.Size().unwrap_or(0);
                        for i in 0..count {
                            if let Ok(u) = list.GetAt(i) {
                                if let Some(item) = item_of(&u) {
                                    items.push(item);
                                }
                            }
                        }
                        Some(items)
                    }
                    Err(_) => None,
                },
                Err(_) => None,
            };

            match polled {
                Some(items) => {
                    tracker.accept(items);
                    last_access = NotifyAccess::Allowed;
                    publish(&shared, last_access, &tracker);
                }
                None => {
                    last_access = NotifyAccess::Unavailable;
                    tracker.reset_baseline();
                    publish(&shared, last_access, &tracker);
                }
            }
        } else {
            last_access = access;
            tracker.reset_baseline();
            publish(&shared, last_access, &tracker);
        }

        thread::sleep(POLL_INTERVAL);
    }
}

fn publish(shared: &Arc<RwLock<NotifySnapshot>>, access: NotifyAccess, tracker: &Tracker) {
    if let Ok(mut s) = shared.write() {
        s.access = access;
        s.items = tracker.items.clone();
        s.at_ms = unix_ms();
    }
}

fn map_access(a: UserNotificationListenerAccessStatus) -> NotifyAccess {
    match a.0 {
        1 => NotifyAccess::Allowed,
        2 => NotifyAccess::Denied,
        _ => NotifyAccess::Unspecified,
    }
}

/// 单条 WinRT 通知 → 内存结构；任何字段取不到都只回退/跳过，不把整轮轮询打崩。
fn item_of(u: &UserNotification) -> Option<NotifyItem> {
    let id = u.Id().ok()?;
    let app = u.AppInfo().ok();
    let app_name = app
        .as_ref()
        .and_then(|a| a.DisplayInfo().ok())
        .and_then(|d| d.DisplayName().ok())
        .map(|h| h.to_string())
        .unwrap_or_default();
    let aumid = app
        .as_ref()
        .and_then(|a| a.AppUserModelId().ok())
        .map(|h| h.to_string())
        .unwrap_or_default();
    let app = if app_name.trim().is_empty() { aumid.clone() } else { app_name };

    let created_ms = u
        .CreationTime()
        .map(|t| filetime_to_unix_ms(t.UniversalTime))
        .unwrap_or(0);

    let mut lines: Vec<String> = Vec::new();
    if let Ok(binding) = u
        .Notification()
        .and_then(|n| n.Visual())
        .and_then(|v| v.GetBinding(&KnownNotificationBindings::ToastGeneric()?))
    {
        if let Ok(texts) = binding.GetTextElements() {
            let count = texts.Size().unwrap_or(0);
            for i in 0..count {
                if let Ok(t) = texts.GetAt(i).and_then(|x| x.Text()) {
                    let line = clean_text(&t.to_string());
                    if !line.is_empty() {
                        lines.push(line);
                    }
                }
            }
        }
    }

    let title = lines.first().cloned().unwrap_or_default();
    let body = lines.iter().skip(1).cloned().collect::<Vec<_>>().join(" ");
    let mut title = truncate_chars(title, MAX_TITLE_CHARS);
    let body = truncate_chars(body, MAX_BODY_CHARS);
    if title.is_empty() {
        title = app.clone();
    }

    Some(NotifyItem {
        id,
        app,
        aumid,
        title,
        body,
        created_ms,
        unread: false,
    })
}

fn filetime_to_unix_ms(filetime_100ns: i64) -> u64 {
    const OFFSET: i64 = 116_444_736_000_000_000;
    let ticks = filetime_100ns.saturating_sub(OFFSET);
    if ticks <= 0 {
        0
    } else {
        (ticks / 10_000) as u64
    }
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 去换行/控制字符、合并空白；卡面只画单行。
fn clean_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut space = false;
    for c in s.chars() {
        if c.is_whitespace() || c.is_control() {
            space = true;
            continue;
        }
        if space && !out.is_empty() {
            out.push(' ');
        }
        space = false;
        out.push(c);
    }
    out.trim().to_owned()
}

fn truncate_chars(s: String, max: usize) -> String {
    if s.chars().count() <= max {
        return s;
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

/// 相对时间文案（与 3 稿同口径：刚刚 / N 分钟前 / N 小时前 / N 天前）。
pub fn relative_time(created_ms: u64, now_ms: u64) -> String {
    if created_ms == 0 {
        return String::new();
    }
    let mins = now_ms.saturating_sub(created_ms) / 60_000;
    if mins < 1 {
        "刚刚".to_owned()
    } else if mins < 60 {
        format!("{mins} 分钟前")
    } else if mins < 24 * 60 {
        format!("{} 小时前", mins / 60)
    } else {
        format!("{} 天前", mins / (24 * 60))
    }
}

/// 纯逻辑的状态机：便于单测，不碰 WinRT。
struct Tracker {
    items: Vec<NotifyItem>,
    seen: HashSet<u32>,
    read: HashSet<u32>,
    dismissed: HashSet<u32>,
    /// 下一次成功轮询作为基线（启动 / 权限恢复后旧通知不算未读）
    baseline: bool,
}

impl Tracker {
    fn new() -> Self {
        Self {
            items: Vec::new(),
            seen: HashSet::new(),
            read: HashSet::new(),
            dismissed: HashSet::new(),
            baseline: true,
        }
    }

    fn reset_baseline(&mut self) {
        self.items.clear();
        self.seen.clear();
        self.read.clear();
        self.baseline = true;
    }

    /// 用一轮完整轮询结果替换快照（通知中心是权威列表：外部 dismiss 的条目会消失）。
    fn accept(&mut self, polled: Vec<NotifyItem>) {
        let baseline = self.baseline;
        let existing: HashMap<u32, bool> = self.items.iter().map(|i| (i.id, i.unread)).collect();
        let mut incoming: HashMap<u32, NotifyItem> = HashMap::with_capacity(polled.len());

        for mut item in polled {
            if self.dismissed.contains(&item.id) {
                continue;
            }
            item.unread = if let Some(prev_unread) = existing.get(&item.id) {
                // 同一条通知内容可能被更新：未读状态继承，不因重新轮询就变已读/未读。
                *prev_unread && !self.read.contains(&item.id)
            } else if baseline {
                false
            } else {
                !self.seen.contains(&item.id) && !self.read.contains(&item.id)
            };
            self.seen.insert(item.id);
            incoming.insert(item.id, item);
        }

        self.items = incoming.into_values().collect();
        self.sort_and_trim();
        self.prune_sets();
        self.baseline = false;
    }

    fn mark_read(&mut self, id: u32) {
        self.read.insert(id);
        if let Some(item) = self.items.iter_mut().find(|i| i.id == id) {
            item.unread = false;
        }
    }

    fn remove(&mut self, id: u32) {
        self.items.retain(|i| i.id != id);
        self.seen.remove(&id);
        self.read.remove(&id);
        self.dismissed.insert(id);
    }

    fn sort_and_trim(&mut self) {
        self.items.sort_by(|a, b| {
            b.created_ms
                .cmp(&a.created_ms)
                .then_with(|| b.id.cmp(&a.id))
        });
        self.items.truncate(MAX_ITEMS);
    }

    fn prune_sets(&mut self) {
        let ids: HashSet<u32> = self.items.iter().map(|i| i.id).collect();
        self.seen.retain(|id| ids.contains(id));
        self.read.retain(|id| ids.contains(id));
        // dismissed 不按当前 items 清：删掉的通知可能仍被通知中心返回一两次，
        // 必须留到进程结束，否则「删了又回来」。
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: u32, created_ms: u64) -> NotifyItem {
        NotifyItem {
            id,
            app: "微信".into(),
            aumid: "WeChat!App".into(),
            title: format!("标题{id}"),
            body: format!("正文{id}"),
            created_ms,
            unread: false,
        }
    }

    #[test]
    fn first_poll_is_baseline() {
        let mut t = Tracker::new();
        t.accept(vec![item(1, 1000), item(2, 2000)]);
        assert_eq!(t.items.len(), 2);
        assert!(t.items.iter().all(|i| !i.unread));
        assert_eq!(t.items[0].id, 2); // 新→旧
    }

    #[test]
    fn later_poll_new_id_is_unread() {
        let mut t = Tracker::new();
        t.accept(vec![item(1, 1000)]);
        t.accept(vec![item(1, 1000), item(2, 2000)]);
        assert_eq!(t.items.len(), 2);
        assert_eq!(t.items[0].id, 2);
        assert!(t.items[0].unread);
        assert!(!t.items[1].unread);
    }

    #[test]
    fn existing_update_keeps_unread_state() {
        let mut t = Tracker::new();
        t.accept(vec![item(1, 1000)]);
        t.accept(vec![item(1, 1000), item(2, 2000)]);
        let mut updated = item(2, 2500);
        updated.body = "更新后的正文".into();
        t.accept(vec![item(1, 1000), updated]);
        assert!(t.items[0].unread, "内容更新不应把未读变已读");
        assert_eq!(t.items[0].body, "更新后的正文");
    }

    #[test]
    fn mark_read_survives_repoll() {
        let mut t = Tracker::new();
        t.accept(vec![item(1, 1000)]);
        t.accept(vec![item(1, 1000), item(2, 2000)]);
        t.mark_read(2);
        t.accept(vec![item(1, 1000), item(2, 2000)]);
        assert!(!t.items[0].unread);
    }

    #[test]
    fn remove_hides_item_even_if_reported_again() {
        let mut t = Tracker::new();
        t.accept(vec![item(1, 1000), item(2, 2000)]);
        t.remove(1);
        t.accept(vec![item(1, 1000), item(2, 2000)]);
        assert_eq!(t.items.len(), 1);
        assert_eq!(t.items[0].id, 2);
    }

    #[test]
    fn ring_buffer_keeps_newest_20() {
        let mut t = Tracker::new();
        let polled: Vec<NotifyItem> = (1..=25).map(|id| item(id, 1000 + id as u64)).collect();
        t.accept(polled);
        assert_eq!(t.items.len(), MAX_ITEMS);
        assert_eq!(t.items[0].id, 25);
        assert_eq!(t.items[MAX_ITEMS - 1].id, 6);
    }

    #[test]
    fn external_dismiss_removes_item() {
        let mut t = Tracker::new();
        t.accept(vec![item(1, 1000), item(2, 2000)]);
        t.accept(vec![item(2, 2000)]);
        assert_eq!(t.items.len(), 1);
        assert_eq!(t.items[0].id, 2);
    }

    #[test]
    fn relative_time_boundaries() {
        assert_eq!(relative_time(0, 10_000), "");
        assert_eq!(relative_time(10_000, 39_999), "刚刚");
        assert_eq!(relative_time(1_000, 61_000), "1 分钟前");
        assert_eq!(relative_time(1_000, 3_601_000), "1 小时前");
        assert_eq!(relative_time(1_000, 86_401_000), "1 天前");
    }

    #[test]
    fn filetime_epoch_conversion() {
        let epoch = 116_444_736_000_000_000i64;
        assert_eq!(filetime_to_unix_ms(epoch), 0);
        assert_eq!(filetime_to_unix_ms(epoch + 10_000_000), 1000);
        assert_eq!(filetime_to_unix_ms(0), 0);
    }

    #[test]
    fn clean_text_collapses_whitespace() {
        assert_eq!(clean_text("  a\n\n b\tc  "), "a b c");
        assert_eq!(clean_text(""), "");
    }

    #[test]
    fn access_mapping() {
        assert_eq!(
            map_access(UserNotificationListenerAccessStatus::Allowed),
            NotifyAccess::Allowed
        );
        assert_eq!(
            map_access(UserNotificationListenerAccessStatus::Denied),
            NotifyAccess::Denied
        );
        assert_eq!(
            map_access(UserNotificationListenerAccessStatus::Unspecified),
            NotifyAccess::Unspecified
        );
    }

    #[test]
    fn unread_count() {
        let mut t = Tracker::new();
        t.accept(vec![item(1, 1000)]);
        t.accept(vec![item(1, 1000), item(2, 2000), item(3, 3000)]);
        t.mark_read(3);
        let snap = NotifySnapshot {
            access: NotifyAccess::Allowed,
            items: t.items,
            at_ms: 1,
        };
        assert_eq!(snap.unread_count(), 1);
    }
}
