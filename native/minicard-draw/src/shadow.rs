//! B3 形状阴影：CSS `box-shadow` 的离屏实现（不依赖 D2D effect，也不做 9 宫格近似）。
//!
//! 口径与 CSS 一致：
//! - `blur` 是**模糊半径**，高斯 σ = blur / 2（CSS 规范里阴影边缘在 2×blur 内完成过渡）。
//! - `spread` 先把形状外扩（可为负），再模糊，最后按 (dx, dy) 平移。
//! - 覆盖率用有符号距离场算（1 像素抗锯齿带），模糊用三次盒模糊近似高斯
//!   （三次盒的方差 = 3·((2r+1)²−1)/12，反解 r = (√(4σ²+1) − 1)/2）。
//!
//! 为什么不用 `CLSID_D2D1Shadow` 效果：效果链要走 COM 效果图与额外的中间纹理，
//! 对「一块静态圆角矩形阴影」是杀鸡用牛刀；这里的 SDF+盒模糊是纯 Rust，
//! 能在单测里断言形状（中心、边缘、衰减），也能在确认参数时不必开窗口。

/// 一个阴影的几何描述（单位 = DIP）。`w/h` 是被投阴影的形状尺寸。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spec {
    pub w: f32,
    pub h: f32,
    pub radius: f32,
    pub spread: f32,
    pub blur: f32,
    pub dx: f32,
    pub dy: f32,
}

impl Spec {
    pub fn sigma(&self) -> f32 {
        (self.blur / 2.0).max(0.0)
    }

    /// 掩码画布的四周边距（保证 3σ 与位移都在画布里）。
    pub fn pad(&self) -> f32 {
        let s = self.sigma();
        (3.0 * s).ceil() + self.dx.abs().max(self.dy.abs()) + self.spread.abs().max(0.0) + 2.0
    }
}

/// 圆角矩形的有符号距离（负 = 内部，单位 DIP）。
pub fn sd_round_rect(px: f32, py: f32, cx: f32, cy: f32, hw: f32, hh: f32, r: f32) -> f32 {
    let r = r.min(hw).min(hh).max(0.0);
    let qx = (px - cx).abs() - (hw - r);
    let qy = (py - cy).abs() - (hh - r);
    let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
    let inside = qx.max(qy).min(0.0);
    outside + inside - r
}

/// 三次盒模糊等效高斯的盒半径（`σ` → `r`）。
pub fn gaussian_r(sigma: f32) -> i32 {
    if sigma <= 0.0 {
        return 0;
    }
    (((4.0 * sigma * sigma + 1.0).sqrt() - 1.0) / 2.0).round().max(1.0) as i32
}

/// 阴影掩码：单通道覆盖率 + 画布相对形状左上角的偏移。
#[derive(Clone, Debug)]
pub struct Mask {
    pub w: u32,
    pub h: u32,
    /// 画布左上角相对「形状左上角」的位移（负 = 影子铺到形状外）
    pub ox: f32,
    pub oy: f32,
    pub alpha: Vec<u8>,
}

pub fn mask(spec: &Spec) -> Mask {
    let pad = spec.pad();
    let w = (spec.w + 2.0 * pad).ceil().max(1.0) as u32;
    let h = (spec.h + 2.0 * pad).ceil().max(1.0) as u32;
    let sigma = spec.sigma();
    let r = gaussian_r(sigma);

    // ① 覆盖率：形状外扩 spread 后居中到画布
    let cx = pad + spec.w / 2.0 + spec.dx;
    let cy = pad + spec.h / 2.0 + spec.dy;
    let hw = (spec.w / 2.0 + spec.spread).max(0.01);
    let hh = (spec.h / 2.0 + spec.spread).max(0.01);
    let rad = (spec.radius + spec.spread).max(0.0);
    let mut alpha = vec![0u8; (w * h) as usize];
    for y in 0..h {
        for x in 0..w {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            let d = sd_round_rect(px, py, cx, cy, hw, hh, rad);
            // 1px 抗锯齿带：d ≤ -0.5 实心，d ≥ 0.5 全空
            let cov = (0.5 - d).clamp(0.0, 1.0);
            alpha[(y * w + x) as usize] = (cov * 255.0).round() as u8;
        }
    }

    // ② 三次盒模糊（水平/垂直分离，滑动窗口 O(1)/像素）
    if r > 0 {
        let mut tmp = vec![0u8; alpha.len()];
        for _ in 0..3 {
            box_h(&alpha, &mut tmp, w, h, r);
            box_v(&tmp, &mut alpha, w, h, r);
        }
    }

    Mask { w, h, ox: -pad, oy: -pad, alpha }
}

fn box_h(src: &[u8], dst: &mut [u8], w: u32, h: u32, r: i32) {
    let (w, h, r) = (w as i32, h as i32, r.max(0));
    let win = (2 * r + 1) as u32;
    for y in 0..h {
        let row = (y * w) as usize;
        let mut sum: u32 = 0;
        // 初始窗口（左侧越界按边缘像素延拓）
        for i in -r..=r {
            let x = i.clamp(0, w - 1);
            sum += src[row + x as usize] as u32;
        }
        for x in 0..w {
            dst[row + x as usize] = (sum / win) as u8;
            let out = (x - r).clamp(0, w - 1);
            let inn = (x + r + 1).clamp(0, w - 1);
            sum = sum + src[row + inn as usize] as u32 - src[row + out as usize] as u32;
        }
    }
}

fn box_v(src: &[u8], dst: &mut [u8], w: u32, h: u32, r: i32) {
    let (w, h, r) = (w as i32, h as i32, r.max(0));
    let win = (2 * r + 1) as u32;
    for x in 0..w {
        let mut sum: u32 = 0;
        for i in -r..=r {
            let y = i.clamp(0, h - 1);
            sum += src[(y * w + x) as usize] as u32;
        }
        for y in 0..h {
            dst[(y * w + x) as usize] = (sum / win) as u8;
            let out = (y - r).clamp(0, h - 1);
            let inn = (y + r + 1).clamp(0, h - 1);
            sum = sum + src[(inn * w + x) as usize] as u32 - src[(out * w + x) as usize] as u32;
        }
    }
}

impl Mask {
    /// 取某点（形状坐标系，原点 = 形状左上角）的覆盖率 0..1。
    pub fn at(&self, x: f32, y: f32) -> f32 {
        let px = x - self.ox;
        let py = y - self.oy;
        if px < 0.0 || py < 0.0 || px >= self.w as f32 || py >= self.h as f32 {
            return 0.0;
        }
        self.alpha[(py as u32 * self.w + px as u32) as usize] as f32 / 255.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(blur: f32) -> Spec {
        Spec { w: 100.0, h: 60.0, radius: 12.0, spread: 0.0, blur, dx: 0.0, dy: 0.0 }
    }

    #[test]
    fn 距离场正负与量值() {
        // 中心：负且约等于 -半高
        let d = sd_round_rect(50.0, 30.0, 50.0, 30.0, 50.0, 30.0, 12.0);
        assert!((d + 30.0).abs() < 0.01, "中心距离 {d}");
        // 右边中点外侧 5px：正好 +5
        let d = sd_round_rect(105.0, 30.0, 50.0, 30.0, 50.0, 30.0, 12.0);
        assert!((d - 5.0).abs() < 0.01, "右侧外侧距离 {d}");
        // (95,55) 在圆角弧内侧（离弧心 (88,48) 只有 9.9 < r=12）→ 内部，负值
        let d = sd_round_rect(95.0, 55.0, 50.0, 30.0, 50.0, 30.0, 12.0);
        assert!(d < 0.0, "圆角弧内侧应为负，实得 {d}");
        // 角上外 45°（离弧心 12.73）→ 外部 +0.73
        let d = sd_round_rect(97.0, 57.0, 50.0, 30.0, 50.0, 30.0, 12.0);
        assert!(d > 0.0 && d < 2.0, "角上外侧距离 {d}");
        // 圆角内的那个角点必须是外部（直角矩形会算成内部）
        let d = sd_round_rect(99.0, 59.0, 50.0, 30.0, 50.0, 30.0, 12.0);
        assert!(d > 0.0, "圆角外应是正的，实得 {d}");
    }

    #[test]
    fn 无模糊时是硬边且中心满覆盖() {
        let m = mask(&spec(0.0));
        assert!((m.at(50.0, 30.0) - 1.0).abs() < 0.02, "中心 {}", m.at(50.0, 30.0));
        assert!(m.at(-4.0, 30.0) < 0.02, "左外侧 {}", m.at(-4.0, 30.0));
        assert!(m.at(104.0, 30.0) < 0.02, "右外侧 {}", m.at(104.0, 30.0));
        assert!((m.at(2.0, 30.0) - 1.0).abs() < 0.06, "左内侧 {}", m.at(2.0, 30.0));
    }

    #[test]
    fn 模糊后边缘单调衰减且画布边角归零() {
        let m = mask(&spec(8.0)); // σ = 4
        // 从中心往右扫：覆盖率不该反弹
        let mut prev = 1.1f32;
        for x in 40..120 {
            let a = m.at(x as f32, 30.0);
            assert!(a <= prev + 0.02, "x={x} 出现反弹：{a} > {prev}");
            prev = a;
        }
        // 画布四角必须为 0（否则说明 3σ 边距不够、影子被裁）。
        // 注意 `at()` 收的是**形状坐标**，画布角要换算回 (ox, oy)…
        let (x0, y0) = (m.ox, m.oy);
        let (x1, y1) = (m.ox + m.w as f32 - 1.0, m.oy + m.h as f32 - 1.0);
        for (x, y) in [(x0, y0), (x1, y0), (x0, y1), (x1, y1)] {
            assert!(m.at(x, y) < 0.02, "画布角 ({x},{y}) 覆盖率 {}", m.at(x, y));
        }
        // 反过来：形状自己的角（形状坐标 0,0）在模糊后**应当**有少量覆盖
        //（离最近的实心像素约 5px，σ=4 时约一成），为 0 说明模糊根本没跑
        let corner = m.at(0.0, 0.0);
        assert!(corner > 0.02 && corner < 0.5, "形状角覆盖率 {corner} 不合常理");
    }

    #[test]
    fn 位移把影子整体推过去() {
        let mut s = spec(0.0);
        s.dy = 6.0;
        let m = mask(&s);
        // 形状正下方 4px（原来实心区外）现在应被影子覆盖
        assert!(m.at(50.0, 62.0) > 0.9, "下方 {}", m.at(50.0, 62.0));
        // 形状上方 4px 应已空
        assert!(m.at(50.0, -4.0) < 0.02, "上方 {}", m.at(50.0, -4.0));
    }

    #[test]
    fn spread_外扩生效() {
        let mut s = spec(0.0);
        s.spread = 6.0;
        let m = mask(&s);
        assert!(m.at(104.0, 30.0) > 0.9, "spread 后右侧 {}", m.at(104.0, 30.0));
        assert!(m.at(110.0, 30.0) < 0.05, "spread 外沿 {}", m.at(110.0, 30.0));
    }

    #[test]
    fn 盒半径与高斯的换算() {
        // σ=4 → r=(√65−1)/2≈3.53 → 4
        assert_eq!(gaussian_r(4.0), 4);
        // σ=9 → r=(√325−1)/2≈8.51 → 9
        assert_eq!(gaussian_r(9.0), 9);
        assert_eq!(gaussian_r(0.0), 0);
        assert!(gaussian_r(0.2) >= 1, "极小的 σ 也至少 1");
    }

    #[test]
    fn 画布边距覆盖三倍西格玛() {
        let s = spec(18.0); // σ = 9
        let m = mask(&s);
        assert!(m.ox <= -(3.0 * 9.0), "左边距 {} 不足 3σ", m.ox);
        assert!(m.oy <= -(3.0 * 9.0), "上边距 {} 不足 3σ", m.oy);
        assert!(m.w as f32 >= s.w + 2.0 * 27.0, "画布宽 {} 不足", m.w);
    }
}
