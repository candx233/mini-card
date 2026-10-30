//! 颜色：CSS `oklch()` → sRGB，以及卡片调色板（fg / 白纱 / 描边 / 阴影 / 条）。

use windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F;

/// oklch → sRGB（CSS Color 4 口径：OKLab 矩阵 → 线性 sRGB → 传输曲线）
pub fn oklch(l: f32, c: f32, h_deg: f32, a: f32) -> D2D1_COLOR_F {
    let h = h_deg.to_radians();
    let (aa, bb) = (c * h.cos(), c * h.sin());
    let l_ = l + 0.396_337_78 * aa + 0.215_803_76 * bb;
    let m_ = l - 0.105_561_35 * aa - 0.063_854_17 * bb;
    let s_ = l - 0.089_484_18 * aa - 1.291_485_55 * bb;
    let (l3, m3, s3) = (l_ * l_ * l_, m_ * m_ * m_, s_ * s_ * s_);
    let lin = [
        4.076_741_7 * l3 - 3.307_711_6 * m3 + 0.230_969_9 * s3,
        -1.268_438 * l3 + 2.609_757_4 * m3 - 0.341_319_4 * s3,
        -0.004_196 * l3 - 0.703_418_6 * m3 + 1.707_614_7 * s3,
    ];
    let enc = |x: f32| {
        let x = x.clamp(0.0, 1.0);
        if x <= 0.003_130_8 {
            12.92 * x
        } else {
            1.055 * x.powf(1.0 / 2.4) - 0.055
        }
    };
    D2D1_COLOR_F { r: enc(lin[0]), g: enc(lin[1]), b: enc(lin[2]), a }
}

pub struct Palette {
    pub debug_alpha: bool,
    pub fg: D2D1_COLOR_F,
    pub veil: D2D1_COLOR_F,
    pub border: D2D1_COLOR_F,
    pub shadow: D2D1_COLOR_F,
    pub track: D2D1_COLOR_F,
    pub fill: D2D1_COLOR_F,
}

impl Palette {
    /// glass: 0..100（config 的 glass_alpha）→ 白纱 alpha 0..0.30；blur 与阴影走固定 token。
    pub fn new(glass: f32, debug_alpha: bool) -> Self {
        let fg = oklch(0.966, 0.005, 258.3, 1.0);
        let veil_a = (glass.clamp(0.0, 100.0) / 100.0 * 0.30).clamp(0.0, 0.30);
        Self {
            debug_alpha,
            fg,
            veil: D2D1_COLOR_F { a: veil_a, ..fg },
            border: oklch(1.0, 0.0, 0.0, 0.10),
            shadow: D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.45 },
            track: D2D1_COLOR_F { a: 0.25, ..fg },
            fill: D2D1_COLOR_F { a: 0.90, ..fg },
        }
    }
}

