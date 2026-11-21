# Mini Card

> Rust 原生渲染 + WebView2 主界面的 Windows 桌面玻璃卡片。

**当前版本：`0.1.5` 。**

有问题随时反馈哦，我会收集问题逐一修复的

## 样式展示

<img src="https://img.candx.cn/blog/minicard/cards.png" width="820" alt="桌面卡片：桌面时钟 / 天气 / 性能监控 / 磁盘容量 / 网络速率 / 番茄钟">

<img src="https://img.candx.cn/blog/minicard/maininterface_dark.png" width="820" alt="主界面：卡片库 / 桌面卡片 / 外观 / 设置">

## 特性

- **十张卡全部 Rust 原生自绘**（D2D + DWrite + DComp）：性能、磁盘、时钟、天气、网络、番茄钟、音乐、日历、待办、通知由一个绘制进程统一绘制；Dock 也是原生绘制。主界面仍是 WebView2。
- **通知卡（新，默认关闭）**：读取 Windows 通知中心，按应用分组显示在桌面；首次开启会弹隐私说明（读全部应用的通知正文、只留在内存、不落盘、可随时关闭）。Win11 优先，Win10 尽力支持。
- **真桌面层**：卡片常驻壁纸之上、普通窗口之下，点击不激活、不抢焦点、不遮住工作窗口
- **磨砂玻璃**：卡片背景是自绘的壁纸切片 + 模糊，玻璃厚度与模糊强度可调，不依赖系统已失效的接口
- **整洁性**：卡片尺寸都是固定比例，位置自由拖动；拖动时与别的卡片或屏幕边对上就磁吸并画出辅助线
- **主界面管理**：卡片库 / 桌面卡片 / 外观 / 设置 四页，加卡、删卡、参数、外观都在这里
- **托盘、单实例、开机自启、位置持久化**

## 其他

欢迎访问我的BLOG：candx.cn

## 数据来源

- 天气实况：中国天气网
- 预报 / 空气质量 / 地理编码：Open-Meteo（CC-BY 4.0）
- 城市代码表：基于 ruixingchen/ChinaCityList（MIT）

## 字体

界面字体使用 **Windows 系统默认字体**（中文 = 微软雅黑 Microsoft YaHei，西文/数字 = Segoe UI），
随系统安装与更新，无需随包分发字体文件。

历史版本曾内嵌 MiSans（小米免费提供）子集于 [`web/fonts/`](web/fonts) 与 `native/assets/fonts/`，
现已停用（2026-11-19 全栈换系统字体），这些文件仅作历史稿留存、不再被程序加载。
MiSans 字体版权归小米科技有限责任公司所有，适用《MiSans 字体知识产权许可协议》，
**不在本仓库 GPL-3.0 的授权范围内**。

## License

[GPL-3.0](LICENSE)

Copyright (c) 2026 candx233 (https://candx.cn)
