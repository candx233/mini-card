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
use windows::Win32::Graphics::Direct2D::{
    D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_ARC_SEGMENT, D2D1_ARC_SIZE_LARGE, D2D1_ARC_SIZE_SMALL,
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
        static CACHE: RefCell<HashMap<u32, ID2D1StrokeStyle>> = RefCell::new(HashMap::new());
    }
    CACHE.with(|c| {
        if let Some(s) = c.borrow().get(&width.to_bits()) {
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
        c.borrow_mut().insert(width.to_bits(), s.clone());
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

/// 轴向裁剪入栈（音乐卡跑马灯、文字溢出裁切）；必须与 `pop_clip` 成对。
pub fn push_clip(g: &Gfx, rect: D2D_RECT_F) {
    unsafe {
        let _ = g.d2d.PushAxisAlignedClip(&rect, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
    }
}

pub fn pop_clip(g: &Gfx) {
    unsafe { g.d2d.PopAxisAlignedClip() };
}
