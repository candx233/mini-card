# 证据表 — mcss-default-1x

## AC 对照表

| AC | 判定步骤 / 命令 | 实际输出（证据） | 结论 |
|---|---|---|---|
| AC1 默认值改动 | 读 `live.rs:43-62`（新注释段 + ss_factor）；新增单测 `默认超采样1倍且可环境变量覆盖`；`cargo test --release 默认超采样` | 单测：无 MC_SS→1、MC_SS=2→2、MC_SS=9→clamp 4、MC_SS=0→clamp 1；`ok`。`unwrap_or(1)` 落地 | 通过 |
| AC2 注释一致 | 读 `live.rs:43-57` | 旧的自相矛盾注释（一处默认1/一处默认2×）替换为统一说明：默认1× + 换雅黑后为何改回 + MC_SS=2 可开回 | 通过 |
| AC3 编译测试 | `cargo build --release`（native bin + main）、`cargo test --release` | native build `Finished ... 26s` exit0；test 47 passed/0 failed（原46+新增1）；main build `Finished ... 33s` exit0；均零新增警告 | 通过 |
| AC4 运行默认1× | 单测覆盖（无 MC_SS→ss_factor()==1，+2/9/0 覆盖与 clamp）；live 全部 7 处建窗点传 `ss_factor()`；spawn 日志 `--live --blur 30` 无 `--ss` | 单测证明 ss_factor() 无变量时=1；live 路径全部走它且无外部覆盖。注：probe 的 `--ss` 走 `native/minicard-bin/src/main.rs:558/695` 的 Args 独立路径（非 src-tauri），与默认值无关，不用于验证 | 通过 |
| AC5 内嵌一致 | 关闭旧实例→重编 native→重编 main→重启→比对 hash | 源 native `91C01CB5…3801`；重启后 APPDATA 部署 native `91C01CB5…3801`（重启前是旧 `846E4908…`）——主程序重新嵌入了新 native，无 D9 旧版坑 | 通过 |
| AC6 真机生效 | 重启 app（PID 35836 / 绘制进程 10120）；`_pw_grab.py 10120` 抓 9 卡+Dock；stdout 存 `grab_live_output.txt` | `windows=10`，逐窗 hwnd/title/rect 已留档（Weather 240×180、Clock 240×120 等）；10 张 PNG printwindow_ok=1；目视 `MiniCardLive_·_Weather_1095_45.png`（空气质量字清晰）、`..._Clock_840_45.png`（大字无台阶）。部署 native hash 已更新 | 通过 |
| AC7 独立复核 | 只读子代理按 engineering-verify 复核 | 见下方「独立复核」 | 待填 |

## 关键证据原文摘录

1. `live.rs`（改动后）— `ss_factor()`: `std::env::var("MC_SS").ok().and_then(|v| v.parse::<u32>().ok()).unwrap_or(1).clamp(1, 4)`。
2. 单测 `live::tests::默认超采样1倍且可环境变量覆盖 ... ok`（cargo test --release，47 passed）。
3. 主程序重启前后部署 native hash：旧 `846E490824F00BE042B99A8D29AF396689B2D3A3540820756BB7CA80BEA3F92C` → 新 `91C01CB5190AE186120EF187971C05B3A1FFD2B2660C93D6C13FE57C63163801`（= 源 native hash）。
4. 真机截图：`F:\mini-card\.engineering\mcss-default-1x\MiniCardLive_·_Weather_1095_45.png`、`..._Clock_840_45.png`（240×180 / 240×120，实际桌面 1× 像素）。

## 变更清单

| 文件 | 改动 | 对应 AC |
|---|---|---|
| `native/minicard-bin/src/live.rs` | `ss_factor()` 默认 2→1 + 注释统一 + 新增单测 | AC1/AC2/AC4 |

## 轮次小结

- R1：锁定 + 改默认值 + 单测 + native/main 重编 + 真机重启 + 截图；独立复核待执行。

## 残余风险 / 未验证项

- 动效期（时钟翻位 60fps、性能/网络条补间）1× 下未单独抓帧量化；静态大字已目视无台阶，动效台阶需用户实机留意。
- 用户 ClearType / OLED 排列未知；影响观感不改变 1× 生效事实。
- 发布资产（tag/zip/latest.json）未刷新，按非目标不在本轮；桌面已是新版本（本地），但公开 release 仍是旧 2× exe——发版另议。
- 工作树含当日 font-swap 等未提交改动；本轮只动了 live.rs 一处，未提交/未推送（发版另议）。

## 独立复核（agent-3，通过可交付）

端到端 7 环全部亲验：① ss_factor 默认1（源码+单测实跑）；② live 全部 7 处建窗传 ss_factor；③ spawn 仅 `--live --blur` 无 `--ss`（代码+spawn log）；④ native --live 在 probe 分支前 return，不走 args.ss；⑤ 部署 native hash = 源 hash（两处 SHA256 两次核）；⑥ 绘制进程 PID 10120 由该 exe 无 --ss 启动；⑦ 真机截图 1× 尺寸、目视无台阶。**判定：真正达成「桌面默认 1×」**，非无效默认值。

| 编号 | 严重度 | 描述 | 处置 |
|---|---|---|---|
| V1 | S4 | 单测 env var 无 RAII 守卫、并行测试风险 | 已改为 EnvGuard(Drop) 恢复；重跑单测 ok；release hash 不变（cfg(test) 不进 release） |
| O1 | S4 | SPEC AC4 原验证方法（probe dump 尺寸）恒真无效 | AC4 改为单测+7处追踪+spawn无覆盖；注明 probe 走独立 Args.ss |
| O2 | S4 | 抓图 stdout 未留档、内存映像版本间接 | 已保存 `grab_live_output.txt`（windows=10 + 逐窗 hwnd/title/rect） |
| O3 | S4 | main.rs 引用未限定文件 | EVIDENCE/SPEC 已限定为 `native/minicard-bin/src/main.rs:558/695` |

## 交付结论（lead）

**通过**：桌面默认超采样 2×→1× 已落地并真机生效。改动仅 `native/minicard-bin/src/live.rs` 一处（默认值+注释+单测）。发版资产/tag 未刷新（非目标），公开 release 仍是旧 2× exe——发版另议。动效期 1× 台阶未量化，静态大字/小字已目视无台阶。
