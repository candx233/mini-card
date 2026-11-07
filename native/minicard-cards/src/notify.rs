//! 通知卡（0.1.3 · 03「按应用分组」样式）—— 240×180，D2D 自绘。
//!
//! 数据来自 `minicard_core::notify`（D2），这里只负责「怎么画」：
//! - 四态：正常（按应用分组，最多 3 组 / 4 条）/ 空（最近没有新通知）/ 未授权 / 不可用；
//! - 组头：16px 应用块 + 应用名 + 总数药丸 + 相对时间；组内 1–2 条，发送者 500 + 正文 muted；
//! - 未读：条目前 4px accent 圆点；抬头右侧「N 条未读」；
//! - 点击命中留给 D4：`hit_at` 返回条目 id / AUMID / 授权引导。
//!
//! 尺寸口径：BASE = 240×180（k = 1），字号与间距全部按 r=16 的倍数写，便于同系列卡统一。

use std::collections::HashMap;

use minicard_core::notify::{relative_time, NotifyAccess, NotifyItem, NotifySnapshot};
use minicard_draw::color::Palette;
use minicard_draw::layout::{layout, Align, Dim, Dir, Justify, Measurer, Node, Placed, TextSpec};
use minicard_draw::paint::{fill_round, set_color, stroke_round};
use minicard_draw::text::{draw_shadowed, Mode as TextMode, TextEngine};
use minicard_shell::gfx::Gfx;
use windows::core::Result;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::{ID2D1Bitmap1, D2D1_INTERPOLATION_MODE_LINEAR};

/// 卡面最多显示组数 / 条目数（与 03 稿一致：3 组、4 条；组内最多 2 条）。
pub const MAX_GROUPS: usize = 3;
pub const MAX_ROWS: usize = 4;

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

fn display_state(snap: Option<&NotifySnapshot>) -> CardState {
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
        let k = (w / 240.0).min(h / 180.0).clamp(0.4, 2.5);
        let r = 16.0 * k;
        Self {
            w,
            h,
            r,
            radius: r,
            pad_t: 0.75 * r,
            pad_l: 0.8125 * r,
            pad_b: 0.625 * r,
            pad_r: 0.8125 * r,
            header_fs: 0.78125 * r,
            header_lh: 0.9375 * r,
            count_fs: 0.59375 * r,
            count_lh: 0.75 * r,
            head_gap: 0.25 * r,
            group_gh: 1.0 * r,
            group_gap: 0.4375 * r,
            app_gap: 0.375 * r,
            app_fs: 0.71875 * r,
            app_lh: 0.875 * r,
            badge_fs: 0.53125 * r,
            badge_lh: 0.6875 * r,
            badge_h: 0.75 * r,
            badge_pad: 0.3125 * r,
            time_fs: 0.5625 * r,
            time_lh: 0.75 * r,
            item_h: 0.875 * r,
            item_gap: 0.1875 * r,
            item_indent: 1.375 * r,
            dot: 0.25 * r,
            body_fs: 0.65625 * r,
            body_lh: 0.875 * r,
            tile: 1.0 * r,
            tile_radius: 0.3125 * r,
            tile_fs: 0.5 * r,
            state_fs: 0.75 * r,
            state_lh: 1.0 * r,
            hint_fs: 0.625 * r,
            hint_lh: 0.875 * r,
            state_gap: 0.25 * r,
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

/// 按应用分组 + 可见配额：先每组 1 条，再把剩余配额轮流加给前几组（组内最多 2 条）。
pub fn group_items(items: &[NotifyItem], max_groups: usize, max_rows: usize) -> Vec<VisibleGroup> {
    if max_groups == 0 || max_rows == 0 {
        return Vec::new();
    }

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
            if take[g] < all[g].items.len().min(2) {
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

pub fn paint(
    g: &Gfx,
    te: &TextEngine,
    pal: &Palette,
    t: &Tokens,
    snap: Option<&NotifySnapshot>,
    now_ms: u64,
    glass: Option<(&ID2D1Bitmap1, D2D_RECT_F)>,
) -> Result<()> {
    let state = display_state(snap);
    let groups = match (state, snap) {
        (CardState::Normal, Some(s)) => group_items(&s.items, MAX_GROUPS, MAX_ROWS),
        _ => Vec::new(),
    };
    let tree = tree(t, snap, state, &groups, now_ms);
    let mut m: &TextEngine = te;
    let placed = layout(&tree, (t.w, t.h), &mut m);
    let hover_item = g
        .hover
        .and_then(|(mx, my)| hovered_item(&placed, mx, my));

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
        paint_tree(g, te, pal, t, snap, &groups, &placed, hover_item)?;
        g.pop_clip();
        g.end()?;
        Ok(())
    }
}

// ── 节点 id ────────────────────────────────────────────────────────────────
// 组头：9100 + g*10 + {0 行,1 应用块,2 名称,3 药丸,4 药丸字,5 时间}
// 条目：10000 + g*100 + i*10 + {0 行,1 未读点,2 文本行}
const ID_HEADER: u32 = 9001;
const ID_COUNT: u32 = 9002;
const ID_STATE: u32 = 9003;
const ID_HINT: u32 = 9004;

fn id_group(g: usize) -> u32 {
    9100 + g as u32 * 10
}
fn id_item(g: usize, i: usize) -> u32 {
    10_000 + g as u32 * 100 + i as u32 * 10
}
fn group_of(id: u32) -> Option<usize> {
    if (9100..9100 + MAX_GROUPS as u32 * 10).contains(&id) {
        Some(((id - 9100) / 10) as usize)
    } else {
        None
    }
}
fn item_of(id: u32) -> Option<(usize, usize)> {
    if (10_000..10_000 + MAX_GROUPS as u32 * 100).contains(&id) {
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

fn tree(
    t: &Tokens,
    snap: Option<&NotifySnapshot>,
    state: CardState,
    groups: &[VisibleGroup],
    now_ms: u64,
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
        })
        .child(
            text_node("通知", t.header_fs, t.header_lh, 600, ID_HEADER)
                .style(|s| s.grow(1.0).basis(0.0)),
        );

    let header = if state == CardState::Normal {
        let unread = snap.map(|s| s.unread_count()).unwrap_or(0);
        let text = if unread > 0 {
            format!("{unread} 条未读")
        } else {
            "全部已读".to_owned()
        };
        header.child(text_node(
            &text,
            t.count_fs,
            t.count_lh,
            400,
            ID_COUNT,
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

    let mut groups_col = Node::new(Dir::Column).style(move |s| {
        s.grow(1.0)
            .basis(0.0)
            .align(Align::Stretch)
            .justify(if groups.len() >= 2 {
                Justify::Between
            } else {
                Justify::Start
            })
    });

    for (g, vg) in groups.iter().enumerate() {
        let gid = id_group(g);
        let mut header_row = Node::new(Dir::Row)
            .id(gid)
            .style(move |s| {
                s.size(Dim::Pct(1.0), Dim::Px(t.group_gh))
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

        for i in 0..vg.items.len() {
            let iid = id_item(g, i);
            let row = Node::new(Dir::Row)
                .id(iid)
                .style(move |s| {
                    s.size(Dim::Pct(1.0), Dim::Px(t.item_h))
                        .pad([0.0, 0.0, 0.0, t.item_indent])
                        .align(Align::Center)
                        .gap(0.25 * t.r)
                })
                .child(
                    Node::new(Dir::Row)
                        .id(iid + 1)
                        .style(move |s| s.size(Dim::Px(t.dot), Dim::Px(t.dot))),
                )
                .child(
                    Node::new(Dir::Row)
                        .id(iid + 2)
                        .style(|s| s.grow(1.0).size(Dim::Auto, Dim::Px(t.item_h))),
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
                    .style(move |s| s.size(Dim::Px(0.875 * t.r), Dim::Px(t.item_h))),
                );
            group_col = group_col.child(row);
        }

        groups_col = groups_col.child(group_col);
    }

    root.child(groups_col)
}

fn paint_tree(
    g: &Gfx,
    te: &TextEngine,
    pal: &Palette,
    t: &Tokens,
    snap: Option<&NotifySnapshot>,
    groups: &[VisibleGroup],
    pl: &Placed,
    hover_item: Option<(usize, usize)>,
) -> Result<()> {
    // 组头：应用块 / 药丸
    if let Some(gidx) = group_of(pl.id) {
        let vg = groups.get(gidx);
        match pl.id - id_group(gidx) {
            0 => {
                // 行本身：不画；子节点递归
            }
            1 => {
                if let Some(vg) = vg {
                    paint_app_tile(g, te, pal, t, pl, vg)?;
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

    // 条目：hover 底 / 未读点 / 文本行 / 删除
    if let Some((gidx, iidx)) = item_of(pl.id) {
        let vg = groups.get(gidx);
        let item = vg
            .and_then(|vg| vg.items.get(iidx))
            .and_then(|&idx| snap.and_then(|s| s.items.get(idx)));
        let off = pl.id - id_item(gidx, iidx);
        match off {
            0 => {
                if hover_item == Some((gidx, iidx)) {
                    set_color(g, pal.fg_soft)?;
                    fill_round(g, rect(pl), 0.375 * t.r)?;
                }
            }
            1 => {
                if item.map(|i| i.unread).unwrap_or(false) {
                    set_color(g, pal.accent)?;
                    fill_round(g, rect(pl), pl.rect.w / 2.0)?;
                }
                return Ok(());
            }
            2 => {
                if let Some(item) = item {
                    paint_item_line(g, te, pal, t, pl, item)?;
                }
                return Ok(());
            }
            3 => {
                // ×：只在悬停到这一行时出现（默认态保持 03 稿的干净观感）
                if hover_item == Some((gidx, iidx)) {
                    if let Some(spec) = pl.text() {
                        draw_shadowed(
                            g,
                            spec,
                            pl.rect.x,
                            pl.rect.y,
                            pl.rect.w,
                            spec.line_h + 6.0,
                            TextMode::Clip,
                            pal.muted,
                            pal.shadow,
                            0.8335,
                            0.833,
                            te,
                        )?;
                    }
                }
                return Ok(());
            }
            _ => {}
        }
    }

    if let Some(spec) = pl.text() {
        let color = match pl.id {
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
        let mode = TextMode::Ellipsis;
        draw_shadowed(
            g,
            spec,
            pl.rect.x,
            pl.rect.y,
            pl.rect.w,
            spec.line_h + 6.0,
            mode,
            color,
            pal.shadow,
            0.8335,
            0.833,
            te,
        )?;
    }

    for c in &pl.children {
        paint_tree(g, te, pal, t, snap, groups, c, hover_item)?;
    }
    Ok(())
}

/// 鼠标落在哪一条的整行上（hover 底与 × 用同一份几何，避免各算各的漂移）。
fn hovered_item(p: &Placed, x: f32, y: f32) -> Option<(usize, usize)> {
    if let Some((g, i)) = item_of(p.id) {
        if p.id == id_item(g, i) {
            let r = p.rect;
            if x >= r.x && y >= r.y && x < r.x + r.w && y < r.y + r.h {
                return Some((g, i));
            }
        }
    }
    for c in &p.children {
        if let Some(hit) = hovered_item(c, x, y) {
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
    vg: &VisibleGroup,
) -> Result<()> {
    let r = rect(pl);
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
        _pal.shadow,
        0.5,
        0.5,
        te,
    )?;
    Ok(())
}

fn paint_item_line(
    g: &Gfx,
    te: &TextEngine,
    pal: &Palette,
    t: &Tokens,
    pl: &Placed,
    item: &NotifyItem,
) -> Result<()> {
    let sender = if item.title.trim().is_empty() {
        item.app.as_str()
    } else {
        item.title.as_str()
    };
    let full = if item.body.trim().is_empty() {
        sender.to_owned()
    } else {
        format!("{sender} · {}", item.body)
    };

    let line_w = pl.rect.w;
    let spec = TextSpec {
        s: sender.to_owned(),
        size: t.body_fs,
        line_h: t.body_lh,
        weight: 500,
        wrap: false,
        align: 0,
        tabular: false,
        spacing_em: 0.0,
    };
    let (sender_w, _) = te.measure(&spec, 0.0);
    // 发送者大致占一半以内且正文还有空间时，分两段画：发送者 fg、正文 muted。
    if !item.body.trim().is_empty() && sender_w < line_w * 0.62 && line_w - sender_w > 18.0 {
        draw_shadowed(
            g,
            &spec,
            pl.rect.x,
            pl.rect.y,
            sender_w + 1.0,
            t.body_lh + 6.0,
            TextMode::Clip,
            pal.fg,
            pal.shadow,
            0.8335,
            0.833,
            te,
        )?;
        let rest = TextSpec {
            s: format!(" · {}", item.body),
            size: t.body_fs,
            line_h: t.body_lh,
            weight: 400,
            ..spec
        };
        draw_shadowed(
            g,
            &rest,
            pl.rect.x + sender_w,
            pl.rect.y,
            (line_w - sender_w).max(1.0),
            t.body_lh + 6.0,
            TextMode::Ellipsis,
            pal.muted,
            pal.shadow,
            0.8335,
            0.833,
            te,
        )?;
    } else {
        let rest = TextSpec {
            s: full,
            weight: 400,
            ..spec
        };
        draw_shadowed(
            g,
            &rest,
            pl.rect.x,
            pl.rect.y,
            line_w,
            t.body_lh + 6.0,
            TextMode::Ellipsis,
            pal.muted,
            pal.shadow,
            0.8335,
            0.833,
            te,
        )?;
    }
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

/// 命中（D4 用）：条目 / 删除 / 授权引导。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Hit {
    Item { id: u32, aumid: String },
    Remove { id: u32 },
    Authorize,
}

pub fn hit_at(
    m: &mut dyn Measurer,
    t: &Tokens,
    snap: Option<&NotifySnapshot>,
    x: f32,
    y: f32,
) -> Option<Hit> {
    let snap = snap?;
    let state = card_state(snap.access, snap.items.len());
    let groups = if state == CardState::Normal {
        group_items(&snap.items, MAX_GROUPS, MAX_ROWS)
    } else {
        Vec::new()
    };
    let now_ms = 0; // 命中与时间文案无关
    let tr = tree(t, Some(snap), state, &groups, now_ms);
    let pl = layout(&tr, (t.w, t.h), m);
    let id = minicard_draw::layout::hit_id(&pl, x, y)?;
    if state == CardState::Unauthorized && (id == ID_STATE || id == ID_HINT) {
        return Some(Hit::Authorize);
    }
    let (g, i) = item_of(id)?;
    let item_idx = groups.get(g)?.items.get(i).copied()?;
    let item = snap.items.get(item_idx)?;
    if id - id_item(g, i) == 3 {
        return Some(Hit::Remove { id: item.id });
    }
    Some(Hit::Item {
        id: item.id,
        aumid: item.aumid.clone(),
    })
}

pub fn tree_hook(t: &Tokens, snap: Option<&NotifySnapshot>, now_ms: u64) -> Node {
    let state = display_state(snap);
    let groups = match (state, snap) {
        (CardState::Normal, Some(s)) => group_items(&s.items, MAX_GROUPS, MAX_ROWS),
        _ => Vec::new(),
    };
    tree(t, snap, state, &groups, now_ms)
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
        let groups = group_items(&sample(), MAX_GROUPS, MAX_ROWS);
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
            MAX_ROWS
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
    fn tree_fits_card_and_draws_four_rows() {
        let t = Tokens::notify(240.0, 180.0);
        let mut items = sample();
        items.truncate(6);
        let snap = NotifySnapshot {
            access: NotifyAccess::Allowed,
            items,
            at_ms: 1,
        };
        let node = tree_hook(&t, Some(&snap), 8000);
        let mut fake = Fake;
        let pl = layout(&node, (t.w, t.h), &mut fake);
        assert!((pl.rect.w - 240.0).abs() < 0.01);
        assert!((pl.rect.h - 180.0).abs() < 0.01);

        fn walk(p: &Placed, f: &mut dyn FnMut(&Placed)) {
            f(p);
            for c in &p.children {
                walk(c, f);
            }
        }
        let mut rows = 0;
        let mut groups = 0;
        let mut removes = 0;
        let mut line_widths: Vec<f32> = Vec::new();
        walk(&pl, &mut |p: &Placed| {
            if let Some((g, i)) = item_of(p.id) {
                if p.id == id_item(g, i) {
                    rows += 1;
                    assert!(
                        p.rect.x >= 0.0
                            && p.rect.y >= 0.0
                            && p.rect.x + p.rect.w <= 240.01
                            && p.rect.y + p.rect.h <= 180.01,
                        "条目行越界：{:?}",
                        p.rect
                    );
                } else if p.id == id_item(g, i) + 2 {
                    line_widths.push(p.rect.w);
                    assert!(
                        p.rect.x + p.rect.w <= 240.01,
                        "正文行越界：{:?}",
                        p.rect
                    );
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
    }

    #[test]
    fn hit_maps_row_and_remove_x() {
        let t = Tokens::notify(240.0, 180.0);
        let snap = NotifySnapshot {
            access: NotifyAccess::Allowed,
            items: sample(),
            at_ms: 1,
        };
        let node = tree_hook(&t, Some(&snap), 8000);
        let mut fake = Fake;
        let pl = layout(&node, (t.w, t.h), &mut fake);

        fn find(p: &Placed, id: u32) -> Option<minicard_draw::layout::Rect> {
            if p.id == id {
                return Some(p.rect);
            }
            for c in &p.children {
                if let Some(r) = find(c, id) {
                    return Some(r);
                }
            }
            None
        }

        let row = find(&pl, id_item(0, 0)).unwrap();
        let remove = find(&pl, id_item(0, 0) + 3).unwrap();
        let mut m1 = Fake;
        assert_eq!(
            hit_at(&mut m1, &t, Some(&snap), row.x + 20.0, row.y + row.h / 2.0),
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
                remove.x + remove.w / 2.0,
                remove.y + remove.h / 2.0
            ),
            Some(Hit::Remove { id: 6 })
        );
    }
}
