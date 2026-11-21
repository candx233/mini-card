# 契约 · release-0.1.5（发版准备）

## 用户原话（照录）

> 你检查一下最新的状态 看一下是不是可以发一版

后续用户决策（ask_user_question 的选择）：
- 下一步范围：**全权准备到可发布（推荐）**——改版本号→修打包脚本→git push→串行重编→重新打包并跑校验；GitHub 上传/Publish 那步给出命令，等用户提供 token 或确认由用户执行。
- 新版本号：**0.1.5**。

## 目标

把 mini-card 当前 HEAD（领先 v0.1.4 八个实质提交）准备成一个可发布的 0.1.5 版本：版本号自洽、打包脚本与源码一致、提交已推送、产物重新构建、发布包本地生成并通过全部可离线校验。

## 验收标准（AC，均可判定）

| AC | 判定方式 | 通过标准 |
|---|---|---|
| AC1 | `git grep -n '0.1.4\|0.1.5' -- tauri.conf.json Cargo.toml README.md` 在 src-tauri 与根 | 三处均为 `0.1.5`，无 `0.1.4` 残留（`notify.rs` 的 B 案注释不属版本号，不在判定范围） |
| AC2 | read `mc_pack_release.py` 的 `README_TXT` | 字体声明为系统默认字体（微软雅黑/Segoe UI），不含 MiSans 声明 |
| AC3 | grep `mc_pack_release.py` | 包内说明文本无「本版新增」「本版起」措辞 |
| AC4 | grep `mc_pack_release.py` | 无 `FONT_DIR` / `MiSans-*.ttf` 断言；保留「主 exe 不早于 native exe」的 mtime 校验 |
| AC5 | `git rev-list --left-right --count origin/main...main` | `0  0`（8 提交已推送） |
| AC6 | 串行 `cargo build --release`（先 native/minicard-bin 后 src-tauri） | 两次 exit 0；mtime 链 `mini-card.exe >= minicard-bin.exe`；新 exe 晚于最后功能提交 |
| AC7 | `python mc_pack_release.py 0.1.5` + 离线校验脚本 | scratch 产出 `MiniCard-v0.1.5-win-x64.zip` 与 `latest.json`；version/tag=0.1.5；包内 exe sha256 == 本地 release exe；`testzip()` 为 None；ed25519 签名用 `update.rs:20` 公钥验过 |
| AC8 | 全量单测（core/cards/draw/bin/src-tauri） | 全部 `test result: ok`，0 failed |
| AC9 | 交付报告 | 显式列出未验证项（远端 draft Release / 上传 / Publish / 代码签名）与复验命令 |

## 非目标

- 不执行 GitHub 资产上传、draft Release 的创建/PATCH、Publish、建 git tag（无 GH_TOK，且属用户拍板的发布动作）。
- 不改任何产品功能代码；`native/minicard-cards/src/notify.rs` 注释中的「B 案（0.1.4）」是方案编号，不是版本声明，不改。
- 不做代码签名（既有策略，见打包脚本 SmartScreen 说明）。
- 不做顺手重构或清理构建缓存。

## 约束

- Windows 平台；路径按脚本内绝对路径（ROOT=F:/mini-card）。
- 重编必须串行：先 `native/minicard-bin`，再 `src-tauri`（`build.rs:13` 要求），否则会嵌入旧 native exe（font-swap D9 的教训）。
- 打包脚本必须用带 cryptography 的 Python 3.12.10。
- 私钥不得离开本机、不得打印内容。
- 使用 `C:\Users\22088\.cargo\bin\cargo.exe` 绝对路径（cargo 不在 PATH）。

## 未知与假设（含风险）

| 编号 | 假设 | 错了会怎样 | 风险 |
|---|---|---|---|
| A1 | 用户确认版本号 0.1.5 | 需回滚三处版本号并重新打包 | 已由用户选择消除 |
| A2 | `git push` 已配好 credential helper，可非交互完成 | 停在 push，上报用户，不硬推 | 中 |
| A3 | 远端 draft Release v0.1.5 尚不存在，需用户后续创建 | 上传步骤需用户自行处理 | 已纳入非目标 |
| A4 | `README_TXT` 中 Dock 回退开关等描述仍属实，仅措辞「本版起/本版新增」过期 | 说明文本仍有个别过时句，但不阻塞发布 | 低 |
| A5 | 打包私钥公钥与 `update.rs:20` 一致 | 签名校验 FAIL | 已核对一致（`BrG25loJ…`），风险消除 |

## 决策记录

- 无偏离契约的决定。若执行中出现需偏离的情况，必须在此登记并说明影响面。
