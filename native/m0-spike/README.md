# M0 spike —— 用 Rust 自绘一张真卡（Direct2D + DirectWrite + DirectComposition）

对应 `docs/rust-renderer-plan.md` §6 的 **M0 可行性关卡**。目标只有一个：**证明「卡片端从 WebView2 迁到自绘」这条路走得通**，并拿到一票否决项的实测数。

一句话结论：**走得通**。同一张性能卡、同一个位置、同一张壁纸，自绘版与 WebView2 版逐像素平均差 **4.0/255（1.6%）**，差异全部落在字形/条缘的抗锯齿上；内存（任务管理器口径）**11.3 MB**、空闲 CPU **0.06%**、exe **254 KB**。

---

## 怎么跑

```bash
cd native/m0-spike
cargo build --release          # 产物 target/release/m0-spike.exe（254 KB）

# 真机看效果（不激活窗口；左键拖动整卡，右键关闭）
./target/release/m0-spike.exe --x 266 --y 309

# 贴到桌面层（壁纸之上、普通窗口之下）
./target/release/m0-spike.exe --x 266 --y 309 --desktop

# 与 WebView2 版逐像素对比（固定数值 + 固定壁纸切片 + 把帧缓冲读回磁盘）
./target/release/m0-spike.exe --x 266 --y 309 --freeze 27,61,12 \
    --wall-raw wall.bgra --wall-w 1199 --wall-h 686 --mon 1920x1080 \
    --dump out.bgra --exit-after 2200
```

`--dump` 写出的是**面板缓冲原始像素**（BGRA，预乘 alpha）+ 同名 `.txt` 里的 `宽x高`；字体用 `native/assets/fonts/` 的子集（`build_misans_ttf.py` 生成，3 档共 3.6 MB，不进 git；见该目录 README）。

主要参数：`--w/--h`（默认 240×120）、`--glass`（0..100，对应 config 的 glass_alpha）、`--blur`（px，对应 glass_blur）、`--font`、`--no-glass`、`--debug-alpha`（半透明红块，用来验 DComp 是否真走逐像素 alpha）、`--viewer x,y,w,h`（DWM 缩略图宿主窗，卡片被压住时也能取证）、`--no-sample`/`--snap-text`（诊断开关）。

---

## 实测结果（2026-10-30，本机 1920×1080 / 100% 缩放 / Win11 26300）

| 判据（plan §6 M0-4） | 目标 | 实测 | 结论 |
|---|---|---|---|
| 视觉（M0-1/M0-2） | 肉眼无差 | 与 `web/card.html` 逐像素 **平均差 4.0/255**，p95 = 8 | ✅ |
| 私有工作集（任务管理器「内存」列） | < 15 MB | **11.3 MB** | ✅ |
| 私有提交（任务管理器「提交大小」列） | — | 54.9 MB（**GPU 驱动 53 MB**，见下） | ⚠️ 口径问题 |
| 空闲 CPU（1 分钟均值） | < 0.5% | **≈0.06%**（单 tick：采样 2.7 ms + 绘制 0.8 ms） | ✅ |
| 启动→首帧 | < 150 ms | **296 ms**（D3D11 设备 143 ms + 首帧 150 ms） | ❌ 未达标 |
| exe | < 4 MB | **254 KB** | ✅ |
| 桌面层（M0-3） | DComp 仍合成 | 插到 `Progman/WorkerW` 锚点正上方，DWM 缩略图取证正常 | ✅ |

逐项对照图/取证脚本见 `docs/verify/minicard_center/_m0_ref_shot.mjs`（把真 `card.html` 在 240×120 下用无头 Edge 渲出来当参照物）。
对照图存档：`docs/showcase/m0-20261030/`（`pair_2x.png` 左=WebView2 右=自绘、`diff_2x.png` 差异×8、`live_card_2x.png` 真机截屏、`native_240x120.png` 自绘帧缓冲）。

### 几何对齐（与 CSS 逐项核过的数）

- 进度条右缘：CPU 27% / RAM 61% / GPU 12% → `x = 68 / 143 / 35`，**两边完全一致**（期望 69.4/144.2/36.4 落在像素边界内）。
- 文字 ink bbox：第 2、3 行**逐像素一致**；第 1 行整体差 1 px（亚像素基线相位，见「踩到的坑」）。
- 卡片圆角、白纱 alpha、描边、玻璃亮度均值：差 ≤ 4/255。

---

## 动效（2026-10-30 实测，`--animate`）

条宽补间按 `card.html` 的 `barEase/barTween/barMs` 一比一搬过来（梯形速度剖面、从当前显示值起步、目标不变不重启时间轴、文字立刻显示目标值只补条），跑 60fps 帧循环实测：

| 指标 | 实测 |
|---|---|
| 帧率 | **60.0–60.2 fps**（300 帧：间隔均值 16.61–16.67 ms，p95 17.06，max 19.03，**>20 ms 的帧 0 个**） |
| 每帧 CPU | 重绘+Present **p50 0.43 ms / p95 0.63 ms**（含 6 次文本查找与 3 条药丸几何） |
| 动效期进程 CPU | **5.4%**（单核口径，1 s 采样 4 次均值）；静止期 **0.06%** |
| 亚像素连续性 | 条右缘逐帧位移 0.03→0.06→0.10→0.15→0.26 px **平滑推进，246 帧里 0 帧跳 >1 px**（用「覆盖积分」量，不是靠肉眼） |

动效 GIF：`docs/showcase/m0-20261030/bar_tween.gif`。

**三个必须记住的坑：**

1. **`WM_TIMER` 做不到 60fps**：系统时钟粒度 15.6 ms → 16 ms 的定时器每两拍才来一次（实测 25 ms 间隔、40 fps）。要么自己按 16.667 ms 的 deadline 睡眠（本 spike 的做法），要么用高精度可等待定时器。
2. **`DwmFlush()` 和 DXGI 帧延时等待对象在本机都不节流**：实测都是立即返回，循环直接跑到 **200 fps / CPU 28–104%**（`SetMaximumFrameLatency(1)` 之后 Present 变 4.8 ms，反而更糟）。别指望它们卡帧——节流自己控。
3. **每帧重建 `IDWriteTextFormat`/`IDWriteTextLayout` 是最大的 CPU 开销**：加缓存（格式按 size/align、布局按文本）后 **0.97 ms → 0.43 ms/帧**，CPU 8–11% → 5.4%。

**还没试的动效**（都是同一类「帧函数 → 属性」的事，不是新机制）：时钟逐位滚动（文本 translate + 轴向裁剪）、勾选描线（`stroke-dashoffset`）、日历翻页、封面旋转、悬停描边（颜色插值）。这些要等 M1 的绘制原语齐了才能逐项对齐。

---

## 踩到的坑（都是复刻 CSS 时真实绊倒过的）


1. **CSS `blur(N)` 的 N 就是高斯 σ，不是半径。** 一开始按「σ = N/2」实现，玻璃明显更锐（高频 std 12.08 vs 浏览器 10.85）；改成 σ = N 后高频 std 11.31、整图平均差从 6.55 掉到 3.44。对照法：把两版都取「无文字区」的高频 std 比。
2. **`.num` 的 `font-variant-numeric: tabular-nums` 必须显式打开。** DirectWrite 默认用比例数字，「61%」比浏览器窄 4 px；给 layout 挂 `DWRITE_FONT_FEATURE_TAG_TABULAR_FIGURES` 后逐像素对齐。
3. **行盒要按 CSS 的 half-leading 算，别用 DWrite 默认行距。** `line-height: 1.15` 在这里是「内容区（ascent+descent = 1.326 em）比行盒还高」，baseline = `(lineH − content)/2 + ascent·size`，用 `SetLineSpacing(UNIFORM, lineH, baseline)` 钉死。MiSans 的实测度量：ascent 1.044 / descent 0.282 / upem 1000。
4. **±1 px 的亚像素相位差是必然的，别追。** 行盒顶 y = 9.1008 / 45.5175 / 81.9342（小数部分各不相同），浏览器与 D2D 的 snap 策略不同 → 同一行可能一个落在 11 一个落在 12。**吸附整像素反而更差**（实测平均差 4.00 → 4.71，第 2 行也被带偏）。plan §5.2 说的「光学补偿全部重标定」指的就是这个量级。
5. **`WS_EX_NOREDIRECTIONBITMAP` + 合成交换链 = 真逐像素 alpha**，`--debug-alpha` 画 50% 红块压白底得 (224,59,71)（= 0.5·红 + 0.5·底），不是纯红；圆角外是真透明。
6. **桌面层不需要 `SetParent(WorkerW)`。** 主程序现方案（插到 `Progman/WorkerW` 锚点正上方）对 DComp 完全无副作用 —— plan §4.3 里「DComp + SetParent 会不会不出图」这个致命风险**可以直接划掉**。
7. **内存要分三个口径报。** `PrivateUsage`（提交）= 54.9 MB，其中 D3D11 设备 + NVIDIA UMD 一次就吃掉 53 MB；任务管理器的「内存」列（私有工作集）只有 **11.3 MB**。plan §4.1 的「30 MB 以内」按工作集口径是成立的，按提交口径永远不成立。
8. **启动成本在 GPU 栈，不在我们的代码**：`D3D11CreateDevice` + D2D/DComp 143 ms，首帧 Present 150 ms（首次管线/着色器编译）。想压到 150 ms 需要「设备提前建 + 首帧预编译」或干脆接受 ~300 ms（主程序现在起一屏 WebView2 是秒级）。

---

## 与最终实现的差距（M0 不做的事）

- 只做了 1 种卡（Performance），9 种卡的视图函数还没有（对应 plan 的 M2）。
- 布局是这张卡硬编码的几何，没有 `minidraw` 场景树 / flex 子集（M1）。
- 壁纸要从「外部先解码成 raw BGRA」改成进程内解码（WIC 或 image crate）；字体要从 7.7 MB 全量 TTF 换成子集（DWrite 不能直接吃 woff2 —— 现有 `web/fonts/*.woff2` 得先用 fontTools 导出 ttf 再子集）。
- 文字阴影只做了偏移（`0 .833px`），`1.667px` 那层模糊省了 —— 视觉上几乎看不出，但要做就得走「渲染到离屏位图 + 高斯」。
- 没有动效帧调度（现在是 1 s 定时器 + 数据变化才重绘）。
