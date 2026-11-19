# 验收证据 · 通知卡 × 交互动画居中、正文加宽、右侧留缝

## AC 对照表

| AC | 命令 / 步骤 | 实际输出摘要 | 结论 |
|---|---|---|---|
| AC1 | `--probe notify-motion=hover` 出 1×/2× 的 `f010`，`measure_after.py` 量橙色墨迹 bbox 与圆底 bbox 中心 | 1×：墨迹中心 (216.50,82.00)、圆心 (216.50,82.50) → dx **+0.00** dy **−0.50** DIP；2×：墨迹 (217.00,82.50)、圆心 (216.75,82.75) → dx **+0.25** dy **−0.25** DIP。均 ≤阈值（1×≤0.5、2×≤1） | 通过 |
| AC2 | 单测 `body_wider_and_x_circle_inset_from_row_edge` + Token 断言 | pad_l/r = 10（旧 13）、item_indent = 18（旧 24）、正文节点 rect.w = **167.5px**（基线 159.5，+8px）、close_w = 18 不变；单测 `... ok` | 通过 |
| AC3 | 1×/2× `f010` 逐像素：沿圆心 y 扫行底右缘 x，减圆底 bbox 右缘 | 1×：行底右缘 229、圆底右缘 225 → 缝 **4.00** DIP；2×：459−451 → **4.00** DIP（÷2）。单测断言 `圆右缘 = ×节点右缘 = 行右缘−4` 通过 | 通过 |
| AC4 | 三态显隐 `check_states.py` + `cargo test` 全量 + `cargo check` | 无 hover：亮/橙/圆 = (0,0,0)；仅行 hover：(28,0,0) 灰 × 无圆；满 hover：(6,22,218) 橙 ×+圆。命中单测 `hit_maps_row_and_remove_x`/`close_hover_hit_only_when_mouse_is_over_the_x` 绿；`cargo test` **128 passed / 0 failed**；bin `cargo check` Finished | 通过 |
| AC5 | `compare-1x.png` / `compare-2x.png` 前后并排 + × 行放大 | 1×/2× 均可见：正文变宽（"repo…"→"repos…"）、× 圆右缘与行右缘出现明显缝、× 居中；1× 字形清晰无裁切/发糊 | 通过 |

## 独立检验（subagent 无上下文只读复验）

- 结论：**AC1–AC5 全部独立复验通过**。检验者从当前源码 `cargo build --release` 重建出图（排除过期构建风险），f010 与实现者帧 MD5 逐字节相同（1× `2f77f782…`、2× `4d0b961f…`）。
- 自建 PIL 脚本重测：AC1 1× dx+0.00/dy−0.50、2× dx+0.25/dy−0.25；AC2 布局树正文 x=36.5 w=167.5；AC3 缝 4.00 DIP（1×/2×）；AC4 三态显隐 + 命中 4 单测 + bin check 绿。
- 越界核对：本次只动 `notify.rs` / `text.rs`，未改字号/1.25×/120ms/曲线/其它卡；`live.rs` 的未提交改动属另一任务，未碰。
- 唯一缺陷 V1（S4）：检验时点早于三态帧落盘，报告帧缺失；实现者复核帧存在且 `check_states.py` 正常 → 已关闭（不可复现）。详见 DEFECTS.md。
- 观察项：1× 垂直偏差 −0.50 恰在阈值上限，系偶数直径圆的像素量化（2× 为 −0.25）；`close_gap_r=0.25r` 随 r 缩放（120×90 卡为 2px），符合比例而非固定 4px。

## 视觉证据索引

- `frames/compare-1x.png`：**1× 前后并排**（上整卡、下 × 行 5× 放大）
- `frames/compare-2x.png`：**2× 前后并排**（上整卡、下 × 行 3× 放大）
- `frames/probe1x/f010.bgra`、`frames/probe2x/f010.bgra`：改动后满 hover 原始帧
- `frames/rowhover/f012.bgra`、`frames/nohover/f012.bgra`：仅行 hover / 无 hover 态核对帧

## 关键原始输出

```
# measure_after.py（改动后）
[AFTER 1x] ink center (216.50,82.00) circle center (216.50,82.50) -> AC1 dx +0.00 dy -0.50 DIP
           AC3 gap = (229-225) = 4.00 DIP
[AFTER 2x] ink center (217.00,82.50) circle center (216.75,82.75) -> AC1 dx +0.25 dy -0.25 DIP
           AC3 gap = (459-451)/2 = 4.00 DIP

# check_states.py（三态显隐）
NO HOVER   f12: bright/orange/circle = (0, 0, 0)
ROW ONLY   f12: bright/orange/circle = (28, 0, 0)
FULL X     f10: bright/orange/circle = (6, 22, 218)

# minicard-cards cargo test
test result: ok. 128 passed; 0 failed; 0 measured; 0 filtered out

# minicard-bin cargo check
Finished `dev` profile ... in 4.05s
```

## 轮次小结

- R0（锁定）：契约成形，基线 127 passed，像素量化基线三项。
- R1（实现）：3 文件改动区（notify.rs 主体 + text.rs 只读方法）；新增 1 单测；128 passed；release build 绿。
- R2（自测）：三 AC 数值达标，三态显隐/补间稳定无回归；前后对比图产出。
- R3（独立检验）：AC1–AC5 全通过；V1（S4）复核不可复现并关闭；收敛。

## 变更清单

| 文件 | 对应 AC |
|---|---|
| `native/minicard-cards/src/notify.rs` | AC1–AC4：Token/布局/右 padding/paint 居中/单测 |
| `native/minicard-draw/src/text.rs` | AC1：`ink_center_bias_2d` 只读度量方法 |
| `.engineering/notify-x-hover-align/*` | 契约、台账、取证脚本与帧、对比图 |

## 测量口径与基线

- 基线（改动前）240×300：pad_l/r=13、item_indent=24、正文 w=159.5、× 节点 x=209 w=18、圆底右缘=行底右缘=227（贴死）、墨迹中心偏左上约 0.25 DIP。
- 改动后：pad_l/r=10、item_indent=18、close_gap_r=4、正文 w=167.5、× 节点右缘=圆底右缘=行右缘−4=225、墨迹-圆心偏差 ≤0.5 DIP。
- 视觉居中用 DWrite `GetOverhangMetrics` 的 `(right−left)/2, (bottom−top)/2`，口径与既有 `ink_center_bias`（日历今日块已像素验证）一致，随每帧字号重算。

## 复验命令

```powershell
cd F:\mini-card\native\minicard-cards
& C:\Users\22088\.cargo\bin\cargo.exe test

cd F:\mini-card\native\minicard-bin
& C:\Users\22088\.cargo\bin\cargo.exe check

# hover 帧取证（写到临时目录）
$dir="$env:TEMP\notify-x-align"; New-Item -ItemType Directory -Force $dir | Out-Null
$env:MC_NOTIFY_MOTION='hover'; $env:MC_NOTIFY_PRE_MS='0'; $env:MC_NOTIFY_HOVER='217,83'
& .\target\release\minicard-bin.exe --probe notify-motion --w 240 --h 300 --ss 1 --no-glass --font F:\mini-card\native\assets\fonts\MiSans-Regular.ttf --dump-seq $dir --exit-after 420
python F:\mini-card\.engineering\notify-x-hover-align\measure_after.py
```
