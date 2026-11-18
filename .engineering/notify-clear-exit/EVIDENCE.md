# 验收证据 · notify-clear-exit

> AC → 命令/步骤 → 实际输出 → 结论。

## AC 对照表

| AC | 命令 / 步骤 | 实际输出 | 结论 |
|---|---|---|---|
| AC1 | `cargo test -p minicard-cards clear_all_collapses -- --nocapture` | `clear_all_collapses_every_row_in_place_without_moving_chrome ... ok`；断言 0/45/90/135/179ms 行高单调递减、180ms 全为 0、`animating()` t<180ms 为 true | **通过** |
| AC2 | `cargo test -p minicard-cards clear_all_survives -- --nocapture` | `... ok`；旧快照 observe 后 4 条仍 suppress、`enter` 为空、布局无闪回；空快照后 suppress 清空 | **通过** |
| AC3 | 同上单测 + `--probe notify-motion` 帧序列 | 单测断言 `ID_DIVIDER`/`ID_HEADER` 的 y/h 在 0..179ms 全程相等；`clear-all2/frame/sheet.png` 逐帧目视卡框/分割线/抬头不动 | **通过** |
| AC4 | `cargo test --manifest-path native/minicard-cards/Cargo.toml` | 基线 115 → 现 **117 passed; 0 failed**（新增 2 条）；`cargo check --manifest-path native/minicard-bin/Cargo.toml` → `Finished dev profile` | **通过** |
| AC5 | `python -m http.server` + `Invoke-WebRequest`；headless Edge 渲染 `file:///.../notify_clear_exit_demo.html?t=300` 与 `?t=420` | `HTTP 200 length=15922`；`_notify_clear_exit_t300.png` 读出 confirm 0.001 / 清除 0.999 / 整列 0.163 / 行高 6.2 DIP / 空态 0；`_notify_clear_exit_t420b.png` 读出空态 0.684、计数与清除已淡出 | **通过** |
| AC6 | 独立评审子代理 `verify-aesthetics` 只读看原生 `sheet.png` 逐格裁剪 + headless Edge 渲染 demo 27 个时间戳 | 结论「有 P2 待定、无 P1 及以上」：清除→确认交叉同矩形无位移、整列 ink 单调 31.4→15.0 零回跳、组头位移 -1/-2/-4/-6/-8 连续、收尾无半行；P2 为既有行高插值文字切片（1× 不可辨）与 demo CSS（已修） | **通过** |

## 执行阶段中间校验

- Demo 渲染中发现并修复 3 个真实缺陷（详见 DEFECTS.md 轮次 1）：主卡只复制了表头导致空白；关键帧缩略图用 240px 宽配 360px 字号导致「邮件」换行/文字裁切；空态时抬头仍显示「1 条未读 / 清除」。
- Demo 逐帧核对（headless Edge `?t=` 深链，`_notify_clear_exit_t300.png` / `_t420b.png`）：t=300ms 时「确认」alpha 0.001、「清除」0.999、整列 0.163、4 行高均 6.2 DIP、空态 0；t=420ms 空态 0.684、计数与清除淡出。
- 原生像素差分（`clear-all2/frame`，vs f010 基线，f012/f014/f016/f018/f020/f024）：卡顶框 / 标题「通知」/ 分割线带 `maxdiff=0`（全部 6 帧）；`ID_HEADER`/`ID_DIVIDER` 布局 y/h 亦由单测断言不变。
- 基线：`cargo test --manifest-path native/minicard-cards/Cargo.toml` → `115 passed`（改动前）。
- 编译：`cargo check --manifest-path native/minicard-bin/Cargo.toml` → `Finished dev profile [unoptimized + debuginfo]`。
- 原生帧序列（无头，合成时间轴 16.667ms/帧，事件 t=200ms，63 帧）：
  ```
  cd native\minicard-bin
  set MC_NOTIFY_MOTION=clear-all & set MC_NOTIFY_PRE_MS=200 & set MC_NOTIFY_FRAME_MS=16.667
  minicard-bin.exe --probe notify-motion --w 240 --h 300 --ss 2 --no-glass \
    --font F:\mini-card\native\assets\fonts\MiSans-Regular.ttf \
    --dump-seq docs\verify\_notify_motion_20261116\clear-all2\frame --exit-after 900
  ```
  输出：`notify-motion: scenario=clear-all frame_ms=16.67 at=200ms items 3→3`；`退出（共 63 帧）`。
- 关键帧拼图：`docs/verify/_notify_motion_20261116/clear-all2/frame/sheet.png`（f010..f030，step 2）。
- 美观中间结论：初版仅行塌缩时正文在 f016/f018 被横切成薄片（既有 P2）；加入 180ms 整列淡出后薄片不可见，收尾干净。

## 独立检验结论（摘录）

**verify-ac：部分通过 → 6 条 AC 逐条通过；发现 V1(S3)/V2(S4)/V3(S4)，均已返工修复（见 DEFECTS.md）。**
关键复核：逐帧差分 f010→f031 视口差值 `8131123 → … → 0` 全程单调递减零回升；AC3 卡框/分割线 region `max=0`。

**verify-aesthetics：无 P1 及以上，2 个 P2（demo CSS / 空态叠字，均已修）。**
关键确认：120ms 交叉同矩形无位移；180ms 淡出+塌缩同拍（源码 `listFade` 与 `rowE` 同取 `1-easeStandard`）；组头连续加速无跳变；收尾 t=380 归零、t=540 空态满。

**返工回归（轮次 3）**：`cargo test` 119 passed；修复后探针帧与修复前逐字节相同（maxdiff=0，happy path 无回归）；demo 重渲染页眉徽章与空态均正常、无任何外部资源引用。

**两个子代理定点复核（最终）**：
- verify-ac：V1/V2/V3 三条**全部关闭**，新单测逐条 `ok`，demo 无外部引用（grep 零命中），探针帧 SHA256 6/6 与新修复前一致；**无新缺陷**。
- verify-aesthetics：V2/V3 视觉**关闭**，`?t=0` 徽章恢复小胶囊、`?t=390/420` 无叠字、系统字体栈无新增换行裁切；**无新视觉缺陷**。
- 收敛达成：轮次 3 无新增缺陷，且两个独立检验者复核均无新发现。

## 残余风险与未验证项（显式列出）

1. **V1-aes（既有 P2，未修）**：塌缩中段正文被横切薄片，1× 实尺寸不可辨（整列淡出遮罩），放大 3× 可见；属既有「逐帧行高插值」方案固有代价，见 tuning 文档 §4。若日后要彻底消除需改「先淡出再收高度」，会变成另一套观感，本轮不混。
2. **O1（观察项）**：180ms 淡出期内极快二次 clear 会让 fade alpha 从 ~0.16 跳回 1.0；真机需在 180ms 内完成「重新 arm + 再确认」且快照仍陈旧，评审未拿到现实复现，未修。视口此时已空，跳变不可见，实际影响极低。
3. **O2（AC3 字面口径）**：像素差分中抬头区有变化，但逐项定位全部来自既有「确认→清除」120ms 交叉（本轮未改）；卡框/分割线/抬头结构 y/h 均 max=0。若把「抬头像素差为 0」按含标签文字的字面理解，需回契约澄清。
4. **未覆盖**：真机 Windows 异步删除时序（≤1.5s 收敛、2s 回滚）只在合成时间轴上验证，无真机日志；`live.rs` 的 UI 点击路径无自动化测试；深浅主题 / DPI / reduced-motion / 真机 `MC_FPS_LOG` 帧率不在本轮材料内；原生空态淡入需快照收敛，无原生帧佐证（仅 demo 证明）。
5. **demo 字体**：为满足单文件改用系统字体栈，与原生 MiSans 字形略有差异；动效与数值不受影响。
6. **O1（复核观察项，S4/未实测）**：新通知在 180ms 淡出窗内到达、又在窗内被系统删掉时，`deferred_clear` 释放路径会给一个已不在快照里的 id 插入一次 180ms `enter`（幽灵计时，不绘制、不可见，白烧约 3 帧）。需「窗内到达又被立刻删除」时序，正常 UI 极难触发；复核者为代码审阅推断、未实测，不影响任何 AC。本轮不改已通过复核的代码，显式留此项。

## 复验命令（用户可重跑）

```powershell
# 1) 单测（应 119 passed）
& 'C:\Users\22088\.cargo\bin\cargo.exe' test --manifest-path F:\mini-card\native\minicard-cards\Cargo.toml

# 2) 编译检查
& 'C:\Users\22088\.cargo\bin\cargo.exe' check --manifest-path F:\mini-card\native\minicard-bin\Cargo.toml

# 3) 原生逐帧取证
cd F:\mini-card\native\minicard-bin
$env:MC_NOTIFY_MOTION='clear-all'; $env:MC_NOTIFY_PRE_MS='200'; $env:MC_NOTIFY_FRAME_MS='16.667'
.\target\debug\minicard-bin.exe --probe notify-motion --w 240 --h 300 --ss 2 --no-glass `
  --font F:\mini-card\native\assets\fonts\MiSans-Regular.ttf `
  --dump-seq F:\mini-card\docs\verify\_notify_motion_20261116\clear-all-final\frame --exit-after 900
python F:\mini-card\docs\verify\_notify_motion_20261116\_nm_sheet.py `
  F:\mini-card\docs\verify\_notify_motion_20261116\clear-all-final\frame 10 30 2 --cols 6 --scale .5

# 4) 互动 demo（浏览器直接打开，拖时间轴 / 一键重播 / ?t= 深链）
start "" "F:\mini-card\docs\verify\notify_clear_exit_demo.html"
start "" "F:\mini-card\docs\verify\notify_clear_exit_demo.html?t=300"
```
