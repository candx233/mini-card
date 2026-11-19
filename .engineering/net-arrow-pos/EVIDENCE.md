# 证据表 — net-arrow-pos

实施 + 复核后回填（2026-11-19）。产物在 `.engineering/net-arrow-pos/`。

## AC 对照表

| AC | 判定步骤 / 命令 | 实际输出（证据） | 结论 |
|---|---|---|---|
| AC1 native 箭头位置 | 重建 release bin 后 `--probe net --w 120 --h 120 --no-glass --ss 4 --dump net_after_ss4.bgra` + 4x 合成 + 墨迹 bbox | 箭头墨迹 x=[9.50,17.25]（右缘 17.25∈[15,19] ✓）、两行箭头 x 差=0.0；下排 gap=5.25px（"8.4" 左缘 22.5 − 17.25）∈[3,7] ✓；上排 gap=7.25（"1.2" 左缘 24.75）仍在合理带边缘。见 net_after_ss4.png / before-after.png | 通过 |
| AC2 web 箭头位置 | CDP 加载 fixture（mock 快照）→ computed + 4x 截图墨迹 bbox | `.net-lab` computed `left=0px` ✓、labX=8/labW=12/numX=21（布局未动）✓；web 箭头墨迹 x=[9.50,17.25]，与 native 像素一致；下排 gap=5.25px。见 web_net_probe.json / web_net_composited.png | 通过 |
| AC3 无回归 | `cargo test -p minicard-cards`；native 数值/单位/分隔线几何 | 127 passed / 0 failed（test-cards.log）。改动只在 `arrow_shift`：绘制时 dx，从 7→0，不占布局、不改数值/单位/分隔线 token。出图数值 1.2/8.4、单位 MB/s、分隔线 y=60 与 before 完全一致 | 通过 |
| AC4 一致性 | 对比 web + native 两处改动与出图 | web `left:-0.4375rem→0` 与 native `arrow_shift:0.4375*r→0.0` 同步归零；两边墨迹 bbox 均 [9.50,17.25]，像素一致 | 通过 |

## 关键证据摘录
1. before（换字体后未修）：箭头 x=[2.50,10.25]，贴左边框（距 ~2px）、gap ~12px（net_before_ss4_native.png）。
2. after（修复）：箭头 x=[9.50,17.25]，gap 5.25px，与换字体前 MiSans 设计意图（墨迹右缘≈17、间隙≈4.7px）吻合（net_after_ss4.png）。
3. 根因量化：MiSans「↑」字身左侧留白 ~5.96px（需 -7px 左移对齐），Segoe UI/YaHei「↑」左侧留白 ~1.5px，同一个 -7px 把墨迹推到 x=2.5。
4. 交付链：native bin 重建（34.82s，0 警告）→ main exe 重建（1m53s，内嵌新 bin）→ APPDATA native hash=源 hash=`2A75A865…B364` → 两进程 17:24 全新启动。

## 残余风险与未验证项
- 上排 "1.2" 的 gap=7.25px（因数字 "1" 左侧字形留白比 "8" 大），略高于目标带 3–7 的上沿 0.25px；视觉上无异常，且与修复前设计一致（数字左缘随字形自然变化，非箭头位置问题）。不单独处理。
- 未按全界面五页做字体回归（本任务范围仅箭头位置，font-swap 任务 AC5 已覆盖）。

## 独立复核（轮次 2）
独立子代理只读复核，逐条重跑/重测（非转述实施者数字）：
- 源码↔产物一致：重跑 probe dump 的 SHA256 与 EVIDENCE 产物完全相同 → 产物确由当前源码生成。
- 像素重测：before [2.5,10.25]、after [9.5,17.25]、MiSans 设计稿 [7,16.75]；↑↓ 两行 bbox 逐列一致，下箭头字形同样适合 left=0。
- `arrow_shift` 尺度判断（关键点）：**不是缺陷**。0 在任何 r 下都是 0（尺度不变）；旧值随 r 缩放是为按比例抵消 MiSans 字形留白，新字体意图「完全不偏移」，恒定 0 是正确表达。浏览器改根字号 8/16/32/40px 实测 left 恒 0px、布局线性缩放，web 侧同样尺度一致。
- 结果：AC1–AC4 全通过；1 条 S4 陈旧注释（V1）已修并重建、最终 probe 复测箭头仍 [9.5,17.25]、APPDATA hash 与源一致。收敛，可交付。

## 复跑命令
```bat
native\minicard-bin\target\release\minicard-bin.exe --probe net --w 120 --h 120 --no-glass --ss 4 --dump .engineering\net-arrow-pos\net_after_ss4.bgra --exit-after 2500
python docs\verify\font-swap-ms-default\bgra2png.py .engineering\net-arrow-pos\net_after_ss4.bgra .engineering\net-arrow-pos\net_after_ss4.png 120 120 4
python .engineering\net-arrow-pos\make_web_fixture.py && python .engineering\net-arrow-pos\verify_web.py
cd native\minicard-cards && cargo test
```
