# 缺陷台账 · release-0.1.5

> 收敛判据：无未关闭 S1/S2/S3；连续一轮无新发现。

## 第 1 轮：执行期自查 + 独立检验者复核

| 编号 | 来源 | 严重度 | 复现步骤 | 证据 | 根因 | 修复 | 状态 | 轮次 |
|---|---|---|---|---|---|---|---|---|
| D1 | 实施者自查 | S2 | 重编后仓库内 `target/release/mini-card.exe` mtime 仍 11/19；打包脚本写死该路径 | 环境 `CARGO_TARGET_DIR=F:\build-targets`；真实产物 `F:\build-targets\release\` mtime 11/21、含 `0.1.5`×2 无 `0.1.4` | 脚本产物路径与 `CARGO_TARGET_DIR` 重定向脱节，照原样打包会把 11/19 旧 exe 打进 0.1.5 包 | 脚本按 `CARGO_TARGET_DIR` 解析 EXE/NATIVE_EXE（无则回退仓库内 target） | 已修 | 1 |
| V1 | 独立检验者 | S3 | `git status` 显示 ` M src-tauri/Cargo.lock`；`git show HEAD:src-tauri/Cargo.lock` 仍是 0.1.4 | `git diff` 显示 lock 内 `mini-card version 0.1.4→0.1.5`；f5bb300 已 push | 首次提交只 add 了 Cargo.toml/tauri.conf/README，漏了被 cargo 自动更新的 Cargo.lock | 补提交 Cargo.lock（0.1.5）；不影响已打包产物，不重打 | 已修 | 1 |
| V2 | 独立检验者 | S3 | 裸版本号防呆 `b"0.1.5" in exe` 对旧 exe 返回 True | 旧 exe 含裸 `0.1.5`×1（来自依赖路径 `http-range-0.1.5`）+ `APPDATAmini-card0.1.4`；新 exe 含 `APPDATAmini-card0.1.5` | 裸串误判：依赖路径里可能有同形版本号 | 防呆改用精确标记 `APPDATAmini-card<ver>` + `re` 扫 `mini-card(\d+\.\d+\.\d+)` 断言无其它版本残留 | 已修 | 1 |
| V3 | 独立检验者 | S4 | DEFECTS.md 中 D1 状态仍为「待修」；台账未入库 | — | 台账未随修复同步、SPEC 单独提交时 DEFECTS 尚未创建 | 更新 D1 状态为已修；DEFECTS.md/EVIDENCE.md 一并补提交 | 已修 | 1 |

## 返工验证（第 1 轮修复后）

- V2 防呆判别力实测：新 exe（标记在、无残留）→ 放行；旧 exe（标记无、有 0.1.4 残留）→ 拦截。
- V1 修复后：`git diff` 无 Cargo.lock 未提交改动，HEAD 源码快照三处版本 + lock 一致。
- 重打包后 11/11 离线校验全 PASS（见 EVIDENCE.md）。
- 全量单测回归：310 passed / 0 failed（core53+cards128+draw59+bin47+shell23）。

## 残余风险 / 未验证项

- 远端 draft Release / 资产上传 / PATCH / Publish / 建 tag / 代码签名：契约非目标，未做。
- AC8 单测由实施者（lead）真实执行并留证；独立检验者受只读约束未复跑，属证据来源差异，非缺陷。

## 轮次小结

- 第 1 轮：新增 4（D1 自查 + V1/V2/V3 独立检验）/ 已修 4 / 剩余 0。
- 返工验证无新发现 → 收敛达成。
