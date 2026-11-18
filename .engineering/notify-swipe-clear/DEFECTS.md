# 缺陷台账 · notify-swipe-clear

> 每轮结束写小结。收敛判据：无未关闭缺陷，且连续一轮无新发现。

## 轮次 1（执行前）

尚未开始执行，无缺陷。

| 编号 | 严重度 | 复现步骤 | 证据 | 根因 | 修复 | 状态 | 轮次 |
|---|---|---|---|---|---|---|---|
| — | — | — | — | — | — | — | — |

## 轮次 2（独立评审发现 → 返工）

| 编号 | 来源 | 严重度 | 复现步骤 | 证据 | 根因 | 修复 | 状态 | 轮次 |
|---|---|---|---|---|---|---|---|---|
| V1 | verify-swipe-ac | S3 | 快照在错开窗口内提前收敛到空 → 尾部未起滑的行瞬间消失（弹出而非滑出）+ 白烧帧 | VT2：mark_cleared 后 50ms observe(空快照) → tree 无行节点、animating=true | `display_state` 返回 Empty → `tree()` 走空态分支，跳过还在 exiting 的尾行 | `paint()` 加渲染态 override：`state==Empty && 有 exiting 在动` 时保持 Normal，退出清空后再进空态；新增单测 `empty_snapshot_mid_stagger_keeps_rendering_until_swipe_finishes` | 已修（待复核） | 2 |
| V2 | verify-swipe-aes | P2 | demo t=560/680 空态：四个组头（含角标）仍悬在「暂无通知」上层 | `--dump-dom ?t=680`：empty opacity 0.996 同时三组 g-head 仍在 | demo 空态只淡入 empty 层，没隐藏 viewport 里的组头 | `renderAt` 空态时 `viewport.opacity=0`（旧内容即时移除）；`?t=680` 重渲染确认只剩「通知」+「暂无通知」 | 已修（待复核） | 2 |
| V3 | verify-swipe-aes | P2 | 快淡领先位移：alpha=0.5 时 dx 仅 8%，像「原地淡出+漂移」而非「划出」 | 数值表 + demo DOM t=250 opacity 0.272 但 dx 73.7 | alpha 用 `1-easeOutQuart`（快淡），dx 用 `ease_standard`，两条曲线不同步 | alpha 改用 `1-ease_standard`（与 dx/h 同曲线，alpha=0.5↔dx=50%）；demo 同步；曾试「h 延后收缩」但破坏既有「配额挤出不跳」不变量，已回退只改 alpha | 已修（待复核） | 2 |

> 未修（记录为残余风险）：
> - verify-swipe-ac V2（S3，**已核证为前轮既有，非本轮引入**）：入场未完成的行（半高）被 clear → 一帧跳满高再滑。`git show HEAD:notify.rs` 的退出分支 `e=1-easeOutQuart(p); h=item_h*e` 在 p=0 时 h=item_h（满高），与本轮公式一致；评审者「本轮从 h=0 改成满高」的判断有误。修它要动退出 h 起点，会破坏「配额挤出严格抵消」不变量，本轮不混。
> - verify-swipe-ac V3（S4）：单条删除进行中再 clear，错开首个可见行晚 60ms 起滑（suppress 造成下标偏移）；观感轻微。
> - verify-swipe-aes V3（复核降为 **P3 观察项**）：组头角标 2→1、时间「1 分钟前→3 分钟前」在退场中途硬切（静态元素文字替换，原生行为，语义正确、无闪烁/几何跳变）；不判 P1，给组头做 120ms 交叉属另一轮视觉改动。

## 轮次 3（用户反馈：组头留到最后才清）

| 编号 | 来源 | 严重度 | 复现步骤 | 证据 | 根因 | 修复 | 状态 | 轮次 |
|---|---|---|---|---|---|---|---|---|
| V4 | 用户原话 | S2（违背核心诉求） | 点确认清除全部：消息一条条右滑消失，但微信/邮件/QQ 的**应用图标+名字组头**全程不动，最后才一起消失 | 原 `clear-all/frame/sheet.png`：f019 消息已滑走、组头仍完整 | `GroupView` 只有 `group_enter_alpha`，组头无退出动画；`tree()`/`paint_tree()` 里组头高度与透明度不随退出变化 | `GroupView` 加 `header_exit`（本组最后一条退出行的 t0，仅整组都在退出时置位）；新增 `header_anim()`（入场×退场右滑）；`tree()` 用其缩放高度/吃组间距；`paint_tree()` 组头分支加 dx 变换+alpha；demo 的 `.g-head` 同步按本组最后行错开右滑 | 已修（待复核） | 3 |

## 轮次 4（独立复核 V4 时发现的边界闪回）

| 编号 | 来源 | 严重度 | 复现步骤 | 证据 | 根因 | 修复 | 状态 | 轮次 |
|---|---|---|---|---|---|---|---|---|
| V5 | verify-head-exit | S3 | 清除错开期内同组来了新消息 → 组头滑到一半突然消失、dx 瞬移回 0、重新淡入 | 逐帧测试 ms=110 组头 dx=73.7/a=0.693 → ms=120 新消息到达后 dx=0/a=0 再淡入 | ① 清除时 seen_groups 被清空 → 新消息触发组重新入场；② header_exit 每帧从「整组都在退」重算，新消息使 all(exit) 为假 → 组头退场取消 | ① `observe` 的 keys 纳入仍在退场的行所属组 key，保住 seen_groups；② Motion 加 `group_header_exit`（clear 时钉住各组头 t0，取本组最后一条），`build_groups` 优先用它；prune 在窗口后清理；新单测 `new_message_mid_clear_does_not_flicker_group_header` | 已修（待复核） | 4 |

> 顺带清理：删掉 `HeaderAnim.e_exit` 死字段、新测试未用的 t/opts（S4）。

## 轮次 5（复核 V5 时发现窗尾弹出）

| 编号 | 来源 | 严重度 | 复现步骤 | 证据 | 根因 | 修复 | 状态 | 轮次 |
|---|---|---|---|---|---|---|---|---|
| V6 | verify-head-exit | S3 | 清除错开期同组新消息让组存活，退场窗结束帧（ms=240）组头 dx 239.6→0 / alpha 0.002→1.0 单帧弹出 | 逐帧测试 ms=235 a=0 → ms=240 a=1.0 | `group_header_exit` 到点被清、组仍存活 → header_exit=None、alpha 一帧满血 | `prune` 清过期记录时若组仍在 `seen_groups`，`group_enter.insert(key,now)` 走 120ms 淡入；新单测扩展断言窗末 ge 首帧≈0、120ms≈1 | 已修 | 5 |

## 轮次小结

- 轮次 1：0 新增 / 0 已修 / 0 剩余。
- 轮次 2：新增 3（V1/V2/V3，独立评审）/ 已修 3 / 剩余 0。两个独立检验者复核确认关闭。
- 轮次 3：新增 1（V4，用户反馈的核心缺陷）/ 已修 1，独立复核确认核心通过。
- 轮次 4：新增 1（V5，复核 V4 时发现）/ 已修 1，复核确认关闭。
- 轮次 5：新增 1（V6，复核 V5 时发现）/ 已修 1，复核确认关闭、无新缺陷。
- **收敛达成**：轮次 5 无新缺陷，独立检验者复核无新发现。
