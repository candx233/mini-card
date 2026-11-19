# 验收证据 · 通知卡 × 放大与悬停交互动画

## AC 对照表

| AC | 命令 / 步骤 | 实际输出摘要 | 结论 |
|---|---|---|---|
| AC1 | `cargo test`（minicard-cards）里 `close_x_is_bigger_without_touching_other_geometry`；`--probe notify --debug-layout` 日志 | token `close_fs=13.00`、`close_w=18.00`（旧 10/14）；布局 `id=10003 x=209 w=18 h=38`、`id=10002 w=159.50`；4 项新单测全绿 | 通过 |
| AC2 | `hit_maps_row_and_remove_x`（既有）+ 上述单测的正文宽断言 | 点 × 中心仍 `Some(Hit::Remove { id: 6 })`；正文 159.5px > 100px；127 passed | 通过 |
| AC3 | `close_hover_tween_follows_120ms_ease_out_quart`；`--probe notify-motion=hover` 帧序列 + Python 像素差分 | 补间中点 (0.5,1)、120ms=1.0、移出 120ms=0；行-only vs X-hover 帧差 bbox=(209,74)-(227,92)、232 像素、X 格外 0 像素 | 通过 |
| AC4 | 既有 hover 单测 + `frames/compare-x-hover-isolated.png` | 行 hover 态 × 仍以 muted 淡入（左图）；鼠标不在 × 上时 close_hover=0，只在 × 上才变橙放大（右图） | 通过 |
| AC5 | `close_hover_participates_in_frame_scheduling` | 补间中 `animating(now)=true`、退场补间中 `animating(later)=true`、prune 后 map 空且 `animating(end)=false` | 通过 |
| AC6 | `analyze_seq.py` / `compare_hover.py` 逐像素差分；单测行高/正文宽断言 | 整卡 diff 只在 hover 行 (13,64)-(227,102)（行 hover 底色为既有行为）；隔离 × 动画后 diff 只在 (209,74)-(227,92)，`outside_X_cell=0`；行高 38 不变 | 通过 |
| AC7 | `cargo test`（minicard-cards）、`cargo check`（minicard-bin） | 基线 123 passed → 改后 **127 passed / 0 failed**；bin check `Finished dev profile ... in 5.38s` | 通过 |
| AC8 | `--probe notify-motion=hover --ss 1/2` 出图 + `frames/compare-static-vs-xhover.png`、`frames/compare-x-cell-1x.png`、`frames/compare-x-1x-sharpness.png`、`frames/seq/hover-tween-strip.png` | 静态 vs hover 并排图明确显示 × 由「不可见」变「放大+橙红+底圈」；1x 下字形清晰无裁切；2x 缩回 1x 与原生 1x 一致；独立检验 AC1–AC7 全通过，AC8 仅因早期证据交付物无效（V1）部分通过，已修复 | 通过（V1 已修） |

## 视觉证据索引

- `frames/compare-static-vs-xhover.png`：**最终静态 vs × hover 并排图**（seq f000 / f010，`notify-motion=hover`）
- `frames/compare-x-hover-isolated.png`：同一时刻「仅行 hover」vs「× hover」整卡对比
- `frames/compare-x-cell-1x.png`：× 单元 1x 原像素放大，无裁切/发糊
- `frames/compare-x-1x-sharpness.png`：2x 缩回 1x vs 原生 1x
- `frames/seq/hover-tween-strip.png`：补间 0 → 67ms → 167ms 三帧
- `frames/seq/x-crop-f000.png` / `x-crop-f010.png`：× 放大像素对照

> 注：早期由 `--probe notify`（每帧重建 Motion，补间不累积）生成的 `static.bgra` / `x-hover.bgra`
> 等帧（含不同样张数据集）已全部删除；所有保留的对比图与 hover 证据均来自
> `notify-motion=hover` 序列帧（`frames/seq/`、`frames/seq-row/`）。

## 关键原始输出

```
# native/minicard-cards/cargo test（基线）
test result: ok. 123 passed; 0 failed

# native/minicard-cards/cargo test（改后）
test result: ok. 127 passed; 0 failed; 0 measured; 0 filtered out

# native/minicard-bin/cargo check
Checking minicard-bin v0.1.0
Finished `dev` profile [unoptimized + debuginfo] target(s) in 5.38s

# compare_hover.py（隔离 × 动画）
f002 row-only vs X-hover: bbox=(209, 74, 227, 92) changed=42  outside_X_cell=0
f004 row-only vs X-hover: bbox=(209, 74, 227, 92) changed=240 outside_X_cell=0
f006 row-only vs X-hover: bbox=(209, 74, 227, 92) changed=232 outside_X_cell=0
f010 row-only vs X-hover: bbox=(209, 74, 227, 92) changed=232 outside_X_cell=0

# --probe notify --debug-layout（× 节点与正文）
id=10000 box x=13 y=64 w=214 h=38
  id=10003 text len=1 x=209 y=64 w=18 h=38
  id=10002 box x=45.5 y=64 w=159.5 h=38
```

## 变更清单

| 文件 | 对应 AC |
|---|---|
| `native/minicard-cards/src/notify.rs` | AC1–AC6：token/布局/动效/paint/命中/单测 |
| `native/minicard-bin/src/main.rs` | AC8：probe 注入 `MC_NOTIFY_HOVER`（仅探针路径） |
| `native/minicard-bin/src/probe.rs` | AC8：`MC_NOTIFY_MOTION=hover` 取证场景 |
| `.engineering/notify-close-x-hover/*` | 契约与证据台账、取证脚本与帧 |

## 轮次小结

- R0（锁定）：契约成形，基线 123 passed。
- R1（实现）：3 文件改动；新增 4 单测；127 passed；bin check 绿。
- R2（独立检验）：AC1–AC7 全通过；AC8 因 V1（早期对比帧未展示 hover）部分通过。功能代码零缺陷。
- R3（返工）：删除 13 个无效帧/图；用 `notify-motion=hover` 的 f000/f010 生成 `compare-static-vs-xhover.png`；
  EVIDENCE 的证据索引与 AC8 结论已更正。V1 关闭。未改任何功能代码，故不重跑功能测试。

## 残余风险与未验证项

- 用户桌面 live 实例未实测（本环境只有离线探针帧）：× 的 120ms 动画需在桌面卡上肉眼确认手感。
- 「交互动画」按 A1 理解为 × 自身 hover 反馈（放大+变色+底圈）；若用户想要旋转/图标变形等别的形态，需返工。
- 命中列宽 14→18px 使正文窄 4px（163.5→159.5）：实测省略号正常，但更长文案的换行/省略需 live 观察。
- 浅色主题下 `fg_soft` 底圈对比度未单独出图（深色主题已验证）；`pal.warn` 为固定橙红，两主题一致。

## 复验命令

```powershell
cd F:\mini-card\native\minicard-cards
& C:\Users\22088\.cargo\bin\cargo.exe test

cd F:\mini-card\native\minicard-bin
& C:\Users\22088\.cargo\bin\cargo.exe check

# hover 帧取证（写到临时目录，不污染仓库）
$dir="$env:TEMP\notify-x-hover"; New-Item -ItemType Directory -Force $dir | Out-Null
$env:MC_NOTIFY_MOTION='hover'; $env:MC_NOTIFY_PRE_MS='0'
$env:MC_NOTIFY_HOVER='218,83'   # × 中心（240×300 卡）
& .\target\release\minicard-bin.exe --probe notify-motion --w 240 --h 300 --ss 1 --no-glass --font F:\mini-card\native\assets\fonts\MiSans-Regular.ttf --dump-seq $dir --exit-after 420
```
