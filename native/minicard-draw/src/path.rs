//! B3 路径几何：把九张卡要用的形状描述成**纯 Rust** 的 `Path`，与 D2D 无关。
//!
//! 分开的理由：几何错（弧端点、勾的进度、箭头方向）是「算错了」，D2D 错是「画错了」，
//! 两者混在一起时只能靠肉眼看图分辨。这里全部可单测，`paint` 只负责翻译成 D2D。
//!
//! 角度口径：**度**，0° 指向 +X（3 点方向），顺时针为正（屏幕坐标 y 向下，与 CSS/SVG 一致）。

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Cmd {
    Move(f32, f32),
    Line(f32, f32),
    /// 椭圆弧：`a0` 起点角、`sweep` 扫过角（正 = 顺时针）
    Arc { cx: f32, cy: f32, rx: f32, ry: f32, a0: f32, sweep: f32 },
    Quad { cx: f32, cy: f32, x: f32, y: f32 },
    Cubic { c1x: f32, c1y: f32, c2x: f32, c2y: f32, x: f32, y: f32 },
    Close,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Path {
    pub cmds: Vec<Cmd>,
}

impl Path {
    pub fn new() -> Self {
        Self { cmds: Vec::new() }
    }

    pub fn move_to(mut self, x: f32, y: f32) -> Self {
        self.cmds.push(Cmd::Move(x, y));
        self
    }
    pub fn line_to(mut self, x: f32, y: f32) -> Self {
        self.cmds.push(Cmd::Line(x, y));
        self
    }
    pub fn quad(mut self, cx: f32, cy: f32, x: f32, y: f32) -> Self {
        self.cmds.push(Cmd::Quad { cx, cy, x, y });
        self
    }
    pub fn cubic(mut self, c1: (f32, f32), c2: (f32, f32), p: (f32, f32)) -> Self {
        self.cmds.push(Cmd::Cubic { c1x: c1.0, c1y: c1.1, c2x: c2.0, c2y: c2.1, x: p.0, y: p.1 });
        self
    }
    pub fn arc(mut self, cx: f32, cy: f32, r: f32, a0: f32, sweep: f32) -> Self {
        self.cmds.push(Cmd::Arc { cx, cy, rx: r, ry: r, a0, sweep });
        self
    }
    pub fn close(mut self) -> Self {
        self.cmds.push(Cmd::Close);
        self
    }

    /// 折线（首点 Move 其余 Line）
    pub fn polyline(pts: &[(f32, f32)]) -> Self {
        let mut p = Self::new();
        for (i, (x, y)) in pts.iter().enumerate() {
            if i == 0 {
                p = p.move_to(*x, *y);
            } else {
                p = p.line_to(*x, *y);
            }
        }
        p
    }

    /// 圆角矩形（四角各 90° 弧，`r = min(r, 短边/2)`）
    pub fn rounded_rect(x: f32, y: f32, w: f32, h: f32, r: f32) -> Self {
        let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
        if r <= 0.0 {
            return Self::polyline(&[(x, y), (x + w, y), (x + w, y + h), (x, y + h)]).close();
        }
        let (x1, y1) = (x + w, y + h);
        Self::new()
            .move_to(x + r, y)
            .line_to(x1 - r, y)
            .arc(x1 - r, y + r, r, -90.0, 90.0)
            .line_to(x1, y1 - r)
            .arc(x1 - r, y1 - r, r, 0.0, 90.0)
            .line_to(x + r, y1)
            .arc(x + r, y1 - r, r, 90.0, 90.0)
            .line_to(x, y + r)
            .arc(x + r, y + r, r, 180.0, 90.0)
            .close()
    }

    /// 圆（用两条半弧拼，跨 360° 时 D2D 的 AddArc 不好处理整圈）
    pub fn circle(cx: f32, cy: f32, r: f32) -> Self {
        Self::new().arc(cx, cy, r, -90.0, 180.0).arc(cx, cy, r, 90.0, 180.0).close()
    }

    /// SVG `path d` 的最小解析（M/m L/l H/h V/v A/a C/c Z/z）。
    ///
    /// 只为天气图标那几条路径服务：`A` 走标准的「端点 → 圆心」换算（SVG 规范的
    /// implementation notes F.6.5，含半径不足时的等比放大）；`Z` 闭合并把笔位移回子路径起点。
    /// 不认识的指令直接停下（宁可少画，不要画错）。
    pub fn svg(d: &str) -> Self {
        let mut p = Self::new();
        // 词法：数字之间靠空格/逗号/负号分隔 —— 空格必须**把当前数字收尾**，
        // 否则「M2 3」会被读成 23（这就是第一版解析出空路径的原因）。
        let mut cmds: Vec<(char, Vec<f32>)> = Vec::new();
        let mut nums: Vec<f32> = Vec::new();
        let mut cur: Option<char> = None;
        let mut buf = String::new();
        let take = |buf: &mut String, nums: &mut Vec<f32>| {
            if !buf.is_empty() {
                if let Ok(v) = buf.parse::<f32>() {
                    nums.push(v);
                }
                buf.clear();
            }
        };
        for c in d.chars() {
            if c.is_ascii_alphabetic() {
                take(&mut buf, &mut nums);
                if let Some(pc) = cur.take() {
                    if !nums.is_empty() {
                        cmds.push((pc, std::mem::take(&mut nums)));
                    }
                }
                cur = Some(c);
            } else if c.is_ascii_digit() || c == '.' || c == 'e' || c == 'E' {
                buf.push(c);
            } else if c == '-' || c == '+' {
                // 正负号一定是新数字的开头（指数里的符号已在上面的分支里并入 buf）
                take(&mut buf, &mut nums);
                buf.push(c);
            } else {
                // 空格 / 逗号 / 其他分隔符
                take(&mut buf, &mut nums);
            }
        }
        take(&mut buf, &mut nums);
        if let Some(pc) = cur {
            if !nums.is_empty() {
                cmds.push((pc, nums));
            }
        }

        let mut cx = 0.0f32;
        let mut cy = 0.0f32;
        let mut sx = 0.0f32;
        let mut sy = 0.0f32;
        for (c, a) in cmds {
            let rel = c.is_ascii_lowercase();
            match c.to_ascii_uppercase() {
                'M' => {
                    let mut i = 0;
                    while i + 1 < a.len() {
                        let (x, y) = if rel { (cx + a[i], cy + a[i + 1]) } else { (a[i], a[i + 1]) };
                        if i == 0 {
                            p = p.move_to(x, y);
                            sx = x;
                            sy = y;
                        } else {
                            p = p.line_to(x, y);
                        }
                        cx = x;
                        cy = y;
                        i += 2;
                    }
                }
                'L' => {
                    let mut i = 0;
                    while i + 1 < a.len() {
                        let (x, y) = if rel { (cx + a[i], cy + a[i + 1]) } else { (a[i], a[i + 1]) };
                        p = p.line_to(x, y);
                        cx = x;
                        cy = y;
                        i += 2;
                    }
                }
                'H' => {
                    for v in a {
                        let x = if rel { cx + v } else { v };
                        p = p.line_to(x, cy);
                        cx = x;
                    }
                }
                'V' => {
                    for v in a {
                        let y = if rel { cy + v } else { v };
                        p = p.line_to(cx, y);
                        cy = y;
                    }
                }
                'Q' => {
                    let mut i = 0;
                    while i + 3 < a.len() {
                        let (mut qx, mut qy, mut x, mut y) = (a[i], a[i + 1], a[i + 2], a[i + 3]);
                        if rel {
                            qx += cx;
                            qy += cy;
                            x += cx;
                            y += cy;
                        }
                        p = p.quad(qx, qy, x, y);
                        cx = x;
                        cy = y;
                        i += 4;
                    }
                }
                'A' => {
                    let mut i = 0;
                    while i + 6 < a.len() {
                        let (rx, ry, rot, large, sweep) = (a[i], a[i + 1], a[i + 2], a[i + 3], a[i + 4]);
                        let (mut x, mut y) = (a[i + 5], a[i + 6]);
                        if rel {
                            x += cx;
                            y += cy;
                        }
                        p = p.svg_arc_to(cx, cy, rx, ry, rot, large != 0.0, sweep != 0.0, x, y);
                        cx = x;
                        cy = y;
                        i += 7;
                    }
                }
                'Z' => {
                    p = p.close();
                    cx = sx;
                    cy = sy;
                }
                _ => break,
            }
        }
        p
    }

    /// SVG 的端点式圆弧 → 圆心式（规范 F.6.5）。
    #[allow(clippy::too_many_arguments)]
    fn svg_arc_to(
        self,
        x1: f32,
        y1: f32,
        rx: f32,
        ry: f32,
        rot_deg: f32,
        large: bool,
        sweep: bool,
        x2: f32,
        y2: f32,
    ) -> Self {
        if rx == 0.0 || ry == 0.0 {
            return self.line_to(x2, y2);
        }
        let (mut rx, mut ry) = (rx.abs(), ry.abs());
        let phi = rot_deg.to_radians();
        let (sinp, cosp) = phi.sin_cos();
        let dx2 = (x1 - x2) / 2.0;
        let dy2 = (y1 - y2) / 2.0;
        let x1p = cosp * dx2 + sinp * dy2;
        let y1p = -sinp * dx2 + cosp * dy2;
        // 半径不足时等比放大
        let lambda = (x1p * x1p) / (rx * rx) + (y1p * y1p) / (ry * ry);
        if lambda > 1.0 {
            let s = lambda.sqrt();
            rx *= s;
            ry *= s;
        }
        let num = rx * rx * ry * ry - rx * rx * y1p * y1p - ry * ry * x1p * x1p;
        let den = rx * rx * y1p * y1p + ry * ry * x1p * x1p;
        let mut coef = if den > 0.0 { (num / den).max(0.0).sqrt() } else { 0.0 };
        if large == sweep {
            coef = -coef;
        }
        let cxp = coef * rx * y1p / ry;
        let cyp = -coef * ry * x1p / rx;
        let cx = cosp * cxp - sinp * cyp + (x1 + x2) / 2.0;
        let cy = sinp * cxp + cosp * cyp + (y1 + y2) / 2.0;
        let ang = |ux: f32, uy: f32, vx: f32, vy: f32| {
            let dot = ux * vx + uy * vy;
            let len = ((ux * ux + uy * uy) * (vx * vx + vy * vy)).sqrt();
            let mut a = (dot / len).clamp(-1.0, 1.0).acos();
            if ux * vy - uy * vx < 0.0 {
                a = -a;
            }
            a
        };
        let ux = (x1p - cxp) / rx;
        let uy = (y1p - cyp) / ry;
        let vx = (-x1p - cxp) / rx;
        let vy = (-y1p - cyp) / ry;
        let a0 = ang(1.0, 0.0, ux, uy);
        let mut da = ang(ux, uy, vx, vy);
        if !sweep && da > 0.0 {
            da -= std::f32::consts::TAU;
        } else if sweep && da < 0.0 {
            da += std::f32::consts::TAU;
        }
        self.arc(cx, cy, rx, a0.to_degrees(), da.to_degrees())
    }

    /// 圆环弧线（番茄钟进度环）：从 12 点起顺时针 `frac` 圈，`frac ∈ [0,1]`
    pub fn ring_arc(cx: f32, cy: f32, r: f32, frac: f32) -> Self {
        let sweep = 360.0 * frac.clamp(0.0, 1.0);
        if sweep <= 0.0 {
            return Self::new();
        }
        // 12 点方向 = -90°
        let p = Self::new().arc(cx, cy, r, -90.0, sweep);
        if sweep >= 360.0 {
            p.close()
        } else {
            p
        }
    }

    /// 待办勾：SVG 原文 `M5.1 12.3 l4.7 4.7 L19.1 6.9`（24 网格）按 `progress` 截断。
    /// 等价于 CSS 的 `stroke-dasharray: L; stroke-dashoffset: L(1-progress)`。
    pub fn check_mark(x: f32, y: f32, scale: f32, progress: f32) -> Self {
        let pts = [(5.1, 12.3), (9.8, 17.0), (19.1, 6.9)];
        let base = Self::polyline(&pts);
        let scaled = base.scale(x, y, scale);
        scaled.truncate(progress)
    }

    /// 线性缩放 + 平移（`scale = 1` 时即平移）。
    pub fn scale(&self, dx: f32, dy: f32, s: f32) -> Self {
        let m = |x: f32, y: f32| (dx + x * s, dy + y * s);
        let mut out = Self::new();
        for c in &self.cmds {
            out.cmds.push(match *c {
                Cmd::Move(x, y) => Cmd::Move(m(x, y).0, m(x, y).1),
                Cmd::Line(x, y) => Cmd::Line(m(x, y).0, m(x, y).1),
                Cmd::Arc { cx, cy, rx, ry, a0, sweep } => Cmd::Arc {
                    cx: m(cx, cy).0,
                    cy: m(cx, cy).1,
                    rx: rx * s,
                    ry: ry * s,
                    a0,
                    sweep,
                },
                Cmd::Quad { cx, cy, x, y } => Cmd::Quad {
                    cx: m(cx, cy).0,
                    cy: m(cx, cy).1,
                    x: m(x, y).0,
                    y: m(x, y).1,
                },
                Cmd::Cubic { c1x, c1y, c2x, c2y, x, y } => Cmd::Cubic {
                    c1x: m(c1x, c1y).0,
                    c1y: m(c1x, c1y).1,
                    c2x: m(c2x, c2y).0,
                    c2y: m(c2x, c2y).1,
                    x: m(x, y).0,
                    y: m(x, y).1,
                },
                Cmd::Close => Cmd::Close,
            });
        }
        out
    }

    /// 弧长（折线 + 真弧长；圆角矩形/勾都能算）。`Close` 那段也算。
    pub fn length(&self) -> f32 {
        let mut total = 0.0;
        let mut cur: Option<(f32, f32)> = None;
        let mut start: Option<(f32, f32)> = None;
        for c in &self.cmds {
            match *c {
                Cmd::Move(x, y) => {
                    cur = Some((x, y));
                    start = Some((x, y));
                }
                Cmd::Line(x, y) => {
                    if let Some((px, py)) = cur {
                        total += dist(px, py, x, y);
                    }
                    cur = Some((x, y));
                }
                Cmd::Arc { cx, cy, rx, ry, a0, sweep } => {
                    // 椭圆弧长用 Ramanujan 近似（rx≈ry 时误差 < 0.01%）
                    let a = rx.abs().max(ry.abs());
                    let b = rx.abs().min(ry.abs());
                    let perim = std::f32::consts::PI
                        * (3.0 * (a + b) - ((3.0 * a + b) * (a + 3.0 * b)).sqrt());
                    total += perim * (sweep.abs() / 360.0);
                    // 弧之后跟着的 Line 从弧**终点**起算，别用弧前的位置
                    cur = Some(Path::arc_end(cx, cy, rx, ry, a0, sweep));
                }
                Cmd::Quad { cx, cy, x, y } => {
                    if let Some((px, py)) = cur {
                        total += dist(px, py, cx, cy) + dist(cx, cy, x, y);
                    }
                    cur = Some((x, y));
                }
                Cmd::Cubic { c1x, c1y, c2x, c2y, x, y } => {
                    if let Some((px, py)) = cur {
                        total += dist(px, py, c1x, c1y)
                            + dist(c1x, c1y, c2x, c2y)
                            + dist(c2x, c2y, x, y);
                    }
                    cur = Some((x, y));
                }
                Cmd::Close => {
                    if let (Some((px, py)), Some((sx, sy))) = (cur, start) {
                        total += dist(px, py, sx, sy);
                    }
                    cur = start;
                }
            }
        }
        total
    }

    /// 按弧长比例截断（`p = 0.6` 只留前 60%）。用于「勾按进度画出来」这类 dash 动效。
    pub fn truncate(&self, p: f32) -> Self {
        let p = p.clamp(0.0, 1.0);
        if p >= 1.0 {
            return self.clone();
        }
        if p <= 0.0 {
            return Self::new();
        }
        let target = self.length() * p;
        let mut walked = 0.0f32;
        let mut out = Self::new();
        let mut cur: Option<(f32, f32)> = None;
        let mut start: Option<(f32, f32)> = None;
        for c in &self.cmds {
            match *c {
                Cmd::Move(x, y) => {
                    out = out.move_to(x, y);
                    cur = Some((x, y));
                    start = Some((x, y));
                }
                Cmd::Line(x, y) => {
                    let (px, py) = match cur {
                        Some(v) => v,
                        None => continue,
                    };
                    let d = dist(px, py, x, y);
                    if walked + d >= target {
                        let t = (target - walked) / d;
                        out = out.line_to(px + (x - px) * t, py + (y - py) * t);
                        return out;
                    }
                    walked += d;
                    out = out.line_to(x, y);
                    cur = Some((x, y));
                }
                Cmd::Arc { cx, cy, rx, ry, a0, sweep } => {
                    let perim_frac = {
                        let a = rx.abs().max(ry.abs());
                        let b = rx.abs().min(ry.abs());
                        std::f32::consts::PI
                            * (3.0 * (a + b) - ((3.0 * a + b) * (a + 3.0 * b)).sqrt())
                            * (sweep.abs() / 360.0)
                    };
                    if walked + perim_frac >= target {
                        let sweep2 = sweep * (target - walked) / perim_frac;
                        out = out.arc(cx, cy, rx.max(ry), a0, sweep2);
                        return out;
                    }
                    walked += perim_frac;
                    out = out.arc(cx, cy, rx.max(ry), a0, sweep);
                    cur = Some(Path::arc_end(cx, cy, rx, ry, a0, sweep));
                }
                Cmd::Quad { cx, cy, x, y } => {
                    out = out.quad(cx, cy, x, y);
                    walked += cur.map(|(px, py)| dist(px, py, cx, cy) + dist(cx, cy, x, y)).unwrap_or(0.0);
                    cur = Some((x, y));
                }
                Cmd::Cubic { c1x, c1y, c2x, c2y, x, y } => {
                    out = out.cubic((c1x, c1y), (c2x, c2y), (x, y));
                    walked += cur
                        .map(|(px, py)| dist(px, py, c1x, c1y) + dist(c1x, c1y, c2x, c2y) + dist(c2x, c2y, x, y))
                        .unwrap_or(0.0);
                    cur = Some((x, y));
                }
                Cmd::Close => {
                    if let (Some((px, py)), Some((sx, sy))) = (cur, start) {
                        let d = dist(px, py, sx, sy);
                        if walked + d >= target {
                            let t = (target - walked) / d;
                            out = out.line_to(px + (sx - px) * t, py + (sy - py) * t);
                            return out;
                        }
                        walked += d;
                        out = out.close();
                    }
                    cur = start;
                }
            }
        }
        out
    }

    /// 弧的起点（D2D 的 ARC_SEGMENT 只给终点，起点得自己 Move 过去）。
    pub fn arc_start(cx: f32, cy: f32, rx: f32, ry: f32, a0: f32) -> (f32, f32) {
        let r = a0.to_radians();
        (cx + rx * r.cos(), cy + ry * r.sin())
    }

    /// 弧的终点
    pub fn arc_end(cx: f32, cy: f32, rx: f32, ry: f32, a0: f32, sweep: f32) -> (f32, f32) {
        let r = (a0 + sweep).to_radians();
        (cx + rx * r.cos(), cy + ry * r.sin())
    }
}

fn dist(x0: f32, y0: f32, x1: f32, y1: f32) -> f32 {
    ((x1 - x0).powi(2) + (y1 - y0).powi(2)).sqrt()
}

/// 截图里判断「勾画到哪」用的：路径上弧长 `p` 处的点。
pub fn point_at_length(path: &Path, len: f32) -> (f32, f32) {
    let t = if path.length() > 0.0 { len / path.length() } else { 0.0 };
    let sub = path.truncate(t);
    let mut last = (0.0, 0.0);
    for c in &sub.cmds {
        match *c {
            Cmd::Move(x, y) | Cmd::Line(x, y) => last = (x, y),
            Cmd::Arc { cx, cy, rx, ry, a0, sweep } => {
                last = Path::arc_end(cx, cy, rx, ry, a0, sweep);
            }
            Cmd::Quad { x, y, .. } | Cmd::Cubic { x, y, .. } => last = (x, y),
            Cmd::Close => {}
        }
    }
    last
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.05
    }

    #[test]
    fn 圆的周长与弧长() {
        let r = 24.0;
        // 整圈：2πr = 150.796
        let full = Path::new().arc(0.0, 0.0, r, -90.0, 360.0);
        assert!(close(full.length(), std::f32::consts::TAU * r), "整圈长 {}", full.length());
        // 3/4 圈
        let q = Path::new().arc(0.0, 0.0, r, -90.0, 270.0);
        assert!(close(q.length(), std::f32::consts::TAU * r * 0.75), "3/4 圈 {}", q.length());
    }

    #[test]
    fn 弧端点角度口径() {
        // 从 12 点（-90°）扫 90° → 终点在 3 点
        let (x, y) = Path::arc_end(0.0, 0.0, 10.0, 10.0, -90.0, 90.0);
        assert!(close(x, 10.0) && close(y, 0.0), "终点 {:?}", (x, y));
        // 起点在 12 点
        let (x, y) = Path::arc_start(0.0, 0.0, 10.0, 10.0, -90.0);
        assert!(close(x, 0.0) && close(y, -10.0), "起点 {:?}", (x, y));
    }

    #[test]
    fn 圆角矩形周长与闭合() {
        let p = Path::rounded_rect(0.0, 0.0, 100.0, 40.0, 10.0);
        // 周长 = 2(w+h) - 8r + 2πr
        let want = 2.0 * (100.0 + 40.0) - 8.0 * 10.0 + std::f32::consts::TAU * 10.0;
        assert!(close(p.length(), want), "周长 {} vs {}", p.length(), want);
        assert_eq!(p.cmds.iter().filter(|c| **c == Cmd::Close).count(), 1);
    }

    #[test]
    fn 半径被短边夹住() {
        // 高 20、r 给 50 → 实际 r = 10
        let p = Path::rounded_rect(0.0, 0.0, 100.0, 20.0, 50.0);
        let want = 2.0 * (100.0 + 20.0) - 8.0 * 10.0 + std::f32::consts::TAU * 10.0;
        assert!(close(p.length(), want), "周长 {}", p.length());
    }

    #[test]
    fn 勾的弧长与截断() {
        // SVG 路径长：|(5.1,12.3)→(9.8,17.0)| + |(9.8,17.0)→(19.1,6.9)|
        let d1 = ((4.7f32).powi(2) + (4.7f32).powi(2)).sqrt();
        let d2 = ((9.3f32).powi(2) + (-10.1f32).powi(2)).sqrt();
        let p = Path::check_mark(0.0, 0.0, 1.0, 1.0);
        assert!(close(p.length(), d1 + d2), "勾长 {} vs {}", p.length(), d1 + d2);

        // 画到 60%：终点应落在第二段上，且剩余长度 = 全长的 40%
        let part = Path::check_mark(0.0, 0.0, 1.0, 0.6);
        assert!(close(part.length(), (d1 + d2) * 0.6), "60% 长 {}", part.length());
        // 0% 什么也没有；100% 与原路径一致
        assert!(Path::check_mark(0.0, 0.0, 1.0, 0.0).cmds.is_empty());
        assert_eq!(Path::check_mark(0.0, 0.0, 1.0, 1.0).length(), p.length());
    }

    #[test]
    fn 缩放同时改坐标与半径() {
        let p = Path::new().arc(10.0, 10.0, 4.0, 0.0, 90.0).scale(5.0, 6.0, 2.0);
        match p.cmds[0] {
            Cmd::Arc { cx, cy, rx, a0, .. } => {
                assert!(close(cx, 25.0) && close(cy, 26.0), "中心 {:?}", (cx, cy));
                assert!(close(rx, 8.0), "半径 {}", rx);
                assert!(close(a0, 0.0), "角度不该被缩放");
            }
            _ => panic!("应为弧"),
        }
    }

    #[test]
    fn 截断按弧长而不是按点数() {
        // 三边不等长的折线：(0,0)→(10,0)→(10,30)
        let p = Path::polyline(&[(0.0, 0.0), (10.0, 0.0), (10.0, 30.0)]);
        assert!(close(p.length(), 40.0), "总长 {}", p.length());
        // 25% = 10 → 正好在第一段末端
        let q = p.truncate(0.25);
        let (x, y) = point_at_length(&p, 10.0);
        assert!(close(x, 10.0) && close(y, 0.0), "25% 处 {:?}", (x, y));
        assert!(close(q.length(), 10.0), "截断长 {}", q.length());
        // 50% = 20 → 第二段中间
        let (x, y) = point_at_length(&p, 20.0);
        assert!(close(x, 10.0) && close(y, 10.0), "50% 处 {:?}", (x, y));
    }

    #[test]
    fn 环弧从十二点起顺时针() {
        let p = Path::ring_arc(50.0, 50.0, 20.0, 0.25);
        // 25% 圈 = 90°：终点在 3 点
        let (x, y) = point_at_length(&p, p.length());
        assert!(close(x, 70.0) && close(y, 50.0), "25% 环终点 {:?}", (x, y));
        // 100% 时闭合
        let full = Path::ring_arc(50.0, 50.0, 20.0, 1.0);
        assert_eq!(full.cmds.iter().filter(|c| **c == Cmd::Close).count(), 1);
    }
}

#[cfg(test)]
mod svg_tests {
    use super::*;

    fn end(p: &Path) -> (f32, f32) {
        p.cmds.last().map(|c| match *c {
            Cmd::Line(x, y) | Cmd::Move(x, y) => (x, y),
            Cmd::Arc { cx, cy, rx, ry, a0, sweep } => Path::arc_end(cx, cy, rx, ry, a0, sweep),
            Cmd::Quad { x, y, .. } | Cmd::Cubic { x, y, .. } => (x, y),
            Cmd::Close => (0.0, 0.0),
        }).unwrap_or((0.0, 0.0))
    }

    #[test]
    fn 直线与相对命令() {
        let p = Path::svg("M2 3 L12 3 l0 7 H2 V3");
        assert_eq!(end(&p), (2.0, 3.0), "闭合后笔位回到起点");
        // 周长：10 + 7 + 10 + 7 = 34
        assert!((p.length() - 34.0).abs() < 0.01, "长度 {}", p.length());
    }

    #[test]
    fn 半圆弧长度与端点() {
        // 半径 5 的半圆：弧长 π·5 ≈ 15.708，终点 (10, 0)
        let p = Path::svg("M0 0 A5 5 0 0 1 10 0");
        let (x, y) = end(&p);
        assert!((x - 10.0).abs() < 0.01 && y.abs() < 0.01, "终点 {x},{y}");
        assert!((p.length() - std::f32::consts::PI * 5.0).abs() < 0.05, "弧长 {}", p.length());
    }

    #[test]
    fn 半径不足时等比放大() {
        // 两点相距 20，但半径写 5 → 规范要求放大到半径 10 的半圆
        let p = Path::svg("M0 0 A5 5 0 0 1 20 0");
        let (x, y) = end(&p);
        assert!((x - 20.0).abs() < 0.05 && y.abs() < 0.05, "终点 {x},{y}（放大后仍落点）");
        assert!((p.length() - std::f32::consts::PI * 10.0).abs() < 0.1, "弧长 {}", p.length());
    }

    #[test]
    fn 圆心与方向() {
        // sweep=1（顺时针）从 (0,0) 到 (10,0)：圆心应在 (5,0)，弧从 180° 走到 360°
        let p = Path::svg("M0 0 A5 5 0 0 1 10 0");
        match p.cmds[1] {
            Cmd::Arc { cx, cy, rx, a0, sweep, .. } => {
                assert!((cx - 5.0).abs() < 0.01 && cy.abs() < 0.01, "圆心 {cx},{cy}");
                assert!((rx - 5.0).abs() < 0.01);
                assert!((a0 - 180.0).abs() < 0.5, "起角 {a0}");
                assert!((sweep - 180.0).abs() < 0.5, "扫角 {sweep}");
            }
            ref c => panic!("第二段应是弧：{c:?}"),
        }
    }
}
