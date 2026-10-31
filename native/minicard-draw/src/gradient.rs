//! B3 渐变：CSS `linear-gradient` / `radial-gradient` 的 D2D 落地。
//!
//! 插值口径按 CSS：**在 sRGB 空间、预乘 alpha 下插值**（`D2D1_COLOR_INTERPOLATION_MODE_PREMULTIPLIED`）。
//! 这条不能随便改成直通（STRAIGHT）：`color-mix(in oklch, accent 72%, transparent)` 这类
//! 铺到透明的渐变，直通插值会在中段鼓出一圈偏亮的边（alpha 与色值各自插值、不是按预乘走）。

use std::cell::RefCell;
use std::collections::HashMap;

use windows::core::{Interface, Result};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D1_COLOR_F, D2D1_GRADIENT_STOP,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_BUFFER_PRECISION_8BPC_UNORM, D2D1_COLOR_INTERPOLATION_MODE_PREMULTIPLIED,
    D2D1_COLOR_SPACE_SRGB, D2D1_EXTEND_MODE_CLAMP, D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES,
    D2D1_RADIAL_GRADIENT_BRUSH_PROPERTIES, D2D1_ROUNDED_RECT, ID2D1Brush,
    ID2D1GradientStopCollection1, ID2D1RenderTarget,
};

use minicard_shell::gfx::Gfx;

use crate::path::Path;
use crate::paint;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stop {
    /// 0..1
    pub pos: f32,
    pub color: D2D1_COLOR_F,
}

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Linear {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Radial {
    pub cx: f32,
    pub cy: f32,
    pub rx: f32,
    pub ry: f32,
    /// 焦点相对圆心的偏移（D2D `gradientOriginOffset` 就是这个语义；0 = 与圆心重合）
    pub fx: f32,
    pub fy: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Grad {
    Linear(Linear),
    Radial(Radial),
}

/// CSS 线性渐变角度 → 起止点。
///
/// CSS 的角度是「0° 朝上、顺时针」，梯度线**穿过盒子中心**，两端各自延伸到
/// 刚好覆盖整个盒子（所以 135° 在正方形上是左上角 → 右下角，而不是任意两点）。
/// 梯度线长度 L = |W·sinθ| + |H·cosθ|。
pub fn css_linear_angle(deg: f32, x: f32, y: f32, w: f32, h: f32) -> Linear {
    let t = deg.to_radians();
    let (s, c) = (t.sin(), t.cos());
    let len = (w * s).abs() + (h * c).abs();
    // 屏幕坐标 y 向下：朝上 = (0,-1)
    let (dx, dy) = (s, -c);
    let (cx, cy) = (x + w / 2.0, y + h / 2.0);
    Linear {
        x0: cx - dx * len / 2.0,
        y0: cy - dy * len / 2.0,
        x1: cx + dx * len / 2.0,
        y1: cy + dy * len / 2.0,
    }
}

fn hash_f32(h: &mut u64, v: f32) {
    *h ^= v.to_bits() as u64;
    *h = h.wrapping_mul(0x100000001b3);
}

fn key_of(grad: &Grad, stops: &[Stop]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    match *grad {
        Grad::Linear(l) => {
            hash_f32(&mut h, 1.0);
            for v in [l.x0, l.y0, l.x1, l.y1] {
                hash_f32(&mut h, v);
            }
        }
        Grad::Radial(r) => {
            hash_f32(&mut h, 2.0);
            for v in [r.cx, r.cy, r.rx, r.ry, r.fx, r.fy] {
                hash_f32(&mut h, v);
            }
        }
    }
    for s in stops {
        hash_f32(&mut h, s.pos);
        for v in [s.color.r, s.color.g, s.color.b, s.color.a] {
            hash_f32(&mut h, v);
        }
    }
    h
}

fn stop_collection(g: &Gfx, stops: &[Stop]) -> Result<ID2D1GradientStopCollection1> {
    let raw: Vec<D2D1_GRADIENT_STOP> = stops
        .iter()
        .map(|s| D2D1_GRADIENT_STOP { position: s.pos.clamp(0.0, 1.0), color: s.color })
        .collect();
    unsafe {
        g.d2d.CreateGradientStopCollection(
            &raw,
            D2D1_COLOR_SPACE_SRGB,
            D2D1_COLOR_SPACE_SRGB,
            D2D1_BUFFER_PRECISION_8BPC_UNORM,
            D2D1_EXTEND_MODE_CLAMP,
            D2D1_COLOR_INTERPOLATION_MODE_PREMULTIPLIED,
        )
    }
}

/// 建（或复用）渐变笔刷。同一组参数只建一次 —— 每帧新建 COM 对象是纯浪费。
pub fn brush(g: &Gfx, grad: &Grad, stops: &[Stop]) -> Result<ID2D1Brush> {
    // 渐变笔刷属于**某个设备上下文**：一进程多窗口（后面要把 9 张卡收进一个进程）时，
    // 不同窗口各有一份，键里不带上下文身份就会把 A 窗的笔刷拿到 B 窗上用（静默画错）。
    let key = (g.d2d.as_raw() as usize, key_of(grad, stops));
    thread_local! {
        static CACHE: RefCell<HashMap<(usize, u64), ID2D1Brush>> = RefCell::new(HashMap::new());
    }
    if let Some(b) = CACHE.with(|c| c.borrow().get(&key).cloned()) {
        return Ok(b);
    }
    let coll = stop_collection(g, stops)?;
    // CreateLinearGradientBrush/CreateRadialGradientBrush 在 ID2D1RenderTarget（基接口）上
    let rt: ID2D1RenderTarget = g.d2d.cast()?;
    let b: ID2D1Brush = unsafe {
        match *grad {
            Grad::Linear(l) => {
                let props = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
                    startPoint: windows_numerics::Vector2 { X: l.x0, Y: l.y0 },
                    endPoint: windows_numerics::Vector2 { X: l.x1, Y: l.y1 },
                };
                rt.CreateLinearGradientBrush(&props, None, &coll)?.cast()?
            }
            Grad::Radial(r) => {
                let props = D2D1_RADIAL_GRADIENT_BRUSH_PROPERTIES {
                    center: windows_numerics::Vector2 { X: r.cx, Y: r.cy },
                    gradientOriginOffset: windows_numerics::Vector2 { X: r.fx, Y: r.fy },
                    radiusX: r.rx,
                    radiusY: r.ry,
                };
                rt.CreateRadialGradientBrush(&props, None, &coll)?.cast()?
            }
        }
    };
    CACHE.with(|c| {
        let mut m = c.borrow_mut();
        if m.len() > 32 {
            m.clear();
        }
        m.insert(key, b.clone());
    });
    Ok(b)
}

/// 圆角矩形填充 + 渐变。
pub fn fill_round(
    g: &Gfx,
    rect: D2D_RECT_F,
    radius: f32,
    grad: &Grad,
    stops: &[Stop],
) -> Result<()> {
    let b = brush(g, grad, stops)?;
    unsafe {
        g.d2d.FillRoundedRectangle(
            &D2D1_ROUNDED_RECT { rect, radiusX: radius, radiusY: radius },
            &b,
        );
    }
    Ok(())
}

/// 任意路径填充 + 渐变。
pub fn fill_path(g: &Gfx, p: &Path, grad: &Grad, stops: &[Stop]) -> Result<()> {
    let b = brush(g, grad, stops)?;
    let geo = paint::geometry(g, p)?;
    unsafe {
        g.d2d.FillGeometry(&geo, &b, None);
    }
    Ok(())
}

/// 便捷：`linear-gradient(Ndeg, a, b)` 这类两档渐变直接吃矩形与角度。
pub fn linear_two(deg: f32, rect: D2D_RECT_F, a: D2D1_COLOR_F, b: D2D1_COLOR_F) -> (Grad, [Stop; 2]) {
    let l = css_linear_angle(
        deg,
        rect.left,
        rect.top,
        rect.right - rect.left,
        rect.bottom - rect.top,
    );
    (
        Grad::Linear(l),
        [
            Stop { pos: 0.0, color: a },
            Stop { pos: 1.0, color: b },
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.02
    }

    #[test]
    fn css_135度在正方形上是左上到右下() {
        let l = css_linear_angle(135.0, 0.0, 0.0, 100.0, 100.0);
        assert!(close(l.x0, 0.0) && close(l.y0, 0.0), "起点 {:?}", (l.x0, l.y0));
        assert!(close(l.x1, 100.0) && close(l.y1, 100.0), "终点 {:?}", (l.x1, l.y1));
    }

    #[test]
    fn css_0度朝上_90度朝右() {
        // 0°：从下往上
        let l = css_linear_angle(0.0, 0.0, 0.0, 100.0, 50.0);
        assert!(close(l.x0, 50.0) && close(l.y0, 50.0), "0° 起点 {:?}", (l.x0, l.y0));
        assert!(close(l.x1, 50.0) && close(l.y1, 0.0), "0° 终点 {:?}", (l.x1, l.y1));
        // 90°：从左往右
        let l = css_linear_angle(90.0, 0.0, 0.0, 100.0, 50.0);
        assert!(close(l.x0, 0.0) && close(l.y0, 25.0), "90° 起点 {:?}", (l.x0, l.y0));
        assert!(close(l.x1, 100.0) && close(l.y1, 25.0), "90° 终点 {:?}", (l.x1, l.y1));
    }

    #[test]
    fn 梯度线长度按_w_sin_加_h_cos() {
        // 45° 在 200×100 上：L = 200·0.7071 + 100·0.7071 = 212.13
        let l = css_linear_angle(45.0, 0.0, 0.0, 200.0, 100.0);
        let len = ((l.x1 - l.x0).powi(2) + (l.y1 - l.y0).powi(2)).sqrt();
        assert!(close(len, 0.70710678 * 300.0), "长度 {len}");
        // 中心必须落在盒子中心
        assert!(close((l.x0 + l.x1) / 2.0, 100.0) && close((l.y0 + l.y1) / 2.0, 50.0));
    }

    #[test]
    fn 角度决定方向() {
        // 180° = 朝下：起点在上、终点在下
        let l = css_linear_angle(180.0, 0.0, 0.0, 40.0, 60.0);
        assert!(l.y0 < l.y1, "180° 应从上往下：{:?}", (l.y0, l.y1));
        // 270° = 朝左：起点在右、终点在左
        let l = css_linear_angle(270.0, 0.0, 0.0, 40.0, 60.0);
        assert!(l.x0 > l.x1, "270° 应从右往左：{:?}", (l.x0, l.x1));
    }

    #[test]
    fn 笔刷缓存键跟着参数走() {
        let a = Grad::Linear(css_linear_angle(135.0, 0.0, 0.0, 10.0, 10.0));
        let b = Grad::Linear(css_linear_angle(135.0, 0.0, 0.0, 10.0, 20.0));
        let s = [Stop { pos: 0.0, color: D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a: 1.0 } }];
        assert_ne!(key_of(&a, &s), key_of(&b, &s), "尺寸不同 ≠ 同一个键");
        assert_eq!(key_of(&a, &s), key_of(&a, &s));
    }
}
