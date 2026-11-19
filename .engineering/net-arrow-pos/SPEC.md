# 契约 — net-arrow-pos（网络卡两个箭头位置修复）

## 原话照录
> 修一下网络卡两个箭头的位置

## 目标
把网络卡上行 ↑ / 下行 ↓ 两个箭头的水平位置恢复到换字体前的设计位置（贴近数值、与数值墨迹保持约 4~5px 间隙，不再贴住卡片左边框）。

## 缺陷性质（缺陷类任务）
- **复现**：`--probe net` 出图，箭头墨迹位于 x≈[2.5,10.25]（css px，120×120 卡），左边距边框仅 ~2px，与数值墨迹间隙 ~12~14px。
- **期望**：箭头墨迹右缘 ≈17，与数值（"8.4" 左缘 x=22）间隙 ≈4~5px；视觉上与换字体前（MiSans 时代 x≈[7,16.75]）一致。
- **实际**：箭头整体左移约 5px，贴边、与数值间隙过大。
- **首次出现**：2026-11-19 未提交的字体全栈替换（MiSans → Microsoft YaHei/Segoe UI，见 `.engineering/font-swap-ms-default/`）。
- **根因**：箭头相对位移 `left:-0.4375rem`(web) / `arrow_shift:0.4375*r`(native，r=16 → 7px) 是按 MiSans「↑」字身左侧留白（~5.96px）标定的；新字体（Segoe UI/YaHei）「↑」左侧留白仅 ~1.5px，同一个 −7px 位移把墨迹推到 x=2.5，贴住左边框。

## 验收标准（AC，可判定）
- **AC1 native 箭头位置**：重建 release bin 后 `--probe net --w 120 --h 120 --no-glass --ss 4` 出图，上行 ↑ 与下行 ↓ 墨迹 bbox：右缘 ∈ [15,19]、左缘 ∈ [7,11]、与数值墨迹（"8.4" 左缘 x≈22）间隙 ∈ [3,7]px（css）。两行箭头 x 一致（差 ≤0.5px）。
- **AC2 web 箭头位置**：`web/card.html` 中 `.net-lab` 的 `left` 位移取消（=0），箭头列仍在固定 12px 列内、数值列布局不动；用 DevTools/CDP 或等价手段确认 `.net-lab` computed `left` = 0px 且数值 x 未变。
- **AC3 无回归**：`cargo test -p minicard-cards` 全绿；native 数值/单位/分隔线几何不变（箭头只改绘制时的 dx，不占布局）。
- **AC4 一致性**：native 与 web 两处箭头位移都改，行为一致（都从 −7px 归零）。

## 非目标
- 不改箭头字号/字重/颜色、不改数值与单位布局、不改分隔线、不改字体栈本身。
- 不改纵向位置（实测纵向墨迹中心与数值中心差 <1px，无需调整）。
- 不动 font-swap 任务的其它结论。

## 约束
- Windows + Tauri + Rust(native D2D) + 内联 WebView HTML。
- 判定以 release 真窗口/probe 出图像素为准，不接受「看代码觉得对」。
- 最终验收由独立子代理只读复核。

## 未知与假设
- **假设 A1**：用户说的「两个箭头的位置」指水平位置（箭头贴左边框、离数值太远），这是换字体后唯一客观可见的回归。**风险**：若用户指纵向或其它语义，需返工；已用 before/after 对照图请用户校准。
- **假设 A2**：目标位置 = 换字体前的视觉位置（墨迹右缘≈17、间隙≈4.7px）。**风险**：审美偏好可能有 ±2px 偏差，属可接受范围。
- 数值区起始 x=21（固定列），不随位数变化 → 移动箭头不会与宽数值重叠。

## 决策记录
- D1：采取「取消固定 −7px 左移」而非「换算新字体的等效位移」——新字体「↑」左侧留白仅 ~1.5px，归零后墨迹 [9.5,17.25]，右缘 17.25 与设计的 17 吻合、间隙 4.75px 与设计 4.75px 吻合，且实现最简单、跨字体栈更稳健。
- D2：web 与 native 同步改，避免预览与真卡不一致。

## 复现/验证命令
```
native\minicard-bin\target\release\minicard-bin.exe --probe net --w 120 --h 120 --no-glass --ss 4 --dump .engineering\net-arrow-pos\net_after_ss4.bgra --exit-after 2500
python docs\verify\font-swap-ms-default\bgra2png.py <dump> <out.png> 120 120 4
cd native\minicard-cards && cargo test
```
