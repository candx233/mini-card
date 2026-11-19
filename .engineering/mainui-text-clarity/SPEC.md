# 契约：主界面文字清晰度诊断与优化方案

任务 slug：`mainui-text-clarity` · 档位：标准（诊断+方案，不改产品代码） · 建立：2026-11-18

## 用户原话（照录）

> 现在主界面的字相比起来都有点糊 你可以看下原因 然后制定一下优化方案哦

> （第二轮补充）就是跟QQ这些软件比起来  你顺便确定一下所有字体都统一了 我感觉可能主界面不自绘的话 得用微软默认的字体 因为确实少了hinting指令

## 目标

1. 诊断主界面（WebView2 渲染的 `web/index.html`，区别于原生自绘卡片）文字「糊」的原因，并制定一份可判定、按性价比排序的优化方案。**参照物已明确 = QQ 这类软件。**
2. 核查「所有字体是否统一」——覆盖主窗 / 卡片 / Dock / 托盘菜单 / motion-lab / native 自绘六面，给确定答案与逐面证据。
3. 评估用户假设「主界面不自绘就该换微软默认字体，因为 MiSans 少了 hinting 指令」——是否成立、收益与代价。

## 本次交付边界（关键）

- **用户要的是「看原因 + 制定优化方案 + 核查字体统一」，不是立刻改代码。** 本轮只交付诊断结论 + 方案文档，改动范围仅限 `.engineering/` 台账与 `docs/` 方案文档；`web/index.html`、`src-tauri/`、字体文件一律不动。
- 「相比起来」参照物**已由用户确认 = QQ 这类软件**（第二轮补充）。QQ NT 是 Electron/Chromium（已核实），与本主窗同为 Chromium 渲染栈——这个事实改写方案主叙事：差距更可能来自字体本身（hinting）与层合成（透明窗 AA），而非渲染器差异。

## 验收标准（AC）

- **AC1 栈归属**：明确区分「主界面 = WebView2/Chromium DOM 渲染」与「桌面卡片 = native/D2D 自绘」，并给出代码证据；证明历史 `font-and-aa-eval.md` 的超采样/补墨结论作用于后者、不能照搬到前者。
- **AC2 根因可证**：给出主界面文字「糊」的根因，每条都有代码/配置证据或权威来源链接，不接受「看起来」。至少覆盖：透明窗导致的 AA 模式、字号/墨量、字体 hinting、CSS 文字平滑属性。
- **AC3 反向排除**：逐条说明哪些常见猜测**不是**主因（DPR/缩放、CSS zoom、ClearType 全局设置、换字体），并给出当时现场量过的事实（DPR=1、zoom=1）。
- **AC4 方案可判定**：优化方案按性价比排序，每条含做法 / 预期收益 / 成本 / 风险 / 可复跑判据（能证明真或假），且明确「推荐先做哪条、哪条要用户拍板」。
- **AC5 落盘与可续作**：方案写入 `docs/`，可被新会话独立读懂；`.engineering/` 三件套齐全，含 AC 对照表与残余风险。
- **AC6 独立复核**：由只读子代理按 `engineering-verify` 复核诊断与方案，输出结构化缺陷清单；实现者不自发合格证。
- **AC7 字体统一核查**：逐面（index/card/dock/menu/motion-lab/native）列出 `@font-face` 档位、`--font-body`/`--font-mono` 栈、字体文件物理版本；给出「是否统一」的确定结论与不一致清单。
- **AC8 换字体假设评估**：核实 MiSans 与雅黑/Segoe UI 的 hinting 表差异（直接读表），评估「主界面换微软默认字体」在 Chromium 栈下的真实收益、代价（字重/跨机一致/缺字/重标定）与替代路径，给可判定建议。

## 非目标

- 不修改任何产品代码、字体、构建配置。
- 不重新跑像素级 A/B 实验（本轮以已有台架产物 + 代码/权威来源为证据）；如做新取证，仅写进方案作为「实施时的验收步骤」。
- 不迁移主界面到 native 自绘（那是 `mainui-native-eval.md` 的独立立项，80–118 人日）。
- 不把「换系统字体」当作本次方案主路（`font-and-aa-eval.md` 已论证不换）。

## 约束

- 平台：Windows，WebView2 (Chromium)，进程 Per-Monitor-V2 DPI aware。
- 主窗配置：`decorations:false` + `transparent:true` + 真透明 + CSS 18px 圆角（证据 `tauri.conf.json`、CSS）。
- 现场基线口径：1920×1080 @ 100%，LogPixels 未设 = 96，DPR=1，zoom=1（`font-and-aa-eval.md` §机器口径、`mainui-fix-plan-20261026.md` §3）。
- 字体：内嵌 MiSans woff2 三档（400/500/600），GB2312 子集；**本次 fontTools 直读四档 woff2 证实**仅 `prep`/`gasp` 表、0 个含指令字形（自动 hinter）；`build_misans_subset.py:9` 只做字形子集、未注入 hinting。
- 历史决策约束：卡片侧已钉死 `GRAYSCALE`；`text-rendering` 三档实测在 1x 像素相同；主窗字下限 2026-10-26 已提到 13px。

## 未知与假设

| 假设 | 错了会怎样 | 处置 |
|---|---|---|
| ~~「相比起来」参照物未定~~ **已确认 = QQ 这类软件**（第二轮用户明确） | — | 方案主叙事改为「比 QQ/Chromium 同类软件」，历史版本分支降为次要；QQ 是 Electron/Chromium 已核实 |
| ~~主窗 `transparent:true`→灰阶 AA 是主因~~ **已由 A/B 闸门在本机否定**（第四轮）：透明:false 真重编后 7 文字带像素逐字节不变 | — | 方案 1/1a/1b 撤销、方案 2 升主路；结论限定「本机当前配置」，见 `docs/verify/mainui_aa_ab/RESULT.md` |
| 用户机器未启用 ClearType / 用了 OLED 非 RGB 排列 | 会叠加彩色边缘感，但改变不了「糊」的方向 | 记为环境风险，方案给自查步骤 |
| 当前 DPR 仍为 1 | 若用户已改 125%/150% 缩放，需按 `WM_DPICHANGED` 欠账另行排查 | 方案含 DPI 自查步骤 |

## 决策记录

- 第二轮范围扩展（用户明确要求）：新增「字体统一核查」与「换微软默认字体假设评估」两个目标（AC7/AC8），参照物由假设改为 QQ。仍是诊断+方案，不改产品代码。
- 关键事实（本轮直接取证）：MiSans 四档 woff2 无 `fpgm`/`cvt`（fpgm 缺失、prep=8 字节、0 含指令字形）；雅黑/Segoe UI/Consolas 有完整 hinting 程序（fpgm 2.5–3.4 KB、cvt 507–1297 项）。用户「MiSans 少 hinting」的判断成立。
- 关键事实：QQ NT = Electron/Chromium（腾讯官方技术文章），与主窗同为 Chromium/Skia 栈；但 QQ 窗口不透明、用系统字体（雅黑），主窗透明 + 内嵌无 hinting 的 MiSans。
- 关键事实：native 三份 TTF 与 web 三份 woff2 同版本（rev 4.0089874、upem 1000、内部字重 330/380/520），仅字符集不同（7770 vs 8157 字形）——同一字体家族的不同子集，不是两套字体。
- 第四轮（用户批准执行 A/B 闸门）：真 `transparent:false` 重编 release，7 个采样文字带逐字节不变、无子像素彩边 → 本机否定「透明窗为糊的主因」，方案 1/1a/1b 撤销、方案 2 升主路。结论已按复核收窄为「本机当前配置下的有限结论」，不普适化。详见 `docs/verify/mainui_aa_ab/RESULT.md`。
- 待定：是否做字体 A/B 注入、是否换字体，需用户在「跨机一致/三真字重」与「hinting 更实」之间拍板；本轮只给建议。

## 第 5 轮执行契约（用户批准做方案 2 + 间距专项）

原话：「你做吧 主界面的间距两个字很怪 可能是太小了 你调整一下」

- **目标**：全量把主界面低于实心拐点的字号提到 ≥12.5px；专项修好位置面板「间距」两字的拥挤+发糊。
- **改动文件**：仅 `web/index.html`（六页共用）；改完重编 release 才生效。
- **真机现状证据**（`docs/verify/mainui-text-clarity/before_geometry.json`）：`.axis-k` 字号 12px、文字宽：X/Y=7.5px、间距=25px，列宽 26px，字距 0.48px，色 #4F5865。
- **AC-E1**：`.axis-k` 计算字号 ≥13px；「间距」文字宽 + 左右各 ≥4px 呼吸 ≤ 列宽（当前 25px/26px 不满足）。
- **AC-E2**：全量小字无 <12.5px 残留（`.rver`/`.deck-empty span`/`.kbd-row .plus`/`.kbd`/`.axis-k` 全部提档）。
- **AC-E3**：实心像素不下降；六页截图无溢出/换行/截断；位置面板 X/Y/间距 三行仍对齐、滑条行程够用。
- **AC-E4**：重编 release，exe mtime 晚于 index.html；真机前后对比 + 重启实机验收。
- **非目标**：不改字体家族、不动 native 卡片、不动透明窗（闸门已证伪方案1）、不换字体。
- **决策**：「间距」列 26→34px 会让数值框/滑条右移 8px、滑条略短——窗口 992px 有空间，属可接受布局代价，真机验证后确认。

## 第 6 轮执行契约（用户选暴力档 + 微软雅黑）

原话：「主要是修了感受不到区别啊」→「我感觉得暴力 因为不然字看着发虚 不如直接用微软雅黑」

- **背景**：第 5 轮微调用户感受不到；字体 A/B（`font_ab_compare.png`）反证纯换雅黑收益极小（+1.6~7.7%）；暴力 A/B（`final_compare.png`）三档实测：开关说明区深色像素 A现役1088 / B暴力MiSans2455 / **C暴力雅黑3209**。用户选 C。
- **目标**：把主界面（仅 `web/index.html`）字体栈换微软雅黑，并落地经实测的「暴力清晰度」配置：字号+1px、字重 500、灰字加深。
- **改动文件**：仅 `web/index.html`。改完重编 release。
- **落地配置（= A/B C 档，逐项）**：
  - `--font-body:'Microsoft YaHei','Segoe UI',system-ui,sans-serif`；`--font-mono:'Microsoft YaHei','Consolas',ui-monospace,monospace`
  - `--muted:#39414D`、`--muted-2:#4F5865`
  - body 15px；h1 18px；sub/btn/input/opt 14.5px；sitem meta/tpl p/chips/snote/prow hint/dinfo meta/cr-sub/axis-k/axis-num/cap2/kbd/rver 13.5px
  - body/button 及小字类 font-weight 500
- **AC-F1**：五档字号/字重/颜色/字体栈与 A/B C 档一致（真机 computed 核对）。
- **AC-F2**：五页无溢出/换行/截断（`docScrollW=innerW`、`bad=[]`），位置面板仍对齐。
- **AC-F3**：开关说明区深色像素较现役显著上升（目标 ≥2500，A/B C 实测 3209）。
- **AC-F4**：重编 release，exe mtime > index.html，真机截图 + 重启验收。
- **非目标**：不动 native 卡片字体（仍 MiSans）、不改卡片库/Dock 预览里的 `card.html`/`dock.html`（它们是独立文档，仍 MiSans——列为残余风险）。
- **已向用户声明的代价**：雅黑跨机像素不一致（随 Windows 版本）、无 500 真面（500 落 Regular/合成），但用户已知悉并明确选择。
- **回滚**：git 可秒级回滚 `web/index.html`；暴力配置集中在一处便于整体撤销。

## 第 7 轮执行契约（用户看完对照后：换回 MiSans）

原话：「换成misans吧」（在看完 `violent_misans_vs_yahei.png` 暴力档 MiSans vs 雅黑对照后）

- **目标**：仅把第 6 轮暴力覆盖块的两行字体变量换回 MiSans；字号/字重/灰字加深全部保留。
- **改动**：`web/index.html` 覆盖块 `--font-body` 回 `'MiSans','Segoe UI',system-ui,'Microsoft YaHei',sans-serif`；`--font-mono` 回 `'Cascadia Mono','Consolas','MiSans',ui-monospace,monospace`；同步改注释（去掉雅黑代价声明）。
- **AC-G1**：computed 正文族 = MiSans、等宽族 = Cascadia/Consolas/MiSans；字号/字重/颜色与第 6 轮一致。
- **AC-G2**：落地版开关说明区深色像素 = 1445，与第 6 轮 A/B 的 B 档（暴力 MiSans）逐像素一致；五页零溢出。
- **AC-G3**：重编 release，exe mtime > index.html，实例运行。
- **理由记录**：MiSans 跨机像素一致 + 与卡片/Dock 预览 iframe 同源；暴力档清晰度来自字号/字重/灰字加深，不依赖换字体（第 6 轮已证雅黑 hinting 收益被 Skia 吃掉）。
