# web/fonts —— 内嵌界面字体（MiSans）

本目录是应用界面字体的唯一来源，随前端资产一起编进 exe / 发布包。

## 文件

| 文件 | CSS 映射 | 说明 |
|---|---|---|
| `MiSans-Regular.woff2`  | `font-weight: 400` | 官方 Regular 母版的字形子集 |
| `MiSans-Medium.woff2`   | `font-weight: 500` | 官方 Medium 母版的字形子集 |
| `MiSans-Semibold.woff2` | `font-weight: 600` | 官方 Semibold 母版的字形子集 |
| `MiSans-Bold.woff2`     | `font-weight: 700` | 官方 Bold 母版的字形子集 |
| `MiSans-License.pdf`    | — | 小米官方许可协议原文（随软件分发，保留版权声明） |
| `build_misans_subset.py`| — | 子集生成脚本（官方字体包不入库，只有生成的 woff2 入库） |

@font-face 写在 `web/card.html` 与 `web/index.html` 的 `<style>` 顶部；四档映射与小米官方 web
用法一致（400/500/600/700 → 上表四档）。字体族名 `MiSans` 在 CSS 里排第一位，系统字体栈兜底缺字。

## 授权（2026-10-10 依官方 FAQ / 许可协议核对）

- MiSans 由小米免费提供、**免费商用**（含商业发布），且**允许嵌入**到软件/应用中使用；
  官方 FAQ 要求：**在软件中特别注明使用了 MiSans 字体**（已写进主界面「设置」页版本行下方）。
- 本目录字体为**未改动字形的子集**（仅按字符集裁剪 + woff2 压缩），字形/字体名/表结构均未改动；
  版权归小米科技有限责任公司所有，适用《MiSans 字体知识产权许可协议》（`MiSans-License.pdf`），
  **不在本仓库 GPL-3.0 授权范围内**。
- 官方来源：<https://hyperos.mi.com/font/>（许可问答：<https://hyperos.mi.com/font/zh/faq/>）

## 重新生成

    uv run --with fonttools --with brotli python web/fonts/build_misans_subset.py <MiSans.zip>

官方字体包（227MB，含 ttf/woff2/otf/可变字体）不入库；字符集 = GB2312 全量 + 仓库源码全部字符 +
常用符号块，缺字由 CSS 回退栈兜底（见脚本内注释）。
