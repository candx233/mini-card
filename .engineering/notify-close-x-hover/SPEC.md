# 契约 · 通知卡 × 放大与悬停交互动画

## 原话（照录）

> 把通知卡的每一条消息的叉叉调整大一点 然后鼠标放上去加上交互动画

## 目标

放大通知卡每条通知右侧的关闭 ×，并让鼠标悬停到 × 上时播放可见的交互动画。

## 验收标准（AC）

| # | 判据（可判定） | 判定方式 |
|---|---|---|
| AC1 | 240×300 卡上 × 字号从 10px（0.625r）增到 ≥12px、命中列宽从 14px（0.875r）增到 ≥17px；其余 token（行高/字号/间距/配色）不变 | Rust 单测断言 `Tokens::notify(240,300)` 的新字段与布局树 `id_item+3` 节点 rect；`cargo test` |
| AC2 | 点击 × 节点中心仍返回 `Hit::Remove`，命中区宽度随放大而增大；正文行不被挤压到 <100px | 扩展 `hit_maps_row_and_remove_x` + 静态容量单测 |
| AC3 | 鼠标进入 × 节点后 120ms 内 close_hover 补间 0→1（ease_out_quart），离开后回落 0；paint 用它驱动字号放大 ≥1.2×、颜色 muted→warn、圆形底圈淡入 | 新增 Motion 单测逐帧断言 + probe hover 帧截图比对 |
| AC4 | 既有整行 hover 淡入（120ms、零位移）不受影响；× 仍只在行 hover 时出现 | 既有 hover 单测全绿 + hover 帧截图比对 |
| AC5 | close_hover 补间期间 `Motion::animating()` 为 true，结束后 `prune` 清理；不出现卡帧/脏帧 | 单测断言 animating/prune 行为 |
| AC6 | 只动 × 节点：卡框/抬头/分割线/文本行/未读点/组头几何在静态与 hover 两态完全一致 | 单测布局快照对比 + probe 像素差分（差异只落在 × 区域） |
| AC7 | 回归：`cargo test`（minicard-cards）全绿；`cargo check -p minicard-bin` 通过 | 命令与输出贴入 EVIDENCE.md |
| AC8 | 1× 真机尺寸下 × 不发糊、不被圆角/列宽裁切；给出静态/hover 并排对比图；独立检验无 P1+ | `--probe notify` 1×/2× 出图 + `engineering-verify` 子代理只读报告 |

## 非目标

- 不改退场（右滑/塌缩）/滚动/清除/状态淡入/配额挤出 等既有动效
- 不改其它卡片、web 主界面、config/数据层、Dock
- 不引入外部依赖；不新增第二套动效系统（复用 `Motion` + `HoverTween`）
- 不做真实拖拽手势；不改点击语义（仍是点 × 删除）

## 约束

- 平台 Windows；Rust crate `native/minicard-cards`（必要时 `minicard-bin` probe）；cargo 在 `C:\Users\22088\.cargo\bin\cargo.exe`
- 只放大用户点名的 ×，其余元素保持不动；幅度控制在 +20~30%（证据来自历史偏好：小幅比例调整 + 真机 1x 验证）
- hover 时长沿用 `HOVER_MS = 120`，缓动沿用 `ease_out_quart`
- × 仍是文本字形 `"×"`，靠字号插值实现「放大」动画（TextEngine 每帧重栅格化，避免位图缩放发糊）
- 工作区已有未提交改动（config.rs / web/index.html / dock 任务台账）**一律不碰**
- 台账落 `.engineering/notify-close-x-hover/`

## 未知与假设（标风险）

| # | 假设 | 风险 / 错了会怎样 |
|---|---|---|
| A1 | 「交互动画」= × 自身的 hover 反馈（放大+变色+底圈），不是改整行淡入 | 若用户想要旋转/图标变形等别的形态，需返工；已在交付里明确说明做了什么动画 |
| A2 | 放大幅度 10→12/13px（+20~30%）符合「大一点」 | 若仍显小，继续小幅上调（需回契约改 AC1 阈值） |
| A3 | 字号插值在 1× 下清晰不糊 | 只在 2× 预览确认不算通过，必须 1× 出图核对 |
| A4 | hover 色用 `pal.warn`（橙红）表达「删除」语义，符合主界面「hover 变红」口径 | 若用户偏好强调蓝/纯白，可换；不影响动画结构 |
| A5 | `g.hover` 已是卡内逻辑坐标（live.rs 现状），× 命中判定按内容坐标 `y+scroll` 反算 | 若坐标口径不一致，× hover 会错位；用 probe 固定鼠标点验证 |

## 决策记录

| 时间 | 决定 | 影响面 |
|---|---|---|
| 2026-11-19 | 新增 Tokens 字段 `close_fs` / `close_w`，只用于 ×，其余 token 不动 | tree() 的 × 节点与 paint 的 × 分支 |
| 2026-11-19 | 复用 `HoverTween` 做 × 专属补间（不新建动画框架） | Motion 增加 `close_hover` map + update/alpha/animating/prune |
| 2026-11-19 | × hover 动画 = 字号 1.0→1.25 + muted→warn + 圆形底圈淡入，120ms | 只改 paint 的 `pl.id - id_item == 3` 分支 |
| 2026-11-19 | 命中列宽 14→18px 使正文行从 163.5px 缩到 159.5px（-4px） | 放大 × 的必然结果；实测正文仍 >100px 且省略号正常，接受 |
| 2026-11-19 | 探针加 `MC_NOTIFY_HOVER="x,y"` 与 `MC_NOTIFY_MOTION=hover` 取证钩子 | 仅在 `minicard-bin --probe` 路径生效，不影响 live 渲染 |

## 判档

**标准档**：涉及动效状态机 + 命中几何 + 既有 hover 回归面 + 真机视觉验收 → 三件套 + 独立检验。
