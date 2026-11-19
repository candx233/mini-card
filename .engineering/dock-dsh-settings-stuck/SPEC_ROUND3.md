# 契约（第 3 轮）：配置加载层去重 Dock 条目 id（B 方案，根治 V1）

任务 slug：`dock-dsh-settings-stuck` · 档位：标准（改共享 core crate + 双进程 + 重编两个 exe） · 建立：2026-11-19

## 用户原话（照录）

> 这个残余风险有什么办法修吗

选择结果：用户在 A/B/C/先不动 四选项中明确选 **B 配置层去重（根治）**。

## 目标

在配置加载层保证 `dock.items[].id` 非空且唯一，使 V1（重复 id 条目面板互斥失效、后端按 id 定位歧义）从数据面根治；不依赖 UI 不变量。

## 为什么要确定性（关键约束）

- **主进程**（`mini-card.exe`）和**原生绘制进程**（`minicard-bin.exe`）都会调 `Config::load()`（`main.rs` 多条命令 + `native/minicard-bin/src/{live,drop,probe}.rs`）。
- 若用随机 id：两进程各自 load 同一份脏配置 → 各生成不同 id → 各自 `save()` → 互相覆盖，内存与盘上 id 不一致 → 面板互斥/后端定位仍会错乱。
- 因此规范化算法必须**确定性**：同一份输入配置 → 同一批新 id。哈希源只能用条目自身稳定字段（`path`/`name`）+ 稳定序号，绝不用时间/随机数/进程态。

## 验收标准（AC）

- **AC-R1 非空唯一**：规范化后所有 `dock.items[].id` 非空且两两不同；空 id、重复 id 都被替换。
- **AC-R2 确定性**：对同一份脏配置调用两次规范化，得到完全相同的 id 集合（双进程一致性前提）；用单元断言证明。
- **AC-R3 幂等**：对已干净配置再跑一次规范化，`changed=false`，id 一个都不动。
- **AC-R4 保序保字段**：规范化只改 `id`，条目顺序、`path`/`name`/`args`/`icon`/`divider_before` 全部不变。
- **AC-R5 一次性迁移**：新增版本位（仿 `dock_native_version` 模式，字段级 `serde(default=legacy)` 返回 0），`Config::load()` 里 `< 版本` 时规范化并 `save()`；老配置缺字段按 0 触发一次，迁移后不再跑。
- **AC-R6 单测覆盖**：core crate 单测覆盖 空 id / 重复 id / 确定性 / 幂等 / 保序 / 版本位 legacy 默认值。
- **AC-R7 双 crate 重编**：`minicard-bin` release 与 `src-tauri` release 均重编成功；`src-tauri` 的 build.rs 内嵌新 `minicard-bin.exe`，主进程首次启动把它解到 `%APPDATA%\mini-card\native\0.1.4\`（字节比较覆盖旧文件）。
- **AC-R8 现场数据收敛**：重启后 live `config.toml` 里 DSH 那条 `id=""` 已被替换成非空唯一 id；其余条目 id 不变；前端面板对 DSH 仍可正常开合（第 1 轮 null 哨兵修复保留，双保险）。
- **AC-R9 独立复核**：只读子代理按 `engineering-verify` 复核，输出结构化缺陷清单。

## 非目标

- 不改前端键控语义（`DOCK_OPT_OPEN` 仍按 id，第 1 轮 null 哨兵保留不改）。
- 不改后端各 `dock_*` 命令的查找逻辑（它们继续按 id 查，配置层已保证唯一）。
- 不重排条目、不改任何条目字段除 `id`。
- 不顺手升级 core crate 其它迁移。

## 约束

- 平台 Windows；`minicard-core` 0.1.0、`mini-card`/`minicard-bin` 0.1.4。
- 版本位沿用现有命名与 legacy 模式：`dock_item_id_versions: u32`，`#[serde(default = "dock_item_ids_version_legacy")]`，legacy 函数返回 0，`const DOCK_IDS_VERSION: u32 = 1`。
- 哈希只用 `path`（必要时 `name`）字节 + 稳定序号；实现放在 `minicard-core/src/config.rs`，不引新依赖（可用已有的 fnv1a 或手写简单 FNV-1a 64 位）。
- 重编前须关闭运行实例（exe 被占用会 `os error 5`），见第 2 轮教训。
- 回滚：`git checkout -- native/minicard-core/src/config.rs` + 重编两 crate；配置迁移不可逆（id 已落盘），但旧 id 本就非法/重复，回滚代码不影响已迁移数据。

## 未知与假设

| 假设 | 错了会怎样 | 处置 |
|---|---|---|
| 两进程读同一份配置时序号一致 | 若两进程读到的配置版本不同，会生成不同 id | load 是「读盘 → 规范化 → save」，两进程读的是同一磁盘文件；确定性由哈希源保证，序号用列表下标（稳定）。迁移一次性，之后 id 已落盘不再生成 |
| 哈希碰撞概率可忽略 | 极小概率两不同条目得到同 id | 生成后做一次唯一性校验，碰撞时加序号重试（salt 循环）直到唯一 |
| 图标缓存按 path 哈希（`ensure_icon`）不依赖 id | 改 id 会让自定义图标失效 | `ensure_icon` 用 `fnv1a_hex(normalize_path)` 做缓存名；`custom_icon` 用 `sanitize_id(id)` 做前缀但**结果写回 `item.icon` 字段**，规范化不改 `icon` 字段，故自定义图标路径保留 |
| UI 添加路径 `dock_add` 仍生成唯一 id | 与迁移冲突 | 迁移是一次性的、且校验唯一；之后 `dock_add` 的 short_id 循环避重逻辑不变 |

## 决策记录

- 选 B 而非 A/C：用户明确要根治。A（前端换键）只治面板症状、后端按 id 定位的歧义仍在；C（改当前 config）不防复发；B 从数据面保证不变量，一次修全链路。
- 确定性优先于随机：因双进程都在 load + save，随机 id 会造成竞态；确定 id 是唯一安全选择。
- 版本位一次性迁移：避免每次 load 都全表扫描哈希，且与项目既有迁移风格一致。
