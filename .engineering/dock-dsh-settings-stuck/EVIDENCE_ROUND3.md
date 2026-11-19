# 证据表 — dock-dsh-settings-stuck 第 3 轮（B 方案根治 V1）

## AC 对照表

| AC | 判定步骤 / 命令 | 实际输出（证据摘要） | 结论 |
|---|---|---|---|
| AC-R1 非空唯一 | `cargo test -p minicard-core` 中 `dock_ids_empty_and_duplicate_become_unique` + live config 检查 | 单测 PASS（断言 all !empty + 两两不同）；live config 16 个 dock id 全唯一、无 `id=""` | 通过 |
| AC-R2 确定性 | 单测 `dock_ids_normalization_is_deterministic` + 代码审查哈希源 | 两份相同脏配置 → ids 完全相同；哈希源 = path+name+index+salt，无时间/随机/进程态 | 通过 |
| AC-R3 幂等 | 单测 `dock_ids_normalization_is_idempotent` | 干净配置 changed=false、id 不变；规范化过一次后再跑 id 不变 | 通过 |
| AC-R4 保序保字段 | 单测 `dock_ids_empty_and_duplicate_become_unique` | 顺序 path 序列不变、args/divider_before 不变、首个合法 id 保留 | 通过 |
| AC-R5 一次性迁移 | 单测 `legacy_config_needs_dock_id_migration` + 代码审查 | 老 config 缺字段 → version=0 → 触发迁移；Default = 1；live config 出现 `dock_item_ids_version=1` | 通过 |
| AC-R6 单测覆盖 | `cargo test -p minicard-core` | `52 passed; 0 failed; 1 ignored`（新增 5 条全绿），EXIT=0 | 通过 |
| AC-R7 双 crate 重编 | `cargo build --release`（minicard-bin 与 src-tauri） | 两者 `Finished release profile` EXIT=0；native bin mtime 13:18:10 部署到 0.1.4；进程 mini-card PID 16740 / minicard-bin PID 712 运行 | 通过 |
| AC-R8 现场数据收敛 | 读 live config + 与 .bak-dockid 备份比对 | 迁移前 DSH `id=""`；迁移后 `id="7aaedf4d"`（确定性哈希），其余字段不变；无重复 id。前端 null 哨兵仍在（web/index.html 未再改） | 通过 |
| AC-R9 独立复核 | 只读子代理 verifier3 两轮（首轮 + O2 修复后收敛复核） | 首轮：9 条 AC 全通过、独立用 BigInteger 重算 FNV 得 `0x7aaedf4d` 与落盘逐字符一致；提 3 条 S4 观察项（O1 证据日志含中间版警告 / O2 候选只查已遍历 id / O3 单测未断言 name+icon）。修复 O2 后收敛复核：O2 闭环、O1/O3 接受关闭、无新增缺陷、可交付 | 通过 |

## 关键证据原文摘录

1. 迁移前备份 `%APPDATA%\mini-card\config.toml.bak-dockid`：`id = ""` / `name = "DeepSeek Harness"`。
2. 迁移后 live config：`dock_item_ids_version = 1`；DSH 块 `id = "7aaedf4d"` / `name = "DeepSeek Harness"` / `path = 'C:\Users\22088\Desktop\DeepSeek Harness.lnk'`；`args=""` `icon=""` `divider_before=false` 不变。
3. 16 个 dock id 唯一性检查：`no duplicates (26 ids)`（含 10 个卡片 id，dock 段 16 条全非空）；迁移后 dock 段无 `id = ""`。
4. `cargo test -p minicard-core`：`test result: ok. 52 passed; 0 failed; 1 ignored`，新增 `dock_ids_empty_and_duplicate_become_unique` / `dock_ids_normalization_is_deterministic` / `dock_ids_normalization_is_idempotent` / `legacy_config_needs_dock_id_migration` / `dock_ids_same_path_different_index_unique` 全 ok。
5. 构建：`native/minicard-bin` `Finished release in 2m22s` EXIT=0；`src-tauri` `Finished release in 38.69s` EXIT=0。
6. git diff（本轮 + 前两轮）：`native/minicard-core/src/config.rs | 167 +`、`web/index.html | 6 ++++--`。

## 轮次小结

- 第 3 轮：用户选 B → 锁契约（SPEC_ROUND3.md）→ core 实现确定性 id 规范化 + 版本位 + 5 单测 → cargo test 52 绿 → 关闭实例 + 备份 config → 双 crate release 重编 → 重启触发迁移 → live config DSH id 从空变 `7aaedf4d`、全 id 唯一、native bin 部署更新、实例运行。
- 第 3 轮复核轮：verifier3 独立复核 9 条 AC 全通过（BigInteger 重算 FNV 与落盘 id 逐字符一致）；提 O1/O2/O3 三条 S4 观察项。实现者修 O2（候选避开全部非空 id + 独立 seen 判重 + 新单测 `dock_ids_legit_id_equal_to_earlier_candidate_stays`）、清 O1（core_test_final.log 无 candidate 警告），O3 经 live diff 充分覆盖不扩测。cargo test 53 绿 → 双 crate 再重编 → 重启 PID 33148/5280。verifier3 收敛复核：O2 闭环、无新增缺陷、可交付。

## 残余风险 / 未验证项

- 真机 GUI 肉眼点击未做（浏览器扩展离线）；第 1 轮 null 哨兵 + 第 3 轮数据去重双保险，前端开合逻辑未因本轮改动变化。
- 迁移已落盘（DSH id 由空变哈希值 `7aaedf4d`），不可逆；但空 id 本就非法，回滚代码不影响已迁移数据。
- 自定义图标：`normalize_dock_item_ids` 不改 `item.icon` 字段，`ensure_icon` 缓存按 path 哈希，故迁移不影响已有图标。
- 新算法（O2 修复后）未在 live 脏数据上实跑（版本位已 1 不重迁移）；正确性由 6 条单测 + 静态不变式 + 独立复核保证。
- 迁移前配置已备份 `%APPDATA%\mini-card\config.toml.bak-dockid`，可人工对照/回退数据。
