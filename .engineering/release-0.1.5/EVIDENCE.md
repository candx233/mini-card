# 证据表 · release-0.1.5

> 实施 + 独立复核后回填（2026-11-21）。所有命令均实际执行，输出摘要如下。

## AC 对照表

| AC | 判定步骤 / 命令 | 实际输出（证据） | 结论 |
|---|---|---|---|
| AC1 版本号三处一致 | `grep 0.1.4\|0.1.5` 三文件 | tauri.conf.json:4 / Cargo.toml:3 / README.md:5 全为 `0.1.5`，无 0.1.4 | 通过 |
| AC2 说明文本字体 | Python 从 zip 读 使用说明.txt 关键字 | 微软雅黑=1、Segoe UI=1、MiSans=0；字体段=「Windows 系统默认字体…无需随包分发字体文件」 | 通过 |
| AC3 无过期措辞 | grep 脚本 + 包内 txt | 「本版新增/本版起」0 命中 | 通过 |
| AC4 脚本去字体断言 | grep 脚本 | FONT_DIR/MiSans/.ttf 0 命中；保留 mtime 校验；产物路径按 CARGO_TARGET_DIR 解析 | 通过 |
| AC5 git 同步 | `git rev-list --left-right --count origin/main...main` | `0  0`；f5bb300（版本提交）已在 origin/main | 通过 |
| AC6 串行重编产物 | `cargo build --release`（先 bin 后 shell，绝对路径）+ Python 扫 exe | 两次 exit 0；`F:\build-targets\release\mini-card.exe` mtime 11/21 22:53:12、sha f8627845…、`0.1.5`×2/`0.1.4`×0；native exe mtime 22:51:36；链成立 | 通过 |
| AC7 发布包完整 | Python 复刻 mc_rel_check 核心 + 包内文本检查 | size 10587438 自洽、sha256 25982d7e… 自洽、testzip=None、包内 exe sha==本地 release exe 字节相同、ed25519 用 update.rs 公钥验过、zip 仅 3 条目无夹带 → 11/11 PASS | 通过 |
| AC8 全量单测 | 5 个 crate 各跑 `cargo test` | core 53 / cards 128 / draw 59 / bin 47 / shell 23，全 `ok`，0 failed（2 ignored 为既有）；共 310 passed | 通过 |
| AC9 交付报告 | 本 EVIDENCE + 最终答复 | 列明未验证项与复验命令 | 通过 |

## 关键过程证据

1. **D1 取证**：`CARGO_TARGET_DIR=F:\build-targets`；仓库内 `src-tauri/target/release/mini-card.exe` 是 11/19 22:17 旧文件（含 `APPDATAmini-card0.1.4`），真实新产物在 build-targets。打包脚本原写死旧路径 → 已改为随环境变量解析。
2. **V2 防呆判别力**：NEW exe → mark_present=True, no_stale=True → 放行；OLD exe → mark_present=False, stale=0.1.4 → 拦截。
3. **包内容**：zip 仅 `mini-card.exe` / `LICENSE` / `使用说明.txt` 三项；10.10 MB（换系统字体后去掉 ~49MiB 字体）。
4. **签名**：latest.json pubkey == update.rs:20 == `BrG25loJ…wdY=`；验签 payload `0.1.5|<sha256>` 通过。

## 第 2 轮：就地安装看效果（D2）

| 项 | 证据 | 结论 |
|---|---|---|
| D2 根因 | `build.rs:25` 原写死仓库内 native 路径，不认 `CARGO_TARGET_DIR`；首次重编嵌入旧 native（sha `c0fc292c`），重启解压旧版 → native 改动不生效 | 已修：build.rs 优先 `$CARGO_TARGET_DIR/release/minicard-bin.exe` + `rerun-if-env-changed` |
| 内嵌校验 | 串行重编后 OUT_DIR 内嵌 native sha=`322435f4`（新）；主 exe sha `f8627845`→`74dc718e` | 通过 |
| 就地替换 | 备份 `%TEMP%\minicard_rollback_20261121_230433`；关旧进程→替换 exe→删旧 0.1.5 目录→原命令行重启（PID 30936 + 绘制 32776） | 通过 |
| 效果铁证 | APPDATA `0.1.5\minicard-bin.exe` sha=`322435f4`==新构建 native；spawn log `0.1.5 --live --blur 30` 无 `--font`；native 日志 9 卡窗口/Dock 17 项/天气刷新 OK | 通过 |
| 连带重打包 | 主 exe 变更后重打 zip+latest.json，10/10 离线校验 PASS，包内 exe sha=`74dc718e`==运行实例 | 通过 |

回滚方法：关掉 mini-card 进程，把 `%TEMP%\minicard_rollback_20261121_230433\mini-card.exe` 复制回 `src-tauri\target\release\`，重启即可回到 0.1.4。

## 复验命令

```bat
:: 校验版本一致
git grep -n "0.1.5" -- src-tauri/tauri.conf.json src-tauri/Cargo.toml README.md
:: 全量单测
for %c in (minicard-core minicard-cards minicard-draw minicard-bin) do cargo test --manifest-path native\%c\Cargo.toml
cargo test --manifest-path src-tauri\Cargo.toml
:: 打包（需 CARGO_TARGET_DIR 指向真实产物目录）+ 离线校验
set CARGO_TARGET_DIR=F:\build-targets
python docs\verify\mc_pack_release.py 0.1.5
:: 上传后（需 GH_TOK + 真实 release_id + tag commit）：
python docs\verify\mc_rel_upload.py <release_id> <tag_commit> 0.1.5
python docs\verify\mc_rel_check.py <release_id> v0.1.5
```
