//! 图元：在 `Gfx` 上画一个形状的最小 API。
//!
//! 用自由函数而不是 `Gfx` 的方法，是为了让 `minicard-shell` 只负责「设备与呈像」，
//! 画笔语义（圆角矩形 / 描边 / 圆）留在绘制层。B 段补路径、圆弧、渐变时也落这里。

use std::cell::RefCell;
use std::collections::HashMap;

use windows::core::Result;
use windows::core::Interface;
use windows_numerics::Vector2;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_SIZE_F, D2D1_BEZIER_SEGMENT, D2D1_COLOR_F, D2D1_FIGURE_BEGIN_FILLED,
    D2D1_FIGURE_END_CLOSED, D2D1_FIGURE_END_OPEN, ID2D1SimplifiedGeometrySink,
};
use windows_numerics::Matrix3x2;
use windows::Win32::Graphics::Direct2D::{
    D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_LAYER_OPTIONS1_NONE, D2D1_LAYER_PARAMETERS1,
    ID2D1Bitmap1, ID2D1Layer, D2D1_ARC_SEGMENT, D2D1_ARC_SIZE_LARGE, D2D1_ARC_SIZE_SMALL,
    D2D1_CAP_STYLE_ROUND, D2D1_DASH_STYLE_SOLID, D2D1_LINE_JOIN_ROUND, D2D1_QUADRATIC_BEZIER_SEGMENT,
    D2D1_ROUNDED_RECT, D2D1_STROKE_STYLE_PROPERTIES1, D2D1_SWEEP_DIRECTION_CLOCKWISE,
    D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE, D2D1_STROKE_TRANSFORM_TYPE_NORMAL, ID2D1Geometry,
    ID2D1GeometrySink, ID2D1PathGeometry1, ID2D1StrokeStyle,
};

use minicard_shell::gfx::Gfx;

use crate::path::{Cmd, Path};

pub fn set_color(g: &Gfx, c: D2D1_COLOR_F) -> Result<()> {
    // ID2D1SolidColorBrush::SetColor 返回 void（不是 HRESULT）
    unsafe { g.brush.SetColor(&c) };
    Ok(())
}

pub fn fill_round(g: &Gfx, rect: D2D_RECT_F, radius: f32) -> Result<()> {
    unsafe {
        g.d2d
            .FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect, radiusX: radius, radiusY: radius }, &g.brush)
    };
    Ok(())
}

pub fn stroke_round(g: &Gfx, rect: D2D_RECT_F, radius: f32, width: f32) -> Result<()> {
    unsafe {
        g.d2d.DrawRoundedRectangle(
            &D2D1_ROUNDED_RECT { rect, radiusX: radius, radiusY: radius },
            &g.brush,
            width,
            None,
        )
    };
    Ok(())
}

pub fn fill_rect(g: &Gfx, rect: D2D_RECT_F) -> Result<()> {
    unsafe { g.d2d.FillRectangle(&rect, &g.brush) };
    Ok(())
}

/// `Path` → D2D 路径几何。每个 `Move` 起一个图元，`Close` 收成闭合图元。
pub fn geometry(g: &Gfx, p: &Path) -> Result<ID2D1PathGeometry1> {
    unsafe {
        let geo = g.factory.CreatePathGeometry()?;
        // AddLine/AddArc 在派生接口 ID2D1GeometrySink 上；BeginFigure/EndFigure 在基接口
        // ID2D1SimplifiedGeometrySink 上（windows-rs 不做基接口自动 deref，得各拿一份）
        let s: ID2D1GeometrySink = geo.Open()?;
        let base: ID2D1SimplifiedGeometrySink = s.cast()?;
        let mut open = false;
        for c in &p.cmds {
            match *c {
                Cmd::Move(x, y) => {
                    if open {
                        base.EndFigure(D2D1_FIGURE_END_OPEN);
                    }
                    base.BeginFigure(Vector2 { X: x, Y: y }, D2D1_FIGURE_BEGIN_FILLED);
                    open = true;
                }
                Cmd::Line(x, y) => {
                    if !open {
                        base.BeginFigure(Vector2 { X: x, Y: y }, D2D1_FIGURE_BEGIN_FILLED);
                        open = true;
                    }
                    s.AddLine(Vector2 { X: x, Y: y });
                }
                Cmd::Arc { cx, cy, rx, ry, a0, sweep } => {
                    let (sx, sy) = Path::arc_start(cx, cy, rx, ry, a0);
                    if !open {
                        base.BeginFigure(Vector2 { X: sx, Y: sy }, D2D1_FIGURE_BEGIN_FILLED);
                        open = true;
                    } else {
                        // 显式接到弧起点，别让 D2D 自己补一条直线
                        s.AddLine(Vector2 { X: sx, Y: sy });
                    }
                    let (ex, ey) = Path::arc_end(cx, cy, rx, ry, a0, sweep);
                    s.AddArc(&D2D1_ARC_SEGMENT {
                        point: Vector2 { X: ex, Y: ey },
                        size: D2D_SIZE_F { width: rx, height: ry },
                        rotationAngle: 0.0,
                        sweepDirection: if sweep >= 0.0 {
                            D2D1_SWEEP_DIRECTION_CLOCKWISE
                        } else {
                            D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE
                        },
                        arcSize: if sweep.abs() > 180.0 {
                            D2D1_ARC_SIZE_LARGE
                        } else {
                            D2D1_ARC_SIZE_SMALL
                        },
                    });
                }
                Cmd::Quad { cx, cy, x, y } => {
                    if open {
                        s.AddQuadraticBezier(&D2D1_QUADRATIC_BEZIER_SEGMENT {
                            point1: Vector2 { X: cx, Y: cy },
                            point2: Vector2 { X: x, Y: y },
                        });
                    }
                }
                Cmd::Cubic { c1x, c1y, c2x, c2y, x, y } => {
                    if open {
                        s.AddBezier(&D2D1_BEZIER_SEGMENT {
                            point1: Vector2 { X: c1x, Y: c1y },
                            point2: Vector2 { X: c2x, Y: c2y },
                            point3: Vector2 { X: x, Y: y },
                        });
                    }
                }
                Cmd::Close => {
                    if open {
                        base.EndFigure(D2D1_FIGURE_END_CLOSED);
                        open = false;
                    }
                }
            }
        }
        if open {
            base.EndFigure(D2D1_FIGURE_END_OPEN);
        }
        s.Close()?;
        Ok(geo)
    }
}

/// 圆头圆角描边样式（对应 SVG 的 `stroke-linecap:round; stroke-linejoin:round`）。
/// 按线宽缓存 —— 宽度种类很少，每帧新建 COM 对象是纯浪费。
pub fn round_stroke(g: &Gfx, width: f32) -> Result<ID2D1StrokeStyle> {
    thread_local! {
        static CACHE: RefCell<HashMap<(usize, u32), ID2D1StrokeStyle>> =
            RefCell::new(HashMap::new());
    }
    // stroke style 建在**该窗口自己的 factory** 上，跨窗口复用会把 A 厂的资源喂给 B 厂
    let ctx = g.factory.as_raw() as usize;
    CACHE.with(|c| {
        if let Some(s) = c.borrow().get(&(ctx, width.to_bits())) {
            return Ok(s.clone());
        }
        let props = D2D1_STROKE_STYLE_PROPERTIES1 {
            startCap: D2D1_CAP_STYLE_ROUND,
            endCap: D2D1_CAP_STYLE_ROUND,
            dashCap: D2D1_CAP_STYLE_ROUND,
            lineJoin: D2D1_LINE_JOIN_ROUND,
            miterLimit: 10.0,
            dashStyle: D2D1_DASH_STYLE_SOLID,
            dashOffset: 0.0,
            transformType: D2D1_STROKE_TRANSFORM_TYPE_NORMAL,
        };
        let s1 = unsafe { g.factory.CreateStrokeStyle(&props, None)? };
        let s: ID2D1StrokeStyle = s1.cast()?;
        c.borrow_mut().insert((ctx, width.to_bits()), s.clone());
        Ok(s)
    })
}

pub fn fill_path(g: &Gfx, p: &Path) -> Result<()> {
    let geo = geometry(g, p)?;
    let g0: ID2D1Geometry = geo.cast()?;
    unsafe { g.d2d.FillGeometry(&g0, &g.brush, None) };
    Ok(())
}

/// 描边路径。`round` = 圆头圆角（图标与勾用），否则平头。
pub fn stroke_path(g: &Gfx, p: &Path, width: f32, round: bool) -> Result<()> {
    let geo: ID2D1Geometry = geometry(g, p)?.cast()?;
    let style = if round { Some(round_stroke(g, width)?) } else { None };
    unsafe { g.d2d.DrawGeometry(&geo, &g.brush, width, style.as_ref()) };
    Ok(())
}

/// 绕点旋转（**弧度**进，内部换成度）。
///
/// 坑：`windows_numerics::Matrix3x2::rotation_around` 文档写的是 `angle`，但底层直接调
/// `D2D1MakeRotateMatrix(d2d1.dll)`，那个 API 的单位是**度**。喂弧度会得到 1/57.3 的转动量
/// —— 0.3 rad 看上去只转了 0.3°，画面上几乎不动，很容易误判成「变换没生效」。
pub fn rotation_rad(angle_rad: f32, cx: f32, cy: f32) -> windows_numerics::Matrix3x2 {
    windows_numerics::Matrix3x2::rotation_around(
        angle_rad.to_degrees(),
        windows_numerics::Vector2 { X: cx, Y: cy },
    )
}

/// 轴向裁剪入栈（音乐卡跑马灯、文字溢出裁切）；必须与 `pop_clip` 成对。
pub fn push_clip(g: &Gfx, rect: D2D_RECT_F) {
    unsafe {
        let _ = g.d2d.PushAxisAlignedClip(&rect, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
    }
}

pub fn pop_clip(g: &Gfx) {
    unsafe { g.d2d.PopAxisAlignedClip() };
}

/// 组不透明度层（`.dim`）。**不是**「把每个颜色乘 alpha」——层内所有绘制先合成到
/// 一张离屏，再整体按 `opacity` 贴回，所以重叠部分不会被重复减淡（两根进度条重叠时
/// 逐色乘 alpha 会叠出 0.25 的效果，层不透明度才是 0.5）。
///
/// RAII：`Drop` 里 `PopLayer`，忘了 pop 也不会把后续绘制留在层里。
pub struct OpacityLayer<'a> {
    g: &'a Gfx,
    _layer: ID2D1Layer,
    popped: bool,
}

impl Drop for OpacityLayer<'_> {
    fn drop(&mut self) {
        if !self.popped {
            unsafe { self.g.d2d.PopLayer() };
        }
    }
}

pub fn push_opacity(g: &Gfx, opacity: f32) -> Result<OpacityLayer<'_>> {
    unsafe {
        let layer = g.d2d.CreateLayer(None)?;
        let params = D2D1_LAYER_PARAMETERS1 {
            contentBounds: D2D_RECT_F {
                left: -1.0e6,
                top: -1.0e6,
                right: 1.0e6,
                bottom: 1.0e6,
            },
            geometricMask: std::mem::ManuallyDrop::new(None),
            maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
            maskTransform: Matrix3x2 {
                M11: 1.0,
                M12: 0.0,
                M21: 0.0,
                M22: 1.0,
                M31: 0.0,
                M32: 0.0,
            },
            opacity: opacity.clamp(0.0, 1.0),
            opacityBrush: std::mem::ManuallyDrop::new(None),
            layerOptions: D2D1_LAYER_OPTIONS1_NONE,
        };
        g.d2d.PushLayer(&params, &layer);
        Ok(OpacityLayer { g, _layer: layer, popped: false })
    }
}

/// 画一块圆角矩形阴影（形状左上角 = `origin`）。掩码按参数缓存，位图也缓存 ——
/// 阴影是静态的，每帧重算掩码 + 重传纹理纯属浪费。
pub fn draw_shadow(
    g: &Gfx,
    spec: &crate::shadow::Spec,
    origin: (f32, f32),
    color: D2D1_COLOR_F,
) -> Result<()> {
    use windows::Win32::Graphics::Direct2D::D2D1_INTERPOLATION_MODE_LINEAR;
    let key = shadow_key(spec, color);
    thread_local! {
        static CACHE: RefCell<HashMap<(usize, u64), (ID2D1Bitmap1, f32, f32, f32, f32)>> =
            RefCell::new(HashMap::new());
    }
    // 位图也是上下文资源：键里必须带上下文身份（多窗口共栈时不同窗口各一份）
    let key = (g.d2d.as_raw() as usize, key);
    if let Some((bmp, ox, oy, bw, bh)) = CACHE.with(|c| c.borrow().get(&key).cloned()) {
        let dst = D2D_RECT_F {
            left: origin.0 + ox,
            top: origin.1 + oy,
            right: origin.0 + ox + bw,
            bottom: origin.1 + oy + bh,
        };
        unsafe {
            g.d2d.DrawBitmap(
                &bmp,
                Some(&dst),
                1.0,
                D2D1_INTERPOLATION_MODE_LINEAR,
                None,
                None,
            )
        };
        return Ok(());
    }

    let m = crate::shadow::mask(spec);
    // 覆盖率 → 预乘 BGRA（阴影只有一种颜色，逐像素乘 alpha 即可）
    let mut bgra = vec![0u8; (m.w * m.h * 4) as usize];
    for i in 0..(m.w * m.h) as usize {
        let a = m.alpha[i] as f32 / 255.0 * color.a;
        bgra[i * 4] = (color.b * a * 255.0).round().clamp(0.0, 255.0) as u8;
        bgra[i * 4 + 1] = (color.g * a * 255.0).round().clamp(0.0, 255.0) as u8;
        bgra[i * 4 + 2] = (color.r * a * 255.0).round().clamp(0.0, 255.0) as u8;
        bgra[i * 4 + 3] = (a * 255.0).round().clamp(0.0, 255.0) as u8;
    }
    let bmp = g.bitmap_from_bgra(&bgra, m.w, m.h)?;
    let dst = D2D_RECT_F {
        left: origin.0 + m.ox,
        top: origin.1 + m.oy,
        right: origin.0 + m.ox + m.w as f32,
        bottom: origin.1 + m.oy + m.h as f32,
    };
    unsafe {
        g.d2d.DrawBitmap(
            &bmp,
            Some(&dst),
            1.0,
            D2D1_INTERPOLATION_MODE_LINEAR,
            None,
            None,
        )
    };
    CACHE.with(|c| {
        let mut mm = c.borrow_mut();
        if mm.len() > 16 {
            mm.clear();
        }
        mm.insert(
            key,
            (bmp, m.ox, m.oy, m.w as f32, m.h as f32),
        );
    });
    Ok(())
}

fn shadow_key(spec: &crate::shadow::Spec, color: D2D1_COLOR_F) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    let mut mix = |v: f32| {
        h ^= v.to_bits() as u64;
        h = h.wrapping_mul(0x100000001b3);
    };
    for v in [spec.w, spec.h, spec.radius, spec.spread, spec.blur, spec.dx, spec.dy] {
        mix(v);
    }
    for v in [color.r, color.g, color.b, color.a] {
        mix(v);
    }
    h
}
