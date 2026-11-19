# 缺陷台账 — dock-dsh-settings-stuck

## D1 空 id 条目与「未展开」哨兵值重叠 → 面板常驻展开且点按钮无效

- **严重度**：S1（用户可见功能失效）
- **复现**：`config.toml` 中 DSH 条目 `id = ""`；进入 Dock 页即见其 `.dopts` 带 `on`；点击「条目设置」按钮无效。
- **证据（修复前）**：`node verify_toggle.js` 输出 `DSH 开合序列: 开→开→开→开→开 (期望 合→开→合→开→合)`，EXIT=1。
- **根因**：`web/index.html` 以 `''` 作「无面板展开」哨兵（L2572），与空 id 相等；渲染 `DOCK_OPT_OPEN === it.id`（L2692）恒真 → 常驻 `on`；切换 `('' === '') ? '' : it.id`（L2706）恒求值为 `''` → 值不变 → 点击无效。本质是哨兵值落在合法数据值域内。
- **修复**：哨兵由 `''` 改为 `null`（L2572 声明 + L2706 切换；L2692 比较式不变）。
- **回归证据（修复后）**：`node verify_toggle.js` EXIT=0，DSH `合→开→合→开→合`、Hermes 同、同实例互斥四断言 PASS。
- **状态**：已修复并真机验证（重编 release 成功、exe mtime 12:56:33 > index 12:54:27、实例重启 PID 24024）。
- **轮次**：第 1 轮发现并修复。

## 独立复核发现的缺陷（verifier）

| 编号 | 标题 | 严重度 | 根因 / 判定 | 处置 | 状态 |
|---|---|---|---|---|---|
| V1 | 重复 id（含多个空 id）互斥失效 | S3 | 面板按 `it.id` 键控，同 id 无法区分；UI 路径造不出重复 id（`dock_add` 用 short_id 保证唯一），旧代码更严重（永久同时展开且都点不动） | 既有设计局限、非本次引入；写入 SPEC 残余风险，交用户拍板；不改键控语义（改索引键会引入 reorder 跳条目新 bug） | 已记录（残余风险，不修） |
| V2 | id 为 JS null/undefined 复现原症状 | S4 | `null === null` 为真；但 `config.rs:132` `id: String` 非 Option、TOML 无 null、serde 必输出字符串，数据域不可达 | 写入 SPEC 残余风险，不修 | 已记录（不可达理论边界） |
| V3 | verify_toggle.js 互斥测试用两个独立实例，测不出同状态机多条目 | S4 | 测试假阴性 | 已改为同一 makeMachine 内两不同 id 条目互测；重跑 EXIT=0，四断言 PASS；verifier 收敛复核确认 write-back 忠实、无假阳性 | 已修复（第 2 轮） |
| V4 | 台账未同步：SPEC 无残余风险章节且措辞过强、DEFECTS 停在待验证、EVIDENCE AC3 引用已删测试名、残余风险成因表述有误 | S4 | 实现者先起草台账、后收到复核发现，未回写 | 已补齐 SPEC 残余风险章节与措辞、更新 DEFECTS 状态、修 EVIDENCE AC3 行 / AC6 / 成因表述 | 已修复（第 2 轮） |

## 轮次小结

- 第 1 轮：锁定契约 → 复现 D1（真实状态机脚本 EXIT=1）→ 修复哨兵 `''`→`null` → 逻辑回归 EXIT=0 → 重编 release 成功、exe 新于源码、重启实例。
- 第 2 轮：独立复核（verifier）判「部分通过」，发现 V1/V2/V3；实现者修 V3、独立确认 V1/V2 处置；verifier 收敛复核确认 V3 闭环、V1/V2 认可为残余风险，新增 V4（台账未同步）；实现者补齐 V4。产品代码无新增缺陷。

## 第 3 轮：V1 根治（用户拍板 B 方案）

### D2 V1 复发风险：重复/空 id 让面板互斥与后端按 id 定位失效（S3）

- **背景**：第 2 轮把 V1 记为残余风险；用户问「有什么办法修吗」并明确选 B（配置层去重根治）。
- **根因**：`dock.items[].id` 的不唯一性没有任何数据层防线——`dock_add` 只在 UI 添加路径保证唯一，手工编辑 config.toml 可直接造出空/重复 id；前端面板按 id 键控、后端 remove/update/click/reorder 全按 id 查找，故一旦重复就处处歧义。
- **修复**：`minicard-core/src/config.rs` 在 `Config::load()` 加一次性迁移（`dock_item_ids_version` 版本位），把空/重复 id 用 `normalize_dock_item_ids()` 补成确定性唯一 id。
  - 确定性关键：候选 = FNV-1a(path + name + index + salt)，不含时间/随机/进程态——因为主进程与原生绘制进程都会 load+save，随机 id 会双进程竞态互相覆盖。
- **回归证据**：`cargo test -p minicard-core` 52 passed / 0 failed（含 5 条新增：空/重复唯一、确定性、幂等、保序、版本位 legacy）；live config 迁移后 `dock_item_ids_version=1`、无 `id=""`、DSH 新 id `7aaedf4d`、16 个 dock id 全唯一、无重复；两 crate release 重编成功、native bin 部署到 0.1.4 目录、实例重启 PID 16740。
- **状态**：已修复并真机验证（待 verifier3 独立复核）。
- **轮次**：第 3 轮。

### D3 编译警告（S4，实现者自检发现并修）

- 初版 `let mut candidate = String::new()` 循环内重赋值触发 `unused_assignments` 警告；改为 `let candidate = loop { ... break cand; }` 表达式形式，重编已无该警告。
- **状态**：已修复（第 3 轮）。

### D4 verifier3 观察项 O2：生成候选只查已遍历 id，可能误改后面的合法 id（S4）

- **复现（verifier3 构造）**：item0 空 id 生成候选 c0；item1 合法 id 恰为 c0 → 旧算法下 item1 被判重复、id 被改，违反「合法 id 原样保留」。
- **根因**：候选排除集 `taken` 只含已遍历项，未含尚未遍历的合法 id（虽概率 ~N/2³²）。
- **修复**：先收齐全部非空 id 作 `reserved` 排除集，候选避开 reserved、生成结果回填 reserved；另设独立 `seen` 判「合法首现」。新增单测动态构造该输入。
- **回归证据**：`cargo test -p minicard-core` 53 passed / 0 failed（含新测 `dock_ids_legit_id_equal_to_earlier_candidate_stays` OK）；verifier3 收敛复核确认闭环、无新增缺陷。O1（中间版警告日志）以 core_test_final.log 清掉；O3（name/icon 单测断言）经 live Compare-Object diff（仅 3 行）判定充分覆盖，不扩测。
- **状态**：已修复并复核（第 3 轮复核轮）。

### 第 3 轮复核轮小结

- 首轮：verifier3 判 9 条 AC 全通过，提 O1/O2/O3。
- 收敛轮：O2 修复 + 新单测 + 双 crate 重编 + 重启，verifier3 确认可交付、无未关闭缺陷。第 3 轮收敛。
