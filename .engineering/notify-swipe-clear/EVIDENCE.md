# 验收证据 · notify-swipe-clear

## AC 对照表

| AC | 命令 / 步骤 | 实际输出 | 结论 |
|---|---|---|---|
| AC1 | `cargo test -p minicard-cards single_remove_swipes_right -- --nocapture` | `... ok`；断言 0/30/60/.../179ms `dx` 单调 0→`t.w`(240±0.5)、`h` 单调 38→0、`alpha` 单调 1→0，下方邮件组头 y 单调不下降 | **通过** |
| AC2 | `cargo test -p minicard-cards clear_all_staggers_rows_right -- --nocapture` | `... ok`；4 行 t0 分别为 at+0/60/120/180ms；`animating` at=200 与 +300ms 为 true、+370ms 为 false；卡框/分割线/抬头 y/h 全段相等 | **通过** |
| AC3 | `cargo test -p minicard-cards clear_all_survives_stale_snapshot -- --nocapture` + `new_arrival_during_clear_staggers -- --nocapture` | 两条 `... ok`；旧快照仍 suppress 不闪回；收敛后 suppress 清空；错开期新通知正常入场 | **通过** |
| AC4 | 单测 chrome 断言 + 原生帧像素差分 | 单测 `ID_DIVIDER`/`ID_HEADER` y/h 全段相等；`clear-all/frame/sheet.png` 目视卡框/分割线/抬头不动；整列淡出层已移除（源码无 `clear_fade_t0`） | **通过** |
| AC5 | `cargo test --manifest-path native/minicard-cards/Cargo.toml` = **120 passed; 0 failed**；`cargo check --manifest-path native/minicard-bin/Cargo.toml` = `Finished dev profile` | 基线 119 → 120（净 +1）；`cargo build` 成功 | **通过** |
| AC6 | headless Edge `?t=250` / `?t=270&mode=single` 截图 + 源码核对 | `_swipe_demo_t250.png`：行1 dx=73.7/高26.3（右滑中）、行2 补位；`_swipe_demo_single.png`：单行右滑；参数 `D_EXIT=180/STAGGER=60`；无外部引用 | **通过** |
| AC7 | 独立检验子代理 `verify-swipe-ac` + 美观评审 `verify-swipe-aes` 只读评审 | 待子代理报告 | 待测 |

## 执行阶段中间校验

- 基线：`cargo test` = 119 passed（改动前）。
- 4 条旧测试（clear_fade / deferred_clear 语义）改写为右滑语义测试，全绿。
- 原生帧（合成时间轴 16.667ms/帧，事件 t=200ms）：
  - `docs/verify/_notify_swipe_clear/clear-all/frame/sheet.png`（f011..f043，step2）—— 四条按 60ms 错开依次右滑，f017 放大可见第一行滑出卡外、下一行补位。
  - `docs/verify/_notify_swipe_clear/remove/frame/sheet.png`（f011..f023）—— 单条右滑 + 下方补位。
  - 探针输出 `scenario=clear-all ... items 3→3` / `scenario=remove ... items 3→2`，各 62 帧。
- 像素差分（clear-all，vs f011 基线；f013/f017/f021/f027/f033/f043）：卡顶框 / 标题 / 分割线三处 **maxdiff=0**（全部 6 帧）→ AC4 佐证。
- demo 自校验：无任何外部引用（`https?://`/`@font-face`/`url(`/`<link`/`<img`/`src=` 零命中），17.5KB 单文件。

## 独立检验结论（摘录）

**verify-swipe-ac（第一轮）**：AC1–AC6 逐条通过；未来 t0/prune/animating/2s 回滚专项全部证伪通过（无泄漏/无永久续帧/回滚不误触发）；发现 V1(S3 快照提前收敛尾行弹出)、V2(S3 疑似前轮既有的入场半高跳满高)、V3(S4 suppress 空洞)。

**verify-swipe-aes（第一轮）**：无 P1，但 3 项 P2——V1 快淡领先位移、V2 demo 空态残留组头、V3 组头角标/时间硬切。正面确认：真逐条错开、整行右移+右侧裁切、补位连续、收尾干净、卡框/分割线/抬头固定。

**返工（轮次 2）**：
- V1(ac) 已修：`paint()` 渲染态 override（有 exiting 在动时暂缓进 Empty）+ 新单测 `empty_snapshot_mid_stagger_...`，已 green。
- V1(aes) 已修：alpha 改 `1-ease_standard`（与 dx/h 同曲线）；原生帧重出、demo 同步。
- V2(aes) 已修：demo 空态隐藏整个 viewport（组头随之消失），`?t=680` 重渲染确认只剩标题+空态。
- 回归：`cargo test` **121 passed**；`squeezed_row_collapses` / `exit_collapse_moves_following_groups_smoothly` 两条既有不变量仍绿（曾试「h 延后收缩」破坏它们，已回退）。
- 两个子代理已收到定点复核请求，结论待回填。

**定点复核（最终）**：
- verify-swipe-aes：V1（快淡领先位移）**关闭**——重看重出帧 f013–f018，文字整段可读地划出、卡右缘裁切干净、无闪烁；V2（demo 空态残留组头）**关闭**——`?t=680` 只剩标题+空态；V3（组头角标/时间硬切）降为 **P3 观察项**，不判 P1；**无新 P1/P2，AC7 可通过**。
- verify-swipe-ac：V1（快照提前收敛）**关闭**——外部测试复刻 paint 管线实证 override 有效（无 override 画 0 行、override 画满 4 行）；V3（suppress 空洞）同意作残余风险；回归 121 passed、两条不变量单测绿；无新 S1–S3。
  - 关于其 V2「入场半高被 clear 跳满高，本轮引入」的判断：**已核证为错误**——`git show HEAD:notify.rs` 的退出分支是 `e = 1 - easeOutQuart(p); h = item_h * e`，p=0 时 h = item_h（满高），与本轮完全相同。该现象是**前轮既有**，非本轮引入；按契约 §非目标不属本轮范围，记为残余风险（DEFECTS.md 已如实标注）。
  - 其文档一致性 S4（SPEC/EVIDENCE 仍写旧 alpha 口径）已修：SPEC A3 与 demo 文案同步为「1-ease_standard 同曲线」。

**收敛达成（轮次 2）**：轮次 2 无新增未关闭缺陷；两个独立检验者复核均无新发现。

## 轮次 3：用户反馈「组头留到最后才清」

**问题**：原实现里应用图标+名字（组头）全程不动、所有消息滑走后才一起消失，违背「一条一条右滑消失」。

**修复**：`GroupView.header_exit`（本组最后一条退出行 t0，仅整组都在退时置位）+ `header_anim()`（组头高/alpha=ge×(1-e)、dx=t.w×e）+ `tree()`/`paint_tree()` 组头分支接入；demo `.g-head` 同步。

**证据**：
- `cargo test` **122 passed**；新单测 `group_header_exits_with_its_last_message` 通过：单条删除时组头不退、清除时各组头 t0 = 本组最后一行、与该行同帧滑完（h/alpha<0.01、dx=t.w±0.5）。
- 原生帧 `clear-all/frame/sheet.png` 重出：f019 微信图标+名字随「设计组」（微信最后一条）一起右滑（放大 hz019.png 可见两者同处卡右缘、已淡化），邮件/QQ 组头仍在原位；demo `?t=350` 同款（微信头 faded 右移、邮件/QQ 头仍在）。
- 像素差分（vs f011）：卡顶框、标题左半（`通知`）maxdiff=0；变化行全在消息视口（组头/消息右滑）与抬头右侧「确认→清除」交叉，属预期。
- 独立检验子代理 `verify-head-exit` 复核（轮次 3）：核心修复通过——122 passed、新单测 `group_header_exits_with_its_last_message` ok；组头 t0 逐帧与「本组最后一行」相等；单条删除组头全程静止；原生帧 f017–f020 微信图标+名字随「设计组」一起右滑、邮件组头仍在；demo `?t=350` 同款。
  - 其 V1（S3 边界）：清除错开期同组新消息到达 → 组头 dx 瞬移回 0 + 重新淡入。已做双重修复（observe keys 纳入退场组 + Motion 钉住 group_header_exit），新单测 `new_message_mid_clear_does_not_flicker_group_header`。
- 轮次 4 回归：`cargo test` **123 passed**；死字段/未用变量已清理；原生帧重出（63 帧）。
  - 复核（轮次 4）：V1 关闭——注入点 ms=110→120 组头 dx 73.71→109.74、alpha 0.693→0.543 单调，无瞬移/重淡入；回归三项（单删静止/清除随末行/组头入场 120ms）全绿。
  - 其 V2（S3）：窗末组头满血弹出——已修（`prune` 过期时见组仍存活 → `group_enter` 走 120ms 淡入）；复核确认 ms=235 a=0 → 240 a=0 → 渐入到 360 a=1，跳变消失、对照纯 clear-all 无误重入，**V2 关闭、无新缺陷、回归 123 passed**。

**收敛达成（轮次 5）**：用户核心诉求（组头随本组最后一条消息一起右滑、不留到最后）已实现并经原生帧 + demo + 单测 + 独立复核四方确认；复核链上发现的 V1/V2 边界闪回均已修复关闭。最终 `cargo test` 123 passed，`cargo check/build` 通过。

## 残余风险与未验证项（显式列出，最终）

1. **ms=120–240 短暂「有活动行但组头不可见」**：清除窗内同组来了新消息时，组头仍按旧 t0 滑出到窗口结束才淡入——这是修 V1 时确定的取舍（保证滑出过程不被打断）。窄时序，语义上组头最终会回来。
2. **组头退场 t0 用 max(快照下标×60ms)**：假设快照按时间降序（新→旧）。现有样张满足；若上游乱序，最大 t0 可能不等于视觉最末行。
3. **前轮既有项**（本轮不混）：入场 <180ms 内 clear 该行从半高跳满高；单删进行中再 clear 首个可见行晚 60ms 起滑；组头角标/时间在退场中途硬切（P3，语义正确无闪烁）。
4. **未覆盖**：真机 Windows 异步删除时序、ss≠2 / 其它卡尺寸 / 深浅主题下组头右滑像素、hover 中组头退场并发帧、reduced-motion / 真机帧率；demo 为独立 mock。
5. **demo 未入库**：`docs/verify/` 整体 `.gitignore`，demo 是未跟踪文件（与既有预览页同口径），可直接打开，交付/可追溯性由用户决定是否需要入库。

## 复验命令（用户可重跑）

```powershell
# 1) 单测（应 123 passed）
& 'C:\Users\22088\.cargo\bin\cargo.exe' test --manifest-path F:\mini-card\native\minicard-cards\Cargo.toml

# 2) 编译检查
& 'C:\Users\22088\.cargo\bin\cargo.exe' check --manifest-path F:\mini-card\native\minicard-bin\Cargo.toml

# 3) 原生逐帧取证（清除全部 / 单条）
cd F:\mini-card\native\minicard-bin
$env:MC_NOTIFY_MOTION='clear-all'; $env:MC_NOTIFY_PRE_MS='200'; $env:MC_NOTIFY_FRAME_MS='16.667'
.\target\debug\minicard-bin.exe --probe notify-motion --w 240 --h 300 --ss 2 --no-glass `
  --font F:\mini-card\native\assets\fonts\MiSans-Regular.ttf `
  --dump-seq F:\mini-card\docs\verify\_notify_swipe_clear\clear-all\frame --exit-after 1000
python F:\mini-card\docs\verify\_notify_motion_20261116\_nm_sheet.py `
  F:\mini-card\docs\verify\_notify_swipe_clear\clear-all\frame 11 43 2 --cols 8 --scale .42
#   MC_NOTIFY_MOTION=remove 出单条右滑帧

# 4) 互动 demo（浏览器直接打开，拖时间轴 / 一键重播 / 深链）
start "" "F:\mini-card\docs\verify\notify_swipe_clear_demo.html"
start "" "F:\mini-card\docs\verify\notify_swipe_clear_demo.html?t=250"
start "" "F:\mini-card\docs\verify\notify_swipe_clear_demo.html?t=270&mode=single"
start "" "F:\mini-card\docs\verify\notify_swipe_clear_demo.html?t=350"   # 组头随最后一条右滑
start "" "F:\mini-card\docs\verify\notify_swipe_clear_demo.html?t=680"
```
