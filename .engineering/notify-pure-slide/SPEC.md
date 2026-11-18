# 契约 · 通知卡纯右滑退场

## 原话（照录）

> 你这个是往清除方向滑掉 不是单纯的往右边滑掉 直接往右滑掉单个清除才能统一啊

用户澄清（本轮问答，逐字确认项）：
- 「往清除方向滑掉」= 当前边右移、边淡出、行高收缩、文字往上飘，观感像「往右上（清除按钮方向）斜着滑掉并消失」。
- 选择的目标 = **「是，就要纯水平平移」**：整行保持原样向右平移出卡外，不淡出不缩高；滑出后再收空位补位。单条和批量都用这套。

## 目标

把通知卡条目（及组头）的退场改为纯水平平移：保持原高度与原不透明度向右滑出卡外，滑出完成后再收掉空位让下方补位；单条与批量清除统一使用这套退场。

## 验收标准（AC）

| # | 判据（可判定） | 判定方式 |
|---|---|---|
| AC1 | 单条 × 退场分两阶段：①滑出段（p∈[0,SLIDE_FRAC]）行高恒 = 38、alpha 恒 = 1、dx 单调 0→卡片宽；②补位段（p>SLIDE_FRAC）dx 保持卡片宽、行高单调 38→0、底边距 0→-gap。下方组头 y 在滑出段不变、补位段单调上移 | 新增 Rust 单测逐帧断言 `RowAnim` 与下方组头 y；`cargo test -p minicard-cards` |
| AC2 | 批量清除仍按快照顺序错开 60ms/行，每行都用 AC1 的两阶段；组头随本组最后一条一起平移出卡外（组头也保持原高/原 alpha，只在补位段收高） | 单测断言各组 header 的 t0 与本组最后一行相等、header 两阶段几何 |
| AC3 | 退场期旧快照仍含这些项时不闪回；快照收敛后 suppress 收敛；清空快照在错开窗内到达不抹掉未滑完的行；窗尾有活动行时组头走 120ms 淡入而非满血弹出 | 单测（沿用并扩展）：`clear_all_survives_stale_snapshot` / `empty_snapshot_mid_stagger` / `new_message_mid_clear_does_not_flicker` 全绿 |
| AC4 | 只动消息区：卡框/分割线/抬头（除既有「确认→清除」交叉）在整段像素差为 0 | `--probe` 帧像素差分 + 单测几何断言 |
| AC5 | 回归：到达/滚动/hover/清除交叉/状态淡入/配额挤出不跳 的既有单测全绿；`cargo check/build -p minicard-bin` 通过 | `cargo test`/`cargo check`/`cargo build` |
| AC6 | demo 单文件 HTML 同步两阶段：一键重播/时间轴/关键帧/`?t=` 深链；无任何外部引用 | HTTP 200 + headless Edge `?t=` 渲染 + 源码核对 |
| AC7 | 独立检验 + 美观评审无 P1 及以上（无跨帧闪烁、突兀跳变、文字上飘、残留半行） | `engineering-verify` 子代理只读报告 + `EVIDENCE.md` |

## 非目标

- 真实拖拽手势（用户此前明确选「点×改成右滑动画」）
- 改布局/信息架构/配色/字号、到达/滚动/hover/状态动效
- 把单条与批量的错开逻辑合并（单条无错开、批量 60ms，这是「统一退场形态」而非「统一错开」）
- 引入外部依赖；改 web 主应用

## 约束

- 平台 Windows；Rust `minicard-cards` / `minicard-bin`；cargo 在 `C:\Users\22088\.cargo\bin\cargo.exe`
- 总退场时长不变：`EXIT_MS = 180`；两阶段切分 `SLIDE_FRAC = 0.6`（滑出 108ms、补位 72ms）
- 右滑位移上限 = 卡片宽 `t.w`；补位用 `mar_b = -item_gap × 补位进度`
- 复用 `Motion.exiting` / `suppressed` / `row_anim` / `paint_row` / `header_anim`，不新增第二套系统
- 台账落在 `.engineering/notify-pure-slide/`

## 未知与假设（标风险）

| # | 假设 | 风险 / 错了会怎样 |
|---|---|---|
| A1 | 滑出段不淡出、内容靠行矩形 clip 出卡即消失；行矩形右缘≈卡内右缘 | 若上游把行宽改窄，内容可能在行内提前被裁，观感变「切掉」；demo 与原生需逐帧核对 |
| A2 | 补位段 alpha 仍为 1（内容已被 clip 裁掉，不可见） | 无视觉影响；若在补位段能看到淡出则为 bug |
| A3 | 两阶段都在 180ms 内完成，总时长不变 | 若用户觉得补位太快，可调 SLIDE_FRAC（需回契约） |

## 决策记录

| 时间 | 决定 | 影响面 |
|---|---|---|
| 2026-11-17 | row_anim 退出拆两阶段：dx 用 `ease(p/SLIDE_FRAC)`，h/mar_b 用 `ease((p-SLIDE_FRAC)/(1-SLIDE_FRAC))`，alpha 恒 1 | 只改退出分支；animating/prune/build_groups 的 180ms 窗口不变 |
| 2026-11-17 | header_anim 同步两阶段；组间距 `gap_eat` 改为跟「补位进度」而非 `1-h` | 组头随最后一条水平滑出，补位段才收高 |

## 判档

**标准档**：跨 native + demo、有回归面 → 三件套 + 独立检验。
