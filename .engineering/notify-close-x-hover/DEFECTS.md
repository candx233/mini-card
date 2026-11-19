# 缺陷台账 · 通知卡 × 放大与悬停交互动画

## 缺陷列表

| 编号 | 标题 | 严重度 | 证据 | 根因 | 修复 | 状态 | 轮次 |
|---|---|---|---|---|---|---|---|
| V1 | 交付的「静态/hover 对比帧」未展示 hover 效果 | S4 | 独立检验：`diff frames/static.bgra frames/x-hover.bgra` → 1116px / bbox 整卡 / 无橙色 ×；两图目视均为无高亮样张 | 早期用 `--probe notify` 出图：该探针每帧重建 `Motion`，120ms 补间无法累积，注入的 `MC_NOTIFY_HOVER` 只改卡框描边；后来虽改用 `--probe notify-motion=hover` 出了有效 `seq` 帧，但未清理误导性旧帧，EVIDENCE.md 仍引用了它们 | 删除无效帧与由它生成的对比图；用 `notify-motion=hover` 的 `seq/f000`（t=0，× 未淡入）与 `seq/f010`（补间完成）生成新的静态 vs hover 并排图；EVIDENCE.md 改指向有效证据 | 已修复 | R2 |

## 轮次小结

- R0（锁定）：无缺陷；基线 123 passed。
- R1（实现/自测）：127 passed，bin check 绿；像素差分确认 × 动画只落在 × 单元。
- R2（独立检验）：AC1–AC7 全通过；AC8 因 V1（证据交付物无效）部分通过。功能代码零缺陷。已修 V1。
- R3（返工收敛）：删除无效帧/图并用 `notify-motion=hover` 的 f000/f010 生成有效并排图；独立检验复验 V1 关闭；
  又按其观察项删除残留的不同样张基线帧 `static.*`。最终 `cargo test` → 127 passed / 0 failed。
  无未关闭缺陷，本轮无新发现 → 收敛。
