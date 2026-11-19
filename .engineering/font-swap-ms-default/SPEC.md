# 契约：全栈换回微软默认字体（Microsoft YaHei / Segoe UI）

任务 slug：`font-swap-ms-default` · 档位：标准（实施 + 真机验证） · 建立：2026-11-19

## 用户原话（照录）

> （本轮）「你评估下转回微软默认字体的方案 因为我看所有软件都没有用别的字体的 而且确实用默认字体明显会清晰很多」
> 「2吧 然后你说的字重问题 进行相对应的调整就行了」
> 「顺便开启工程模式 如果你要开始做的话」

## 目标

1. **全栈**把正文族换成微软默认字体：中文 = 微软雅黑（`Microsoft YaHei`），西文/数字 = Segoe UI；覆盖 web 五页（index/card/dock/menu/motion-lab）+ native 自绘（cards/dock/live/bin）。
2. **字重塌陷做对应调整**：雅黑无 500/600 真面（只有 Light 200 / Regular 400 / Bold 700），对现有 400/500/600 三真字面做语义保真的重映射，不出现观感突变/宽度溢出/合成加粗。
3. 兑现"所有软件都用系统默认字体"的诉求；既然用系统字体集，顺带**根治 native 私有字集的缺字（豆腐块）问题**（历史文档多次建议的系统回退，换系统字体后天然获得）。

## 历史反复（必须先读，避免重蹈覆辙）

- 第 6 轮（11-18）：用户「感觉得暴力…不如直接用微软雅黑」→ 仅 web/index.html 落地雅黑暴力档，深色像素 1088→3209。
- 第 7 轮（11-19 早）：用户看完 `violent_misans_vs_yahei.png` 后「换成misans吧」→ 换回 MiSans，保留暴力三件套（字号+1/字重500/灰字加深）。
- 本轮（11-19 下午）：用户经评估后明确拍板方案 2 = **全栈换雅黑**，并授权字重调整。

> 关键教训（EVIDENCE.md:37）：雅黑无 500 真面，computed=500 实落 Regular、不加墨（400≡500 逐像素相同）；第 6 轮 +195% 收益来自**字号+1 / 灰字加深 / 换字形**，不是字重。所以本轮字重调整的目的是**保持层级语义 + 避免突变**，不是靠它提清晰度。

## 验收标准（AC）

- **AC1 全栈无遗漏**：六面（五 HTML + native）正文族全部落在雅黑/Segoe UI；`--font-body`、`--font-mono`、native `TextEngine` family、@font-face 声明逐面核对；root 下不留 MiSans 引用（design-shots 展品除外）。判据：`grep -rn "MiSans" web/ native/*/src .engineering/font-swap-ms-default` 命中的是注释/历史说明而非生效声明。
- **AC2 字重语义保真**：web 的 400/500/600 字体栈在系统字体下能正确选面（雅黑 Regular/Bold、Segoe UI 各档）；native 的 `css_weight` 用系统字体字面表重标，500 不再塌陷成合成加粗。判据：真机 computed + native 日志字面表 + 三真字面文案层级（标题 > 正文 > 说明）肉眼可辨。
- **AC3 tnum 数字稳定**：`font-variant-numeric:tabular-nums` / native `DWRITE_FONT_FEATURE_TAG_TABULAR_FIGURES` 在 Segoe UI / 雅黑上逐秒刷新宽度不跳。判据：时钟秒位、性能卡数值连拍宽度不变。
- **AC4 缺字回退**：native 任意中文/生僻字不再豆腐块（系统字体集覆盖 GB2312 外）。判据：造 20 个 GB2312 外汉字探针，缺字 0 命中。
- **AC5 真机无回归**：五页无溢出/换行/截断（`docScrollW=innerW`、`bad=[]`）；卡片文字不越界、右留白 ≥3px；位置面板仍对齐。
- **AC6 编译与单测**：debug + release 均 0 警告；`cargo test` 全绿（度量/字重相关单测按雅黑字面表重标后通过）。
- **AC7 recompile 生效**：release exe mtime > 所有改动文件；真机截图后重启实例验收。
- **AC8 独立复核**：只读子代理按工程模式复核，缺陷清单收敛（无未关闭 S1/S2/S3）。

## 非目标

- 不改字号/灰字加深的"暴力三件套"（用户本轮只授权换字体 + 字重调整，字号配色保持第 7 轮现状）。
- 不迁移主界面到 native 自绘（独立立项，80–118 人日）。
- 不改透明窗（A/B 闸门已证伪方案 1）。
- 不删除 design-shots 展品内的 MiSans（那是历史设计稿，不随产品发）。

## 约束

- 平台 Windows，WebView2 (Chromium) + native D2D/DWrite 双栈。
- windows crate 0.62.2：`IDWriteFactory::GetSystemFontCollection(&mut Option<IDWriteFontCollection>, bool)` 可用（已核实签名，registry line 2089）。
- 雅黑只有 Light/Regular/Bold 三档；Segoe UI 有 Light/Semilight/Regular/Semibold/Bold。
- 跨机像素不一致是已知代价，用户已知悉并接受（评估 §代价清单）。
- 字体文件路径：`C:\Windows\Fonts\msyh.ttc` 等（已核实存在）。

## 字重映射方案（核心）

**web（`--font-body` / `--font-mono`）**
- body 栈：`'Microsoft YaHei', 'Segoe UI', system-ui, sans-serif`
- mono 栈：`'Segoe UI', 'Cascadia Mono', 'Consolas', ui-monospace, monospace`（保留 tnum 数字；中文无字形时回落雅黑由 body 兜底——需在 AC1 核对）
- 字重：CSS font-weight 500/600 保留原声明（雅黑无真面时浏览器自动选 Regular/Bold）。因第 6 轮已证 500≡400 不加墨，500 声明保留不影响观感，但 600 需确认落 Bold 且不"发块"。

**native（`TextEngine::from_system`）**
- 用 `GetSystemFontCollection` + family `Microsoft YaHei` 建字体集（替代 AddFontFile 私有集）→ 天然获得系统回退。
- `declared_css_weight`：雅黑字面是标准 OS/2 值（200/400/700），现有函数已把标准值原样返回，无需改。
- `css_weight`：500→400（Regular）、600→700（Bold），符合雅黑字面；需重标 `faces` 表并更新相关单测断言（不再期望 330/380/520）。
- `SMALL_EMBOLD`（≤15px 400 升 500 补墨）：雅黑无 500 面，升 500 会落回 Regular —— 这条补墨路径**失效**。处置：改用更粗字重或直接关掉（超采样掉墨由雅黑 hinting + 更粗字形部分抵消），真机 A/B 定。
- ink_center_bias / ascent / descent / 数字宽：雅黑度量与 MiSans 不同，相关硬编码断言需按雅黑字面重标。

## 决策记录

- 用户拍板方案 2（全栈换），明确「字重问题进行相对应的调整就行了」→ 字重映射由实现方按雅黑字面表决定，不逐条回问。
- 用户要求开启工程模式 → 建本三件套 + 独立复核。
- 第 6/7 轮反复已记录；本轮是用户经评估后的最终决定，不再建议回滚，除非真机 A/B 证明明显更差（届时先问用户）。

## 回滚

- web：`git checkout -- web/*.html`（暴力三件套在第 7 轮已提交，可秒回）。
- native：`git checkout -- native/`；重新 `cargo build --release` 恢复 exe。
