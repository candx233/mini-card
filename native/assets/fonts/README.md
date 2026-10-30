# 自绘卡字体（MiSans TTF 子集）

DirectWrite **不能**直接读 woff2，所以 `web/fonts/*.woff2` 必须转成 ttf 再子集。产物由脚本生成，**不进 git**（`.gitignore` 里有 `assets/fonts/*.ttf`），打包时跑一遍脚本即可。

```bash
python native/assets/fonts/build_misans_ttf.py            # 生成（需要 fonttools + brotli）
python native/assets/fonts/build_misans_ttf.py --verify    # 校验度量 / 缺字
```

## 口径

| 档 | 字符集 | 产物 | 为什么 |
|---|---|---|---|
| 400 Regular | GB2312 + `web/*.html` 全字符（7563 字） | 1670 KB | 待办条目、正文 —— 会显示用户输入的中文 |
| 600 Semibold | 同上 | 1653 KB | 歌名/歌手、城市名、日历标题 —— 也是动态中文 |
| 500 Medium | **仅**界面文案（1180 字） | 256 KB | 只用于温度数字（`.w-temp`）与节日名（`.cal-holiday` 是固定表） |
| **合计** | | **3579 KB** | 现版 WebView2 用的是 4 档 woff2 共 3.8 MB，基本持平 |

- **GB2312 不能省**：歌名/歌手/待办/城市名都是任意中文，窄子集会出现豆腐块。超出 GB2312 的字与现版一样回退系统字体（Microsoft YaHei），行为不变。
- 700 Bold 只有托盘菜单用（那条线留在 Tauri），**不生成**。
- 度量必须保持一致：`upem 1000 / hhea 1044,-282 / OS/2 win 1044,282`（脚本 `--verify` 会卡这三项，变了行盒基线整体偏）。

## 实测

- 子集版与全量 7.7 MB TTF 在同一个渲染器里**逐像素完全相同**（240×120 性能卡：平均差 0.000）。
- 与 WebView2 版对照仍是平均差 4.00/255（字体子集化没有引入任何观感变化）。

## ⚠️ MiSans 本身没有这些字符（运行时回退系统字体）

`⏮ ⏭ ↔ ⇧ ⌘ ⎋ ⏎ ◻ ⚠ ・`

**音乐卡的 ⏮/⏭ 按钮用的就是这两个字形**——在浏览器里今天也是回退到 Segoe UI Symbol 渲染的。自绘这边不要指望字体：E2 段把它们**画成路径**（两个三角 + 两条竖线），否则字形会随系统字体版本变。
