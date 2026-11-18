# 验收证据 · notify-pure-slide

## AC 对照表

| AC | 命令 / 步骤 | 实际输出 | 结论 |
|---|---|---|---|
| AC1 | `cargo test ... single_remove_swipes_right_with_following_rows_filling_in` / `local_remove_collapses...` | `... ok`；断言滑出段 0/30/60/90/108ms 行高恒 38、alpha 恒 1、dx 单调 0→240；补位段 dx 恒 240、行高单调→0；下方组头 y 滑出段不动、补位段单调上移 | **通过** |
| AC2 | `cargo test ... group_header_exits_with_its_last_message` / `clear_all_staggers...` | `... ok`；4 组头 t0 = 本组最后一行 t0（微信 +60/邮件 +120/QQ +180ms）；组头两阶段：滑出段满高满透明只平移、补位段收高 | **通过** |
| AC3 | `cargo test ... clear_all_survives_stale_snapshot` / `empty_snapshot_mid_stagger` / `new_message_mid_clear_does_not_flicker` | 三条 `... ok`；旧快照不闪回、收敛、窗内空快照不抹行、窗尾活动行走 120ms 淡入（slide 字段未破坏前轮修复） | **通过** |
| AC4 | 单测 chrome 断言 + 原生帧像素差分 | 单测 `ID_DIVIDER`/`ID_HEADER` y/h 全段相等；像素差分（clear-all，vs f011，f013/17/21/27/35/43）卡框区、标题区 **maxdiff=0** | **通过** |
| AC5 | `cargo test --manifest-path native/minicard-cards/Cargo.toml` / `cargo check/build --manifest-path native/minicard-bin/Cargo.toml` | **123 passed; 0 failed**；`check`/`build` 均 `Finished dev profile` | **通过** |
| AC6 | headless Edge `?t=250/340/420` + `?t=283&mode=single` 截图 + 源码核对 | `?t=250` 行1 满高半透明滑至卡右缘被裁（文字不淡出不缩高）；`?t=340` 补位段；demo `SLIDE_FRAC=0.6`、rowSwipe 两阶段；无任何外部引用 | **通过** |
| AC7 | 独立检验子代理 `verify-pure-ac` + 美观评审 `verify-pure-aes` | AC 检验：**通过、零缺陷**（逐帧外部探针 + 像素差分 + 123 passed）；美观评审：**美观通过**（无 P1/P2，3 条 P3 观察） | **通过** |

## 执行阶段中间校验

- 基线 `cargo test` 123 passed（本轮前）。
- 原生帧：`docs/verify/_notify_pure_slide/{clear-all,remove}/frame/sheet.png`。remove 场景 f013→f016 第一行整行右移、满高满透明、在卡右缘被裁；f017+ 收高度、设计组上移补位。无文字上飘、无横切薄片。
- demo `?t=250`：张三整行右滑中，文字清晰、高度未缩、未上飘（见 `_pureslide_250.png`）。
- 关键实现决策：配额挤出（adopt_squeezed）与系统侧消失（vanished）保留旧的同拍塌缩（`slide=false`），因为它们必须与入场行「一增一减」严格抵消，否则整组高度在交换中途鼓起、下方组先跳——这是 §约束核心不变量。

## 独立检验结论

- **verify-pure-ac：通过，缺陷清单为空**。逐帧外部状态机探针实证两阶段：单删 h 在 0–108ms 恒 38、下方组头 y 恒 146；109–179ms h 37.98→0.004、y 146→105 单调；mar_b 0→-gap。clear-all 错开（at+150ms 行高 [5.2,38,38,38]）、组头随本组最后行、前轮三处修复未被 slide 字段破坏。像素差分卡框四边 + 标题 maxdiff=0。`cargo test` 123 passed（连跑 3 次稳定）。
- **verify-pure-aes：美观通过，无 P1/P2**。文字像素带 y 全程恒定（不上飘）、滑出段亮度分位 p50/p90/p99 逐帧完全相同（不淡出）、dx 单调右移至卡缘被裁；无横切薄片/半行残留；批量 60ms 依次滑、组头随本组最后行；单调无回弹、收尾全空。3 条 P3 观察：抬头计数一次性数据更新、demo `?t=420` 标注与实际阶段略偏、demo 单条模式简化为直接空态（页面已披露）。

## 收敛

轮次 1 无未关闭缺陷；两个独立检验者均无新发现 → **收敛达成**。

## 残余风险与未验证项（显式列出）

1. **demo 为独立 mock**：`?t=420` 深链时第 4 行仍在滑出段（标注「补位后」略偏）；单条模式删 1 行后直接淡入空态（原生保留其余行），页面已注明简化。不影响原生实现。
2. **抬头计数一次性更新**：删除时「N 条未读→共 M 条」同帧变一次（非动画缺陷，plan §3.1 明确「计数同帧更新」，但严格字面解读会与 AC4「抬头像素差为 0」冲突；本轮按 y/h 几何口径判 0）。
3. **真机时序 / 高 DPI / 非 240×300 尺寸**未实测：Windows 异步删除 ≤1.5s 收敛用 stale-snapshot 单测模拟；探针仅 240×300 ss=2；实机帧率需 `MC_FPS_LOG`。
4. **真实拖拽手势**属契约非目标（用户选「点×改右滑」）。

## 复验命令

```powershell
& 'C:\Users\22088\.cargo\bin\cargo.exe' test --manifest-path F:\mini-card\native\minicard-cards\Cargo.toml
& 'C:\Users\22088\.cargo\bin\cargo.exe' build --manifest-path F:\mini-card\native\minicard-bin\Cargo.toml
cd F:\mini-card\native\minicard-bin
$env:MC_NOTIFY_MOTION='remove'; $env:MC_NOTIFY_PRE_MS='200'; $env:MC_NOTIFY_FRAME_MS='16.667'
.\target\debug\minicard-bin.exe --probe notify-motion --w 240 --h 300 --ss 2 --no-glass `
  --font F:\mini-card\native\assets\fonts\MiSans-Regular.ttf `
  --dump-seq F:\mini-card\docs\verify\_notify_pure_slide\remove\frame --exit-after 1000
python F:\mini-card\docs\verify\_notify_motion_20261116\_nm_sheet.py `
  F:\mini-card\docs\verify\_notify_pure_slide\remove\frame 11 23 1 --cols 7 --scale .42
start "" "F:\mini-card\docs\verify\notify_swipe_clear_demo.html?t=250"
```
