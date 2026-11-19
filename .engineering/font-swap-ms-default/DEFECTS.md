# 缺陷台账 — font-swap-ms-default

> 收敛判据 = 无未关闭 S1/S2/S3。独立复核由只读子代理执行。

## 第 1 轮：独立复核（verifier，只读）

| 编号 | 严重度 | 证据 | 根因 | 修复 | 状态 | 轮次 |
|---|---|---|---|---|---|---|
| D1 | S3 | `src-tauri/build.rs:27-29`、`main.rs:12,45-50,2291,2323-2326`、README:37-38 | release 仍内嵌三份 MiSans TTF、解压 APPDATA、传 --font，白背 ~49MB，README 仍声明 MiSans，违反 AC1「全栈」 | build.rs 去 FONT_* 常量与 panic 检查；native_embed 只解 minicard-bin.exe、签名改 Option\<PathBuf\>；ensure_native_cards 删 font_dir_of 与 --font 传递。新主 exe 21.53MB（原~49MB），assets.rs 只剩 NATIVE_BIN，spawn 日志无 --font | 已修 | 1 |
| D2 | S3 | `EVIDENCE.md` AC4 待回填；`text.rs:168-172` | ①20 字缺字探针无执行证据；②FindFamilyName 失败直接 Err→绘制进程启动失败；③回退字体度量不同 | ①加 `系统字体集覆盖_gb2312_外汉字` 单测（20 字 miss=[]，test-draw-r2.log）；②from_system 加 family 兜底链（雅黑→Segoe UI→YaHei UI→Arial→系统首 family）；③记残余风险（混排基线，见下） | 已修（③残余） | 1 |
| D3 | S3 | `index.html` 多处 600；`card.html:215,224,455,499,542`；verify-live.log h1 weight=600 | 600→Bold(700) 相对 MiSans Semibold(600真面)实质加重，未确认是否发块/截断 | 真机 computed + 截图核对：`ms_page-page-desk.png` 的 18px/600 标题清晰不发块、五页 bad=[]；native 时钟大字（600→Bold）出图正常（clock_yahei.png）。结论： Bold 可接受，无需下调 | 已修（真机确认） | 1 |
| D4 | S3 | `text.rs:76-89`；SPEC §62；超采样掉墨 6.5–12.3% | SMALL_EMBOLD 升 500 在雅黑落回 400，补墨空操作，契约要求处置 | A/B 实证：MC_SMALL_EMBOLD=0/1 两档 todo dump 逐字节相同、墨量差 0；在 format_w 调用处显式判断「升档若落回同一字面则跳过」，雅黑自动不升、MiSans 历史稿照常 | 已修 | 1 |
| D5 | S3 | verify-live.log 仅 5 个 index section；EVIDENCE AC5 待回填 | card/dock/menu 独立页无验证 | 新增 verify_subpages.py，CDP 逐页验证：三页字体栈均雅黑、scrollW=992=innerW 无溢出（subpages.json） | 已修 | 1 |
| D6 | S4 | `web/fonts/README.md`、`native/assets/fonts/README.md`、`text.rs:659` | 字体 README 仍描述 MiSans 为界面字体；text.rs 注释「1.326em」已不适用 | 根 `README.md:35-43` 已更新为系统默认字体声明、标注 MiSans 仅历史留存（第 2 轮修）；web/fonts/子目录 README 仍为历史说明（保留）；text.rs:659 注释已不适用但代码用 self.ascent/descent 自适应、逻辑正确 | 已修（根 README），子目录 README 遗留 S4 | 2 |
| D7 | S4 | `text.rs` tnum 宽度断言用 MiSans 夹具；雅黑测试只断 3 个 css_weight | 产品走雅黑但度量/宽度测试仍用 MiSans 夹具；CI 上「找不到雅黑静默 return」会假绿 | 本机真实执行了雅黑测试（有 ok 无跳过）；残余风险记载。native 四卡真机 dump 已覆盖宽度稳定性（AC3） | 遗留（S4） | 1 |
| D8 | S4 | `index.html:2574,2707`、`main.rs:324-340`、`notify.rs`(+235) | 字体 diff 混入 dock 设置 / notify hover 其它任务改动 | 确认是其它任务既存改动，与字体无关；不在本轮归因，保持不动 | 遗留（S4，非字体） | 1 |

## 第 1 轮实施期自查（实施者发现）

| 编号 | 严重度 | 证据 | 根因 | 修复 | 状态 | 轮次 |
|---|---|---|---|---|---|---|
| D9 | S2 | 内嵌 native exe hash C80512B6 ≠ 源 77FD3EC7 | 重编时 bin 与 src-tauri 两 job 并行，build.rs 在 native exe 重编完成前复制了旧版 → 用户实际绘制进程缺 D2/D4 修复 | 串行重编：先确保 native release 最新，再单独重编 src-tauri；验证内嵌 hash=源 hash=77FD3EC7，APPDATA 解压更新为最新版（16:42:59） | 已修 | 1 |

## 第 2 轮：收敛复核

只读子代理复核判定 **可以交付**：9 项缺陷（5×S3 + 1×S2 + 3×S4）中 S2/S3 全部关闭、证据链完整；无新 S1/S2/S3。复核另提 5 条 S4：

| 编号 | 严重度 | 问题 | 处置 | 状态 |
|---|---|---|---|---|
| N1 | S4 | 兜底顺序 Segoe UI 在 YaHei UI 前，无雅黑系统度量基准可能落无 CJK 字体 | 调整为 YaHei UI → Segoe UI → Arial（text.rs:227-229） | 已修（第 2 轮） |
| N2 | S4 | README 已更新但台账未同步 | DEFECTS D6 状态已更新 | 已修（第 2 轮） |
| N3 | S4 | MC_DBG_WEIGHT 打印原始 weight 非 effective | 打印 effective（text.rs:418） | 已修（第 2 轮） |
| N4 | S4 | target/build 下两个旧 hash 目录残留含 FONT_* 的旧 assets.rs | 构建缓存，当前 OUT_DIR 未引用；不清理亦可，不影响产物 | 遗留（S4） |
| N5 | S4 | 终极兜底取系统首个 family 极端情况可能符号字体 | 几乎不可达；雅黑/Segoe/YaHei UI/Arial 四档已覆盖所有常规系统 | 遗留（S4） |

## 残余风险 / 未验证项

- **D2③**：回退字体（SimSun 等）ascent/descent 与雅黑不同，生僻字混排可能垂直错位——本机雅黑全覆盖，未触发；非雅黑系统（英文/精简版）未实测，属已知边界。
- **跨机不一致**：雅黑随 Windows 版本更新字形/hinting，内嵌 MiSans 逐像素同源的优势丢失——用户拍板时已知悉接受。
- **600→Bold**：本机截图确认不发块，但未做逐像素墨量 A/B 量化（D3 以真机观感+无溢出判据关闭）。
- **D6/D7/D8**：S4 遗留项，不阻塞交付；对外 README 字体声明待单独文档更新。

## 轮次小结

- 第 1 轮：独立复核 8 条（5×S3+3×S4）+ 实施自查 1 条（D9/S2），全部处置：S3/S2 已修，3 条 S4 记遗留。
- 待第 2 轮复核确认收敛。
