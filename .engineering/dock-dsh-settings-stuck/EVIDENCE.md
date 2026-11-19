# 证据表 — dock-dsh-settings-stuck

## AC 对照表（v2，复核后）

| AC | 判定步骤 / 命令 | 实际输出（证据摘要） | 结论 |
|---|---|---|---|
| AC1 默认收起 | `node verify_toggle.js`（抽取 index.html 真实三处代码，空 id 条目初始态） | 修复后 DSH 序列首态 = `合`（`合→开→合→开→合`）；修复前为 `开→开→开→开→开`。verifier 内联驱动 default 亦 off | 通过 |
| AC2 开合切换 | 同上，空 id 条目连续点 4 次 | `DSH 开合序列: 合→开→合→开→合` EXIT=0；verifier 复跑 `off→ON→off→ON→off` | 通过 |
| AC3 无回归 | 同一状态机内两不同 id 条目互斥（修复后的 verify_toggle.js）+ 非空 id 条目序列 | `Hermes 开合序列: 合→开→合→开→合` PASS；`展开 A 后 A 开/B 合`、`展开 B 后 A 合/B 开` 四断言 PASS。**限制**：重复 id 场景下互斥失效（旧代码更严重），现场唯一 id 数据域内通过——见残余风险 V1 | 通过（可达数据域内）+ 残余风险 V1 |
| AC4 逻辑可证 | 脚本从 `web/index.html` 正则抽取 `DOCK_OPT_OPEN` 初值/渲染判定/切换表达式，用 `new Function` 驱动，非手抄答案 | 输出三处源码片段：初值 `null`、渲染 `(DOCK_OPT_OPEN === it.id ? ' on' : '')`、切换 `... ? null : it.id;`；EXIT=0。verifier 确认 write-back 忠实、无假阳性 | 通过 |
| AC5 真机生效 | `cargo build --release` + exe/index mtime 比对 + 重启 | `Finished release profile in 25.24s` EXIT=0；exe mtime 12:56:33 > index mtime 12:54:27 = True；codegen asset 12:55:21 介于两者；重启后 mini-card PID 24024 StartTime 12:56:44 > exe mtime | 通过 |
| AC6 独立复核 | 只读子代理 verifier 两轮按 engineering-verify 复核 | 第 1 轮「部分通过」，发现 V1/V2/V3；第 2 轮收敛复核：V3 闭环、V1/V2 认可为残余风险、产品代码无新增缺陷、git diff 仅 web/index.html（4+/2-）；新增 V4（台账未同步）已由实现者补齐 | 通过 |

## 关键证据原文摘录

1. 现场数据 `%APPDATA%\mini-card\config.toml`：`[[dock.items]]` 中 `id = ""` / `name = "DeepSeek Harness"`，其余条目 id 非空（`net-ease`、`5af3b579`、`1ea07de8` 等）。
2. 根因链（`web/index.html` 修复前）：L2572 `var DOCK_OPT_OPEN = '';`、L2692 `(DOCK_OPT_OPEN === it.id ? ' on' : '')`、L2706 `DOCK_OPT_OPEN = (DOCK_OPT_OPEN === it.id) ? '' : it.id;`。空 id → `'' === ''` 恒真 → 常驻 `on`；点击 → `''` 不变 → 无效。
3. 修复（修复后）：L2572 `var DOCK_OPT_OPEN = null;`（附注释说明为何不能用 `''`）、L2706 `... ? null : it.id;`；L2692 比较式不变。
4. 复现证据（修复前 EXIT=1）：`DSH 开合序列: 开→开→开→开→开  (期望 合→开→合→开→合)`，其余 PASS。
5. 回归证据（修复后 EXIT=0）：DSH 与 Hermes 序列均 `合→开→合→开→合`；同实例互斥四断言 PASS；重复 id 边界探测诚实记录 `D1=开 D2=开`（非断言，不计入 EXIT）。
6. 构建：首轮因运行实例占用 exe 失败（`failed to remove file ...mini-card.exe 拒绝访问 os error 5`）；关闭 PID 14260/28004 后重编成功。
7. V2 不可达证据：`native/minicard-core/src/config.rs:131-132` `pub struct DockItem { pub id: String, ... }`，非 Option；TOML 无 null，serde 必输出字符串。

## 轮次小结

- 第 1 轮：锁定契约 → 真实状态机脚本复现 D1（EXIT=1）→ 修复哨兵 `''`→`null` → 逻辑回归 EXIT=0 → 重编 release 成功、exe 新于源码、重启实例。
- 第 2 轮：独立复核（verifier）判部分通过 → 修 V3（互斥测试改同一实例）→ 独立确认 V1/V2 处置 → verifier 收敛复核确认 V3 闭环、V1/V2 认可为残余风险 → 修 V4（台账同步）。产品修复在可达数据域内收敛为通过。

## 残余风险 / 未验证项

- **V1（S3，待用户拍板）**：重复 id（含多条空 id）条目互斥失效——展开一条则所有同 id 条目一起展开。成因是 `DOCK_OPT_OPEN === it.id` 对两个 `id=''` 同时为真（与 `null !== ''` 无关）。UI 路径造不出重复 id（`dock_add` 用 short_id 保证唯一），现场仅一条空 id，不受影响。若要支持重复 id 互斥需另行立项（在配置加载层保证 id 唯一）。
- **V2（S4，不可达）**：id 为 JS null/undefined 时复现原症状；`config.rs:132 id: String` 非 Option，数据域不可达，不修。
- 真机 GUI 肉眼点击未做（浏览器扩展离线）；开合结论由驱动真实源码片段的状态机脚本 + 独立复核给出。
- exe 内嵌 index.html 为压缩资产，无法逐字节证明嵌入新代码；以时间点证据（exe > index、codegen asset 介于其间）佐证。

## 交付清单

- 产品代码：`web/index.html`（4 增 2 删：2 行功能改动 + 2 行注释）。
- 台账：`.engineering/dock-dsh-settings-stuck/{SPEC.md,DEFECTS.md,EVIDENCE.md,verify_toggle.js,probe_edges.js,build.log}`。
- 复验命令：`node F:\mini-card\.engineering\dock-dsh-settings-stuck\verify_toggle.js`（期望 EXIT=0）。
