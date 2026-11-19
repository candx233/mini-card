# 缺陷台账 — mcss-default-1x

> 严重度：S1 阻断 / S2 影响判断 / S3 影响可信度 / S4 文档瑕疵。

| 编号 | 严重度 | 描述 | 证据 | 根因 | 修复 | 状态 | 轮次 |
|---|---|---|---|---|---|---|---|
| V1 | S4 | 单测 env var 无 RAII 守卫 | 独立复核 O1 | cfg(test) 并行测试潜在污染 | EnvGuard(Drop) 恢复；重跑 ok；release hash 不变 | 已修 | R1 |
| O1 | S4 | SPEC AC4 验证方法恒真无效（probe 走 Args.ss） | 独立复核 | 规格缺陷 | AC4 改单测+路径追踪+spawn 无覆盖 | 已修 | R1 |
| O2 | S4 | 抓图 stdout 未留档 | 独立复核 | 证据间接 | 保存 grab_live_output.txt（逐窗 hwnd/rect） | 已修 | R1 |
| O3 | S4 | main.rs 引用未限定文件 | 独立复核 | 引文卫生 | 限定为 native/minicard-bin/src/main.rs | 已修 | R1 |
