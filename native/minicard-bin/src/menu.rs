//! 自绘右键菜单（Dock 用）。
//!
//! 系统 `TrackPopupMenu` 是 Win32 老样式（灰底方角、系统字体），跟自绘 Dock 完全不搭。
//! 这里做一个自己的弹出窗：D2D 绘制、圆角 12、行高 32、内边距 6、hover 强调色 16%，
//! 口径照 `web/dock.html` 的 `.menu/.mi`。
//!
//! 用法：`menu::show(...)` **阻塞**到选中 / Esc / 点到外面，返回选中项下标。

use std::cell::RefCell;

use minicard_draw::color::Palette;
use minicard_draw::glass;
use minicard_draw::layout::TextSpec;
use minicard_draw::paint::{fill_round, set_color, stroke_round};
use minicard_draw::text::{Mode, TextEngine};
use minicard_shell::gfx::Gfx;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForSystem;
use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
use windows::Win32::UI::WindowsAndMessaging::*;

const ROW_H: f32 = 32.0;
const PAD: f32 = 6.0;
const RADIUS: f32 = 12.0;
const FS: f32 = 13.0;
const TEXT_PAD: f32 = 10.0;

/// 菜单锚点（屏幕像素）：水平中心 + 底边应该落在哪。
/// Dock 的用法是「图标中心 x」+「条顶再往上 6px」，跟 `web/dock.html` 的 `.menu{bottom: bar + 6}` 一致。
#[derive(Clone, Copy)]
pub struct Anchor {
    pub cx: i32,
    pub bottom: i32,
}

/// 由锚点算出菜单窗左上角（屏幕像素），屏幕内夹紧。
/// 菜单要「贴着条往上长」，所以是底边对齐而不是左上角对齐。
pub fn place(anchor: Anchor, w_px: i32, h_px: i32, screen: (i32, i32)) -> (i32, i32) {
    let x = (anchor.cx - w_px / 2).min(screen.0 - w_px - 8).max(8);
    let y = (anchor.bottom - h_px).max(8);
    (x, y)
}

/// 一项：(文案, 危险色?)
#[derive(Clone)]
pub struct Item {
    pub label: String,
    pub danger: bool,
    pub disabled: bool,
}

thread_local! {
    static ST: RefCell<State> = RefCell::new(State::default());
}

#[derive(Default)]
struct State {
    hwnd: isize,
    hover: Option<usize>,
    chosen: Option<usize>,
    closed: bool,
}

fn class_name() -> PCWSTR {
    w!("MiniCardNativeMenu")
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_MOUSEMOVE => {
            let y = ((lp.0 >> 16) & 0xFFFF) as i16 as f32;
            let idx = row_at(y);
            ST.with(|s| {
                let mut s = s.borrow_mut();
                if s.hover != idx {
                    s.hover = idx;
                }
            });
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let y = ((lp.0 >> 16) & 0xFFFF) as i16 as f32;
            if let Some(i) = row_at(y) {
                ST.with(|s| {
                    let mut s = s.borrow_mut();
                    s.chosen = Some(i);
                    s.closed = true;
                });
            }
            LRESULT(0)
        }
        WM_KEYDOWN => {
            if wp.0 as u32 == 0x1B {
                ST.with(|s| s.borrow_mut().closed = true);
                return LRESULT(0);
            }
            LRESULT(0)
        }
        WM_KILLFOCUS => {
            // 失去焦点（点到别处）就收
            ST.with(|s| s.borrow_mut().closed = true);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

/// 窗内 y → 行下标（含顶部内边距）
fn row_at(y: f32) -> Option<usize> {
    let inner = y - PAD;
    if inner < 0.0 {
        return None;
    }
    let i = (inner / ROW_H).floor() as usize;
    let total = ITEM_COUNT.with(|c| *c.borrow());
    if i < total {
        Some(i)
    } else {
        None
    }
}

thread_local! {
    static ITEM_COUNT: RefCell<usize> = RefCell::new(0);
}

fn ensure_class() -> bool {
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            lpszClassName: class_name(),
            ..Default::default()
        };
        // 已注册会返回 0，不用管
        let _ = RegisterClassW(&wc);
    }
    true
}

/// 弹出菜单并阻塞到有结果。返回选中的下标（None = 取消/点到外面）。
pub fn show(
    anchor: Anchor,
    items: &[Item],
    te: &TextEngine,
    pal: &Palette,
    owner: isize,
    wall: Option<&glass::Wall>,
    blur: f32,
    mon: (f32, f32, f32, f32),
) -> Option<usize> {
    if items.is_empty() {
        return None;
    }
    ensure_class();
    let dpi = unsafe { GetDpiForSystem() } as f32;
    let dpi = if dpi > 0.0 { dpi } else { 96.0 };
    let s = dpi / 96.0;

    // 先按最长文案量宽度
    let mut text_w = 0.0f32;
    for it in items {
        let spec = TextSpec {
            s: it.label.clone(),
            size: FS,
            line_h: 20.0,
            weight: 400,
            wrap: false,
            align: 0,
            tabular: false,
            spacing_em: 0.0,
        };
        if let Ok(lay) = te.layout_spec(&spec, 600.0, 22.0, Mode::Clip) {
            let mut m = windows::Win32::Graphics::DirectWrite::DWRITE_TEXT_METRICS::default();
            let _ = unsafe { lay.GetMetrics(&mut m) };
            text_w = text_w.max(m.widthIncludingTrailingWhitespace);
        }
    }
    let w_dip = (text_w + TEXT_PAD * 2.0 + PAD * 2.0).max(140.0);
    let h_dip = items.len() as f32 * ROW_H + PAD * 2.0;
    let w_px = (w_dip * s).round() as i32;
    let h_px = (h_dip * s).round() as i32;

    let mw = unsafe { GetSystemMetrics(SM_CXSCREEN) };
    let mh = unsafe { GetSystemMetrics(SM_CYSCREEN) };
    let (x, y) = place(anchor, w_px, h_px, (mw, mh));
    minicard_shell::mlog!(
        "menu：{}×{} 锚点 cx={} bottom={} → rect {x},{y} {}×{}",
        w_dip.round(),
        h_dip.round(),
        anchor.cx,
        anchor.bottom,
        w_px,
        h_px
    );

    let hwnd = unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOREDIRECTIONBITMAP,
            class_name(),
            w!(""),
            WS_POPUP,
            x,
            y,
            w_px,
            h_px,
            None,
            None,
            Some(hinst.into()),
            None,
        )
    };
    let Ok(hwnd) = hwnd else {
        return None;
    };
    let Ok(gfx) = Gfx::new(hwnd, w_px as u32, h_px as u32, dpi, RADIUS) else {
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
        return None;
    };

    // 玻璃：跟 Dock 同一口径（壁纸切片 + 高斯），菜单才有「浮在桌面上」的质感
    let pad_px = ((blur * 3.0).max(60.0) * s).round() as i32;
    let glass_bmp = wall.and_then(|w| {
        let cap = glass::from_wallpaper(
            w,
            (mon.2, mon.3),
            (x as f32 - mon.0, y as f32 - mon.1),
            (w_px as u32, h_px as u32),
            pad_px as f32,
            blur * s,
        );
        gfx.bitmap_from_bgra(&cap.bgra, cap.w, cap.h).ok()
    });

    ST.with(|s2| {
        let mut st = s2.borrow_mut();
        *st = State {
            hwnd: hwnd.0 as isize,
            hover: None,
            chosen: None,
            closed: false,
        };
    });
    ITEM_COUNT.with(|c| *c.borrow_mut() = items.len());

    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        let _ = SetForegroundWindow(hwnd);
    }

    // 模态循环：自己的消息泵 + 重绘 + 点外面自动收
    let mut last_hover: Option<usize> = None;
    let mut first = true;
    loop {
        unsafe {
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        let (closed, hover, chosen) = ST.with(|s| {
            let s = s.borrow();
            (s.closed, s.hover, s.chosen)
        });
        if closed {
            let _ = paint(&gfx, te, pal, items, hover, w_dip, h_dip, glass_bmp.as_ref(), pad_px as f32);
            let r = chosen;
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            ST.with(|s| s.borrow_mut().hwnd = 0);
            return r;
        }
        // 点到菜单外面 → 收（owner 窗口的按下不会进这里，所以只看光标与鼠标键）
        unsafe {
            let mut p = POINT::default();
            let _ = GetCursorPos(&mut p);
            let mut r = RECT::default();
            let _ = GetWindowRect(hwnd, &mut r);
            let in_rect = p.x >= r.left && p.x < r.right && p.y >= r.top && p.y < r.bottom;
            if !in_rect && (GetAsyncKeyState(0x01) as u16 & 0x8001) != 0 {
                ST.with(|s| s.borrow_mut().closed = true);
            }
        }
        if hover != last_hover || first {
            last_hover = hover;
            first = false;
            let _ = paint(&gfx, te, pal, items, hover, w_dip, h_dip, glass_bmp.as_ref(), pad_px as f32);
        }
        let _ = owner; // owner 只用于日志/将来抢焦点
        std::thread::sleep(std::time::Duration::from_millis(4));
    }
}

#[allow(clippy::too_many_arguments)]
pub fn paint(
    g: &Gfx,
    te: &TextEngine,
    pal: &Palette,
    items: &[Item],
    hover: Option<usize>,
    w: f32,
    h: f32,
    glass: Option<&windows::Win32::Graphics::Direct2D::ID2D1Bitmap1>,
    pad: f32,
) -> windows::core::Result<()> {
    unsafe {
        g.begin()?;
        let full = D2D_RECT_F { left: 0.0, top: 0.0, right: w, bottom: h };
        if let Some(b) = glass {
            // 圆角裁剪后铺壁纸切片，再压一层暗色（比纯色底好看得多）
            g.push_clip_round(full, RADIUS)?;
            g.d2d.DrawBitmap(
                b,
                Some(&D2D_RECT_F {
                    left: -pad,
                    top: -pad,
                    right: w + pad,
                    bottom: h + pad,
                }),
                1.0,
                windows::Win32::Graphics::Direct2D::D2D1_INTERPOLATION_MODE_LINEAR,
                None,
                None,
            );
            set_color(
                g,
                windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F {
                    a: 0.62,
                    ..pal.bubble_bg
                },
            )?;
            g.d2d
                .FillRectangle(&full, &g.brush);
            g.pop_clip();
        } else {
            set_color(
                g,
                windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F {
                    a: 0.96,
                    ..pal.bubble_bg
                },
            )?;
            fill_round(g, full, RADIUS)?;
        }
        // 每个 hover 项垫一层强调色 16%
        if let Some(i) = hover.filter(|i| items.get(*i).is_some_and(|it| !it.disabled)) {
            let top = PAD + i as f32 * ROW_H;
            let c = if items[i].danger {
                windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F {
                    r: 0.95,
                    g: 0.30,
                    b: 0.30,
                    a: 0.22,
                }
            } else {
                windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F { a: 0.16, ..pal.accent }
            };
            set_color(g, c)?;
            fill_round(
                g,
                D2D_RECT_F {
                    left: PAD,
                    top,
                    right: w - PAD,
                    bottom: top + ROW_H,
                },
                8.0,
            )?;
        }
        for (i, it) in items.iter().enumerate() {
            let spec = TextSpec {
                s: it.label.clone(),
                size: FS,
                line_h: 20.0,
                weight: 400,
                wrap: false,
                align: 0,
                tabular: false,
                spacing_em: 0.0,
            };
            let lay = te.layout_spec(&spec, w - PAD * 2.0 - TEXT_PAD * 2.0, 22.0, Mode::Clip)?;
            let hovered = hover == Some(i);
            let color = if it.disabled {
                windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F { a: 0.38, ..pal.fg }
            } else if it.danger && hovered {
                // 红底红字看不清，但 hover 变白又丢了危险语义 → 提亮到浅红
                windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F {
                    r: 1.0,
                    g: 0.58,
                    b: 0.58,
                    a: 1.0,
                }
            } else if it.danger {
                windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F {
                    r: 0.95,
                    g: 0.36,
                    b: 0.36,
                    a: 1.0,
                }
            } else {
                pal.fg
            };
            set_color(g, color)?;
            let y = PAD + i as f32 * ROW_H + (ROW_H - 20.0) / 2.0;
            g.d2d.DrawTextLayout(
                windows_numerics::Vector2 { X: PAD + TEXT_PAD, Y: y },
                &lay,
                &g.brush,
                windows::Win32::Graphics::Direct2D::D2D1_DRAW_TEXT_OPTIONS_NONE,
            );
        }
        set_color(
            g,
            windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F {
                a: 0.22,
                ..pal.fg
            },
        )?;
        stroke_round(g, full, RADIUS, 1.0)?;
        g.end()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 1920×1080 屏，菜单 208×108：底边贴锚点，水平以图标中心居中
    #[test]
    fn 贴条往上长() {
        let a = Anchor { cx: 760, bottom: 997 };
        let (x, y) = place(a, 208, 108, (1920, 1080));
        assert_eq!((x, y), (760 - 104, 997 - 108));
        assert_eq!(x + 208 / 2, a.cx);      // 水平居中于图标
        assert_eq!(y + 108, a.bottom);      // 底边 = 锚点（条顶再往上 6px）
    }

    /// 靠左的图标：菜单会出窗口 → 夹到 x=8，仍不越过右边界
    #[test]
    fn 左侧夹紧() {
        let (x, y) = place(Anchor { cx: 40, bottom: 997 }, 208, 108, (1920, 1080));
        assert_eq!(x, 8);
        assert_eq!(y, 889);
    }

    /// 靠右：x = 屏宽 - 菜单宽 - 8
    #[test]
    fn 右侧夹紧() {
        let (x, _) = place(Anchor { cx: 1900, bottom: 997 }, 208, 108, (1920, 1080));
        assert_eq!(x, 1920 - 208 - 8);
    }

    /// 真机实测口径：Dock 窗原点 (426,831)、条顶窗内 172、第 5 个图标中心 384、dpi 96
    /// → 锚点 (810, 997)，菜单 208×108 → rect (706, 889)，底边正好离条顶 6px
    #[test]
    fn dock_实测锚点() {
        let a = Anchor {
            cx: 426 + 384,
            bottom: 831 + 172 - 6,
        };
        let (x, y) = place(a, 208, 108, (1920, 1080));
        assert_eq!((x, y), (706, 889));
        assert_eq!(y + 108, 831 + 172 - 6); // 底边 = 条顶 - 6
    }

    /// 锚点离屏顶太近：y 至少 8，不会跑出去
    #[test]
    fn 顶部夹紧() {
        let (_, y) = place(Anchor { cx: 600, bottom: 100 }, 208, 108, (1920, 1080));
        assert_eq!(y, 8);
    }
}
