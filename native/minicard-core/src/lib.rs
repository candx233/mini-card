//! mini-card 卡片端共享内核。
//!
//! 迁移动机（0.1.2 自绘卡，见 `docs/TODO-012-renderer.md` A 段）：`config.toml`、采样快照、
//! 天气与媒体数据的抓取**必须两个外壳共用同一份实现**（Tauri 主界面 + 自绘卡进程），
//! 否则字段一旦漂移就是「配置莫名回退」那类事故。
//!
//! 这里只放**跟 UI 框架无关**的代码；广播（`emit`）、窗口、托盘、命令 ACL 全在各自外壳里：
//!   · `config`  —— 配置结构与读写（纯 std + serde + toml）
//!   · `data`    —— CPU/内存/磁盘/网络/GPU 采样（sysinfo + PDH）
//!   · `weather` —— 天气抓取与文案映射；`refresh_local` 只取数不广播
//!   · `media`   —— SMTC 音乐会话；`Sampler::tick()` 只出 payload，广播由外壳做
//!   · `util`    —— base64 等小工具

pub mod config;
pub mod data;
pub mod media;
pub mod util;
pub mod weather;

pub use util::base64_encode;
