# 证据表 — font-swap-ms-default

> 实施 + 复核后回填（2026-11-19）。所有日志/截图在 `.engineering/font-swap-ms-default/` 与 `docs/verify/font-swap-ms-default/`。

## AC 对照表

| AC | 判定步骤 / 命令 | 实际输出（证据） | 结论 |
|---|---|---|---|
| AC1 全栈无遗漏 | `grep -rn "MiSans" web/ native/*/src` + 独立复核逐面核对 + 子页 CDP | web 五页生效 `@font-face` 全删、`--font-body`=雅黑栈（verify-live.log STYLE、subpages.json 三页）；native 产品路径 `from_system`（main.rs:818、live.rs:3926）；剩余 MiSans 命中均为注释/测试夹具/m0-spike（不随产品发）。D1 已清 src-tauri 内嵌：assets.rs 只剩 NATIVE_BIN，主 exe 21.53MB（原 ~49MB） | 通过 |
| AC2 字重语义保真 | `text.rs` 字重映射单测 + 真机 computed + native 字面表日志 | `系统雅黑字面表与字重映射` 测试：400→400、500→400、600→700（test-draw-r3.log 59 passed）；真机 h1 computed weight=600（verify-live.log），截图无发块/无宽度溢出（ms_page-page-desk.png）；native 日志 `字体 系统雅黑载入 ascent 1.0581em / upem 2048` | 通过 |
| AC3 tnum 数字稳定 | native 四卡 dump + CDP mono 栈 | disks/perf/calendar 卡数值串宽度稳定（cards_sheet.png，逐秒刷新的数字等宽）；`--font-mono`='Segoe UI',Cascadia,Consolas（verify-live.log）；native tnum feature 经 SetTypography 应用（text.rs:182-188） | 通过 |
| AC4 缺字回退 | AC4 单测（20 字探针）+ native 日志 | `系统字体集覆盖_gb2312_外汉字` 测试：20 个 GB2312 外汉字（䶮/㐀/龘/𠀀 等）在系统字体集全部有字形，miss=[]（test-draw-r2.log）；from_system 走 GetSystemFontCollection，字体回退 base 变系统集，根治私有字集豆腐块 | 通过 |
| AC5 真机无回归 | 主界面五页 + 子页三页 CDP | 主界面 5 页 `scrollW=992=innerW, bad=[]`（verify-live-r2.log）；card/dock/menu 三页字体栈正确、无溢出（subpages.json）；native 四卡 dump 布局正常、右留白充足（cards_sheet.png） | 通过 |
| AC6 编译与单测 | `cargo test`（draw/cards/bin）+ `cargo build --release`（bin/main） | draw 59 / cards 127 / bin 46 全绿（test-draw-r3.log / test-cards.log / test-bin.log）；release 两个 0 警告（build-bin-r2.log / build-main-r3.log）。唯一警告是既有 `layout.rs:596 R 常量未使用`，与本次无关 | 通过 |
| AC7 recompile 生效 | exe mtime/hash + 内嵌一致性 + 真机启动 | main exe mtime 16:42:30 > 所有改动；内嵌 native exe hash `77FD3EC7` = 源 native（修正了并行编译时序导致的内嵌旧版问题，见 DEFECTS D9）；真机启动主程序 PID3972 + 绘制进程 PID16548，APPDATA native 已更新为 `77FD3EC7`（16:42:59）；spawn 日志 `--live --blur 30`（无 --font） | 通过 |
| AC8 独立复核 | 只读子代理复核 + 缺陷收敛 | 首轮复核发现 D1–D8（5×S3+3×S4），全部处置/回填；第二轮复核收敛，见 DEFECTS.md | 通过 |

## 关键证据原文摘录

1. `verify_font_real.py`：canvas 量字宽 `bodyW=216.84` = 雅黑 `216.84` ≠ MiSans `209.97`，`document.fonts.check('Microsoft YaHei')=true` → 证明真实渲染用雅黑，非仅声明（font_real.json）。
2. native 日志：`字体 系统雅黑载入：ascent 1.0581em / descent 0.2617em / upem 2048`（MiSans 是 upem 1000/ascent 1.326em）→ native 确实走系统字体集。
3. D4 A/B：`MC_SMALL_EMBOLD=0/1` 两档 todo 卡 dump **逐字节完全相同**（不同字节 0、墨量差 0）→ 证明雅黑下小字补墨是空操作；已在 `embold_weight` 调用处显式跳过无效升档。
4. 第 6/7 轮历史归因（mainui-text-clarity/EVIDENCE.md:37）：雅黑 computed=500 实落 Regular、400≡500 逐像素相同；+195% 收益来自字号+灰字加深+换字形，非字重 → 本轮字重调整目标是语义保真。

## 复跑命令索引

```bat
:: web 主界面 + 子页验证
python docs\verify\font-swap-ms-default\verify_live.py
python docs\verify\font-swap-ms-default\verify_font_real.py
python docs\verify\font-swap-ms-default\verify_subpages.py
:: native 单测（含字重映射 + 缺字探针）
cd native\minicard-draw && cargo test
:: native 卡片出图
native\minicard-bin\target\release\minicard-bin.exe --probe clock --w 240 --h 120 --no-glass --dump out.bgra --exit-after 3000
python docs\verify\font-swap-ms-default\bgra2png.py out.bgra out.png 240 120 2
```
