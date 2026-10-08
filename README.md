# Mini Card

> Rust + WebView2 打造的 Windows 桌面玻璃卡片。

**状态：`0.1.0` 已发布。**

有问题随时反馈哦，我会收集问题逐一修复的

## 样式展示

<img src="https://img.candx.cn/blog/minicard/cards.png" width="820" alt="桌面卡片：桌面时钟 / 天气 / 性能监控 / 磁盘容量 / 网络速率 / 番茄钟">

<img src="https://img.candx.cn/blog/minicard/maininterface.png" width="820" alt="主界面：卡片库 / 桌面卡片 / 外观 / 设置">

## 特性

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

## License

[GPL-3.0](LICENSE)

Copyright (c) 2026 candx233 (https://candx.cn)
