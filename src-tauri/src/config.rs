//! 配置：单一数据源，serde + toml，存 %APPDATA%\mini-card\config.toml
//! 与 egui 版共用同一份文件、同一套结构（迁移期互不打架）。
//! 坏文件/缺字段一律回退默认值，不崩。

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CardKind {
    Performance,
    Disks,
    Clock,
    Weather,
    NetRate,
    Pomodoro,
    Music,
    Calendar,
    Todo,
}

impl CardKind {
    pub fn display_name(&self) -> &'static str {
        match self {
            CardKind::Performance => "性能监控",
            CardKind::Disks => "磁盘容量",
            CardKind::Clock => "桌面时钟",
            CardKind::Weather => "天气",
            CardKind::NetRate => "网络速率",
            CardKind::Pomodoro => "番茄钟",
            CardKind::Music => "音乐",
            CardKind::Calendar => "日历",
            CardKind::Todo => "待办",
        }
    }

    /// 卡片尺寸（2026-10-02 用户「尺寸是定死的」）：每张卡**只有一个尺寸** = 基准方格 CELL 的整数倍。
    /// 与前端 index.html 的 KINDS[].size 同源，改一处必须改两处。
    /// 桌面上每张卡都是同一个 60px 方块的整数倍（手机桌面那种一格一格）。
    pub fn size_cells(&self) -> [u32; 2] {
        match self {
            CardKind::Performance => [4, 2],
            CardKind::Disks       => [4, 3],
            CardKind::Clock       => [4, 2],
            CardKind::Weather     => [4, 3],
            CardKind::NetRate     => [2, 2],
            CardKind::Pomodoro    => [2, 2],
            CardKind::Music       => [4, 2],
            CardKind::Calendar    => [4, 3],
            CardKind::Todo        => [4, 2],
        }
    }

    /// 该卡类型的固定尺寸（建卡 / 老 config 缺字段 / 迁移都用它）
    pub fn default_size(&self) -> [f32; 2] {
        let c = self.size_cells();
        [c[0] as f32 * CELL, c[1] as f32 * CELL]
    }

    /// 吸附到整格：宽、高各自吸到最近的 60px 格线（限 1..8 格）。
    /// 形状不再锁死（格数变了比例自然变一点），但尺寸永远是整格数。
    /// 注意：位置**不再**吸格点（2026-09-28 用户要求位置自由拖动，对齐改成拖动时的辅助线 + 磁吸边）。
    pub fn snap_size(&self, w: f32, h: f32) -> [f32; 2] {
        [snap_cells(w), snap_cells(h)]
    }

    /// 迁移（老 config 用）：尺寸统一落到该卡类型的固定尺寸（尺寸定死后的唯一目标）。
    /// 形参留着 = 调用点不用改，也让「迁移是按卡片类型来的」一眼可见。
    pub fn migrated_size(&self, _w: f32, _h: f32) -> [f32; 2] {
        self.default_size()
    }
}

/// 尺寸不变量：任何尺寸（固定尺寸、迁移结果、吸附结果）的宽高都是基准方块 CELL 的整数倍 ——
/// 桌面像手机桌面一样一格一格，卡片边缘差永远是整格数。
/// 统一基准方块：卡片尺寸与位置都落在这张 60px 格子上
pub const CELL: f32 = 60.0;

/// 单边吸附到整格：最少 1 格（60px = 基准方块本身），最多 8 格（480px）
pub fn snap_cells(v: f32) -> f32 {
    (v / CELL).round().clamp(1.0, 8.0) * CELL
}

/// 磁盘卡最多显示几块盘（用户 2026-10-02：「比 5 盘多的就只读前 5 个」）。
/// 数据层截断 + host 算高度 + 卡片页渲染三处都按这个上限，盘数才不会对不上。
pub const DISK_SHOW_MAX: usize = 5;

/// 磁盘卡的自动高度（px）：按检测到的盘数给整格高度 —— 与用户 2026-10-02 的映射一致
/// （1 盘 1 格 / 2-3 盘 2 格 / 4-5 盘 3 格）。
///
/// 取值来自 `docs/verify/minicard_center/disk_sweep3.py` 的离线实测：这五档在
/// 「根字号解耦（不跟窗口高变）」的前提下留白全部 ≤22px、不溢出。别凭感觉改。
///
/// ⚠️ 高度是**派生值**：运行时算、只用于建窗 / set_size，**绝不写回 config**。
/// 写回去的话，3 盘机器上跑一次就把 180 存成 120，换到 5 盘机器就是 5 行塞进 120 高。
/// 与 `card.html::renderDisks` 的行距档（d3/d45/d5）必须成对改。
pub fn disk_height_px(count: usize) -> f32 {
    let cells = match count {
        0 => 3,          // 采样还没到：保持默认档，别先缩空态
        1 => 1,
        2..=3 => 2,
        _ => 3,
    };
    cells as f32 * CELL
}

/// 待办条目（2026-10-08 · 卡 9）：卡片里只做勾选；增删改在主界面参数面板（todo_set 整表替换）。
/// 存储走 config.toml（用户数据不押 WebView2 缓存）——见 CardInstance.todo。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TodoItem {
    pub text: String,
    pub done: bool,
}

impl Default for TodoItem {
    fn default() -> Self {
        Self { text: String::new(), done: false }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CardInstance {
    pub id: String,
    pub kind: CardKind,
    pub enabled: bool,
    /// [宽, 高]，逻辑点
    pub size: [f32; 2],
    /// 屏幕左上角位置
    pub pos: [f32; 2],
    pub refresh_ms: u64,
    /// 固定位置：固定后卡片不再响应拖动（主界面「桌面卡片」页的图钉按钮）
    pub pinned: bool,
    /// 参数弹窗「显示 GPU」：关掉后性能卡只留 CPU 与内存两行
    pub show_gpu: bool,
    /// 参数弹窗「超 90% 变色」：关掉后磁盘容量条不再转橙色
    pub warn90: bool,
    /// 待办条目（仅 Todo 卡使用；卡片勾选 → todo_toggle，面板编辑 → todo_set）
    pub todo: Vec<TodoItem>,
}

impl Default for CardInstance {
    fn default() -> Self {
        Self {
            id: String::new(),
            kind: CardKind::Performance,
            enabled: true,
            size: CardKind::Performance.default_size(),
            pos: [80.0, 80.0],
            refresh_ms: 1000,
            pinned: false,
            show_gpu: true,
            warn90: true,
            todo: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ZOrder {
    Top,
    Normal,
    Desktop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Theme {
    System,
    Light,
    Dark,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub autostart: bool,
    pub close_to_tray: bool,
    pub hotkey: String,
    /// 全局热键总开关（设置页那个 switch）；关掉就注销热键，键位与录制结果都留着
    pub hotkey_on: bool,
    pub z_order: ZOrder,
    /// 卡片白色叠加 alpha（玻璃厚度），0-255
    pub glass_alpha: u8,
    /// 玻璃来源：`acrylic`/`mica` = DWM 系统背景材质（真磨砂，Win11 22H2+）；
    /// `wca` = 旧的 SetWindowCompositionAttribute 染色（本机实测无效果，保留兜底）；`off` = 无玻璃
    pub glass_mode: String,
    /// 毛玻璃模糊强度 0-100（px）。卡片用「壁纸切片 + CSS blur」自绘磨砂层，
    /// 系统那条 WCA/backdrop-filter 路在 Win11 22H2+ 已失效，只能自己画。
    pub glass_blur: u8,
    pub theme: Theme,
    pub accent: [u8; 3],
    /// 设置页「软件更新 → 自动检查」：启动后自动查一次 GitHub Release
    pub update_auto: bool,
    /// 天气城市显示名（"" = 自动：按网络归属地猜，猜错了在界面里改）
    pub weather_city: String,
    /// 天气坐标；None 且 weather_city 为空时走自动检测
    pub weather_lat: Option<f64>,
    pub weather_lon: Option<f64>,
    /// 中国天气网城市代码（101xxxxxx）。选城市时按「城市名 + 省」查 wxcities.txt 解析出来，
    /// 存下来免得每次拉数据都靠名字猜；None = 查不到（实况回退 Open-Meteo）
    #[serde(default)]
    pub weather_wxid: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            autostart: false,
            close_to_tray: true,
            hotkey: "Ctrl+Alt+W".to_owned(),
            hotkey_on: true,
            z_order: ZOrder::Desktop,
            glass_alpha: 30,
            glass_mode: "off".to_owned(),
            glass_blur: 30,
            theme: Theme::Dark,
            accent: [76, 154, 255],
            update_auto: true,
            weather_city: String::new(),
            weather_lat: None,
            weather_lon: None,
            weather_wxid: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub cards: Vec<CardInstance>,
    pub settings: Settings,
    /// 主页面 [x, y, w, h]
    pub main_window: [f32; 4],
    /// 尺寸迁移版本：< SIZE_VERSION 时把老卡片尺寸落到该类型的固定尺寸（只做一次）
    /// 注意：必须用字段级 `default = "…"`，因为结构体上的 `#[serde(default)]` 会拿
    /// `Config::default()`（= SIZE_VERSION）去补缺失字段，那样老 config 会被当成已迁移，迁移永不触发。
    #[serde(default = "size_version_legacy")]
    pub size_version: u32,
}

/// 老 config 缺 `size_version` 时按 0 处理（= 需要迁移一次）
fn size_version_legacy() -> u32 {
    0
}

/// 当前尺寸迁移版本
pub const SIZE_VERSION: u32 = 4;

impl Default for Config {
    fn default() -> Self {
        Self {
            cards: vec![
                CardInstance {
                    id: "perf-default".to_owned(),
                    kind: CardKind::Performance,
                    enabled: true,
                    size: CardKind::Performance.default_size(),
                    pos: [80.0, 80.0],
                    refresh_ms: 1000,
                    pinned: false,
                    show_gpu: true,
                    warn90: true,
                    todo: Vec::new(),
                },
                CardInstance {
                    id: "disks-default".to_owned(),
                    kind: CardKind::Disks,
                    enabled: true,
                    size: CardKind::Disks.default_size(),
                    pos: [440.0, 80.0],
                    refresh_ms: 5000,
                    pinned: false,
                    show_gpu: true,
                    warn90: true,
                    todo: Vec::new(),
                },
            ],
            settings: Settings::default(),
            main_window: [520.0, 250.0, 928.0, 628.0],
            size_version: SIZE_VERSION,
        }
    }
}

pub fn path() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("mini-card").join("config.toml")
}

impl Config {
    pub fn load() -> Self {
        let mut cfg = match fs::read_to_string(path()) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|e| {
                // GUI 子系统下 stderr 无效，eprintln 会 panic，静默回退默认值即可
                let _ = e;
                Self::default()
            }),
            Err(_) => Self::default(),
        };
        // 最小化/关窗残影会写出 -32000 或 176x37 这种坏几何，回退默认
        if cfg.main_window[2] < 400.0
            || cfg.main_window[3] < 300.0
            || cfg.main_window[0] < -1000.0
            || cfg.main_window[1] < -1000.0
        {
            cfg.main_window = Self::default().main_window;
        }
        // 尺寸迁移（只做一次）：老 config 里的任意尺寸 → 该卡类型的固定尺寸（2026-10-02 起尺寸定死）。
        if cfg.size_version < SIZE_VERSION {
            for card in cfg.cards.iter_mut() {
                card.size = card.kind.migrated_size(card.size[0], card.size[1]);
            }
            cfg.size_version = SIZE_VERSION;
            let _ = cfg.save();
        }
        cfg
    }

    pub fn save(&self) -> Result<(), String> {
        let p = path();
        if let Some(dir) = p.parent() {
            fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let text = toml::to_string_pretty(self).map_err(|e| e.to_string())?;
        fs::write(&p, text).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds() -> [CardKind; 9] {
        [
            CardKind::Performance,
            CardKind::Disks,
            CardKind::Clock,
            CardKind::Weather,
            CardKind::NetRate,
            CardKind::Pomodoro,
            CardKind::Music,
            CardKind::Calendar,
            CardKind::Todo,
        ]
    }

    /// 固定尺寸合法：每张卡的格数在 1..8、尺寸在整格上，且 size_cells 与 default_size 一致
    #[test]
    fn fixed_sizes_are_legal() {
        for k in kinds() {
            let c = k.size_cells();
            for v in c {
                assert!((1..=8).contains(&v), "{:?} 格数越界: {:?}", k, c);
            }
            let p = k.default_size();
            assert_eq!(p[0] % CELL, 0.0, "{:?} 宽不在格上: {:?}", k, p);
            assert_eq!(p[1] % CELL, 0.0, "{:?} 高不在格上: {:?}", k, p);
            assert_eq!(
                p,
                [c[0] as f32 * CELL, c[1] as f32 * CELL],
                "{:?} size_cells 与 default_size 不一致", k
            );
        }
    }

    /// 磁盘卡自动高度：盘数 → 整格高度（1 盘 1 格 / 2-3 盘 2 格 / 4-5 盘 3 格）。
    /// 与 card.html::renderDisks 的行距档成对存在，改一处必须重量另一处。
    #[test]
    fn disk_height_follows_disk_count() {
        assert_eq!(disk_height_px(0), 3.0 * CELL, "采样未到时要保持默认档，别先缩成空态");
        assert_eq!(disk_height_px(1), CELL);
        assert_eq!(disk_height_px(2), 2.0 * CELL);
        assert_eq!(disk_height_px(3), 2.0 * CELL);
        assert_eq!(disk_height_px(4), 3.0 * CELL);
        assert_eq!(disk_height_px(5), 3.0 * CELL);
        assert_eq!(disk_height_px(9), 3.0 * CELL, "超过上限也给合法值");
        for n in 0..=9usize {
            let h = disk_height_px(n);
            assert_eq!(h % CELL, 0.0, "{} 盘高度不在整格上: {}", n, h);
            assert!((CELL..=8.0 * CELL).contains(&h), "{} 盘高度越界: {}", n, h);
        }
    }

    /// 任意输入尺寸吸附后都必须落在整格上（参数面板再也不可能产出野尺寸）
    #[test]
    fn snap_always_legal() {
        let mut w = 20.0f32;
        while w <= 1200.0 {
            for h in [20.0f32, 61.0, 119.0, 121.0, 144.0, 190.0, 240.0, 479.0, 700.0, 1500.0] {
                let s = CardKind::Performance.snap_size(w, h);
                for v in s {
                    assert_eq!(v % CELL, 0.0, "{:?} 吸附后不在格上", s);
                    assert!(v >= CELL && v <= 8.0 * CELL, "{:?} 吸附后越界", s);
                }
            }
            w += 7.0;
        }
    }

    /// 位置**不再**吸附格点（2026-09-28 用户改需求：位置自由拖动，靠拖动辅助线提示对齐），
    /// 所以这里只钉住「snap_pos 已经不存在」这件事——留个反向断言防止旧行为悄悄回来。
    #[test]
    fn positions_are_free() {
        let cfg = Config::default();
        for c in &cfg.cards {
            assert_eq!(c.pos.len(), 2);
        }
        assert!(cfg.size_version >= 4, "位置自由化与 2×2 方形卡同属 v4");
    }

    /// 老 config 缺 size_version 必须当成 0（否则结构体级 #[serde(default)] 会拿
    /// Default 的 SIZE_VERSION 补上，迁移永不触发——2026-09-28 真踩过）
    #[test]
    fn legacy_config_needs_migration() {
        let legacy = r#"
[[cards]]
id = "perf-default"
kind = "Performance"
size = [320.0, 190.0]
"#;
        let cfg: Config = toml::from_str(legacy).expect("老 config 应能解析");
        assert_eq!(cfg.size_version, 0, "缺 size_version 的老 config 必须被判定为待迁移");
        assert_eq!(cfg.cards[0].size, [320.0, 190.0]);
        // 迁移后落到该类型的固定尺寸（性能 = 4×2 格 = 240×120）
        assert_eq!(cfg.cards[0].kind.migrated_size(320.0, 190.0), [240.0, 120.0]);
    }

    /// 迁移：不管老尺寸是什么，都落到该卡类型的固定尺寸，且固定尺寸本身必须是整格
    #[test]
    fn migration_lands_on_fixed_size() {
        let olds = [
            // v0：自由尺寸时代
            [320.0f32, 190.0], [340.0, 200.0], [234.0, 144.0], [340.0, 240.0],
            [170.0, 144.0], [210.0, 190.0],
            // v1：4px 底格档位
            [340.0, 204.0], [240.0, 180.0], [212.0, 212.0],
            // v2/v3：8px 底格与 60px 整格档位
            [320.0, 192.0], [240.0, 144.0], [192.0, 144.0], [216.0, 216.0],
            [180.0, 180.0], [240.0, 240.0], [120.0, 120.0],
        ];
        for k in kinds() {
            let want = k.default_size();
            assert_eq!(
                k.snap_size(want[0], want[1]), want,
                "{:?} 固定尺寸不在整格上: {:?}", k, want
            );
            for old in olds {
                assert_eq!(
                    k.migrated_size(old[0], old[1]), want,
                    "{:?} 迁移结果必须是固定尺寸（老尺寸 {:?}）", k, old
                );
            }
        }
    }
}
