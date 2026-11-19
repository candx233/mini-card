# 证据表 — mainui-text-clarity

## AC 对照表（v2，复核后）

| AC | 判定步骤 / 命令 | 实际输出（证据） | 结论 |
|---|---|---|---|
| AC1 栈归属 | 读 `src-tauri/src/main.rs:3431`、`tauri.conf.json:7`、`web/index.html:9-11`；对比 `docs/research-013/font-and-aa-eval.md` §6 | 主窗 = `WebviewWindowBuilder::from_config` → `web/index.html`（DOM+woff2）；卡片 = `Gfx::new_scaled`/`MC_SS`/`text.rs` DWrite 自绘。两栈无共享渲染路径；`mainui-fix-plan-20261026.md` §3 标题即「主界面文字清晰度」针对 `web/index.html` | 通过 |
| AC2 根因可证 | 逐条查代码/配置 + 权威来源 + 直接读字体表 + A/B 闸门实测 | A（透明窗）：已由 A/B 闸门检验——真 `transparent:false` 重编后 7 文字带像素逐字节不变 → **本机当前配置下透明窗不是主因**（结论已收窄，见 `docs/verify/mainui_aa_ab/RESULT.md`），方案 1 撤销。B：`mainui-fix-plan:82` 19 vs 79 实心像素。C：fontTools 直读四档 woff2 = 仅 prep/gasp、**0 含指令字形**，已证实；29.8/17.8 出处为 L1 span（微信语境） | 通过（A 已从「假设」更新为「实测否定其为主因」） |
| AC3 反向排除 | 查 `mainui-fix-plan:3,80`、`font-and-aa-eval:11,54-60`、`mainui-native-eval:12,16` | DPR=1/zoom=1；换字体 4–8 人日/三真字面；迁移 native 80–118 人日；「换 ClearType」理由改正为层合成条件（V6 已修） | 通过 |
| AC4 方案可判定 | 读 `docs/mainui-text-clarity-plan.md` §0 + A/B RESULT | 闸门执行后重排：方案 1/1a/1b 撤销（无确定收益）、方案 2（小字提档/补墨）升主路、换字体待 A/B 拍板、方案 3 存档。每条含做法/收益/成本/风险/判据。plan §3/§4/§5 的旧方案已加 v3「勿按原文执行」横幅，以 §0 与 RESULT 为准 | 通过（依闸门结果更新） |
| AC5 落盘可续作 | 文件检查 | 三件套 + 方案文档齐全；AC 表、残余风险、命令索引、两条参照物分支均在，新会话可读 | 通过 |
| AC6 独立复核 | 第 1 轮 verifier 复核 + 第 2 轮收敛复核 | 第 1 轮 9 条缺陷全修；第 2 轮确认 V1–V9 收敛、无新增功能缺陷，N1/N2 两条 S4 文档瑕疵亦已修 | 通过 |
| AC7 字体统一核查 | 逐面读 `web/{index,card,dock,menu,motion-lab}.html` + native `text.rs`；fontTools 读 TTF/woff2 元数据 | 正文族六面 = MiSans；native TTF 与 web woff2 同 rev 4.0089874/upem1000/字重330-380-520（同源子集）。不一致 3 处：mono 栈三种写法、@font-face 3档vs4档、Semibold 文件名大小写。详见 `docs/font-unification-and-swap-eval.md` §2 | 通过 |
| AC8 换字体假设评估 | fontTools 直读 hinting 表 + 权威来源（Edge 团队 / Chromium font_render_params_win.cc / FreeType） | MiSans 无 fpgm/cvt（prep 8B、0 指令字形）；雅黑 fpgm 2566B/cvt507、Segoe 2652B/1297。但 QQ 同为 Chromium、Skia 最终栅格化忽略水平 hinting；换字体不改透明窗 AA 模式、三真字重塌两档、跨机不一致、4–8 人日重标。详见 eval §1/§3 | 通过（评估阶段结论；第6轮用户拍板后仍在 WebView 主界面落地雅黑暴力档，见下） |
| AC-F1~F4 暴力档落地 | 真机 computed + 五页溢出 + 深色像素 + 重编验证 | web/index.html 末尾集中覆盖块：雅黑栈/15-18-14.5-13.5px/weight500/#39414D。final_style.json 逐项吻合、final_pages.json 五页 bad=[]、final_ink.json 深色像素 1088→3209(+195%)、exe mtime 23:04:41>index 23:03:53、release 运行 PID39952 | 通过（verifier5） |

## 关键证据原文摘录

1. `tauri.conf.json:21-23` — `"decorations": false, "transparent": true, "shadow": false`。
2. `web/index.html:51` — `html, body { ... background: transparent; ... }`；`:61` `.app { ... border-radius:18px; overflow:hidden; }`。
3. `web/index.html:54` — `-webkit-font-smoothing: antialiased; text-rendering: optimizeLegibility;`；`:9-11` 三档 MiSans @font-face。
4. `main.rs:3399` — `DWMWCP_DONOTROUND：圆角交给 CSS（主窗真透明 + --radius-win 18px）`。
5. `mainui-fix-plan-20261026.md:80-83` — 11.5px 19 实心像素 / gray=105；12.5px 79；13px 78。
6. fontTools 直读：`MiSans-{Regular,Medium,Semibold,Bold}.woff2` → hint 表 `['prep','gasp']`、含指令字形 0（四档一致）。
7. `grep -rn '29.8\|17.8' docs/*.md` → 仅命中方案文档；真实来源 L1 span `span_3fe9478842c46f1371f7`（主题「候选条 vs 微信」）。
8. Electron FAQ（PR #18101 diff）原文：「Sub-pixel anti-aliasing needs a non-transparent background of the layer… Notice that just setting the background in the CSS does not have the desired effect.」merge commit `e63f527e`（已去掉不可核实的 683bef5b）。
9. 权威链接：[web.dev Antialiasing 101](https://web.dev/articles/antialiasing-101)、[Electron PR #18101](https://github.com/electron/electron/pull/18101)、[Electron #6344](https://github.com/electron/electron/issues/6344)、[WebView2 #4945](https://github.com/MicrosoftEdge/WebView2Feedback/issues/4945)。

## 轮次小结

- 第 1 轮：锁定 + 诊断 + 方案落盘；未改产品代码。独立复核发现 9 条缺陷（2×S2、3×S3、4×S4），全部已修（见 DEFECTS.md）。
- 第 2 轮：verifier 确认 v2 收敛、V1–V9 全修、无新增功能缺陷；N1/N2 两条 S4 文档瑕疵已修。AC1–AC6 全通过。
- 第 3 轮：范围扩展（参照物=QQ + 字体统一 AC7 + 换字体评估 AC8）；verifier2 独立复现全部核心事实，W1/W2 两条 S4 已修。AC1–AC8 全通过。
- 第 4 轮：用户批准执行 A/B 闸门。真·不透明重编实测 → 7 文字带逐字节不变 → 主因 A 本机证伪、方案 1 撤销、方案 2 升主路。verifier3 复核：数据成立但结论过强，V1–V3（区域描述/台账矛盾/回滚脚本）已修，V4–V7 取证精度已按复核收窄。结论已改写为「本机当前配置下的有限结论」。AC1–AC8 全通过。
- 第 5 轮：用户批准执行方案 2 + 间距专项。改 `web/index.html`（axis-k 12→13px/去字距/列宽26→34；rver/deck-empty span/kbd-row .plus/kbd→12.5px），重编 release。真机验证：axis-k=13px、间距文字宽26px/列34px（8px呼吸）、全量无<12.5px、墨量+17.1%（measure_ink.py）、五页零溢出。verifier4 判通过可交付，E1/E2 两条 S4 已修。AC-E1~E4 全通过。
- 第 6 轮：用户嫌微调无感、选暴力档+雅黑。真机三档 A/B（final_compare.png）深色像素 A现役1088/B暴力MiSans2455/C暴力雅黑3209 → 落地 C 档（仅 web/index.html 末尾集中覆盖块：雅黑栈+字号+1+字重500+muted#39414D），重编 release。验证：computed 逐项吻合（final_style.json）、五页零溢出（final_pages.json）、开关说明区深色像素 1088→3209(+195%, final_ink.json)。verifier5 判通过可交付。AC-F1~F4 全通过。
  - **重要归因修正**：雅黑无 500 真面，computed=500 实落 Regular、不加墨（verifier5 canvas 实测 400≡500 逐像素相同）；+195% 收益来自**字号+1 / 灰字加深 / 换字形**，非字重。C 档是三因素叠加的结果，勿误归因于「500 字重」这一杠杆。
  - iframe 预览（card.html/dock.html）仍 MiSans，主界面框架曾为雅黑——第 7 轮已换回 MiSans，此不一致消除。
- 第 7 轮：用户看完暴力档 MiSans vs 雅黑对照（`violent_misans_vs_yahei.png`）后要换回 MiSans。仅改 `web/index.html` 覆盖块两行字体变量回 MiSans + 注释，其余暴力设置不动。真机验证：computed 正文族 MiSans、等宽族 Cascadia/Consolas/MiSans，字号15/18/14.5/13.5 与 weight500/#39414D 不变（final_style.json 覆写）、开关说明区深色像素 1445 = 第6轮 A/B 的 B 档逐像素一致、五页 bad=[]、exe mtime 12:45:06>index 12:43:40、实例 PID14260 运行。AC-G1~G3 全通过。

## 残余风险 / 未验证项

- A/B 结论限定在「本机当前 WebView2 版本 / DPR=1 / zoom=1 / 7 个采样文字带」；不能普适化为「透明窗永不导致灰阶」。未测组合：不透明+font-smoothing:auto（后者 Windows 为 no-op，优先级低）。
- WebView2 Runtime 版本、`document.fonts.check('MiSans')` 是否真用内嵌字、DPI 缩放、跨机一致性：未测。
- QQ 更清楚来自字形/字重的解释是推断，本闸门未直接对照测量。
- 29.8/17.8 数据出处为 L1 span（微信语境），仅用于对应分支。
