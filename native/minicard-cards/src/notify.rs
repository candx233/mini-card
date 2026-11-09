//! 通知卡（0.1.3 · 03「按应用分组」样式）—— 默认 240×240（4×4），D2D 自绘。
//!
//! 数据来自 `minicard_core::notify`（D2），这里只负责「怎么画」：
//! - 四态：正常（按应用分组）/ 空 / 未授权 / 不可用；
//! - 组头：18px 应用块 + 应用名 + 总数药丸 + 相对时间；
//! - 条目：双行（标题 600 fg / 正文 muted），各自单行省略；
//! - 未读：条目前 4px accent 圆点；抬头右侧「N 条未读 / 共 N 条」；
//! - 点击命中：`hit_at` 返回条目 id / AUMID / 删除 / 授权引导。
//!
//! B 案（0.1.4）：分割线以下是一个可连续滚动的「视口」节点（id=9007）——
//! 所有组按 `group_items_limit(ALL_GROUPS, ALL_ROWS, per_app)` 竖排，`paint` 对视口做
//! 矩形 clip + `SetTransform(0,-scroll)`；滚轮一格 = 2 行（76px @240），180ms
//! easeOutQuart 补间。只有内容溢出视口时才出现 2px 细滚动条；默认样张 3 组 4 条
//! 刚好不溢出 → 静止态绘制顺序/坐标/颜色与旧版逐像素一致。
//!
//! 动效状态机在 [`Motion`] 里：scroll 连续滚动 + enter/exit/suppress + hover +
//! clear_swap + state_swap；**首次 `observe` 只种子 seen、不播动画**（启动不整卡入场）。
//!
//! 尺寸口径：BASE = 240×180（k = 1）算字号；双行骨架按 240×240 排版，字号不随大卡放大。

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use minicard_core::notify::{relative_time, NotifyAccess, NotifyItem, NotifySnapshot};
use minicard_draw::color::Palette;
use minicard_draw::layout::{layout, Align, Dim, Dir, Justify, Measurer, Node, Placed, TextSpec};
use minicard_draw::paint::{fill_rect, fill_round, set_color, stroke_round};
use minicard_draw::text::{draw_shadowed, Mode as TextMode, TextEngine};
use minicard_shell::gfx::Gfx;
use windows::core::Result;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::{ID2D1Bitmap1, D2D1_INTERPOLATION_MODE_LINEAR};

/// 卡面一屏的组数 / 行数（240×300 = 3 组 4 条）。只给 `capacity()` 与布局单测用；
/// 连续滚动不再按它分页（所有组都排进视口，滚动只是平移）。
pub const MAX_GROUPS: usize = 3;
pub const MAX_ROWS: usize = 8;
/// 滚动数据上限：先按应用分组，再按屏裁；超过 12 个应用/24 条时只保留前 12 组。
pub const ALL_GROUPS: usize = 12;
pub const ALL_ROWS: usize = 24;

/// 视口节点 id：分割线之后、grow 占满剩余高度；组列表整体平移到这个视口里。
pub const ID_VIEWPORT: u32 = 9007;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CardState {
    Normal,
    Empty,
    Unauthorized,
    Unavailable,
    /// 全局开关关闭（settings.notify_card=false）：卡片保留，但不读通知
    Disabled,
}

pub fn card_state(access: NotifyAccess, item_count: usize) -> CardState {
    match access {
        NotifyAccess::Allowed if item_count == 0 => CardState::Empty,
        NotifyAccess::Allowed => CardState::Normal,
        NotifyAccess::Denied | NotifyAccess::Unspecified => CardState::Unauthorized,
        NotifyAccess::Unavailable => CardState::Unavailable,
    }
}

/// 快照 → 卡面四态（`None` = 全局关闭 = Disabled）。`Motion::observe` / `paint` 共用。
pub fn display_state(snap: Option<&NotifySnapshot>) -> CardState {
    match snap {
        Some(s) => card_state(s.access, s.items.len()),
        None => CardState::Disabled,
    }
}

pub fn state_texts(state: CardState) -> (&'static str, &'static str) {
    match state {
        CardState::Normal => ("", ""),
        CardState::Empty => ("最近没有新通知", ""),
        CardState::Unauthorized => ("需要授权读取通知", "点此打开设置"),
        CardState::Unavailable => ("通知读取暂不可用", "稍后自动重试"),
        CardState::Disabled => ("通知卡未启用", "在设置中开启"),
    }
}

pub struct Tokens {
    pub w: f32,
    pub h: f32,
    pub r: f32,
    pub radius: f32,
    pub pad_t: f32,
    pub pad_l: f32,
    pub pad_b: f32,
    pub pad_r: f32,
    pub header_fs: f32,
    pub header_lh: f32,
    pub count_fs: f32,
    pub count_lh: f32,
    /// 正常态抬头下的 1px 分割线高度（固定物理 1px，不随 r 缩，避免亚像素糊成灰带）
    pub rule_h: f32,
    pub head_gap: f32,
    pub group_gh: f32,
    pub group_gap: f32,
    pub app_gap: f32,
    pub app_fs: f32,
    pub app_lh: f32,
    pub badge_fs: f32,
    pub badge_lh: f32,
    pub badge_h: f32,
    pub badge_pad: f32,
    pub time_fs: f32,
    pub time_lh: f32,
    /// 双行条目：标题行 / 正文行 / 整行高度
    pub title_fs: f32,
    pub title_lh: f32,
    pub item_h: f32,
    pub item_gap: f32,
    pub item_indent: f32,
    pub dot: f32,
    pub body_fs: f32,
    pub body_lh: f32,
    pub tile: f32,
    pub tile_radius: f32,
    pub tile_fs: f32,
    pub state_fs: f32,
    pub state_lh: f32,
    pub hint_fs: f32,
    pub hint_lh: f32,
    pub state_gap: f32,
}

impl Tokens {
    pub fn notify(w: f32, h: f32) -> Self {
        // BASE = 240×180；同样例 16px 圆角，k=1 时字号/间距与 03 稿逐项对齐。
        // BASE = 240×180；大卡不放大字号（避免 300×300 反而只剩 2 组），多出来的空间用来放行
        let k = (w / 240.0).min(h / 180.0).clamp(0.4, 1.0);
        let r = 16.0 * k;
        Self {
            w,
            h,
            r,
            radius: r,
            pad_t: 0.8125 * r,
            pad_l: 0.8125 * r,
            pad_b: 0.6875 * r,
            pad_r: 0.8125 * r,
            header_fs: 0.90625 * r,
            header_lh: 1.0625 * r,
            count_fs: 0.75 * r,
            count_lh: 0.875 * r,
            rule_h: 1.0,
            head_gap: 0.1875 * r,
            group_gh: 1.5 * r,
            group_gap: 0.4375 * r,
            app_gap: 0.375 * r,
            app_fs: 0.8125 * r,
            app_lh: 1.0 * r,
            badge_fs: 0.59375 * r,
            badge_lh: 0.75 * r,
            badge_h: 0.8125 * r,
            badge_pad: 0.3125 * r,
            time_fs: 0.625 * r,
            time_lh: 0.8125 * r,
            title_fs: 0.84375 * r,
            title_lh: 1.03125 * r,
            item_h: 2.375 * r,
            item_gap: 0.1875 * r,
            item_indent: 1.5 * r,
            dot: 0.28125 * r,
            body_fs: 0.75 * r,
            body_lh: 0.9375 * r,
            tile: 1.25 * r,
            tile_radius: 0.375 * r,
            tile_fs: 0.5625 * r,
            state_fs: 0.8125 * r,
            state_lh: 1.125 * r,
            hint_fs: 0.6875 * r,
            hint_lh: 0.9375 * r,
            state_gap: 0.3125 * r,
        }
    }
}

/// 通知卡绘制选项（来自 settings；probe/单测用默认）。
#[derive(Clone, Copy, Debug)]
pub struct Options {
    pub show_body: bool,
    pub per_app: usize,
    /// 抬头「清除」已点过一次，等待 3 秒内再点确认
    pub clear_armed: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            show_body: true,
            per_app: 2,
            clear_armed: false,
        }
    }
}

/// 一组可见应用（供绘制/命中复用）。
#[derive(Clone, Debug)]
pub struct VisibleGroup {
    pub app: String,
    pub aumid: String,
    pub newest_ms: u64,
    /// 该应用的通知总数（含未显示的）
    pub total: usize,
    pub unread: usize,
    /// 在 `snap.items` 里的下标，按时间新→旧
    pub items: Vec<usize>,
}

/// 按应用分组 + 可见配额：先每组 1 条，再把剩余配额轮流加给前几组（组内最多 `per_app` 条）。
pub fn group_items(items: &[NotifyItem], max_groups: usize, max_rows: usize) -> Vec<VisibleGroup> {
    group_items_limit(items, max_groups, max_rows, 2)
}

pub fn group_items_limit(
    items: &[NotifyItem],
    max_groups: usize,
    max_rows: usize,
    per_app: usize,
) -> Vec<VisibleGroup> {
    if max_groups == 0 || max_rows == 0 {
        return Vec::new();
    }
    let per_app = per_app.clamp(1, 3);

    struct Acc {
        app: String,
        aumid: String,
        items: Vec<usize>,
        newest_ms: u64,
        unread: usize,
    }

    let mut order: Vec<String> = Vec::new();
    let mut accs: HashMap<String, Acc> = HashMap::new();
    for (idx, it) in items.iter().enumerate() {
        let key = if it.aumid.trim().is_empty() {
            format!("name:{}", it.app)
        } else {
            format!("aumid:{}", it.aumid)
        };
        match accs.get_mut(&key) {
            Some(acc) => {
                acc.items.push(idx);
                acc.unread += usize::from(it.unread);
            }
            None => {
                order.push(key.clone());
                accs.insert(
                    key,
                    Acc {
                        app: if it.app.trim().is_empty() { "其他".to_owned() } else { it.app.clone() },
                        aumid: it.aumid.clone(),
                        newest_ms: it.created_ms,
                        unread: usize::from(it.unread),
                        items: vec![idx],
                    },
                );
            }
        }
    }

    let all: Vec<Acc> = order.into_iter().filter_map(|k| accs.remove(&k)).collect();
    let n = all.len().min(max_groups);
    let mut take = vec![0usize; n];
    let mut budget = max_rows;
    for slot in take.iter_mut() {
        if budget == 0 {
            break;
        }
        *slot = 1;
        budget -= 1;
    }
    // 第二轮起轮流给每组加到「组内最多 2 条」
    while budget > 0 {
        let mut progressed = false;
        for g in 0..n {
            if budget == 0 {
                break;
            }
            if take[g] < all[g].items.len().min(per_app) {
                take[g] += 1;
                budget -= 1;
                progressed = true;
            }
        }
        if !progressed {
            break;
        }
    }

    all.into_iter()
        .take(n)
        .zip(take)
        .filter(|(_, take)| *take > 0)
        .map(|(acc, take)| VisibleGroup {
            app: acc.app,
            aumid: acc.aumid,
            newest_ms: acc.newest_ms,
            total: acc.items.len(),
            unread: acc.unread,
            items: acc.items.into_iter().take(take).collect(),
        })
        .collect()
}

/// 按卡高算「能放几组 / 几条」：双行骨架 + 抬头分割线下，240×300 是 3 组 / 4 条，
/// 240×240 是 2 组 / 3 条，240×180 是 1 组 / 2 条（均以 `capacity_scales_with_height` 与
/// `--probe notify --debug-layout` 实测为准；旧注释里的「240×240 三组 / 240×180 两组」已作废）。
/// 组数 ≥ 行数，避免 `group_items` 先每组 1 条时出现空组。
pub fn capacity(t: &Tokens) -> (usize, usize) {
    // 正常态 = 抬头 + 分割线 + 消息列表：根 gap 在 header/divider/groups 之间出现两次，
    // 分割线本身也占 1px。这里必须把这 3px 的净增量算进去，否则 240×300 会挤掉最后一条。
    let avail = t.h - t.pad_t - t.pad_b - t.header_lh - t.rule_h - 2.0 * t.head_gap;
    for groups in (1..=MAX_GROUPS).rev() {
        for rows in (groups..=MAX_ROWS).rev() {
            let mut counts = vec![0usize; groups];
            for i in 0..rows {
                counts[i % groups] += 1;
            }
            let height: f32 = counts
                .iter()
                .map(|&n| {
                    t.group_gh
                        + t.item_gap
                        + n as f32 * t.item_h
                        + (n.saturating_sub(1)) as f32 * t.item_gap
                })
                .sum::<f32>()
                + (groups.saturating_sub(1)) as f32 * t.group_gap;
            if height <= avail + 0.01 {
                return (groups, rows);
            }
        }
    }
    (1, 1)
}

// ── B 案动效状态机（0.1.4） ────────────────────────────────────────────────
//
// 时长/曲线（用户拍板：120 hover、180 arrive/exit/scroll、120 clear swap、160 state，
// 统一 easeOutQuart）。所有时刻都用 `Instant` 传进来，补间求值纯函数 —— 便于单测
// 用「t0 + 90ms」这种确定性时间点验中间帧。

/// 新条目行高入场 / 塌缩退出的时长（ms）
const ENTER_ROW_MS: f32 = 180.0;
/// 新组组头入场（ms）
const ENTER_GROUP_MS: f32 = 120.0;
/// 本地删除的退出塌缩（ms）
const EXIT_MS: f32 = 180.0;
/// 滚轮连续滚动补间（ms）
const SCROLL_MS: f32 = 180.0;
/// 清除 / 确认两段文字交叉淡化（ms）
const CLEAR_SWAP_MS: f32 = 120.0;
/// 空 / 授权 / 不可用态内容淡入（ms）
const STATE_FADE_MS: f32 = 160.0;
/// 行 hover 底色 / × 淡入淡出（ms）
const HOVER_MS: f32 = 120.0;
/// 本地删除后快照预期 ≤1s 收敛；2s 还没收敛就撤销抑制、重放一次入场
const SUPPRESS_MS: f32 = 2000.0;
/// 新条目入场的初始上浮量（DIP，-4 → 0）
const ENTER_DY: f32 = 4.0;
/// 滚轮一格 = 2 行（内容滚动距离 = 2 × item_h = 76px @240 宽）
pub const WHEEL_LINES: f32 = 2.0;

fn ease_out_quart(u: f32) -> f32 {
    let u = u.clamp(0.0, 1.0);
    let inv = 1.0 - u;
    1.0 - inv * inv * inv * inv
}

/// 线性进度 0..1（曲线在调用处再套）。
fn linear_progress(now: Instant, t0: Instant, ms: f32) -> f32 {
    let d = now.saturating_duration_since(t0).as_secs_f32() * 1000.0;
    (d / ms.max(1.0)).clamp(0.0, 1.0)
}

fn ms_since(now: Instant, t0: Instant) -> f32 {
    now.saturating_duration_since(t0).as_secs_f32() * 1000.0
}

/// 单行 hover 补间：只动透明度（零位移）。多行各存一份，鼠标在行间移动时两份同时补。
#[derive(Clone, Copy)]
struct HoverTween {
    from: f32,
    to: f32,
    t0: Instant,
}

impl HoverTween {
    fn value(&self, now: Instant) -> f32 {
        let u = linear_progress(now, self.t0, HOVER_MS);
        self.from + (self.to - self.from) * ease_out_quart(u)
    }
    fn active(&self, now: Instant) -> bool {
        (self.to - self.from).abs() > 0.0001 && linear_progress(now, self.t0, HOVER_MS) < 1.0
    }
}

/// 正在退出的条目：留着数据才能在 180ms 里把这一行画成「高度塌缩 + 淡出」。
struct ExitRow {
    item: NotifyItem,
    t0: Instant,
}

/// 通知卡动效状态机（B 案）。live 只持有一个，跨帧传引用：
/// - 连续滚动：`scroll_by` 叠加 target，`paint` 里按 180ms easeOutQuart 求显示值；
/// - 新通知：`observe` 做 id diff（首次只种子 seen，不播 → 启动不整卡入场）；
/// - 本地删除：`mark_removed` 把 id 放进 suppress，快照过滤掉这一条，它同时走 180ms 退出；
/// - hover / clear_swap / state_swap：都只补透明度，不动几何。
pub struct Motion {
    /// 当前显示的滚动值（DIP；补间中间值也是它）
    pub scroll: f32,
    scroll_from: f32,
    scroll_to: f32,
    scroll_t0: Option<Instant>,
    /// 首次 observe 只种子 seen / seen_groups，不播任何入场（启动/首帧静止）
    pub initialized: bool,
    /// 已见过的通知 id（含被 suppress 的本地删除项，直到快照收敛）
    seen: HashSet<u32>,
    /// 已见过的组 key（aumid / name:应用名）
    seen_groups: HashSet<String>,
    /// 新条目 → 入场起始时刻（180ms）
    enter: HashMap<u32, Instant>,
    /// 新组 → 组头入场起始时刻（120ms）
    group_enter: HashMap<String, Instant>,
    /// 本地删除中：id → 抑制起始时刻（命中过滤 + 2s 回滚判据）
    suppressed: HashMap<u32, Instant>,
    /// 正在退出（塌缩）的条目：id → (数据, 起始时刻)
    exiting: HashMap<u32, ExitRow>,
    /// `mark_removed` 早于 `observe`（拿不到条目数据）时的兜底：下一次 observe 补上
    exit_pending: HashMap<u32, Instant>,
    /// hover 补间：条目 id → 透明度（120ms）
    hover: HashMap<u32, HoverTween>,
    /// 清除 → 确认的交叉目标（false = 清除，true = 确认）
    clear_armed: bool,
    clear_from: f32,
    clear_t0: Option<Instant>,
    /// 四态切换 → 新内容 160ms 淡入
    state_t0: Option<Instant>,
    prev_state: Option<CardState>,
    /// 上一版快照（检测消失做退出动画；mark_removed 从这取条目数据）
    last_ids: Vec<u32>,
    last_items: Vec<NotifyItem>,
    last_at_ms: u64,
}

impl Motion {
    pub fn new() -> Self {
        Self {
            scroll: 0.0,
            scroll_from: 0.0,
            scroll_to: 0.0,
            scroll_t0: None,
            initialized: false,
            seen: HashSet::new(),
            seen_groups: HashSet::new(),
            enter: HashMap::new(),
            group_enter: HashMap::new(),
            suppressed: HashMap::new(),
            exiting: HashMap::new(),
            exit_pending: HashMap::new(),
            hover: HashMap::new(),
            clear_armed: false,
            clear_from: 0.0,
            clear_t0: None,
            state_t0: None,
            prev_state: None,
            last_ids: Vec::new(),
            last_items: Vec::new(),
            last_at_ms: 0,
        }
    }

    /// 数据更新入口：id diff → 入场/退出；四态切换；suppress 收敛与 2s 回滚。
    /// 首次调用只种子（`initialized = true`），**不播动画**。
    pub fn observe(&mut self, snap: Option<&NotifySnapshot>, state: CardState, now: Instant) {
        let items: &[NotifyItem] = snap.map(|s| s.items.as_slice()).unwrap_or(&[]);
        if !self.initialized {
            self.seen = items.iter().map(|i| i.id).collect();
            self.seen_groups = items.iter().map(group_key).collect();
            self.last_ids = items.iter().map(|i| i.id).collect();
            self.last_items = items.to_vec();
            self.last_at_ms = snap.map(|s| s.at_ms).unwrap_or(0);
            self.prev_state = Some(state);
            self.initialized = true;
            return;
        }

        // ① 四态切换：新内容 160ms 淡入（旧内容即时移除；抬头/分割线/卡框不动）
        if self.prev_state != Some(state) {
            self.prev_state = Some(state);
            self.state_t0 = Some(now);
        }

        let present: HashSet<u32> = items.iter().map(|i| i.id).collect();

        // ② suppress 收敛 / 2s 回滚
        let mut converged: Vec<u32> = Vec::new();
        let mut revert: Vec<u32> = Vec::new();
        for (&id, &t0) in self.suppressed.iter() {
            if !present.contains(&id) {
                converged.push(id);
            } else if ms_since(now, t0) >= SUPPRESS_MS {
                revert.push(id);
            }
        }
        for id in converged {
            // 快照收敛（删成功）只撤销「过滤」；退出塌缩动画还在 180ms 里跑完，
            // 不能在这里把 exiting 清掉，否则数据侧删得快时行会闪一下消失。
            self.suppressed.remove(&id);
            self.seen.remove(&id);
            self.exit_pending.remove(&id);
        }
        for id in revert {
            // 删除没生效（2s 还在）：撤销抑制，重放一次 180ms 入场
            self.suppressed.remove(&id);
            self.exiting.remove(&id);
            self.exit_pending.remove(&id);
            self.enter.insert(id, now);
        }

        // ③ mark_removed 早于 observe 时，从当前快照补回退出动画的数据
        if !self.exit_pending.is_empty() {
            let pending: Vec<(u32, Instant)> =
                self.exit_pending.iter().map(|(&k, &v)| (k, v)).collect();
            for (id, t0) in pending {
                if let Some(it) = items.iter().find(|i| i.id == id) {
                    self.exiting.entry(id).or_insert(ExitRow { item: it.clone(), t0 });
                    self.exit_pending.remove(&id);
                }
            }
        }

        // ④ 新 id → 180ms 行高入场（首次 observe 已 return，不会触发）
        for it in items {
            if self.suppressed.contains_key(&it.id) {
                continue;
            }
            if self.seen.insert(it.id) {
                self.enter.insert(it.id, now);
            }
        }

        // ⑤ 从快照里消失的 id（容量挤出 / 系统侧清理）→ 180ms 退出
        let ids: Vec<u32> = items.iter().map(|i| i.id).collect();
        let mut vanished: Vec<(u32, NotifyItem)> = Vec::new();
        for old in &self.last_items {
            if present.contains(&old.id) || self.suppressed.contains_key(&old.id) {
                continue;
            }
            if self.seen.remove(&old.id) {
                vanished.push((old.id, old.clone()));
            }
        }
        for (id, item) in vanished {
            self.exiting.insert(id, ExitRow { item, t0: now });
            self.enter.remove(&id);
        }

        // ⑥ 新组组头 → 120ms 入场（组消失后再出现也要重新入场）
        let keys: HashSet<String> = items
            .iter()
            .filter(|i| !self.suppressed.contains_key(&i.id))
            .map(group_key)
            .collect();
        self.seen_groups.retain(|k| keys.contains(k));
        for k in &keys {
            if self.seen_groups.insert(k.clone()) {
                self.group_enter.insert(k.clone(), now);
            }
        }
        self.group_enter.retain(|k, _| keys.contains(k));

        // ⑦ 记上一版（内容也刷新，mark_removed 才有条目数据可画）
        let at = snap.map(|s| s.at_ms).unwrap_or(0);
        if self.last_ids != ids || self.last_at_ms != at {
            self.last_ids = ids;
            self.last_items = items.to_vec();
            self.last_at_ms = at;
        }
    }

    /// 是否还有动效在跑（live 用它决定 60fps 重绘还是静置节拍）。
    ///
    /// 注意：suppress 等待快照收敛期间**不算**动画 —— 收敛/2s 回滚由 1s 数据节拍里的
    /// `observe` 负责，没必要为此白烧 2 秒 60fps。
    pub fn animating(&self, now: Instant) -> bool {
        if self
            .scroll_t0
            .is_some_and(|t0| linear_progress(now, t0, SCROLL_MS) < 1.0)
        {
            return true;
        }
        if self.enter.values().any(|t0| ms_since(now, *t0) < ENTER_ROW_MS) {
            return true;
        }
        if self
            .group_enter
            .values()
            .any(|t0| ms_since(now, *t0) < ENTER_GROUP_MS)
        {
            return true;
        }
        if self.exiting.values().any(|e| ms_since(now, e.t0) < EXIT_MS) {
            return true;
        }
        if self.hover.values().any(|h| h.active(now)) {
            return true;
        }
        if self
            .clear_t0
            .is_some_and(|t0| ms_since(now, t0) < CLEAR_SWAP_MS)
        {
            return true;
        }
        if self
            .state_t0
            .is_some_and(|t0| ms_since(now, t0) < STATE_FADE_MS)
        {
            return true;
        }
        false
    }

    /// 当前滚动显示值（补间求值；不动状态，tree_hook/hit_at 只读用）。
    pub fn scroll_at(&self, now: Instant) -> f32 {
        match self.scroll_t0 {
            Some(t0) => {
                let u = linear_progress(now, t0, SCROLL_MS);
                self.scroll_from + (self.scroll_to - self.scroll_from) * ease_out_quart(u)
            }
            None => self.scroll,
        }
    }

    /// 滚轮 / 触控板：`delta_px > 0` = 内容向上滚（看更旧，offset 增大）。
    /// 连滚时在当前 **target** 上叠加，并从当前显示值重新起跑 180ms 补间。
    pub fn scroll_by(&mut self, delta_px: f32, max_px: f32, now: Instant) {
        let max = max_px.max(0.0);
        let current = self.scroll_at(now);
        let base = if self.scroll_t0.is_some() {
            self.scroll_to
        } else {
            current
        };
        let target = (base + delta_px).clamp(0.0, max);
        self.scroll = current;
        if (target - current).abs() <= 0.01 {
            self.scroll = target;
            self.scroll_from = target;
            self.scroll_to = target;
            self.scroll_t0 = None;
            return;
        }
        self.scroll_from = current;
        self.scroll_to = target;
        self.scroll_t0 = Some(now);
    }

    /// 立刻跳到某个滚动值（probe / 取证用：不播补间，出图就是目标位置）。
    pub fn jump_scroll(&mut self, offset_px: f32, max_px: f32) {
        let v = offset_px.clamp(0.0, max_px.max(0.0));
        self.scroll = v;
        self.scroll_from = v;
        self.scroll_to = v;
        self.scroll_t0 = None;
    }

    pub fn reset_scroll(&mut self) {
        self.scroll = 0.0;
        self.scroll_from = 0.0;
        self.scroll_to = 0.0;
        self.scroll_t0 = None;
    }

    /// 本地删除一条（live 在 `Hit::Remove` 里调用）：
    /// 立刻把 id 放进 suppress（快照过滤掉它、命中不认它），同时开始 180ms 塌缩退出。
    pub fn mark_removed(&mut self, id: u32, now: Instant) {
        if self.suppressed.contains_key(&id) {
            return;
        }
        self.suppressed.insert(id, now);
        self.enter.remove(&id);
        if let Some(item) = self.last_items.iter().find(|i| i.id == id).cloned() {
            self.exiting.insert(id, ExitRow { item, t0: now });
        } else {
            // 还没 observe 过这一版：先记下，下次 observe 从快照里补条目数据
            self.exit_pending.insert(id, now);
        }
    }

    /// 清除按钮「清除 ↔ 确认」状态变化（live 在 3s 确认窗开关时调用；paint 也会按
    /// `Options.clear_armed` 自动同步）。两段文字同 rect 交叉淡化 120ms。
    pub fn set_clear_armed(&mut self, armed: bool, now: Instant) {
        if self.clear_armed == armed {
            return;
        }
        if !self.initialized {
            // 启动首帧就是确认态：直接落位，不播交叉
            self.clear_armed = armed;
            self.clear_from = if armed { 1.0 } else { 0.0 };
            self.clear_t0 = None;
            return;
        }
        self.clear_from = self.clear_swap_alpha(now);
        self.clear_armed = armed;
        self.clear_t0 = Some(now);
    }

    /// 清除 → 确认的交叉进度：0 = 全「清除」，1 = 全「确认」。
    pub fn clear_swap_alpha(&self, now: Instant) -> f32 {
        let target = if self.clear_armed { 1.0 } else { 0.0 };
        match self.clear_t0 {
            Some(t0) => {
                let u = linear_progress(now, t0, CLEAR_SWAP_MS);
                self.clear_from + (target - self.clear_from) * ease_out_quart(u)
            }
            None => target,
        }
    }

    /// 四态内容淡入进度（只在非正常态用；正常态的行动画走 enter/exit）。
    pub fn state_alpha(&self, now: Instant) -> f32 {
        match self.state_t0 {
            Some(t0) => ease_out_quart(linear_progress(now, t0, STATE_FADE_MS)),
            None => 1.0,
        }
    }

    /// 读 hover 透明度（0..1）。paint 每帧先算当前悬停行再更新。
    pub fn hover_alpha(&self, id: u32, now: Instant) -> f32 {
        self.hover
            .get(&id)
            .map(|h| h.value(now).clamp(0.0, 1.0))
            .unwrap_or(0.0)
    }

    /// 更新 hover 目标：新行淡入、其余行淡出（120ms，零位移，可多行同时补）。
    pub fn update_hover(&mut self, target: Option<u32>, now: Instant) {
        for (&id, h) in self.hover.iter_mut() {
            if Some(id) != target && h.to != 0.0 {
                let from = h.value(now);
                *h = HoverTween { from, to: 0.0, t0: now };
            }
        }
        if let Some(id) = target {
            match self.hover.get_mut(&id) {
                Some(h) => {
                    if h.to != 1.0 {
                        let from = h.value(now);
                        *h = HoverTween { from, to: 1.0, t0: now };
                    }
                }
                None => {
                    self.hover.insert(
                        id,
                        HoverTween { from: 0.0, to: 1.0, t0: now },
                    );
                }
            }
        }
    }

    /// 把过期补间清掉，并结算滚动（paint 每帧调一次；tree_hook 只读不调）。
    fn prune(&mut self, now: Instant) {
        if let Some(t0) = self.scroll_t0 {
            if linear_progress(now, t0, SCROLL_MS) >= 1.0 {
                self.scroll = self.scroll_to;
                self.scroll_t0 = None;
            }
        }
        self.enter.retain(|_, t0| ms_since(now, *t0) < ENTER_ROW_MS);
        self.group_enter
            .retain(|_, t0| ms_since(now, *t0) < ENTER_GROUP_MS);
        self.exiting.retain(|_, e| ms_since(now, e.t0) < EXIT_MS);
        self.exit_pending.retain(|_, t0| ms_since(now, *t0) < SUPPRESS_MS);
        self.hover.retain(|_, h| h.to > 0.0 || h.active(now));
        if self
            .clear_t0
            .is_some_and(|t0| ms_since(now, t0) >= CLEAR_SWAP_MS)
        {
            self.clear_from = if self.clear_armed { 1.0 } else { 0.0 };
            self.clear_t0 = None;
        }
        if self
            .state_t0
            .is_some_and(|t0| ms_since(now, t0) >= STATE_FADE_MS)
        {
            self.state_t0 = None;
        }
    }

    /// 内容变短（删除/清除）时把补间目标夹回合法区间，避免一直追一个够不到的值。
    fn clamp_scroll(&mut self, max_px: f32) {
        let max = max_px.max(0.0);
        self.scroll_to = self.scroll_to.clamp(0.0, max);
        self.scroll_from = self.scroll_from.clamp(0.0, max);
        if self.scroll > max {
            self.scroll = max;
        }
    }
}

impl Default for Motion {
    fn default() -> Self {
        Self::new()
    }
}

/// 分组 key：口径与 `group_items_limit` 完全一致（aumid 优先，空则按应用名）。
fn group_key(item: &NotifyItem) -> String {
    if item.aumid.trim().is_empty() {
        format!("name:{}", item.app)
    } else {
        format!("aumid:{}", item.aumid)
    }
}

// ── 连续滚动的几何 ────────────────────────────────────────────────────────

/// 视口高度（正常态分割线以下的剩余高度）。与 root 的列布局口径一致：
/// 内高 − 抬头 − 分割线 − 两处 head_gap。非正常态没有视口，返回 0。
pub fn viewport_height(t: &Tokens) -> f32 {
    (t.h - t.pad_t - t.pad_b - t.header_lh - t.rule_h - 2.0 * t.head_gap).max(0.0)
}

/// 内容总高（所有组按 `group_items_limit(ALL_GROUPS, ALL_ROWS, per_app)` 排布）。
///
/// 注意用**名义** item_h / group_gh：入场/退场的瞬时高度不让滚动 clamp 跟着抖，
/// 动画期间多出来的空白最多 180ms 且都在视口外。
pub fn content_height(t: &Tokens, snap: Option<&NotifySnapshot>, opts: Options) -> f32 {
    let Some(snap) = snap else { return 0.0 };
    let all = group_items_limit(&snap.items, ALL_GROUPS, ALL_ROWS, opts.per_app);
    let mut h = 0.0f32;
    for (i, vg) in all.iter().enumerate() {
        if i > 0 {
            h += t.group_gap;
        }
        let n = vg.items.len();
        h += t.group_gh
            + t.item_gap
            + n as f32 * t.item_h
            + n.saturating_sub(1) as f32 * t.item_gap;
    }
    h
}

/// 当前能滚动的最大距离（DIP）：`content_h − viewport_h`，不溢出门 0。
/// `motion` 预留给后续「动画高度也参与 clamp」的口径；本轮按名义高度算。
pub fn max_scroll_px(
    t: &Tokens,
    snap: Option<&NotifySnapshot>,
    opts: Options,
    _motion: &Motion,
) -> f32 {
    if display_state(snap) != CardState::Normal {
        return 0.0;
    }
    (content_height(t, snap, opts) - viewport_height(t)).max(0.0)
}

/// 滚轮一格的内容滚动距离 = 2 × item_h（240 宽 = 76px）。
pub fn wheel_step_px(t: &Tokens) -> f32 {
    WHEEL_LINES * t.item_h
}

/// 显示用滚动值：补间值再夹到 [0, max]（内容变短时立刻贴底，不画到空白里）。
fn display_scroll(motion: &Motion, now: Instant, max_scroll: f32) -> f32 {
    motion.scroll_at(now).clamp(0.0, max_scroll.max(0.0))
}

// ── 视口内的行模型（含入场/退场的瞬时状态） ───────────────────────────────

/// 行数据来源：快照下标 / 正在退出的条目（数据存在 Motion 里）。
#[derive(Clone, Copy)]
enum RowRef {
    Snap(usize),
    Exit(u32),
}

struct RowView {
    r: RowRef,
    /// 条目 id（hover / 命中都按它对齐）
    uid: u32,
    /// 入场起始时刻（180ms）
    enter: Option<Instant>,
    /// 退出起始时刻（180ms 塌缩）
    exit: Option<Instant>,
}

/// 视口里的一组：组头 + 若干行（组顺序 / 行配额与旧 `group_items_limit` 同口径）。
struct GroupView {
    key: String,
    app: String,
    aumid: String,
    newest_ms: u64,
    total: usize,
    rows: Vec<RowView>,
}

fn resolve_row<'a>(
    snap: Option<&'a NotifySnapshot>,
    motion: &'a Motion,
    r: RowRef,
) -> Option<&'a NotifyItem> {
    match r {
        RowRef::Snap(i) => snap?.items.get(i),
        RowRef::Exit(id) => motion.exiting.get(&id).map(|e| &e.item),
    }
}

/// 生成视口内的组/行模型：快照（suppress 过滤）按时间序排，退出的行插回原时间位置；
/// 再按 `group_items_limit` 的「先每组 1 条、再轮流补到 per_app 条」分配行配额。
fn build_groups(
    snap: Option<&NotifySnapshot>,
    opts: Options,
    motion: &Motion,
    now: Instant,
) -> Vec<GroupView> {
    let Some(snap) = snap else { return Vec::new() };

    struct Entry {
        uid: u32,
        created: u64,
        r: RowRef,
        enter: Option<Instant>,
        exit: Option<Instant>,
    }

    let mut list: Vec<Entry> = Vec::new();
    for (idx, it) in snap.items.iter().enumerate() {
        if motion.suppressed.contains_key(&it.id) {
            continue;
        }
        list.push(Entry {
            uid: it.id,
            created: it.created_ms,
            r: RowRef::Snap(idx),
            enter: motion.enter.get(&it.id).copied(),
            exit: None,
        });
    }

    // 退出中的行按 created_ms 插回原位置：这一行原地塌缩，下面的行连续上移。
    let mut exits: Vec<(u32, &ExitRow)> = motion
        .exiting
        .iter()
        .filter(|(_, e)| ms_since(now, e.t0) < EXIT_MS)
        .map(|(&id, e)| (id, e))
        .collect();
    exits.sort_by(|a, b| b.1.item.created_ms.cmp(&a.1.item.created_ms));
    for (id, e) in exits {
        if list.iter().any(|x| x.uid == id) {
            continue;
        }
        let ent = Entry {
            uid: id,
            created: e.item.created_ms,
            r: RowRef::Exit(id),
            enter: None,
            exit: Some(e.t0),
        };
        let pos = if ent.created == 0 {
            None
        } else {
            list.iter().position(|x| x.created < ent.created)
        };
        match pos {
            Some(p) => list.insert(p, ent),
            None => list.push(ent),
        }
    }

    struct Acc {
        key: String,
        app: String,
        aumid: String,
        newest_ms: u64,
        total: usize,
        rows: Vec<RowView>,
    }

    let mut order: Vec<String> = Vec::new();
    let mut accs: HashMap<String, Acc> = HashMap::new();
    for e in list {
        let Some(item) = resolve_row(Some(snap), motion, e.r) else {
            continue;
        };
        let key = group_key(item);
        let row = RowView {
            r: e.r,
            uid: e.uid,
            enter: e.enter,
            exit: e.exit,
        };
        match accs.get_mut(&key) {
            Some(acc) => {
                acc.total += 1;
                acc.rows.push(row);
            }
            None => {
                order.push(key.clone());
                accs.insert(
                    key.clone(),
                    Acc {
                        key,
                        app: if item.app.trim().is_empty() {
                            "其他".to_owned()
                        } else {
                            item.app.clone()
                        },
                        aumid: item.aumid.clone(),
                        newest_ms: item.created_ms,
                        total: 1,
                        rows: vec![row],
                    },
                );
            }
        }
    }

    let all: Vec<Acc> = order.into_iter().filter_map(|k| accs.remove(&k)).collect();
    let n = all.len().min(ALL_GROUPS);
    let per_app = opts.per_app.clamp(1, 3);
    let mut take = vec![0usize; n];
    let mut budget = ALL_ROWS;
    for slot in take.iter_mut() {
        if budget == 0 {
            break;
        }
        *slot = 1;
        budget -= 1;
    }
    while budget > 0 {
        let mut progressed = false;
        for g in 0..n {
            if budget == 0 {
                break;
            }
            if take[g] < all[g].rows.len().min(per_app) {
                take[g] += 1;
                budget -= 1;
                progressed = true;
            }
        }
        if !progressed {
            break;
        }
    }

    all.into_iter()
        .take(n)
        .zip(take)
        .filter(|(_, take)| *take > 0)
        .map(|(mut acc, take)| {
            acc.rows.truncate(take);
            GroupView {
                key: acc.key,
                app: acc.app,
                aumid: acc.aumid,
                newest_ms: acc.newest_ms,
                total: acc.total,
                rows: acc.rows,
            }
        })
        .collect()
}

/// 一行的瞬时几何：高度 / 透明度 / 位移 / 收尾时的负底边距。
/// 入场：0 → item_h、-4 → 0；退出：item_h → 0，同时底边距 0 → -item_gap ——
/// 这样最后一帧的「行高 + 间距」和被删掉后的布局严格对齐（不会在 180ms 收尾跳 3px），
/// 而且 `measure()` 对容器只看子节点高度，底边距能让下一组的 y 跟着连续上移。
#[derive(Clone, Copy)]
struct RowAnim {
    h: f32,
    alpha: f32,
    dy: f32,
    mar_b: f32,
}

fn row_anim(t: &Tokens, row: &RowView, now: Instant) -> RowAnim {
    if let Some(t0) = row.exit {
        let e = 1.0 - ease_out_quart(linear_progress(now, t0, EXIT_MS));
        return RowAnim {
            h: t.item_h * e,
            alpha: e,
            dy: 0.0,
            mar_b: -t.item_gap * (1.0 - e),
        };
    }
    if let Some(t0) = row.enter {
        let e = ease_out_quart(linear_progress(now, t0, ENTER_ROW_MS));
        if e < 1.0 {
            return RowAnim {
                h: t.item_h * e,
                alpha: e,
                dy: -ENTER_DY * (1.0 - e),
                mar_b: 0.0,
            };
        }
    }
    RowAnim {
        h: t.item_h,
        alpha: 1.0,
        dy: 0.0,
        mar_b: 0.0,
    }
}

fn group_enter_alpha(motion: &Motion, key: &str, now: Instant) -> f32 {
    match motion.group_enter.get(key) {
        Some(&t0) => ease_out_quart(linear_progress(now, t0, ENTER_GROUP_MS)),
        None => 1.0,
    }
}

fn alpha_color(c: D2D1_COLOR_F, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { a: c.a * a, ..c }
}

pub fn paint(
    g: &Gfx,
    te: &TextEngine,
    pal: &Palette,
    t: &Tokens,
    snap: Option<&NotifySnapshot>,
    icons: &HashMap<String, ID2D1Bitmap1>,
    opts: Options,
    motion: &mut Motion,
    now: Instant,
    glass: Option<(&ID2D1Bitmap1, D2D_RECT_F)>,
) -> Result<()> {
    let state = display_state(snap);
    // 数据 diff / 四态 / suppress 收敛都在这里收口（live 提前调用 observe 也幂等）
    motion.observe(snap, state, now);
    motion.set_clear_armed(opts.clear_armed, now);
    motion.prune(now);

    let max_scroll = max_scroll_px(t, snap, opts, motion);
    motion.clamp_scroll(max_scroll);
    let scroll = display_scroll(motion, now, max_scroll);
    motion.scroll = scroll;

    let groups = build_groups(snap, opts, motion, now);
    let content_h = content_height(t, snap, opts);
    let tr = tree(t, snap, state, &groups, content_h, opts, motion, now);
    let mut m: &TextEngine = te;
    let placed = layout(&tr, (t.w, t.h), &mut m);

    // hover：只在视口内、按「屏幕 y + scroll」反算行；Motion 里补 120ms 透明度
    let hover = hovered_row_uid(&placed, &groups, g.hover, scroll);
    motion.update_hover(hover, now);

    let ctx = PaintCtx {
        snap,
        icons,
        groups: &groups,
        motion,
        opts,
        now,
        clear_swap: motion.clear_swap_alpha(now),
        state_fade: if state == CardState::Normal {
            1.0
        } else {
            motion.state_alpha(now)
        },
        scroll,
        max_scroll,
    };

    unsafe {
        g.begin()?;
        g.push_clip()?;
        if let Some((bmp, rect)) = glass {
            g.d2d.DrawBitmap(
                bmp,
                Some(&rect),
                1.0,
                D2D1_INTERPOLATION_MODE_LINEAR,
                None,
                None,
            );
        }
        let full = D2D_RECT_F {
            left: 0.0,
            top: 0.0,
            right: t.w,
            bottom: t.h,
        };
        set_color(g, pal.veil)?;
        fill_round(g, full, t.radius)?;
        set_color(
            g,
            if g.hover.is_some() {
                pal.fg30
            } else {
                pal.border
            },
        )?;
        stroke_round(
            g,
            D2D_RECT_F {
                left: 0.5,
                top: 0.5,
                right: t.w - 0.5,
                bottom: t.h - 0.5,
            },
            (t.radius - 0.5).max(0.0),
            1.0,
        )?;
        paint_tree(g, te, pal, t, &ctx, &placed)?;
        g.pop_clip();
        g.end()?;
        Ok(())
    }
}

// ── 节点 id ────────────────────────────────────────────────────────────────
// 组头：9100 + g*10 + {0 行,1 应用块,2 名称,3 药丸,4 药丸字,5 时间}
// 条目：10000 + g*100 + i*10 + {0 行,1 未读点,2 文本行,3 ×}
// 视口：9007（见上）；分割线 9006。
const ID_HEADER: u32 = 9001;
const ID_COUNT: u32 = 9002;
const ID_STATE: u32 = 9003;
const ID_HINT: u32 = 9004;
const ID_CLEAR: u32 = 9005;
/// 正常态抬头下的 1px 分割线（不参与点击：hit_at 只认 9002/9003/9004/条目）
const ID_DIVIDER: u32 = 9006;

fn id_group(g: usize) -> u32 {
    9100 + g as u32 * 10
}
fn id_item(g: usize, i: usize) -> u32 {
    10_000 + g as u32 * 100 + i as u32 * 10
}
fn group_of(id: u32) -> Option<usize> {
    // 连续滚动后组数上限是 ALL_GROUPS（不再是一屏 3 组）
    if (9100..9100 + ALL_GROUPS as u32 * 10).contains(&id) {
        Some(((id - 9100) / 10) as usize)
    } else {
        None
    }
}
fn item_of(id: u32) -> Option<(usize, usize)> {
    if (10_000..10_000 + ALL_GROUPS as u32 * 100).contains(&id) {
        let x = id - 10_000;
        Some(((x / 100) as usize, ((x % 100) / 10) as usize))
    } else {
        None
    }
}

fn text_node(s: &str, size: f32, lh: f32, weight: u16, id: u32) -> Node {
    text_node_aligned(s, size, lh, weight, id, 0)
}

fn text_node_aligned(s: &str, size: f32, lh: f32, weight: u16, id: u32, align: u8) -> Node {
    Node::text(TextSpec {
        s: s.to_string(),
        size,
        line_h: lh,
        weight,
        wrap: false,
        align,
        tabular: true,
        spacing_em: 0.0,
    })
    .id(id)
}

/// 相对时间的基准（毫秒）：每次 paint 取一次，和旧实现 `now_ms` 参数同口径。
fn unix_ms_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn tree(
    t: &Tokens,
    snap: Option<&NotifySnapshot>,
    state: CardState,
    groups: &[GroupView],
    content_h: f32,
    opts: Options,
    motion: &Motion,
    now: Instant,
) -> Node {
    let root = Node::new(Dir::Column).style(move |s| {
        s.size(Dim::Px(t.w), Dim::Px(t.h))
            .pad([t.pad_t, t.pad_r, t.pad_b, t.pad_l])
            .gap(t.head_gap)
    });

    let header = Node::new(Dir::Row)
        .id(9000)
        .style(move |s| {
            s.size(Dim::Pct(1.0), Dim::Px(t.header_lh))
                .align(Align::Center)
                .gap(0.25 * t.r)
        })
        .child(
            text_node("通知", t.header_fs, t.header_lh, 600, ID_HEADER)
                .style(|s| s.grow(1.0).basis(0.0)),
        );

    let header = if state == CardState::Normal {
        let total = snap.map(|s| s.items.len()).unwrap_or(0);
        let unread = snap.map(|s| s.unread_count()).unwrap_or(0);
        // 「全部已读」不是 Windows 的真实状态（启动基线也会把旧通知标已读）→ 没有新通知时显示总数
        let text = if unread > 0 {
            format!("{unread} 条未读")
        } else {
            format!("共 {total} 条")
        };
        // B 案：不再有「 · 1/N」页标（滚轮是连续滚动，没有页的概念）
        header
            .child(text_node(
                &text,
                t.count_fs,
                t.count_lh,
                400,
                ID_COUNT,
            ))
            .child(text_node(
                if opts.clear_armed { "确认" } else { "清除" },
                t.count_fs,
                t.count_lh,
                400,
                ID_CLEAR,
            ))
    } else {
        header
    };

    let root = root.child(header);

    if state != CardState::Normal {
        let (main, hint) = state_texts(state);
        let mut center = Node::new(Dir::Column)
            .style(move |s| {
                s.grow(1.0)
                    .basis(0.0)
                    .align(Align::Center)
                    .justify(Justify::Center)
                    .gap(t.state_gap)
            })
            .child(text_node(
                main,
                t.state_fs,
                t.state_lh,
                500,
                ID_STATE,
            ));
        if !hint.is_empty() {
            center = center.child(text_node(hint, t.hint_fs, t.hint_lh, 400, ID_HINT));
        }
        return root.child(center);
    }

    // 正常态：抬头与消息列表之间加一条 1px 分割线（pal.border = fg 10%，浅色主题自动黑 10%）。
    // 节点高度吃 Tokens::rule_h；capacity() 与 viewport_height() 都同步扣掉它和两侧各一个 head_gap。
    let divider = Node::new(Dir::Row)
        .id(ID_DIVIDER)
        .style(move |s| s.size(Dim::Pct(1.0), Dim::Px(t.rule_h)));
    let root = root.child(divider);

    // 视口：100% 宽 + grow 占满分割线以下；所有组竖排在里面，paint 时整体平移。
    let mut groups_col = Node::new(Dir::Column).style(move |s| {
        s.size(Dim::Pct(1.0), Dim::Px(content_h))
            .align(Align::Stretch)
            .justify(Justify::Start)
            .gap(t.group_gap)
    });

    let now_ms = unix_ms_now();
    for (g, vg) in groups.iter().enumerate() {
        let gid = id_group(g);
        // 新组组头：高度 group_gh*e + 透明度 e（120ms）；静止时 e=1，和旧布局完全一致。
        let ge = group_enter_alpha(motion, &vg.key, now);
        let header_h = t.group_gh * ge;
        let mut header_row = Node::new(Dir::Row)
            .id(gid)
            .style(move |s| {
                s.size(Dim::Pct(1.0), Dim::Px(header_h))
                    .align(Align::Center)
                    .gap(t.app_gap)
            })
            .child(
                Node::new(Dir::Row)
                    .id(gid + 1)
                    .style(move |s| s.size(Dim::Px(t.tile), Dim::Px(t.tile))),
            )
            .child(
                text_node(&vg.app, t.app_fs, t.app_lh, 500, gid + 2)
                    .style(|s| s.grow(1.0).basis(0.0)),
            );

        let badge = Node::new(Dir::Row)
            .id(gid + 3)
            .style(move |s| {
                s.size(Dim::Auto, Dim::Px(t.badge_h))
                    .pad([0.0, t.badge_pad, 0.0, t.badge_pad])
                    .align(Align::Center)
                    .justify(Justify::Center)
            })
            .child(text_node(
                &vg.total.to_string(),
                t.badge_fs,
                t.badge_lh,
                400,
                gid + 4,
            ));
        header_row = header_row.child(badge);

        let time = relative_time(vg.newest_ms, now_ms);
        header_row = header_row.child(text_node_aligned(
            &time,
            t.time_fs,
            t.time_lh,
            400,
            gid + 5,
            1,
        ));

        let mut group_col = Node::new(Dir::Column)
            .style(move |s| s.gap(t.item_gap).align(Align::Stretch))
            .child(header_row);

        for (i, row) in vg.rows.iter().enumerate() {
            let iid = id_item(g, i);
            // 入场 item_h*e、退出 item_h→0（底边距 0→-item_gap 吃掉行间 gap，收尾不跳）。
            // 行高同步写进子节点：layout 的 `measure()` 对容器只看子节点，子节点跟着缩，
            // 下一组的 y 才会跟「其余行连续上移」一起动。
            let anim = row_anim(t, row, now);
            let row_h = anim.h.max(0.0);
            let mar_b = anim.mar_b;
            let dot = t.dot.min(row_h);
            let node = Node::new(Dir::Row)
                .id(iid)
                .style(move |s| {
                    s.size(Dim::Pct(1.0), Dim::Px(row_h))
                        .mar([0.0, 0.0, mar_b, 0.0])
                        .pad([0.0, 0.0, 0.0, t.item_indent])
                        .align(Align::Center)
                        .gap(0.25 * t.r)
                })
                .child(
                    Node::new(Dir::Row)
                        .id(iid + 1)
                        .style(move |s| s.size(Dim::Px(dot), Dim::Px(dot))),
                )
                .child(
                    Node::new(Dir::Row)
                        .id(iid + 2)
                        .style(move |s| s.grow(1.0).size(Dim::Auto, Dim::Px(row_h))),
                )
                .child(
                    Node::text(TextSpec {
                        s: "×".into(),
                        size: 0.625 * t.r,
                        line_h: t.item_h,
                        weight: 400,
                        wrap: false,
                        align: 2,
                        tabular: false,
                        spacing_em: 0.0,
                    })
                    .id(iid + 3)
                    .style(move |s| s.size(Dim::Px(0.875 * t.r), Dim::Px(row_h))),
                );
            group_col = group_col.child(node);
        }

        groups_col = groups_col.child(group_col);
    }

    let viewport = Node::new(Dir::Column)
        .id(ID_VIEWPORT)
        .style(|s| s.size(Dim::Pct(1.0), Dim::Auto).grow(1.0).basis(0.0))
        .child(groups_col);
    root.child(viewport)
}

/// 绘制上下文：一帧内不变的量 + 已求值的动画中间量；递归只读。
struct PaintCtx<'a> {
    snap: Option<&'a NotifySnapshot>,
    icons: &'a HashMap<String, ID2D1Bitmap1>,
    groups: &'a [GroupView],
    motion: &'a Motion,
    opts: Options,
    now: Instant,
    /// 清除 → 确认的交叉进度：0 = 全「清除」，1 = 全「确认」
    clear_swap: f32,
    /// 空 / 授权 / 不可用态内容淡入
    state_fade: f32,
    /// 视口内容显示滚动值（DIP）
    scroll: f32,
    /// 内容高出视口的部分（> 0 才画滚动条）
    max_scroll: f32,
}

fn clip_rect(r: D2D_RECT_F) -> D2D_RECT_F {
    D2D_RECT_F {
        left: r.left,
        top: r.top,
        right: r.right.max(r.left),
        bottom: r.bottom.max(r.top),
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_text(
    g: &Gfx,
    te: &TextEngine,
    spec: &TextSpec,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    mode: TextMode,
    color: D2D1_COLOR_F,
    shadow: D2D1_COLOR_F,
) -> Result<()> {
    draw_shadowed(g, spec, x, y, w, h, mode, color, shadow, 0.8335, 0.833, te)
}

fn paint_children(
    g: &Gfx,
    te: &TextEngine,
    pal: &Palette,
    t: &Tokens,
    ctx: &PaintCtx,
    pl: &Placed,
) -> Result<()> {
    for c in &pl.children {
        paint_tree(g, te, pal, t, ctx, c)?;
    }
    Ok(())
}

fn paint_tree(
    g: &Gfx,
    te: &TextEngine,
    pal: &Palette,
    t: &Tokens,
    ctx: &PaintCtx,
    pl: &Placed,
) -> Result<()> {
    // ── 视口：矩形 clip + SetTransform(0,-scroll) 画子树（静止 scroll=0 且不溢出时完全跳过）──
    if pl.id == ID_VIEWPORT {
        return paint_viewport(g, te, pal, t, ctx, pl);
    }

    // ── 新组组头：整行裁在自身矩形内、120ms 淡入（图标/药丸/时间一起，不做单独动效）──
    if let Some(gidx) = group_of(pl.id) {
        if pl.id == id_group(gidx) {
            let e = ctx
                .groups
                .get(gidx)
                .map(|vg| group_enter_alpha(ctx.motion, &vg.key, ctx.now))
                .unwrap_or(1.0);
            if e < 0.9999 {
                let r = clip_rect(rect(pl));
                if e <= 0.001 || (r.bottom - r.top) <= 0.1 {
                    return Ok(());
                }
                g.push_clip_round(r, 0.0)?;
                // 用大 contentBounds 的层（paint::push_opacity）：滚动变换下 Gfx 自带的
                // push_opacity 只有卡面 bounds，会把滚到卡外的行裁掉。
                let layer = minicard_draw::paint::push_opacity(g, e)?;
                let out = paint_children(g, te, pal, t, ctx, pl);
                drop(layer);
                g.pop_clip_round();
                return out;
            }
        }
    }

    // ── 条目行：入场/退出的行高 + 淡入淡出 + 上浮；静止态不建层（逐像素与旧版一致）──
    if let Some((gidx, iidx)) = item_of(pl.id) {
        if pl.id == id_item(gidx, iidx) {
            return paint_row(g, te, pal, t, ctx, pl, gidx, iidx);
        }
    }

    // ── 组头的子节点：应用块 / 药丸 ──
    if let Some(gidx) = group_of(pl.id) {
        let vg = ctx.groups.get(gidx);
        match pl.id - id_group(gidx) {
            0 => {
                // 行本身：不画；子节点递归
            }
            1 => {
                if let Some(vg) = vg {
                    paint_app_tile(g, te, pal, t, pl, vg, ctx.icons.get(&vg.aumid))?;
                }
                return Ok(());
            }
            3 => {
                // 药丸底：比 fg 7% 的 badge_bg 再亮一档，240×180 上才看得出是个计数
                set_color(g, pal.track12)?;
                fill_round(g, rect(pl), pl.rect.h / 2.0)?;
            }
            _ => {}
        }
    }

    // ── 条目的子节点：未读点 / 文本行 / ×（× 跟 hover 补间一起淡）──
    if let Some((gidx, iidx)) = item_of(pl.id) {
        let row = ctx.groups.get(gidx).and_then(|vg| vg.rows.get(iidx));
        let item = row.and_then(|r| resolve_row(ctx.snap, ctx.motion, r.r));
        let hover = row
            .map(|r| ctx.motion.hover_alpha(r.uid, ctx.now))
            .unwrap_or(0.0);
        match pl.id - id_item(gidx, iidx) {
            1 => {
                if item.map(|i| i.unread).unwrap_or(false) {
                    set_color(g, pal.accent)?;
                    fill_round(g, rect(pl), pl.rect.w / 2.0)?;
                }
                return Ok(());
            }
            2 => {
                if let Some(item) = item {
                    paint_item_content(g, te, pal, t, pl, item, ctx.opts.show_body)?;
                }
                return Ok(());
            }
            3 => {
                // ×：只在悬停到这一行时出现（默认态保持 03 稿的干净观感）
                if hover > 0.001 {
                    if let Some(spec) = pl.text() {
                        draw_text(
                            g,
                            te,
                            spec,
                            pl.rect.x,
                            pl.rect.y,
                            pl.rect.w,
                            spec.line_h + 6.0,
                            TextMode::Clip,
                            alpha_color(pal.muted, hover),
                            alpha_color(text_shadow(pal), hover),
                        )?;
                    }
                }
                return Ok(());
            }
            _ => {}
        }
    }

    // ── 抬头分割线：1px 实线，颜色跟主题（深色白 10% / 浅色黑 10%）──
    if pl.id == ID_DIVIDER {
        set_color(g, pal.border)?;
        fill_rect(g, rect(pl))?;
        return Ok(());
    }

    // ── 清除 ↔ 确认：同一 rect 两段两字文字交叉 120ms（静止态只画目标那一段）──
    if pl.id == ID_CLEAR {
        if let Some(spec) = pl.text() {
            paint_clear_label(g, te, pal, ctx, pl, spec)?;
        }
        return Ok(());
    }

    if let Some(spec) = pl.text() {
        let mut color = match pl.id {
            ID_COUNT | ID_HINT => pal.muted,
            _ => {
                if let Some(gidx) = group_of(pl.id) {
                    if pl.id - id_group(gidx) >= 4 {
                        pal.muted
                    } else {
                        pal.fg
                    }
                } else {
                    pal.fg
                }
            }
        };
        // 空 / 授权 / 不可用态：内容 160ms 淡入（抬头「通知」不跟着淡）
        let mut alpha = 1.0;
        if matches!(pl.id, ID_STATE | ID_HINT) {
            alpha = ctx.state_fade;
            if alpha <= 0.001 {
                return Ok(());
            }
            color = alpha_color(color, alpha);
        }
        draw_text(
            g,
            te,
            spec,
            pl.rect.x,
            pl.rect.y,
            pl.rect.w,
            spec.line_h + 6.0,
            TextMode::Ellipsis,
            color,
            alpha_color(text_shadow(pal), alpha),
        )?;
    }

    paint_children(g, te, pal, t, ctx, pl)
}

/// 条目行：入场（h 0→item_h、-4→0、alpha 0→1，180ms）/ 退出（h item_h→0、alpha→0，180ms）。
/// 静止态不建 clip / opacity 层，走和旧版完全一样的绘制笔顺。
fn paint_row(
    g: &Gfx,
    te: &TextEngine,
    pal: &Palette,
    t: &Tokens,
    ctx: &PaintCtx,
    pl: &Placed,
    gidx: usize,
    iidx: usize,
) -> Result<()> {
    let Some(row) = ctx.groups.get(gidx).and_then(|vg| vg.rows.get(iidx)) else {
        return Ok(());
    };
    let anim = row_anim(t, row, ctx.now);
    let hover = ctx.motion.hover_alpha(row.uid, ctx.now);
    if anim.alpha <= 0.001 || anim.h <= 0.1 {
        return Ok(());
    }
    let animated =
        anim.alpha < 0.9999 || (anim.h - t.item_h).abs() > 0.001 || anim.dy.abs() > 0.001;
    if !animated {
        if hover > 0.001 {
            set_color(g, alpha_color(pal.fg_soft, hover))?;
            fill_round(g, rect(pl), 0.375 * t.r)?;
        }
        return paint_children(g, te, pal, t, ctx, pl);
    }

    let r = clip_rect(rect(pl));
    g.push_clip_round(r, 0.0)?;
    // 大 contentBounds 的不透明度层：视图滚动后行可能落在卡外，Gfx::push_opacity 的
    // 卡面 bounds 会把它裁掉。
    let layer = minicard_draw::paint::push_opacity(g, anim.alpha)?;
    let mut old = windows_numerics::Matrix3x2::default();
    let shifted = anim.dy.abs() > 0.001;
    if shifted {
        unsafe { g.d2d.GetTransform(&mut old) };
        let m = windows_numerics::Matrix3x2 {
            M11: 1.0,
            M12: 0.0,
            M21: 0.0,
            M22: 1.0,
            M31: 0.0,
            M32: anim.dy,
        } * old;
        unsafe { g.d2d.SetTransform(&m) };
    }
    let mut out = Ok(());
    if hover > 0.001 {
        out = set_color(g, alpha_color(pal.fg_soft, hover))
            .and_then(|_| fill_round(g, rect(pl), 0.375 * t.r));
    }
    if out.is_ok() {
        out = paint_children(g, te, pal, t, ctx, pl);
    }
    if shifted {
        unsafe { g.d2d.SetTransform(&old) };
    }
    drop(layer);
    g.pop_clip_round();
    out
}

/// 视口绘制：仅在内容溢出时做矩形 clip；scroll != 0 时在现有变换上叠 (0, -scroll)。
/// 不溢出 = 不建层、不平移（默认样张静止态与旧版逐像素相同）。
fn paint_viewport(
    g: &Gfx,
    te: &TextEngine,
    pal: &Palette,
    t: &Tokens,
    ctx: &PaintCtx,
    pl: &Placed,
) -> Result<()> {
    let vp = rect(pl);
    let overflow = ctx.max_scroll > 0.01;
    if !overflow {
        return paint_children(g, te, pal, t, ctx, pl);
    }

    g.push_clip_round(vp, 0.0)?;
    let mut old = windows_numerics::Matrix3x2::default();
    let shifted = ctx.scroll.abs() > 0.001;
    if shifted {
        unsafe { g.d2d.GetTransform(&mut old) };
        // 行向量口径（同 calendar 翻页）：先平移再乘现有基础变换（含 --ss 缩放）
        let m = windows_numerics::Matrix3x2 {
            M11: 1.0,
            M12: 0.0,
            M21: 0.0,
            M22: 1.0,
            M31: 0.0,
            M32: -ctx.scroll,
        } * old;
        unsafe { g.d2d.SetTransform(&m) };
    }
    let out = paint_children(g, te, pal, t, ctx, pl);
    if shifted {
        unsafe { g.d2d.SetTransform(&old) };
    }
    g.pop_clip_round();
    if out.is_err() {
        return out;
    }

    // 2px 细滚动条：浮在卡片**右内边距**里（离开内容右缘，不压组头时间/条目文字）。
    // 轨道暗（fg 6%）、拇指亮（fg 24%）——旧版反了，视觉上像一条通顶亮线。
    if let Some(bar) = scrollbar_geometry(vp, t.w, ctx.max_scroll, ctx.scroll) {
        set_color(g, pal.fg_soft)?;
        fill_round(g, bar.track, 1.0)?;
        set_color(g, pal.badge_border)?;
        fill_round(g, bar.thumb, 1.0)?;
    }
    out
}

struct Scrollbar {
    track: D2D_RECT_F,
    thumb: D2D_RECT_F,
}

/// 滚动条几何：2px 宽，浮在卡片**右内边距**里（离卡片右缘 6px、离内容右缘 ≥4px），
/// 这样不会压在组头右侧的时间文字/药丸上；轨道上下各缩 1px 做圆头。
/// 无溢出返回 None（默认样张 3 组 4 条不溢出 → 不出滚动条）；
/// 窄到内边距里放不下也返回 None（宁可不出条，也不压内容）。
fn scrollbar_geometry(
    viewport: D2D_RECT_F,
    card_w: f32,
    max_scroll: f32,
    scroll: f32,
) -> Option<Scrollbar> {
    if max_scroll <= 0.5 {
        return None;
    }
    let viewport_h = viewport.bottom - viewport.top;
    let content_h = viewport_h + max_scroll;
    if viewport_h <= 8.0 || content_h <= 0.0 {
        return None;
    }
    const BAR_W: f32 = 2.0;
    // 条右缘离卡片右缘的距离（卡内边距 13px，条浮在留白里）
    const CARD_INSET: f32 = 6.0;
    let right = card_w - CARD_INSET;
    let left = right - BAR_W;
    // 必须完全落在「内容右缘 … 卡片描边」之间
    if left <= viewport.right + 1.0 || right >= card_w - 1.0 {
        return None;
    }
    let top = viewport.top + 1.0;
    let bottom = viewport.bottom - 1.0;
    let track_h = (bottom - top).max(1.0);
    let min_thumb = 12.0f32.min(track_h);
    let thumb_h = (track_h * viewport_h / content_h).clamp(min_thumb, track_h);
    let scroll = scroll.clamp(0.0, max_scroll);
    let y = top + (track_h - thumb_h) * (scroll / max_scroll);
    Some(Scrollbar {
        track: D2D_RECT_F {
            left,
            top,
            right,
            bottom,
        },
        thumb: D2D_RECT_F {
            left,
            top: y,
            right,
            bottom: y + thumb_h,
        },
    })
}

/// 清除 ↔ 确认的交叉：目标两字按进度淡入、另一段在同一个 rect 里淡出。
/// 两段都是两字宽（Fake 与 MiSans 下同宽），换字瞬间不挪位。
///
// TODO(0.98): 清除按钮的按压缩放（scale 0.98）需要新增 Press 事件（按下/抬起），
// 本轮用户明确不做，只做「清除 → 确认」的文字交叉。
fn paint_clear_label(
    g: &Gfx,
    te: &TextEngine,
    pal: &Palette,
    ctx: &PaintCtx,
    pl: &Placed,
    spec: &TextSpec,
) -> Result<()> {
    let target_a = if ctx.opts.clear_armed {
        ctx.clear_swap
    } else {
        1.0 - ctx.clear_swap
    };
    if target_a > 0.004 {
        let color = if ctx.opts.clear_armed {
            pal.accent
        } else {
            pal.muted
        };
        draw_text(
            g,
            te,
            spec,
            pl.rect.x,
            pl.rect.y,
            pl.rect.w,
            spec.line_h + 6.0,
            TextMode::Ellipsis,
            alpha_color(color, target_a),
            alpha_color(text_shadow(pal), target_a),
        )?;
    }
    let ghost_a = 1.0 - target_a;
    if ghost_a > 0.004 {
        let ghost = if ctx.opts.clear_armed { "清除" } else { "确认" };
        let mut ghost_spec = spec.clone();
        ghost_spec.s = ghost.to_owned();
        let color = if ghost == "确认" {
            pal.accent
        } else {
            pal.muted
        };
        draw_text(
            g,
            te,
            &ghost_spec,
            pl.rect.x,
            pl.rect.y,
            pl.rect.w,
            spec.line_h + 6.0,
            TextMode::Ellipsis,
            alpha_color(color, ghost_a),
            alpha_color(text_shadow(pal), ghost_a),
        )?;
    }
    Ok(())
}

fn find_placed_id(p: &Placed, id: u32) -> Option<&Placed> {
    if p.id == id {
        return Some(p);
    }
    p.children.iter().find_map(|c| find_placed_id(c, id))
}

/// 鼠标落在视口内哪一条的整行上（屏幕坐标先 +scroll 反算内容坐标）。
fn hovered_row_uid(
    placed: &Placed,
    groups: &[GroupView],
    hover: Option<(f32, f32)>,
    scroll: f32,
) -> Option<u32> {
    let (mx, my) = hover?;
    let vp = find_placed_id(placed, ID_VIEWPORT)?;
    if mx < vp.rect.x || mx >= vp.rect.right() || my < vp.rect.y || my >= vp.rect.bottom() {
        return None;
    }
    let (gidx, iidx) = hovered_row_in(placed, mx, my + scroll)?;
    let row = groups.get(gidx)?.rows.get(iidx)?;
    if row.exit.is_some() {
        return None;
    }
    Some(row.uid)
}

fn hovered_row_in(p: &Placed, x: f32, y: f32) -> Option<(usize, usize)> {
    if let Some((g, i)) = item_of(p.id) {
        if p.id == id_item(g, i) {
            let r = p.rect;
            if x >= r.x && y >= r.y && x < r.x + r.w && y < r.y + r.h {
                return Some((g, i));
            }
        }
    }
    for c in &p.children {
        if let Some(hit) = hovered_row_in(c, x, y) {
            return Some(hit);
        }
    }
    None
}

fn rect(pl: &Placed) -> D2D_RECT_F {
    D2D_RECT_F {
        left: pl.rect.x,
        top: pl.rect.y,
        right: pl.rect.x + pl.rect.w,
        bottom: pl.rect.y + pl.rect.h,
    }
}

fn paint_app_tile(
    g: &Gfx,
    te: &TextEngine,
    _pal: &Palette,
    t: &Tokens,
    pl: &Placed,
    vg: &GroupView,
    icon: Option<&ID2D1Bitmap1>,
) -> Result<()> {
    let r = rect(pl);
    if let Some(bmp) = icon {
        // 真图标：裁成圆角块，避免把方图标生硬压在卡面上
        g.push_clip_round(r, t.tile_radius)?;
        unsafe {
            g.d2d.DrawBitmap(
                bmp,
                Some(&r),
                1.0,
                D2D1_INTERPOLATION_MODE_LINEAR,
                None,
                None,
            );
        }
        g.pop_clip_round();
        return Ok(());
    }
    set_color(g, app_color(&vg.app))?;
    fill_round(g, r, t.tile_radius)?;
    let letter: String = vg.app.chars().next().map(|c| c.to_string()).unwrap_or_default();
    if letter.is_empty() {
        return Ok(());
    }
    let spec = TextSpec {
        s: letter,
        size: t.tile_fs,
        line_h: t.tile,
        weight: 600,
        wrap: false,
        align: 2,
        tabular: false,
        spacing_em: 0.0,
    };
    draw_shadowed(
        g,
        &spec,
        pl.rect.x,
        pl.rect.y,
        pl.rect.w,
        t.tile,
        TextMode::Clip,
        D2D1_COLOR_F {
            r: 1.0,
            g: 1.0,
            b: 1.0,
            a: 1.0,
        },
        text_shadow(_pal),        0.5,
        0.5,
        te,
    )?;
    Ok(())
}

fn clean_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut pending_space = false;
    for ch in s.chars() {
        if matches!(
            ch,
            '\u{200b}' | '\u{200c}' | '\u{200d}' | '\u{200e}' | '\u{200f}' | '\u{2060}' | '\u{feff}'
        ) {
            continue;
        }
        if ch.is_whitespace() {
            pending_space = !out.is_empty();
            continue;
        }
        if pending_space {
            out.push(' ');
            pending_space = false;
        }
        out.push(ch);
    }
    out.trim().to_owned()
}

/// 标题里的 `<noreply@example.com>` 会吃掉整行；主体保留，邮箱段直接删掉。
fn strip_email(s: &str) -> String {
    if let Some(lt) = s.find('<') {
        if let Some(gt) = s[lt..].find('>') {
            let inside = &s[lt..lt + gt];
            if inside.contains('@') {
                return s[..lt].trim_end().to_owned();
            }
        }
    }
    s.to_owned()
}

fn item_title(item: &NotifyItem) -> String {
    let t = strip_email(&clean_text(&item.title));
    if t.is_empty() {
        clean_text(&item.app)
    } else {
        t
    }
}

fn item_body(item: &NotifyItem) -> String {
    let b = clean_text(&item.body);
    if b.is_empty() {
        format!("来自 {}", clean_text(&item.app))
    } else {
        b
    }
}

/// 双行条目：标题（600 fg）+ 正文（400 muted），各自单行省略。
fn paint_item_content(
    g: &Gfx,
    te: &TextEngine,
    pal: &Palette,
    t: &Tokens,
    pl: &Placed,
    item: &NotifyItem,
    show_body: bool,
) -> Result<()> {
    let title = item_title(item);
    let body = item_body(item);
    let x = pl.rect.x;
    let w = pl.rect.w;

    let title_spec = TextSpec {
        s: title,
        size: t.title_fs,
        line_h: t.title_lh,
        weight: 600,
        wrap: false,
        align: 0,
        tabular: false,
        spacing_em: 0.0,
    };
    draw_shadowed(
        g,
        &title_spec,
        x,
        pl.rect.y + 0.5,
        w,
        t.title_lh + 4.0,
        TextMode::Ellipsis,
        pal.fg,
        text_shadow(pal),
        0.8335,
        0.833,
        te,
    )?;
    if !show_body {
        return Ok(());
    }

    let body_spec = TextSpec {
        s: body,
        size: t.body_fs,
        line_h: t.body_lh,
        weight: 400,
        wrap: false,
        align: 0,
        tabular: false,
        spacing_em: 0.0,
    };
    draw_shadowed(
        g,
        &body_spec,
        x,
        pl.rect.y + t.title_lh + 0.5,
        w,
        t.body_lh + 4.0,
        TextMode::Ellipsis,
        pal.muted,
        text_shadow(pal),
        0.8335,
        0.833,
        te,
    )?;
    Ok(())
}

/// 品牌色只做「认得出」这一档；未知应用按名称哈希落到一组低饱和色。
fn app_color(app: &str) -> D2D1_COLOR_F {
    let lower = app.to_lowercase();
    if lower.contains("微信") || lower.contains("wechat") {
        color(0.20, 0.72, 0.40)
    } else if lower.contains("qq") {
        color(0.18, 0.68, 0.87)
    } else if lower.contains("邮件") || lower.contains("mail") || lower.contains("outlook") {
        color(0.32, 0.55, 0.92)
    } else {
        const PAL: [(f32, f32, f32); 6] = [
            (0.36, 0.52, 0.86),
            (0.55, 0.42, 0.82),
            (0.82, 0.52, 0.30),
            (0.28, 0.65, 0.60),
            (0.78, 0.42, 0.58),
            (0.48, 0.60, 0.42),
        ];
        let sum: u32 = app.bytes().map(|b| b as u32).sum();
        let (r, gg, b) = PAL[(sum as usize) % PAL.len()];
        color(r, gg, b)
    }
}

fn color(r: f32, g: f32, b: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r, g, b, a: 1.0 }
}

/// 深色主题用白/黑阴影压住壁纸；亮色主题是深色字，再叠白影只会糊，直接关掉。
fn text_shadow(pal: &Palette) -> D2D1_COLOR_F {
    if pal.fg.r > 0.5 {
        pal.shadow
    } else {
        D2D1_COLOR_F {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 0.0,
        }
    }
}

/// 命中（D4 用）：条目 / 删除 / 授权引导。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Hit {
    Item { id: u32, aumid: String },
    Remove { id: u32 },
    /// 抬头「清除」：清空 Windows 通知中心里本卡看得到的通知
    ClearAll,
    Authorize,
}

/// 命中：条目 / 删除 / 授权引导。
///
/// 视口内条目的几何要按 scroll 反算：屏幕 y 落在视口里才生效，内容坐标 = y + scroll。
/// 删除中（正在塌缩）的行不可命中 —— suppress 已经把它从快照列表里过滤掉了。
pub fn hit_at(
    m: &mut dyn Measurer,
    t: &Tokens,
    snap: Option<&NotifySnapshot>,
    opts: Options,
    motion: &Motion,
    x: f32,
    y: f32,
) -> Option<Hit> {
    let snap = snap?;
    // hit 没有独立 now 参数：取当前时刻求动画几何（点击发生在两帧之间，误差 < 1 帧）
    let now = Instant::now();
    let state = card_state(snap.access, snap.items.len());
    let groups = build_groups(Some(snap), opts, motion, now);
    let content_h = content_height(t, Some(snap), opts);
    let tr = tree(t, Some(snap), state, &groups, content_h, opts, motion, now);
    let pl = layout(&tr, (t.w, t.h), m);

    // 抬头「清除」/ 授权引导在视口外，仍按绝对坐标
    let direct = minicard_draw::layout::hit_id(&pl, x, y);
    if state == CardState::Normal && direct == Some(ID_CLEAR) {
        return Some(Hit::ClearAll);
    }
    if state == CardState::Unauthorized && matches!(direct, Some(ID_STATE) | Some(ID_HINT)) {
        return Some(Hit::Authorize);
    }
    if state != CardState::Normal {
        return None;
    }

    // 条目：只对视口内生效；屏幕 y + scroll = 内容坐标
    let vp = find_placed_id(&pl, ID_VIEWPORT)?;
    if x < vp.rect.x || x >= vp.rect.right() || y < vp.rect.y || y >= vp.rect.bottom() {
        return None;
    }
    let max_scroll = max_scroll_px(t, Some(snap), opts, motion);
    let scroll = display_scroll(motion, now, max_scroll);
    let content_y = y + scroll;
    let mut id = None;
    for c in vp.children.iter().rev() {
        if let Some(found) = minicard_draw::layout::hit_id(c, x, content_y) {
            id = Some(found);
            break;
        }
    }
    let id = id?;
    let (g, i) = item_of(id)?;
    let row = groups.get(g)?.rows.get(i)?;
    if row.exit.is_some() {
        return None;
    }
    if id - id_item(g, i) == 3 {
        return Some(Hit::Remove { id: row.uid });
    }
    let item = resolve_row(Some(snap), motion, row.r)?;
    Some(Hit::Item {
        id: item.id,
        aumid: item.aumid.clone(),
    })
}

/// probe / 单测的布局钩子：返回当前快照 + 动效状态下的布局树（滚动偏移只在 paint 里
/// 用 SetTransform 表达，不进布局坐标）。
pub fn tree_hook(
    t: &Tokens,
    snap: Option<&NotifySnapshot>,
    opts: Options,
    motion: &Motion,
    now: Instant,
) -> Node {
    let state = display_state(snap);
    let groups = build_groups(snap, opts, motion, now);
    let content_h = content_height(t, snap, opts);
    tree(t, snap, state, &groups, content_h, opts, motion, now)
}

pub fn dump_layout(p: &Placed, depth: usize) {
    let pad = " ".repeat(depth * 2);
    // 隐私：通知正文不进日志；这里只打长度，够对节点几何即可
    let role = if let Some(s) = p.text() {
        format!("text len={}", s.s.chars().count())
    } else {
        "box".to_string()
    };
    minicard_shell::mlog!(
        "{}id={:<5} {:<24} x={:7.2} y={:7.2} w={:7.2} h={:7.2}",
        pad,
        p.id,
        role,
        p.rect.x,
        p.rect.y,
        p.rect.w,
        p.rect.h
    );
    for c in &p.children {
        dump_layout(c, depth + 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use minicard_core::notify::NotifyItem;
    use std::time::{Duration, Instant};

    struct Fake;
    impl Measurer for Fake {
        fn text(&mut self, t: &TextSpec, max_w: f32) -> (f32, f32) {
            let natural = 0.6 * t.size * t.s.chars().count() as f32;
            let w = if max_w > 0.0 { natural.min(max_w) } else { natural };
            (w, t.line_h)
        }
        fn ascent(&mut self, t: &TextSpec) -> f32 {
            t.line_h * 0.8
        }
    }

    fn item(id: u32, app: &str, aumid: &str, created: u64, unread: bool) -> NotifyItem {
        NotifyItem {
            id,
            app: app.into(),
            aumid: aumid.into(),
            title: format!("发送者{id}"),
            body: format!("正文{id}"),
            created_ms: created,
            unread,
        }
    }

    fn sample() -> Vec<NotifyItem> {
        // 新→旧：微信 3、邮件 2、QQ 1
        vec![
            item(6, "微信", "wechat", 6000, true),
            item(5, "微信", "wechat", 5000, false),
            item(4, "微信", "wechat", 4000, false),
            item(3, "邮件", "mail", 3000, true),
            item(2, "邮件", "mail", 2000, false),
            item(1, "QQ", "qq", 1000, false),
        ]
    }

    /// `--probe notify` 默认样张的形状：3 组 4 条（2+1+1），240×300 刚好不溢出。
    fn sample_four() -> Vec<NotifyItem> {
        vec![
            item(6, "微信", "wechat", 6000, true),
            item(4, "微信", "wechat", 4000, false),
            item(3, "邮件", "mail", 3000, true),
            item(1, "QQ", "qq", 1000, false),
        ]
    }

    fn snap_of(items: Vec<NotifyItem>) -> NotifySnapshot {
        NotifySnapshot {
            access: NotifyAccess::Allowed,
            items,
            at_ms: 1,
        }
    }

    /// 12 组 × 2 行 = 24 行（数据上限），内容远高于 240×300 视口 → 必然溢出。
    fn many_items() -> Vec<NotifyItem> {
        let mut out = Vec::new();
        for gi in 0..ALL_GROUPS as u32 {
            for j in 0..2u32 {
                out.push(item(
                    500 + gi * 2 + j,
                    &format!("应用{gi}"),
                    &format!("app{gi}"),
                    100_000 - (gi * 2 + j) as u64 * 100,
                    j == 0,
                ));
            }
        }
        out
    }

    fn layout_of(node: &Node, t: &Tokens) -> Placed {
        let mut fake = Fake;
        layout(node, (t.w, t.h), &mut fake)
    }

    #[test]
    fn state_selection_covers_four_states() {
        assert_eq!(card_state(NotifyAccess::Allowed, 3), CardState::Normal);
        assert_eq!(card_state(NotifyAccess::Allowed, 0), CardState::Empty);
        assert_eq!(card_state(NotifyAccess::Denied, 3), CardState::Unauthorized);
        assert_eq!(
            card_state(NotifyAccess::Unspecified, 0),
            CardState::Unauthorized
        );
        assert_eq!(
            card_state(NotifyAccess::Unavailable, 0),
            CardState::Unavailable
        );
        assert_eq!(state_texts(CardState::Empty).0, "最近没有新通知");
        assert_eq!(state_texts(CardState::Unauthorized).0, "需要授权读取通知");
        assert_eq!(state_texts(CardState::Unavailable).0, "通知读取暂不可用");
        assert_eq!(display_state(None), CardState::Disabled);
        assert_eq!(state_texts(CardState::Disabled).0, "通知卡未启用");
    }

    #[test]
    fn groups_merge_by_app_and_distribute_rows() {
        let groups = group_items(&sample(), 3, 4);
        assert_eq!(groups.len(), 3);
        assert_eq!(groups[0].app, "微信");
        assert_eq!(groups[0].total, 3);
        assert_eq!(groups[0].items.len(), 2, "第一组拿 2 条");
        assert_eq!(groups[1].app, "邮件");
        assert_eq!(groups[1].items.len(), 1);
        assert_eq!(groups[2].app, "QQ");
        assert_eq!(groups[2].items.len(), 1);
        assert_eq!(
            groups.iter().map(|g| g.items.len()).sum::<usize>(),
            4
        );
    }

    #[test]
    fn group_falls_back_to_app_name_without_aumid() {
        let items = vec![item(1, "系统", "", 1000, true)];
        let groups = group_items(&items, MAX_GROUPS, MAX_ROWS);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].app, "系统");
        assert_eq!(groups[0].unread, 1);
    }

    #[test]
    fn group_caps_respected() {
        let groups = group_items(&sample(), 2, 2);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups.iter().map(|g| g.items.len()).sum::<usize>(), 2);
        assert!(group_items(&sample(), 0, 4).is_empty());
        assert!(group_items(&sample(), 3, 0).is_empty());
    }

    #[test]
    fn per_app_limit_from_settings() {
        let one = group_items_limit(&sample(), 3, 8, 1);
        assert!(one.iter().all(|g| g.items.len() == 1));
        let two = group_items_limit(&sample(), 3, 8, 2);
        assert_eq!(two[0].items.len(), 2);
        let three = group_items_limit(&sample(), 3, 8, 3);
        assert_eq!(three[0].items.len(), 3, "第一组有 3 条时应能放到 3");
    }

    #[test]
    fn capacity_scales_with_height() {
        assert_eq!(capacity(&Tokens::notify(240.0, 300.0)), (3, 4));
        assert_eq!(capacity(&Tokens::notify(240.0, 240.0)), (2, 3));
        // 更矮的卡 k 会按高度缩小（字号同步变小），因此仍能放 2 组；只约束不越界
        let (g, r) = capacity(&Tokens::notify(240.0, 120.0));
        assert!(g >= 1 && r >= 2, "240×120 至少 1 组 2 行：{:?}", (g, r));
    }

    /// 测试内找节点矩形（按 id，深度优先）。
    fn find_rect(p: &Placed, id: u32) -> Option<minicard_draw::layout::Rect> {
        if p.id == id {
            return Some(p.rect);
        }
        p.children.iter().find_map(|c| find_rect(c, id))
    }

    /// 测试内找节点（按 id，深度优先）。
    fn find_node(p: &Placed, id: u32) -> Option<&Placed> {
        if p.id == id {
            return Some(p);
        }
        p.children.iter().find_map(|c| find_node(c, id))
    }

    fn rect_of(r: &minicard_draw::layout::Rect) -> D2D_RECT_F {
        D2D_RECT_F {
            left: r.x,
            top: r.y,
            right: r.x + r.w,
            bottom: r.y + r.h,
        }
    }

    /// 抬头右侧「N 条未读 / 清除」放大：count_fs 0.75r=12、count_lh 0.875r=14；
    /// 「通知」标题字号与抬头行高 header_lh=17 不动。B 案去掉「 · 1/N」页标后，
    /// 最长抬头是「N 条未读」，仍不许换行 / 挤掉标题。
    #[test]
    fn header_count_and_clear_grow_to_12px_without_squeezing_title() {
        let t = Tokens::notify(240.0, 300.0);
        assert!(
            (t.count_fs - 12.0).abs() < 0.01,
            "「N 条未读 / 清除」字号应为 0.75r=12，实际 {}",
            t.count_fs
        );
        assert!(
            (t.count_lh - 14.0).abs() < 0.01,
            "行高应同步为 0.875r=14，实际 {}",
            t.count_lh
        );
        // 「通知」标题字号、抬头行高不变（分割线靠它们定位）
        assert!((t.header_fs - 14.5).abs() < 0.01);
        assert!((t.header_lh - 17.0).abs() < 0.01);

        let snap = snap_of(sample());
        let motion = Motion::new();
        let now = Instant::now();
        let node = tree_hook(&t, Some(&snap), Options::default(), &motion, now);
        let pl = layout_of(&node, &t);

        let count = find_rect(&pl, ID_COUNT).expect("未读计数");
        let clear = find_rect(&pl, ID_CLEAR).expect("清除");
        let title = find_rect(&pl, ID_HEADER).expect("通知标题");
        // Fake measurer 单行高 = line_h；若被挤到换行会是 28
        assert!(
            (count.h - 14.0).abs() < 0.01,
            "未读计数行高 {} 应为 14 且单行",
            count.h
        );
        assert!(
            (clear.h - 14.0).abs() < 0.01,
            "清除行高 {} 应为 14 且单行",
            clear.h
        );
        assert!(
            (title.h - t.header_lh).abs() < 0.01,
            "「通知」标题行高应仍为 header_lh=17，实际 {}",
            title.h
        );
        assert!(clear.x >= count.x + count.w, "未读与清除不能重叠");
        assert!(
            title.w > 2.0 * t.header_fs,
            "标题槽位应至少留两个字宽，实际 {}",
            title.w
        );

        // 大量组/行（内容溢出、抬头最长）也要单行、不挤压标题
        let many = snap_of(many_items());
        let node = tree_hook(&t, Some(&many), Options::default(), &motion, now);
        let pl = layout_of(&node, &t);
        let count = find_rect(&pl, ID_COUNT).expect("溢出态未读计数");
        let clear = find_rect(&pl, ID_CLEAR).expect("溢出态清除");
        let header = find_rect(&pl, 9000).expect("抬头行");
        assert!(
            (count.h - t.count_lh).abs() < 0.01,
            "溢出态抬头也不许换行：行高 {} != {}",
            count.h,
            t.count_lh
        );
        assert!(
            count.w + clear.w + 2.0 * 0.25 * t.r + 2.0 * t.header_fs <= header.w + 0.01,
            "抬头把「通知」挤到不足两字宽：{} + {} + gap 对 {}",
            count.w,
            clear.w,
            header.w
        );
        // 去掉页标：抬头文本里不能再出现「 · 」
        let count_node = find_node(&pl, ID_COUNT).expect("计数节点");
        let s = count_node.text().expect("计数是文字").s.as_str();
        assert!(!s.contains('·'), "B 案不应再有分页页标：{s}");
    }

    /// 正常态：抬头与首组头之间恰好一条 1px 分割线（id=9006），
    /// 上下各一个 head_gap；非正常态不画；不参与点击命中。
    /// 同时守住静态容量：240×300 的默认样张（3 组 4 条）最后一条不越过底部内边距。
    #[test]
    fn normal_state_divider_between_header_and_first_group() {
        let t = Tokens::notify(240.0, 300.0);
        let snap = snap_of(sample_four());
        let motion = Motion::new();
        let now = Instant::now();
        let node = tree_hook(&t, Some(&snap), Options::default(), &motion, now);
        let pl = layout_of(&node, &t);

        let header = find_rect(&pl, 9000).expect("抬头行");
        let divider = find_rect(&pl, ID_DIVIDER).expect("正常态应有抬头分割线");
        let first_group = find_rect(&pl, id_group(0)).expect("首组头");

        assert!(
            (divider.h - 1.0).abs() < 0.01,
            "分割线应为 1px，实际高 {}",
            divider.h
        );
        assert!(
            (divider.x - t.pad_l).abs() < 0.01
                && (divider.w - (t.w - t.pad_l - t.pad_r)).abs() < 0.01,
            "分割线应横跨内容区（同 pad 左右）：{:?}",
            divider
        );
        assert!(header.y + header.h <= divider.y + 0.01);
        assert!(divider.y + divider.h <= first_group.y + 0.01);
        assert!(
            (divider.y - (header.y + header.h) - t.head_gap).abs() < 0.01,
            "抬头→分割线间距应为 head_gap={}，实际 {}",
            t.head_gap,
            divider.y - (header.y + header.h)
        );
        assert!(
            (first_group.y - (divider.y + divider.h) - t.head_gap).abs() < 0.01,
            "分割线→首组头间距应为 head_gap={}，实际 {}",
            t.head_gap,
            first_group.y - (divider.y + divider.h)
        );

        // 静态容量不降：3 组 4 条，最后一条 bottom 不越过卡片底部内边距
        let mut groups = 0usize;
        let mut rows = 0usize;
        let mut last_bottom = 0.0f32;
        pl.walk(&mut |p: &Placed| {
            if let Some(g) = group_of(p.id) {
                if p.id == id_group(g) {
                    groups += 1;
                }
            }
            if let Some((g, i)) = item_of(p.id) {
                if p.id == id_item(g, i) {
                    rows += 1;
                    last_bottom = last_bottom.max(p.rect.y + p.rect.h);
                }
            }
        });
        assert_eq!((groups, rows), (3, 4), "240×300 默认样张仍应为 3 组 4 条");
        assert!(
            last_bottom <= t.h - t.pad_b + 0.01,
            "最后一条越过底部内边距：bottom={} > {}",
            last_bottom,
            t.h - t.pad_b
        );

        // 分割线不参与点击：落在线中间不该命中任何东西
        let mut m = Fake;
        assert_eq!(
            hit_at(
                &mut m,
                &t,
                Some(&snap),
                Options::default(),
                &motion,
                divider.x + divider.w / 2.0,
                divider.y + divider.h / 2.0
            ),
            None,
            "1px 分割线不应参与点击命中"
        );

        // 非正常态（空）不画分割线
        let empty = NotifySnapshot {
            access: NotifyAccess::Allowed,
            items: Vec::new(),
            at_ms: 1,
        };
        let node = tree_hook(&t, Some(&empty), Options::default(), &motion, now);
        let pl = layout_of(&node, &t);
        assert!(
            find_rect(&pl, ID_DIVIDER).is_none(),
            "空态不该有抬头分割线"
        );
    }

    /// 默认样张（3 组 4 条，240×300）静止态必须和旧版逐像素同布局：
    /// 视口 y / 高、组/行坐标全部按旧实现钉死；同时验证视口节点与内容高度关系。
    /// 内容 250 ≤ 视口 252 → 不溢出、不画滚动条。
    #[test]
    fn static_default_sample_keeps_legacy_geometry_and_viewport() {
        let t = Tokens::notify(240.0, 300.0);
        let snap = snap_of(sample_four());
        let motion = Motion::new();
        let now = Instant::now();
        let node = tree_hook(&t, Some(&snap), Options::default(), &motion, now);
        let pl = layout_of(&node, &t);
        assert!((pl.rect.w - 240.0).abs() < 0.01);
        assert!((pl.rect.h - 300.0).abs() < 0.01);

        // 视口：分割线以下、grow 占满剩余高度
        let vp = find_rect(&pl, ID_VIEWPORT).expect("正常态必须有视口节点");
        assert!((vp.x - t.pad_l).abs() < 0.01);
        assert!((vp.w - (t.w - t.pad_l - t.pad_r)).abs() < 0.01);
        assert!(
            (vp.y - (t.pad_t + t.header_lh + 2.0 * t.head_gap + t.rule_h)).abs() < 0.01,
            "视口 y 应为 37，实际 {}",
            vp.y
        );
        assert!(
            (vp.h - viewport_height(&t)).abs() < 0.01,
            "视口高应=viewport_height()=252，实际 {}",
            vp.h
        );

        // 视口内只有 groups_col，显式高度 = 内容高（名义 item_h / group_gh）
        let content_h = content_height(&t, Some(&snap), Options::default());
        assert!(
            (content_h - 250.0).abs() < 0.01,
            "4 条内容高应=2×106+2×65+2×7=250，实际 {content_h}"
        );
        let vp_node = find_node(&pl, ID_VIEWPORT).unwrap();
        assert_eq!(vp_node.children.len(), 1, "视口下应只有一个 groups_col");
        let groups_col = &vp_node.children[0];
        assert!(
            (groups_col.rect.h - content_h).abs() < 0.01,
            "groups_col 应使用显式内容高，实际 {} != {}",
            groups_col.rect.h,
            content_h
        );
        assert!((groups_col.rect.y - vp.y).abs() < 0.01);
        assert!(content_h <= vp.h + 0.01, "默认样张不应溢出：{content_h} vs {}", vp.h);
        assert_eq!(
            max_scroll_px(&t, Some(&snap), Options::default(), &motion),
            0.0,
            "默认样张不溢出 → 最大滚动 0"
        );
        assert!(scrollbar_geometry(rect_of(&vp), t.w, 0.0, 0.0).is_none());

        // 旧版几何逐项钉死（Fake measurer 不影响纵向；组槽位用 measure() 的口径：
        // 组头测高 = 子节点 intrinsic 20，而不是排列用的 24，所以下一组会比标称早 4px）：
        // 组0 头 37 / 行 64、105；组1 头 146 / 行 173；组2 头 214 / 行 241，末行底 279。
        let cases: &[(u32, f32)] = &[
            (id_group(0), 37.0),
            (id_item(0, 0), 64.0),
            (id_item(0, 1), 105.0),
            (id_group(1), 146.0),
            (id_item(1, 0), 173.0),
            (id_group(2), 214.0),
            (id_item(2, 0), 241.0),
        ];
        for &(id, y) in cases {
            let r = find_rect(&pl, id).unwrap_or_else(|| panic!("缺节点 {id}"));
            assert!(
                (r.y - y).abs() < 0.01,
                "id={id} 的 y 应为 {y}（旧版静态坐标），实际 {}",
                r.y
            );
        }
        let last = find_rect(&pl, id_item(2, 0)).expect("末行");
        assert!(
            last.y + last.h <= t.h - t.pad_b + 0.01,
            "末行底 {} 越过底部内边距 {}",
            last.y + last.h,
            t.h - t.pad_b
        );

        // 行数/组数/删除命中区；行都在卡内且正文撑满
        let mut rows = 0usize;
        let mut groups = 0usize;
        let mut removes = 0usize;
        let mut line_widths: Vec<f32> = Vec::new();
        pl.walk(&mut |p: &Placed| {
            if let Some((g, i)) = item_of(p.id) {
                if p.id == id_item(g, i) {
                    rows += 1;
                    assert!(
                        p.rect.x >= 0.0
                            && p.rect.y >= 0.0
                            && p.rect.x + p.rect.w <= 240.01
                            && p.rect.y + p.rect.h <= 300.01,
                        "条目行越界：{:?}",
                        p.rect
                    );
                } else if p.id == id_item(g, i) + 2 {
                    line_widths.push(p.rect.w);
                } else if p.id == id_item(g, i) + 3 {
                    removes += 1;
                }
            }
            if let Some(g) = group_of(p.id) {
                if p.id == id_group(g) {
                    groups += 1;
                }
            }
        });
        assert_eq!(groups, 3);
        assert_eq!(rows, 4);
        assert_eq!(removes, 4, "每条都要有删除命中区");
        assert_eq!(line_widths.len(), 4);
        assert!(
            line_widths.iter().all(|w| *w > 100.0),
            "正文行应撑满剩余宽度：{line_widths:?}"
        );

        // 溢出样张：所有行都排进视口，超出视口的部分靠 paint 的矩形 clip 收掉
        let many = snap_of(many_items());
        let content_many = content_height(&t, Some(&many), Options::default());
        let max_many = max_scroll_px(&t, Some(&many), Options::default(), &motion);
        assert!(
            (max_many - (content_many - viewport_height(&t))).abs() < 0.01 && max_many > 0.0,
            "溢出样张 max_scroll 应=content_h-viewport_h：{max_many} / {content_many}"
        );
        let node = tree_hook(&t, Some(&many), Options::default(), &motion, now);
        let pl = layout_of(&node, &t);
        let vp = find_rect(&pl, ID_VIEWPORT).expect("溢出样张也应有视口");
        let mut count = 0usize;
        let mut beyond = 0usize;
        pl.walk(&mut |p: &Placed| {
            if let Some((g, i)) = item_of(p.id) {
                if p.id == id_item(g, i) {
                    count += 1;
                    if p.rect.y + p.rect.h > vp.y + vp.h + 0.01 {
                        beyond += 1;
                    }
                }
            }
        });
        assert_eq!(count, 24, "24 行都应在树里（视口只负责裁剪，不负责少画）");
        assert!(beyond > 0, "溢出样张应有行伸到视口之外（被 clip）");
        assert!(
            scrollbar_geometry(rect_of(&vp), t.w, max_many, 0.0).is_some(),
            "溢出样张应有滚动条"
        );
    }

    #[test]
    fn hit_maps_row_and_remove_x() {
        let t = Tokens::notify(240.0, 300.0);
        let snap = snap_of(sample_four());
        let motion = Motion::new();
        let node = tree_hook(&t, Some(&snap), Options::default(), &motion, Instant::now());
        let pl = layout_of(&node, &t);

        let row = find_rect(&pl, id_item(0, 0)).unwrap();
        let remove = find_rect(&pl, id_item(0, 0) + 3).unwrap();
        let mut m1 = Fake;
        assert_eq!(
            hit_at(
                &mut m1,
                &t,
                Some(&snap),
                Options::default(),
                &motion,
                row.x + 20.0,
                row.y + row.h / 2.0
            ),
            Some(Hit::Item {
                id: 6,
                aumid: "wechat".into()
            })
        );
        let mut m2 = Fake;
        assert_eq!(
            hit_at(
                &mut m2,
                &t,
                Some(&snap),
                Options::default(),
                &motion,
                remove.x + remove.w / 2.0,
                remove.y + remove.h / 2.0
            ),
            Some(Hit::Remove { id: 6 })
        );
        // 抬头「清除」仍可点
        let clear = find_rect(&pl, ID_CLEAR).unwrap();
        let mut m3 = Fake;
        assert_eq!(
            hit_at(
                &mut m3,
                &t,
                Some(&snap),
                Options::default(),
                &motion,
                clear.x + clear.w / 2.0,
                clear.y + clear.h / 2.0
            ),
            Some(Hit::ClearAll)
        );
        // 视口外（卡内边距下方）不命中
        let mut m4 = Fake;
        assert_eq!(
            hit_at(
                &mut m4,
                &t,
                Some(&snap),
                Options::default(),
                &motion,
                row.x + 20.0,
                t.h - 2.0
            ),
            None
        );
    }

    // ── B 案：连续滚动 / 动效验收 ──────────────────────────────────────────

    /// 滚动 clamp（0 / content_h - viewport_h / 中间）+ 一格 = 2 行 = 76px。
    #[test]
    fn scroll_clamps_to_zero_max_and_middle() {
        let t = Tokens::notify(240.0, 300.0);
        let snap = snap_of(many_items());
        let opts = Options::default();
        let mut motion = Motion::new();
        let max = max_scroll_px(&t, Some(&snap), opts, &motion);
        assert!(max > 0.0, "溢出样张应有可滚动距离");
        assert!(
            (max - (content_height(&t, Some(&snap), opts) - viewport_height(&t))).abs() < 0.01
        );
        assert!((wheel_step_px(&t) - 76.0).abs() < 0.01, "一格 = 2 行 = 76px");

        let now = Instant::now();
        // 上界：一次滚到天上去也只到 max
        motion.scroll_by(1_000_000.0, max, now);
        let v = motion.scroll_at(now + Duration::from_millis(200));
        assert!((v - max).abs() < 0.01, "应 clamp 到 max={max}，实际 {v}");
        // 下界：回滚也只到 0
        motion.scroll_by(-1_000_000.0, max, now);
        let v = motion.scroll_at(now + Duration::from_millis(400));
        assert!(v.abs() < 0.01, "应 clamp 到 0，实际 {v}");
        // 中间值
        motion.scroll_by(wheel_step_px(&t), max, now);
        let mid = motion.scroll_at(now + Duration::from_millis(200));
        assert!((mid - 76.0).abs() < 0.01, "一格应滚 76px，实际 {mid}");
        assert!(mid > 0.0 && mid < max);
        // reset_scroll
        motion.reset_scroll();
        assert_eq!(motion.scroll_at(now), 0.0);
    }

    /// 180ms easeOutQuart 补间：单调收敛，180ms 整到 target。
    #[test]
    fn scroll_tween_is_monotonic_and_converges_in_180ms() {
        let t = Tokens::notify(240.0, 300.0);
        let snap = snap_of(many_items());
        let mut motion = Motion::new();
        let max = max_scroll_px(&t, Some(&snap), Options::default(), &motion);
        let t0 = Instant::now();
        motion.scroll_by(76.0, max, t0);
        let mut prev = -1.0f32;
        for ms in [0u64, 15, 30, 60, 90, 120, 150, 179, 180] {
            let v = motion.scroll_at(t0 + Duration::from_millis(ms));
            assert!(
                v >= prev - 0.001,
                "{ms}ms 处补间不单调：{v} < {prev}"
            );
            assert!(v >= -0.001 && v <= 76.001, "{ms}ms 处越界：{v}");
            prev = v;
        }
        let end = motion.scroll_at(t0 + Duration::from_millis(180));
        assert!((end - 76.0).abs() < 0.01, "180ms 应到达 target，实际 {end}");
        assert!(motion.animating(t0 + Duration::from_millis(100)));
        assert!(!motion.animating(t0 + Duration::from_millis(200)));
    }

    /// 连滚叠加 target，并从当前显示值重新起跑；向上/向下方向与滚轮一致。
    #[test]
    fn wheel_accumulates_target_and_direction_follows_delta() {
        let t = Tokens::notify(240.0, 300.0);
        let snap = snap_of(many_items());
        let mut motion = Motion::new();
        let max = max_scroll_px(&t, Some(&snap), Options::default(), &motion);
        let t0 = Instant::now();
        let step = wheel_step_px(&t);

        motion.scroll_by(step, max, t0);
        assert!(motion.scroll_to > 0.0, "正 delta = 向内容下方滚（offset 增大）");
        let shown = motion.scroll_at(t0 + Duration::from_millis(50));
        motion.scroll_by(step, max, t0 + Duration::from_millis(50));
        assert!(
            (motion.scroll_to - 2.0 * step).abs() < 0.01,
            "连滚应在 target 上叠加：{}",
            motion.scroll_to
        );
        assert!(
            (motion.scroll_from - shown).abs() < 0.01,
            "重新起跑应从当前显示值开始：{} != {shown}",
            motion.scroll_from
        );
        let settled = motion.scroll_at(t0 + Duration::from_millis(50 + 200));
        assert!((settled - 2.0 * step).abs() < 0.01);

        // 反向（wheel up）offset 减小
        motion.scroll_by(-step, max, t0);
        assert!((motion.scroll_to - step).abs() < 0.01);
        // 已是 0 时再往上滚不会变负
        motion.jump_scroll(0.0, max);
        motion.scroll_by(-step, max, t0);
        assert_eq!(motion.scroll_to, 0.0);
    }

    /// 首次 observe 只种子（启动不整卡入场）；第二次出现新 id 才播 180ms 行高入场。
    #[test]
    fn first_observe_seeds_without_enter_then_new_id_animates() {
        let t = Tokens::notify(240.0, 300.0);
        let opts = Options::default();
        let base = snap_of(vec![item(1, "微信", "wechat", 1000, false)]);
        let mut motion = Motion::new();
        let t0 = Instant::now();
        motion.observe(Some(&base), CardState::Normal, t0);
        assert!(motion.initialized);
        assert!(motion.enter.is_empty(), "首次 observe 必须只种子、不播入场");

        let next = snap_of(vec![
            item(2, "微信", "wechat", 2000, true),
            item(1, "微信", "wechat", 1000, false),
        ]);
        motion.observe(Some(&next), CardState::Normal, t0);
        assert!(motion.enter.contains_key(&2), "新 id 应记录入场起点");

        // e=0：新行高 0，旧行满高
        let node = tree_hook(&t, Some(&next), opts, &motion, t0);
        let pl = layout_of(&node, &t);
        let new_row = find_rect(&pl, id_item(0, 0)).expect("新行");
        assert!(new_row.h.abs() < 0.01, "t0 时新行高应为 0，实际 {}", new_row.h);
        let old_row = find_rect(&pl, id_item(0, 1)).expect("旧行");
        assert!((old_row.h - t.item_h).abs() < 0.01);

        // 90ms：行高严格介于 0..item_h（验收：e 时刻介于 0 与 item_h）
        let node = tree_hook(&t, Some(&next), opts, &motion, t0 + Duration::from_millis(90));
        let pl = layout_of(&node, &t);
        let mid = find_rect(&pl, id_item(0, 0)).expect("入场中的行");
        assert!(
            mid.h > 0.01 && mid.h < t.item_h - 0.01,
            "90ms 时行高应在 (0, item_h)，实际 {}",
            mid.h
        );

        // 180ms：满高（入场结束）
        let node = tree_hook(&t, Some(&next), opts, &motion, t0 + Duration::from_millis(190));
        let pl = layout_of(&node, &t);
        let full = find_rect(&pl, id_item(0, 0)).expect("入场完成的行");
        assert!((full.h - t.item_h).abs() < 0.01);
    }

    /// 新组的组头 120ms 内从 0 长到 group_gh。
    #[test]
    fn new_group_header_grows_in_120ms() {
        let t = Tokens::notify(240.0, 300.0);
        let opts = Options::default();
        let base = snap_of(vec![item(1, "微信", "wechat", 1000, false)]);
        let next = snap_of(vec![
            item(9, "新应用", "newapp", 9000, true),
            item(1, "微信", "wechat", 1000, false),
        ]);
        let mut motion = Motion::new();
        let t0 = Instant::now();
        motion.observe(Some(&base), CardState::Normal, t0);
        motion.observe(Some(&next), CardState::Normal, t0);
        assert!(motion.group_enter.contains_key("aumid:newapp"));

        let h_at = |ms: u64| {
            let node =
                tree_hook(&t, Some(&next), opts, &motion, t0 + Duration::from_millis(ms));
            let pl = layout_of(&node, &t);
            find_rect(&pl, id_group(0)).expect("新组组头").h
        };
        assert!(h_at(0).abs() < 0.01, "t0 组头高应为 0");
        let mid = h_at(60);
        assert!(mid > 0.01 && mid < t.group_gh - 0.01, "60ms 应介于 0..group_gh，实际 {mid}");
        assert!((h_at(130) - t.group_gh).abs() < 0.01, "120ms 后组头满高");
    }

    /// 本地删除：180ms 行高塌缩 + suppress 过滤快照（删除中的行不可命中）。
    #[test]
    fn local_remove_collapses_and_suppressed_row_is_not_hittable() {
        let t = Tokens::notify(240.0, 300.0);
        let snap = snap_of(sample_four());
        let opts = Options::default();
        let mut motion = Motion::new();
        let t0 = Instant::now();
        motion.observe(Some(&snap), CardState::Normal, t0);

        let pl = layout_of(&tree_hook(&t, Some(&snap), opts, &motion, t0), &t);
        let row = find_rect(&pl, id_item(0, 0)).expect("首行");
        let mut m = Fake;
        assert_eq!(
            hit_at(
                &mut m,
                &t,
                Some(&snap),
                opts,
                &motion,
                row.x + 20.0,
                row.y + row.h / 2.0
            ),
            Some(Hit::Item {
                id: 6,
                aumid: "wechat".into()
            })
        );

        // 删除：立刻 suppress + 开始塌缩
        motion.mark_removed(6, t0);
        assert!(motion.suppressed.contains_key(&6));
        let groups = build_groups(Some(&snap), opts, &motion, t0);
        let first = &groups[0].rows[0];
        assert_eq!(first.uid, 6, "退出中的行仍留在原位");
        assert!(first.exit.is_some(), "这一行应标记为退出中");
        let mut m = Fake;
        assert_eq!(
            hit_at(
                &mut m,
                &t,
                Some(&snap),
                opts,
                &motion,
                row.x + 20.0,
                row.y + row.h / 2.0
            ),
            None,
            "删除中的行不可命中"
        );

        // 90ms：行高介于 0..item_h
        let pl = layout_of(
            &tree_hook(&t, Some(&snap), opts, &motion, t0 + Duration::from_millis(90)),
            &t,
        );
        let mid = find_rect(&pl, id_item(0, 0)).expect("塌缩中的行");
        assert!(
            mid.h > 0.0 && mid.h < t.item_h,
            "90ms 时退出中的行高应在 (0, item_h)，实际 {}",
            mid.h
        );

        // 数据侧 100ms 就收敛（比 180ms 塌缩快）：撤销 suppress，但塌缩动画仍要跑完
        let fast = snap_of(vec![
            item(4, "微信", "wechat", 4000, false),
            item(3, "邮件", "mail", 3000, true),
            item(1, "QQ", "qq", 1000, false),
        ]);
        motion.observe(Some(&fast), CardState::Normal, t0 + Duration::from_millis(100));
        assert!(motion.suppressed.is_empty(), "快照收敛后应撤销 suppress");
        assert!(
            motion.exiting.contains_key(&6),
            "快照收敛得快也不能把 180ms 退出动画截断"
        );
        let pl = layout_of(
            &tree_hook(&t, Some(&fast), opts, &motion, t0 + Duration::from_millis(120)),
            &t,
        );
        assert!(
            find_rect(&pl, id_item(0, 0)).is_some(),
            "退出中的行在快照收敛后仍应占位到 180ms"
        );

        // 200ms：退出动画结束，被删除的行不再排进视口（4 条 → 3 条）
        let groups = build_groups(Some(&fast), opts, &motion, t0 + Duration::from_millis(200));
        assert!(
            groups.iter().all(|g| g.rows.iter().all(|r| r.uid != 6)),
            "退出结束后被删除的行不应再排进视口"
        );
        assert_eq!(
            groups.iter().map(|g| g.rows.len()).sum::<usize>(),
            3,
            "4 条删 1 条应剩 3 条"
        );
    }

    /// 退出塌缩时后面的组也要连续上移（靠行高 + 负底边距同时缩小，layout 的 measure 才跟得上），
    /// 180ms 收尾后与「这一行被删掉」的最终布局严格对齐（不跳 item_gap）。
    #[test]
    fn exit_collapse_moves_following_groups_smoothly() {
        let t = Tokens::notify(240.0, 300.0);
        let snap = snap_of(sample_four());
        let opts = Options::default();
        let mut motion = Motion::new();
        let t0 = Instant::now();
        motion.observe(Some(&snap), CardState::Normal, t0);
        let group_y = |motion: &Motion, ms: u64| {
            let node =
                tree_hook(&t, Some(&snap), opts, motion, t0 + Duration::from_millis(ms));
            let pl = layout_of(&node, &t);
            find_rect(&pl, id_group(1)).expect("第二组头").y
        };
        let y0 = group_y(&motion, 0);
        assert!((y0 - 146.0).abs() < 0.01, "静止时第二组 y=146，实际 {y0}");

        motion.mark_removed(6, t0);
        let y30 = group_y(&motion, 30);
        let y60 = group_y(&motion, 60);
        let y150 = group_y(&motion, 150);
        let y179 = group_y(&motion, 179);
        assert!(
            y0 > y30 && y30 >= y60 && y60 >= y150 && y150 >= y179 - 0.001,
            "删除中的组间位移应单调上移：{y0} → {y30} → {y60} → {y150} → {y179}"
        );
        let final_y = group_y(&motion, 200);
        assert!(
            (y179 - final_y).abs() < 0.2,
            "180ms 末帧应与删除后的最终布局对齐：{y179} vs {final_y}"
        );
    }

    /// suppress 2s 还没收敛 → 撤销抑制、重放一次 180ms 入场。
    #[test]
    fn suppress_expires_after_two_seconds_and_replays_enter() {
        let t = Tokens::notify(240.0, 300.0);
        let snap = snap_of(sample_four());
        let opts = Options::default();
        let mut motion = Motion::new();
        let t0 = Instant::now();
        motion.observe(Some(&snap), CardState::Normal, t0);
        motion.mark_removed(6, t0);
        assert!(motion.suppressed.contains_key(&6));

        // 1s 还没到 2s：仍抑制
        motion.observe(Some(&snap), CardState::Normal, t0 + Duration::from_millis(1000));
        assert!(motion.suppressed.contains_key(&6), "1s 内快照还没收敛也应继续抑制");

        // 2.1s：撤销抑制并重放入场
        let revert_at = t0 + Duration::from_millis(2100);
        motion.observe(Some(&snap), CardState::Normal, revert_at);
        assert!(!motion.suppressed.contains_key(&6), "2s 后应撤销 suppress");
        assert!(motion.enter.contains_key(&6), "撤销后应重放一次入场");
        assert!(!motion.exiting.contains_key(&6));

        let pl = layout_of(&tree_hook(&t, Some(&snap), opts, &motion, revert_at), &t);
        let start = find_rect(&pl, id_item(0, 0)).expect("重放入场的行");
        assert!(start.h.abs() < 0.01, "重放起点行高应为 0，实际 {}", start.h);
        let pl = layout_of(
            &tree_hook(
                &t,
                Some(&snap),
                opts,
                &motion,
                revert_at + Duration::from_millis(190),
            ),
            &t,
        );
        let end = find_rect(&pl, id_item(0, 0)).expect("重放完成的行");
        assert!((end.h - t.item_h).abs() < 0.01);
    }

    /// 清除 → 确认：两段两字文字同一 rect 交叉淡化 120ms；两字同宽。
    #[test]
    fn clear_label_crossfades_two_equal_width_texts() {
        let t = Tokens::notify(240.0, 300.0);
        let snap = snap_of(sample_four());
        let mut motion = Motion::new();
        let t0 = Instant::now();
        motion.observe(Some(&snap), CardState::Normal, t0);
        assert_eq!(motion.clear_swap_alpha(t0), 0.0);

        motion.set_clear_armed(true, t0);
        assert!(motion.clear_swap_alpha(t0).abs() < 0.001);
        let mid = motion.clear_swap_alpha(t0 + Duration::from_millis(60));
        assert!(
            mid > 0.01 && mid < 0.99,
            "60ms 应在「清除→确认」交叉中，实际 {mid}"
        );
        assert!(motion.animating(t0 + Duration::from_millis(60)));
        let end = motion.clear_swap_alpha(t0 + Duration::from_millis(120));
        assert!((end - 1.0).abs() < 0.001, "120ms 应到确认态，实际 {end}");

        // 回退（3s 到期）也 120ms
        motion.set_clear_armed(false, t0 + Duration::from_millis(300));
        let back = motion.clear_swap_alpha(t0 + Duration::from_millis(360));
        assert!(back > 0.01 && back < 0.99, "回退 60ms 应在交叉中，实际 {back}");
        let back_end = motion.clear_swap_alpha(t0 + Duration::from_millis(420));
        assert!(back_end.abs() < 0.001);

        // 两字同宽（Fake 度量按字数；真字体里「清除」「确认」也都是两字宽）
        let spec = |s: &str| TextSpec {
            s: s.into(),
            size: t.count_fs,
            line_h: t.count_lh,
            weight: 400,
            wrap: false,
            align: 0,
            tabular: true,
            spacing_em: 0.0,
        };
        let mut fake = Fake;
        let (w_clear, _) = fake.text(&spec("清除"), 0.0);
        let (w_confirm, _) = fake.text(&spec("确认"), 0.0);
        assert!(w_clear > 0.0);
        assert!((w_clear - w_confirm).abs() < 0.01, "两字同宽，换字不挪位");
    }

    /// hover：120ms 透明度补间，零位移（几何一点不变）。
    #[test]
    fn hover_tween_fades_in_120ms_without_moving_layout() {
        let t = Tokens::notify(240.0, 300.0);
        let snap = snap_of(sample_four());
        let opts = Options::default();
        let mut motion = Motion::new();
        let t0 = Instant::now();
        motion.observe(Some(&snap), CardState::Normal, t0);
        let pl0 = layout_of(&tree_hook(&t, Some(&snap), opts, &motion, t0), &t);
        let row0 = find_rect(&pl0, id_item(0, 0)).unwrap();

        motion.update_hover(Some(6), t0);
        assert!(motion.hover_alpha(6, t0).abs() < 0.01);
        let mid = motion.hover_alpha(6, t0 + Duration::from_millis(60));
        assert!(mid > 0.01 && mid < 0.99, "60ms 应在 hover 淡入中，实际 {mid}");
        assert!((motion.hover_alpha(6, t0 + Duration::from_millis(130)) - 1.0).abs() < 0.01);
        assert!(motion.animating(t0 + Duration::from_millis(60)));

        motion.update_hover(None, t0 + Duration::from_millis(130));
        let out = motion.hover_alpha(6, t0 + Duration::from_millis(190));
        assert!(out > 0.01 && out < 0.99, "移开后应在淡出中，实际 {out}");
        assert!(motion.hover_alpha(6, t0 + Duration::from_millis(300)).abs() < 0.01);
        assert!(!motion.animating(t0 + Duration::from_millis(300)));

        // 零位移：hover 前后行几何完全相同
        let pl1 = layout_of(&tree_hook(&t, Some(&snap), opts, &motion, t0), &t);
        let row1 = find_rect(&pl1, id_item(0, 0)).unwrap();
        assert_eq!(row0, row1);
    }

    /// 空 / 授权 / 不可用态：内容 160ms 淡入；首次 observe 不淡入。
    #[test]
    fn state_fade_runs_160ms_only_after_first_observe() {
        let t = Tokens::notify(240.0, 300.0);
        let normal = snap_of(sample_four());
        let empty = snap_of(Vec::new());
        let mut motion = Motion::new();
        let t0 = Instant::now();
        motion.observe(Some(&normal), CardState::Normal, t0);
        motion.observe(Some(&empty), CardState::Empty, t0 + Duration::from_millis(100));
        assert_eq!(motion.state_alpha(t0 + Duration::from_millis(100)), 0.0);
        let mid = motion.state_alpha(t0 + Duration::from_millis(180));
        assert!(mid > 0.01 && mid < 0.99, "80ms 应在淡入中，实际 {mid}");
        assert!((motion.state_alpha(t0 + Duration::from_millis(300)) - 1.0).abs() < 0.001);

        let mut fresh = Motion::new();
        fresh.observe(Some(&empty), CardState::Empty, t0);
        assert_eq!(
            fresh.state_alpha(t0),
            1.0,
            "启动首帧（首次 observe）应直接显示，不淡入"
        );
        // 空态内容节点在（淡入只作用在 ID_STATE / ID_HINT 的文字色上，抬头/分割线不参与）
        let pl = layout_of(
            &tree_hook(&t, Some(&empty), Options::default(), &fresh, t0),
            &t,
        );
        assert!(find_rect(&pl, ID_STATE).is_some(), "空态应有状态文案");
    }

    /// 滚动条：内容溢出才有；2px；贴视口右侧内缩；滚到底时拇指贴底。
    #[test]
    fn scrollbar_appears_only_when_content_overflows() {
        let t = Tokens::notify(240.0, 300.0);
        let opts = Options::default();
        let motion = Motion::new();
        let now = Instant::now();

        // 默认样张：不溢出
        let four = snap_of(sample_four());
        let pl = layout_of(&tree_hook(&t, Some(&four), opts, &motion, now), &t);
        let vp = find_rect(&pl, ID_VIEWPORT).unwrap();
        let max4 = max_scroll_px(&t, Some(&four), opts, &motion);
        assert_eq!(max4, 0.0);
        assert!(scrollbar_geometry(rect_of(&vp), t.w, max4, 0.0).is_none());

        // 溢出样张：有滚动条
        let many = snap_of(many_items());
        let pl = layout_of(&tree_hook(&t, Some(&many), opts, &motion, now), &t);
        let vp = find_rect(&pl, ID_VIEWPORT).unwrap();
        let vp_rect = rect_of(&vp);
        let max_many = max_scroll_px(&t, Some(&many), opts, &motion);
        let bar = scrollbar_geometry(vp_rect, t.w, max_many, 0.0).expect("溢出时应有滚动条");
        assert!(
            (bar.track.right - bar.track.left - 2.0).abs() < 0.01,
            "滚动条应为 2px 宽"
        );
        assert!(
            bar.track.left > vp_rect.right,
            "滚动条应在内容右缘之外（浮在卡片右内边距里，不压文字）：bar.left={} vp.right={}",
            bar.track.left,
            vp_rect.right
        );
        assert!(
            bar.track.right <= t.w - 4.0,
            "滚动条离卡片右缘至少留 4px：bar.right={} card.w={}",
            bar.track.right,
            t.w
        );
        assert!(bar.thumb.top >= bar.track.top);
        // 滚到底：拇指贴底
        let bottom = scrollbar_geometry(vp_rect, t.w, max_many, max_many).unwrap();
        assert!((bottom.thumb.bottom - bottom.track.bottom).abs() < 0.01);
        assert!(bottom.thumb.top > bar.thumb.top);
    }

    /// 命中随滚动偏移：屏幕 y + scroll 才对应布局坐标；视口外一律不命中。
    #[test]
    fn hit_follows_scroll_offset_and_is_clipped_to_viewport() {
        let t = Tokens::notify(240.0, 300.0);
        let snap = snap_of(many_items());
        let opts = Options::default();
        let mut motion = Motion::new();
        let t0 = Instant::now();
        motion.observe(Some(&snap), CardState::Normal, t0);
        let max = max_scroll_px(&t, Some(&snap), opts, &motion);
        let step = wheel_step_px(&t);
        motion.jump_scroll(step, max);

        let pl = layout_of(&tree_hook(&t, Some(&snap), opts, &motion, t0), &t);
        let vp = find_rect(&pl, ID_VIEWPORT).expect("视口");
        // 布局里首组头仍在视口顶（滚动是 paint 的 SetTransform，不在布局坐标里）
        let first = find_rect(&pl, id_group(0)).unwrap();
        assert!((first.y - vp.y).abs() < 0.01, "布局坐标不预扣 scroll");
        assert!(first.y - step < vp.y, "滚动后首组头 y 应在视口上方（屏幕坐标 -scroll）");

        // 找一条「屏幕坐标滚动后完整落在视口里」的行
        let mut target = None;
        'outer: for g in 0..ALL_GROUPS {
            for i in 0..2 {
                if let Some(r) = find_rect(&pl, id_item(g, i)) {
                    let screen_y = r.y - step;
                    if screen_y >= vp.y + 1.0 && screen_y + r.h <= vp.y + vp.h - 1.0 {
                        target = Some((g, i, r));
                        break 'outer;
                    }
                }
            }
        }
        let (g, i, r) = target.expect("滚动后应有完整落在视口内的行");
        let groups = build_groups(Some(&snap), opts, &motion, t0);
        let uid = groups[g].rows[i].uid;
        let aumid = groups[g].aumid.clone();
        let mut m = Fake;
        assert_eq!(
            hit_at(
                &mut m,
                &t,
                Some(&snap),
                opts,
                &motion,
                r.x + 20.0,
                r.y + r.h / 2.0 - step
            ),
            Some(Hit::Item { id: uid, aumid }),
            "内容坐标 = 屏幕 y + scroll"
        );

        // 屏幕 y 在视口上方：即使内容坐标落在某行里也不命中（视口裁剪对命中同样生效）
        let mut m = Fake;
        assert_eq!(
            hit_at(
                &mut m,
                &t,
                Some(&snap),
                opts,
                &motion,
                r.x + 20.0,
                vp.y - 2.0
            ),
            None,
            "视口外的屏幕坐标不应命中任何行"
        );
    }
}
