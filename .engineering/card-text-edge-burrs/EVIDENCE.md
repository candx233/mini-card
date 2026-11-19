# 验收证据 — card-text-edge-burrs

## 结论（先行）

**通过 6/6。** 根因 D0 已修并验证：live 默认超采样从 1× 恢复为 2×，重启后日志确认 ss=2，抓图边缘阶梯消失，小字墨量最低保留 92.1%（在 −10% 底线内）。独立复核发现 2 条台账瑕疵（V1 S3 / V2 S4）均已修，产品面无 S1/S2。

## AC 对照表

| AC | 命令/步骤 | 实际输出 | 结论 |
|---|---|---|---|
| **AC1 默认恢复 2×** | `cd native/minicard-bin && cargo test 默认超采样`（`test-ss.log`） | `test live::tests::默认超采样2倍且可环境变量覆盖 ... ok`；1 passed；EXIT=0。断言：无 MC_SS→2，MC_SS=1→1，=9→4，=0→1 | 通过 |
| **AC2 构建绿** | native `cargo build --release` → src-tauri `cargo build --release`（`build-native.log`/`build-main.log`） | native: Finished release in 25.86s EXIT=0，warning=0 error=0；main: Finished release in 1m48s EXIT=0，warning=0 error=0 | 通过 |
| **AC3 部署生效** | sha256 比对 + 重启 + 查 native log | 源 release hash=`0DB3ED03748B8B3141C42D68B81E4BB3458EF441B640783544CBF6DAADDD2C06`，部署 APPDATA 后同值 MATCH=True；重启后 **20:42:24** 启动窗口出现 **×10** `文字栅格化：超采样 ss=2 → grayscaleEnhancedContrast=0.5`（9 卡 + 1 Dock；改前 19:10:15 启动窗口该行 0 条）。经独立复核订正：原记 20:35:44/×9 为 epoch 换算笔误（V1） | 通过 |
| **AC4 边缘平滑** | PrintWindow 抓常驻卡（`live/` vs `live_after/`）放大对照 `before_after_edges.png`；受控同内容 A/B `edge_zoom_ss1_vs_ss2.png` | 视觉：改前时钟「20:」、天气「8°」大字斜边呈硬阶梯；改后同字形斜边为平滑灰阶过渡，毛刺消失。受控 A/B（同一 wx dump）ss=1 斜边阶梯明显 vs ss=2 平滑 | 通过（视觉，见下「度量说明」） |
| **AC5 小字不塌** | `measure_air_ink2.py wx_ss1.bgra wx_ss2.bgra` | 逐带墨量差：城市 −7.9%、温度大字 −2.9%、日期行 −4.8%、高低温 −5.0%、底行小字 **+3.6%**，整卡 −1.1%。最差 −7.9% < −10% 底线 | 通过 |
| **AC6 独立复核** | 只读子代理 verifier 按 engineering-verify（全新 target 从零编译、rustc 边界探针、内嵌 hash 4 段探针、自抓图 MAD 比对、自跑墨量） | 产品面无 S1/S2；AC1–AC5 均以其亲自证据判通过。发现 V1（S3 台账 epoch 笔误）+V2（S4 判据矛盾），均已修（见 DEFECTS 第 2 轮） | 通过 |

## 度量说明（AC4 的诚实边界）

- 我构造了数个「客观边缘指标」（`measure_smoothness.py` / `measure_burrs.py` / `measure_edge_width.py` / `measure_live_edges.py`）：单行 10–90 坡宽 p50 都是 2.00px（两档无差），fringe/Laplacian 方向相互矛盾。
- 原因：「毛刺」是**斜边上的阶梯形态**（相邻扫描线边缘位置的大跳变），不是单行坡宽；而 box 降采样把硬阶梯打散成更细的灰阶过渡，简单的坡宽/梯度指标测不出这个形态差异。numpy 盒式重建也不完全等于 DComp 合成器的真实降采样。
- 独立复核（verifier）补了一个**能区分的客观指标**，实测时钟卡边缘带：改前中间灰像素占比 13.03% / 灰阶数 212 → 改后 14.12% / 灰阶数 231（方向一致：硬阶梯被更多灰阶过渡取代），MAD=10.52；自抓 after 图与实现者 after 图 MAD=0.00（可复跑、状态稳定）。
- 因此 AC4 = 放大对照图（主证据，项目惯例）+ 中间灰占比/灰阶数（客观旁证）。单行 10–90 坡宽已由 DEFECTS N1、SPEC AC4 明确排除为判据，消除自相矛盾。项目早期文档 `clk_ab_yahei.png` 亦标注 ss=1「sharper, check jaggies」、ss=2「smoother steps」，与本轮结论一致。

## 勘察证据（已取得）

- `desktop-before.png` / `live/`：当前桌面与常驻卡抓图（PrintWindow，绕开浏览器遮挡）。
- `live_after/`：修复重启后的常驻卡抓图。
- `edge_zoom_ss1_vs_ss2.png`：同一雅黑 release weather dump 的 ss=1 原生 vs ss=2 盒降 20× 放大对照。
- `before_after_edges.png`：改前/改后时钟·天气·磁盘大字斜边 12× 放大对照。
- native log：`超采样 ss=2` 改前仅见于 18:20/18:25/18:57 的 probe 启动，live 启动（19:10:15）无该行 → 改前 live 跑 ss=1；改后（20:42:24，epoch 1795092144947）出现 ×10 → ss=2 生效。

## 轮次小结

- 第 0 轮：勘察锁定根因 D0（ss 默认 2→1），建立契约与台账。
- 第 1 轮：实施恢复默认 2×；构建/单测/部署/重启全部通过；AC1–AC5 通过；自查披露 N1。
- 第 2 轮：独立复核产品面通过（无 S1/S2），发现 V1/V2 两条台账瑕疵均已修；连续两轮无新增产品缺陷，收敛。
