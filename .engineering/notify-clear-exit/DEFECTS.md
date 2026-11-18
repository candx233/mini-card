# 缺陷台账 · notify-clear-exit

> 每轮结束写小结。收敛判据：无未关闭缺陷，且连续一轮无新发现。

## 轮次 1（执行期自检 + 独立评审）

| 编号 | 严重度 | 复现步骤 | 证据 | 根因 | 修复 | 状态 | 轮次 |
|---|---|---|---|---|---|---|---|
| D1 | S2 | 浏览器打开 demo，主卡只有抬头、列表空白 | `_notify_clear_exit_demo_shot.png` 主卡无行 | `card.appendChild(buildCard().firstChild)` 只搬了第一个子节点 | 改成 `card.innerHTML = buildCard().innerHTML` | 已修 | 1 |
| D2 | S3 | 关键帧缩略图里「邮件」换行成「邮/件」，正文被硬裁 | `_notify_clear_exit_strip_crop.png` 放大可见 | 缩略图用 240px 宽却套 360px 卡的字号 | 缩略图改为主卡同尺寸整体上等比缩放（`.mini` 360×450 @0.4167），并给 `.g-name`/`.time` 加 nowrap | 已修 | 1 |
| D3 | S3 | demo 到空态后抬头仍显示「1 条未读 / 清除」 | `_notify_clear_exit_t420.png` | 空态淡入时没同步淡出计数/清除节点（原生不建这俩节点） | `renderAt` 用 `(1-emptyA)` 淡出 count/clear | 已修 | 1 |
| D4 | S3 | 初版仅行塌缩时，正文在 f016/f018 被横切成薄片 | `clear-all/frame/sheet.png`（旧） | 行高插值但内容未同步淡出（既有 P2，tbl 见 tuning 文档 §4） | 新增 180ms 整列淡出层（`clear_fade_alpha` + viewport `push_opacity`），与塌缩同拍 | 已修 | 1 |
| D5 | S4 | 单测断言 prune 后 fade alpha 应为 0，实际流程返回 1 | `clear_all_collapses...` 首次 FAIL：`left:0 right:1` | 测试没调用 `paint` 里的 `prune`；prune 后返回常态 1.0 才是正确语义 | 测试显式调 `prune`，断言回到 1.0 | 已修 | 1 |

## 轮次 2（独立评审发现 → 返工）

| 编号 | 来源 | 严重度 | 复现步骤 | 证据 | 根因 | 修复 | 状态 | 轮次 |
|---|---|---|---|---|---|---|---|---|
| V1 | verify-ac | S3 | clear 淡出期内（<180ms）新通知到达：`mark_cleared@200` → `+50ms observe([99,...])` → paint 在 180ms 先 prune 把 `clear_fade_t0=None`，alpha 0→1 跳变，新行 e≈0.94 一帧满高闪现 | 临时测试给出 alpha@170=0.002 / @180=1.0 与新行 36px 满高 | 淡出层作用整个视口，撤层时把新行一起从 0 提到 1 | 新增 `deferred_clear: Vec<u32>`：fade 期新 id 进 suppressed+deferred，prune 撤层时释放并 `enter.insert(now)`；新增单测 `new_arrival_during_clear_fade_is_deferred_then_enters_smoothly` | 已修（待复核） | 2 |
| V2-ac | verify-ac | S4 | 空快照误调 `mark_cleared(&[])` → `animating@0/@90=true`，视口空却白烧 3 帧 60fps | 临时测试 SCN1 | 没判空 | `mark_cleared` 开头 `if items.is_empty(){return;}` + 单测 `mark_cleared_on_empty_snapshot_is_noop` | 已修（待复核） | 2 |
| V3-ac | verify-ac | S4 | 单独拷 demo HTML 到无字体目录，@font-face 引用 3 个外部 woff2 | `:8-10` 源码 | 契约写「必须单文件」但引了外部字体 | 删掉 3 条 @font-face，改系统字体栈（Segoe UI / Microsoft YaHei / PingFang SC） | 已修（待复核） | 2 |
| V2-aes | verify-aesthetics | P2 | demo 页眉「可检验 demo」徽章被撑成 1180px 通栏并换行 | `shots/title_crop.png` | `.badge` 定义两次，卡内药丸的 `display:flex` 全局覆盖页眉 `inline-block` | 页眉两处改 `class="tag"`，CSS `.tag` 独立定义 | 已修（待复核） | 2 |
| V3-aes | verify-aesthetics | P2/契约偏差 | demo t=390–460 同时可见「1 条未读/清除」与「暂无通知」叠字 | demo `?t=420` 旧版截图 | 我用 `(1-emptyA)` 交叉淡出抬头，违反 B 案 §3.6「即时移除」 | 改 `emptyA>0?'0':'1'` 即时移除 | 已修（待复核） | 2 |

> V1-aes（塌缩中段文字切片，P2）：评审确认 1× 实尺寸不可辨（整列淡出遮罩），属既有行高插值方案的固有代价
> （tuning 文档 §4 已记录为可接受 P2），本轮不引入「先淡出再收高度」的新一套观感。列为残余风险，不修。

## 轮次 3（返工回归）

- `cargo test --manifest-path native/minicard-cards/Cargo.toml` → **119 passed; 0 failed**（117 + V1/V2-ac 两条新单测）。
- `cargo check --manifest-path native/minicard-bin/Cargo.toml` → `Finished dev profile`。
- 修复后 exe 重跑 `--probe notify-motion clear-all`，f010/f012/f016/f020/f024/f030 与修复前 **逐字节相同（maxdiff=0）** —— V1 修复不影响 happy path。
- demo 重渲染 `?t=0`：页眉徽章为小胶囊；`?t=420`：抬头计数/清除即时移除、空态正常。demo 已无任何外部引用（17KB）。
- 独立复核：verify-ac 判 V1/V2/V3 全部关闭、无新缺陷；verify-aesthetics 判 V2/V3 关闭、无新视觉缺陷。

## 轮次小结

- 轮次 1：新增 5（自检）/ 已修 5 / 剩余 0。
- 轮次 2：新增 5（独立评审 V1/V2-ac/V3-ac/V2-aes/V3-aes）/ 已修 5 / 剩余 0。
- 轮次 3：无新增；回归全绿；两个独立检验者复核均无新发现。**收敛达成**（连续一轮无新增 + 无未关闭缺陷）。
