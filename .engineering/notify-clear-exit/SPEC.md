# 契约 · 通知卡清除消息补动效

## 原话（照录）

> 给通知卡的清除消息补上动效 顺便检查一下动效的美观程度 做完之后给我一个可以检验的demo

用户澄清（本轮问答）：
- 补的动效 = **「确认后消息立即整列退场」**（不等系统快照更新的 1.5s；把 web 预览的效果接进原生）
- demo 形式 = **互动 HTML 预览页**

## 目标

给通知卡「清除→确认」二次确认后的消息列表补上即时本地退场动效，并交付可交互检验的单文件 HTML demo，附带美观程度的独立评审证据。

## 验收标准（AC）

| # | 判据（可判定） | 判定方式 |
|---|---|---|
| AC1 | 点「确认」的同一帧起，所有可见行进入 180ms 塌缩：行高 38→0、透明度 1→0，全程单调；t<180ms 时 `Motion::animating()` 为 true | 新增单测断言 t=0/45/90/180ms 的 `RowAnim`；`cargo test -p minicard-cards` |
| AC2 | 退场期间系统快照仍含这些项时，行不「闪回」重播入场（suppress 生效）；快照清空后 suppress 收敛 | 单测：confirm 后 `observe(原快照)`，再 `observe(空快照)`，断言退出态与收敛 |
| AC3 | 退场只动消息区：卡框、分割线、抬头像素在整段动画中差为 0；空态 160ms 淡入沿用既有实现 | `--probe` 帧序列 + 像素差分；或单测几何断言 |
| AC4 | 已有交互零回归：到达/删除/滚动/清除↔确认交叉/hover/状态 的现有单测全绿，基线 `115 passed` | `cargo test --manifest-path native/minicard-cards/Cargo.toml`（用 `C:\Users\22088\.cargo\bin\cargo.exe`） |
| AC5 | demo 为**单文件 HTML**，浏览器直接打开即可重播「清除→确认→整列退场→空态」，支持逐帧拖动/关键帧条；时长与曲线标注且与原生一致（交叉 120ms easeOutQuart、退场 180ms ease_standard、空态 160ms easeOutQuart） | 本地 HTTP 200 + 无头 Edge 渲染成功 + 源码参数逐项核对 |
| AC6 | 美观检查有独立评审证据：关键帧拼图 + 独立检验子代理只读评审，无 P1 及以上缺陷（无跨帧闪烁、无突兀跳变、缓动顺滑） | `engineering-verify` 子代理报告 + `EVIDENCE.md` 记录 |

## 非目标（本轮明确不做）

- 清除按钮的 0.98 按压缩放（需新增 `UiEvent::Press`；用户本轮未选此项）
- 改布局、信息架构、配色、字号、既有时长口径（120/160/180ms）
- 改 web 主应用（`web/card.html` 等）
- demo 引入外部依赖 / 联网（必须单文件、离线可开）
- 逐行 stagger 瀑布式退场作为默认（见假设 A1；仅作为 demo 里的对比变体）

## 约束

- 平台：Windows；Rust `minicard-cards` / `minicard-bin`；cargo 在 `C:\Users\22088\.cargo\bin\cargo.exe`
- 复用现有 `Motion.exiting` / `suppressed` / `ease_standard`，**不新增第二套动效系统**
- 时长：退场 180ms（行高/透明度用 `ease_standard`），保留清除↔确认 120ms、空态 160ms 原曲线
- 隐私：`Motion` 只存 id/t0，不把标题/正文写日志
- 台账固定落在 `.engineering/notify-clear-exit/`，任务结束保留

## 未知与假设（标风险）

| # | 假设 | 风险 / 错了会怎样 |
|---|---|---|
| A1 | 「整列退场」= 所有行**同时**塌缩（与 web 预览整列淡出同拍），非逐行 stagger | 若用户想要瀑布式逐行退场，观感不同；demo 提供 A/B 变体供一眼拍板，改起来只需给 t0 加行序偏移 |
| A2 | 确认后 `snap.items` 仍含已清项（Windows 异步 ≤1.5s），靠 suppress 压住；>2s 未收敛按既有机制回弹重入 | 沿用现有单条删除约定，非本轮新风险 |
| A3 | 空态出现时机 = 真实快照收敛后（原生），demo 里按「退场 180ms 后紧接空态 160ms」演示连续流程 | 真机可能有 ~0.5–1s 空白间歇；若评审判为美观缺陷，再考虑乐观置空（会扩大改动面，需回契约） |
| A4 | demo 是独立 HTML mock，非原生卡截图 | mock 与原生观感有差异；由 `--probe` 原生帧序列补证像素级一致性 |

## 决策记录

| 时间 | 决定 | 影响面 |
|---|---|---|
| 2026-11-17 | 采用「复用 exiting/suppressed 做本地退场」，不新增动效字段 | 只改 `Motion` 加一个方法 + `live.rs` 一次调用 + 单测；回归面小 |
| 2026-11-17 | demo 自建单文件 HTML，不复用 848 行的 `notify_motion_preview.html` | 避免改动既有 demo；新 demo 可独立交付 |

## 判档

**标准档**：跨 native + demo 多文件、有回归面 → 三件套 + 独立检验。
