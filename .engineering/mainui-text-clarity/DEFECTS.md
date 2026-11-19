# 缺陷台账 — mainui-text-clarity

> 本轮产物是诊断+方案文档，非产品代码；缺陷 = 诊断/方案的证据缺口或逻辑漏洞。
> 收敛判据 = 无未关闭缺陷且连续一轮无新发现。

## 第 1 轮：独立复核（verifier，只读）

| 编号 | 严重度 | 证据 | 根因 | 修复 | 状态 | 轮次 |
|---|---|---|---|---|---|---|
| V1 | S2 | `grep -rn '29.8\|17.8' docs/*.md` 仅命中方案自身；真实来源为 Memmy L1 span `span_3fe9478842c46f1371f7`（主题「候选条 vs 微信」），非 `font-and-aa-eval.md §2`；EVIDENCE 误写 `font-and-aa-eval.md:2` | 引用出处张冠李戴，且把「对比微信」语境的数据挪用于「对比卡片」主叙事 | 改正出处为 L1 span；把 29.8/17.8 移入「若参照物是微信」分支，主叙事只用可归属本项目的证据 | 已修 | 1 |
| V2 | S3 | `build_misans_subset.py:9` 只说「只做字形子集」，`:95` 仅 drop DSIG；方案 §7 自认未读表 | 「MiSans 无 hinting」写成事实却无直接证据 | 用 fontTools 直接读四档 woff2：**仅 prep/gasp 表、0 个含指令字形** → 升级为已证实，附命令与输出 | 已修（升级为已证实） | 1 |
| V3 | S3 | `cdp_live_ink.js` 不存在，实际为 `minicard_center/cdp_live_ink.py` | 笔误 | 命令索引改为真实文件；主窗墨量取证改用 `mc_live_probe.py <probe.js> <out.png>` | 已修 | 1 |
| V4 | S2 | Electron PR #18101 diff 原文：「Notice that just setting the background in the CSS does not have the desired effect」；§3 同时改 background + border-radius | A/B 闸门用 CSS 注入可能假阴性→误跳过方案1；且变量混淆无法归因 | 改为双层 A/B：Tier1 CSS 注入仅作「阳性有效、阴性不定」筛选；Tier2 窗口级不透明（临时改 config 重编或 WebView2 controller 背景色）为决定性判据；分变量迭代 | 已修 | 1 |
| V5 | S4 | §0 列 1>2>3，但方案2 自称稳/便宜/无视觉代价却排在方案1拍板之后；方案1b 未核实即排除 | 排序按「收益绝对值」而非自称「性价比」 | 重排为：验证闸门 → 方案2（先做）→ 方案1b 可行性 spike → 方案1a（拍板后）→ 方案3；补 1b 可行性判据 | 已修 | 1 |
| V6 | S4 | 方案「换 ClearType」行引 `font-and-aa-eval.md §1.1/§3`（native DWrite/gfx.rs）否决 WebView2 的 ClearType | 违反 AC1 分栈原则；结论对、理由错 | 改写为「WebView2 子像素 AA 由层合成条件决定，不由 native DWrite 设置决定；无需系统级改 ClearType」 | 已修 | 1 |
| V7 | S4 | 方案称 Electron「commit 683bef5b」，PR #18101 merge commit 实为 `e63f527e…`，无法核实 | 引用未证实 hash | 去掉 hash，只保留可核实的 PR 链接 | 已修 | 1 |
| V8 | S4 | §0/§4 写 `build_misans_subset.py:8`（「只做字形子集」实为 `:9`）；`.snote` 基础规则 `:381` 为 13px、仅 `:896` 内联 12.5px | 轻微行号偏移/措辞松 | 改正行号；`.snote` 从「低于拐点」清单剔除或标注例外 | 已修 | 1 |
| V9 | S4 | 「若参照物是历史版本」分支无具体判定回归步骤；缺「若参照物是微信/其它 App」分支（而 V1 数据真实语境正是微信） | 假设分支不完整 | 补两支：历史版本用 `git log` + 定点 A/B 的操作步骤；微信/其它 App 分支纳入 29.8/17.8 数据并说明其适用边界 | 已修 | 1 |

## 第 2 轮：收敛复核（verifier，只读）

| 编号 | 严重度 | 证据 | 根因 | 修复 | 状态 | 轮次 |
|---|---|---|---|---|---|---|
| N1 | S4 | `SPEC.md:39` 仍写「`font-and-aa-eval.md` 论证 MiSans 无 hinting」，但该文档从未如此论证 | V1 出处错误在契约文件残留（plan/EVIDENCE 已改、SPEC 未同步） | 改为「本次 fontTools 直读四档 woff2 证实」+ 脚本注释佐证 | 已修 | 2 |
| N2 | S4 | plan 复跑命令含 `...` 与 `<fonttools目录>` 占位，不可直接粘贴 | 命令索引不完整 | 补完整可执行的 hinting 复核命令 + `uv run` 免装包版，并声明占位参数含义 | 已修 | 2 |

## 轮次小结

- 第 1 轮：独立复核发现 9 条（2×S2、3×S3、4×S4），全部已修；V2 由「事实但无证据」直接读表升级为「已证实」。
- 第 2 轮：verifier 确认 V1–V9 真修好、无新增功能缺陷、整体收敛；余 2 条 S4 文档瑕疵（N1/N2）亦已修。
- 收敛达成：无未关闭缺陷，第 2 轮后无新发现。AC1–AC6 全通过。

## 第 3 轮：范围扩展（参照物=QQ + 字体统一核查 + 换字体评估）

| 编号 | 严重度 | 证据 | 根因 | 修复 | 状态 | 轮次 |
|---|---|---|---|---|---|---|
| W1 | S4 | eval §0.2/§3.2「500/600 各 7 处」把 @font-face 声明计入 | 计数口径 | 改为「各 6 处样式 + 各 1 处 @font-face 声明」 | 已修 | 3 |
| W2 | S4 | eval §1.1「Skia 不读 ClearType Tuner」过绝对（Edge 已加开关、Chromium 有 feature） | 表述精度 | 加「默认路径下」限定并注明开关存在 | 已修 | 3 |

轮次小结：AC7/AC8 核心事实全部经 verifier2 独立复现（fontTools 直读、源码 grep、git、权威来源逐项核实），无 S1/S2/S3；W1/W2 两条 S4 已修。AC1–AC8 全通过。

## 第 4 轮：用户批准执行 A/B 闸门

| 编号 | 严重度 | 证据 | 根因 | 修复 | 状态 | 轮次 |
|---|---|---|---|---|---|---|
| Q1 | S2 | verifier3 判定「主因 A 被证伪」措辞强于证据力；只能得出「本机当前配置下 7 文字带不变、无证据支持透明窗为主因」的弱结论 | 因果结论超出数据范围 | RESULT.md 与 plan §0 收窄为有限结论，显式声明不普适化 | 已修 | 4 |
| Q2 | S3 | RESULT 原写「8400 px 变化仅四角」，实测约 332 在四角、约 8068 在卡片预览合成区 | 区域描述错误 | 改正为分区描述；明确「文字带 0 变化」只对 7 采样带成立 | 已修 | 4 |
| Q3 | S3 | EVIDENCE/SPEC/plan 多处仍写「主因 A 待实测/方案 1 要做」，与闸门结果矛盾（AC5） | 台账未同步 | SPEC 假设行+决策记录、EVIDENCE AC2/AC4/残余风险、plan v3 横幅均已同步 | 已修 | 4 |
| Q4 | S3 | `capture_opaque.py` 备份路径相对仓库根却按 OUT 拼接→重复，回滚必失败 | 路径解析 bug | 按 ROOT 解析、每段还原独立 try、加 hash 校验 | 已修 | 4 |
| Q5 | S4 | RESULT 头条 ink 数值（346.9 等）与 analyze.py/metrics.json（348.3 等）对不上；spread 数值无落盘出处 | 手算数字未回填 | RESULT 改用 analyze.py 实际输出；spread 标注为临时量测脚本 | 已修 | 4 |
| Q6 | S4 | analyze.py key 只用 x，三个 `.prow .hint` x 相同→metrics.json 丢两条 | key 碰撞 | key 加 y | 已修 | 4 |
| Q7 | S4 | chroma>12 阈值紧贴字体本色极差（muted 22），当子像素旁证效度弱 | 指标效度 | RESULT 改用中性字通道极差分布为主判据，chroma 降为旁证 | 已修 | 4 |
| Q8 | S4 | ab_gate.py Tier2 同时改背景 override+CSS 注入，非严格单变量 | 归因精度 | RESULT 合并表述为「不透明背景组合干预」，以干净的第 5 行（真重编）为准 | 已修 | 4 |

轮次小结：闸门数据经 verifier3 独立重算成立；Q1–Q8 全修（1×S2、3×S3、4×S4）。收敛达成：AC1–AC8 全通过，无未关闭缺陷。

## 第 5 轮：用户批准执行方案 2（小字提档 + 间距专项）

改 `web/index.html`：axis-k 12→13px + 去 letter-spacing + 列宽 26→34px；rver/deck-empty span/kbd-row .plus/kbd 11.5|12→12.5px。重编 release。

| 编号 | 严重度 | 证据 | 根因 | 修复 | 状态 | 轮次 |
|---|---|---|---|---|---|---|
| E1 | S4 | verifier4：「ink 49.7→58.2」无落盘证据、verify_after docstring 称量墨量但没算 | 手算数字未归档 | 新增 measure_ink.py + ink_result.json（可复跑），墨量 +17.1% | 已修 | 5 |
| E2 | S4 | verifier4：「六页」措辞不符（实际 5 个 .page） | 措辞误差 | verify_after.py docstring/注释改「五页」 | 已修 | 5 |

轮次小结：AC-E1~E4 全部满足（13px/去字距/列宽34px、无<12.5px、墨量+17%/五页无溢出、重编生效且实例运行）。verifier4 判「通过可交付」，E1/E2 两条 S4 已修。

## 第 6 轮：用户选暴力档 + 微软雅黑（感受不到微调的区别）

改 `web/index.html` 末尾集中覆盖块（雅黑栈 + 字号+1 + 字重500 + muted#39414D），重编 release。三档真机 A/B：深色像素 A现役1088 / B暴力MiSans2455 / C暴力雅黑3209 → 落地 C。

| 编号 | 严重度 | 证据 | 根因 | 修复 | 状态 | 轮次 |
|---|---|---|---|---|---|---|
| O1 | S4 | DEFECTS/EVIDENCE 台账止于第5轮，缺第6轮 AC-F 记录 | 台账未同步 | 补 EVIDENCE AC-F1~F4 行 + 第6轮小结 + 归因修正说明 | 已修 | 6 |
| O2 | S4 | 雅黑无 500 真面，computed=500 实落 Regular、不加墨（verifier5 canvas 实测 400≡500 逐像素相同） | 字重杠杆在雅黑下无效 | EVIDENCE 记录归因修正：+195% 来自字号/色深/换字形，非字重；勿误判杠杆 | 已修（记录） | 6 |
| O3 | S4 | 无改前 exe hash 存档，「hash 变化」无法独立核对 | 未记录基线 hash | mtime+构建日志+运行实例已充分证明重编生效，不做额外动作 | 接受（非缺陷） | 6 |
| O4 | S4 | git diff 含第5轮4处改动（相对 HEAD 未提交） | 第5轮改动尚未提交，非误改 | 属已批准的第5轮，提交时一并说明即可 | 接受（非缺陷） | 6 |

轮次小结：AC-F1~F4 全部满足（computed 逐项吻合、五页零溢出、深色像素+195%、重编并运行）。verifier5 判「通过、请用户实机验收」，4 条 S4 已处理/记录。

## 第 7 轮：换回 MiSans（用户看完对照后）

仅改 `web/index.html` 覆盖块两行字体变量回 MiSans + 注释；暴力三件套不动。重编 release。

- 无缺陷。验证：computed 正文族 MiSans、等宽族 Cascadia/Consolas/MiSans，深色像素 1445 = 第6轮 A/B 的 B 档逐像素一致，五页 bad=[]，重编并运行。AC-G1~G3 全通过。
- 小改动（两行变量 + 注释 + 台账），单文件可秒级回滚、无回归面，按对话内台账验收，不另起独立复核。收敛达成。
