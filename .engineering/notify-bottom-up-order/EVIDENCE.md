# 验收证据 · notify-bottom-up-order

| AC | 命令 / 步骤 | 实际输出 | 结论 |
|---|---|---|---|
| AC1 | `cargo test ... clear_all_staggers...` + 原生帧 | `... ok`；单测断言 i=0(顶)t0=+(n-1)×60、i=n-1(底)t0=now；原生帧 f013 邮件(底)先滑、f017 设计组、f021 张三(顶)最后 | **通过** |
| AC2 | `cargo test ... group_header_exits_with_its_last_message` | `... ok`；各组 header_exit=本组最后退场那条（自下而上=本组最顶/最新那条）；QQ=0、邮件=60、微信=120（该测试先单删了 id6） | **通过** |
| AC3 | `cargo test ... single_remove_swipes...` 等两阶段单测 | 全绿；两阶段形态未改（滑出段满高满透明、补位段收高） | **通过** |
| AC4 | `cargo test ... survives_stale / empty_snapshot_mid / new_message_mid / new_arrival` | 全绿；前轮修复未被只改错开下标破坏 | **通过** |
| AC5 | 像素差分 + 单测几何 | 卡框/标题区 maxdiff=0（f013/17/21/27/35/43）；几何 y/h 断言绿 | **通过** |
| AC6 | headless Edge `?t=230/290` + 源码 | `?t=230` QQ(底)先滑、`?t=290` QQ 没了邮件在滑、微信未动；demo 用 `(N-1-k)` 反转、无外部引用 | **通过** |
| AC7 | `cargo test` **123 passed**；`cargo check/build` 通过；独立检验 | 见下 | **通过** |

## 执行阶段中间校验

- 核心改动：`mark_cleared` 错开序号 `i → (n-1-i)`，仅一行公式；组头 max t0 逻辑不变（自然适配）。
- 原生帧 `docs/verify/_notify_bottomup/clear-all/frame/sheet.png`：自下而上依次右滑，动画形态（满高满透明水平平移→收高补位）不变。
- demo `?t=230`：最底 QQ 已开始滑、上方未动；`?t=290`：QQ 消失、邮件在滑、微信未动。

## 独立检验结论（verify-bottomup）

**通过，AC1–AC7 全部满足。** 顺序经三条独立证据链确认：① 外部状态机测试直接读 exiting t0：`i=0(顶)+180ms / i=1+120 / i=2+60 / i=3(底)+0ms`；② 原生帧质心：底行 GitHub f013(~217ms)→中行设计组 f016(~267ms)→顶行张三 f020(~333ms)；③ demo `?t=230` 底行先滑、`?t=290` 底两行已滑顶两行未动。组头随本组最后退场那条（微信头 +180ms 与张三同帧）。反向构造（仅剩 1 条、全部已 suppress 再 clear）全绿。动画形态、前轮修复、demo 同步均正常。

发现 2 条 S4 陈旧文案（注释/表格仍写旧顺序公式），**已修**（notify.rs 注释 + demo 表格）。

## 收敛与残余风险

轮次 1 无未关闭 P1–P3 缺陷；独立检验无新发现 → **收敛达成**。

残余（显式列出）：
- 严格逐像素 diff 未做改前/改后两版对比（本轮仅反转错开下标、几何通路未改，单测几何断言绿、帧质心确认卡框/标题不动，风险低）。
- 未实测 n≥12 的超长错开窗口总长（公式 `(n-1)×60+180` 保证，单测覆盖到 n=4）。
- demo 仅在 headless Edge 核对 `?t=230/290` 两帧；真机异步删除时序、高 DPI、实机帧率未测。

## 复验命令

```powershell
& 'C:\Users\22088\.cargo\bin\cargo.exe' test --manifest-path F:\mini-card\native\minicard-cards\Cargo.toml  # 123 passed
& 'C:\Users\22088\.cargo\bin\cargo.exe' check --manifest-path F:\mini-card\native\minicard-bin\Cargo.toml
# demo（自下而上）
start "" "F:\mini-card\docs\verify\notify_swipe_clear_demo.html?t=230"   # 底行 QQ 先滑
start "" "F:\mini-card\docs\verify\notify_swipe_clear_demo.html?t=290"   # 底两行已滑、上两行未动
```
