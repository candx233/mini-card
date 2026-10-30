//! 玻璃：截「窗口背后那块屏幕」→ 高斯近似模糊（三次盒模糊，与 Skia 的 blur() 同款近似）。
//!
//! 与现版 card.html 的 `.wall` 同构：都是「取壁纸对应区域、自己糊」，
//! 区别是现在由我们自己截（不用 DWM Acrylic，也不用 backdrop-filter）。

use std::ffi::c_void;
use windows::core::Result;
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC,
    SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HGDIOBJ, SRCCOPY,
};

pub struct Capture {
    /// BGRA8，行序自上而下
    pub bgra: Vec<u8>,
    pub w: u32,
    pub h: u32,
}

/// 截主屏上的一块（物理像素坐标）。`w/h` 必须 > 0。
pub fn capture(x: i32, y: i32, w: i32, h: i32) -> Result<Capture> {
    unsafe {
        let screen = GetDC(None);
        let mem = CreateCompatibleDC(Some(screen));
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h, // 负数 = 自上而下
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut c_void = std::ptr::null_mut();
        let dib = CreateDIBSection(Some(mem), &bmi, DIB_RGB_COLORS, &mut bits, None, 0)?;
        let old = SelectObject(mem, HGDIOBJ(dib.0));
        let r = BitBlt(mem, 0, 0, w, h, Some(screen), x, y, SRCCOPY);
        let n = (w as usize) * (h as usize) * 4;
        let mut bgra = vec![0u8; n];
        if r.is_ok() && !bits.is_null() {
            std::ptr::copy_nonoverlapping(bits as *const u8, bgra.as_mut_ptr(), n);
        }
        SelectObject(mem, old);
        let _ = DeleteObject(HGDIOBJ(dib.0));
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
        r?;
        Ok(Capture { bgra, w: w as u32, h: h as u32 })
    }
}

/// 三次盒模糊近似高斯。`sigma` 单位 = 像素。
/// 盒宽 w 的方差 = (w²−1)/12；三趟 → 3(w²−1)/12 = σ² → w ≈ 2σ（与 CSS blur(radius) 的 σ=radius/2 对齐）
pub fn blur(c: &mut Capture, sigma: f32) {
    if sigma <= 0.5 {
        return;
    }
    let w = (2.0 * sigma).round() as i32; // 盒窗口宽（奇数更好，避免半像素偏置）
    let r = (w / 2).max(1);
    let mut tmp = vec![0u8; c.bgra.len()];
    for _ in 0..3 {
        box_h(&c.bgra, &mut tmp, c.w as i32, c.h as i32, r);
        box_v(&tmp, &mut c.bgra, c.w as i32, c.h as i32, r);
    }
}

/// 滑动窗口盒模糊（O(1)/像素）：窗口 = [x−r, x+r]
fn box_h(src: &[u8], dst: &mut [u8], w: i32, h: i32, r: i32) {
    for y in 0..h {
        let row = (y * w * 4) as usize;
        let mut acc = [0u32; 4];
        let mut cnt = 0u32;
        let last = r.min(w - 1);
        for x in 0..=last {
            let o = row + (x * 4) as usize;
            for c in 0..4 {
                acc[c] += src[o + c] as u32;
            }
            cnt += 1;
        }
        for x in 0..w {
            if x > 0 {
                let rem = x - 1 - r;
                if rem >= 0 {
                    let o = row + (rem * 4) as usize;
                    for c in 0..4 {
                        acc[c] -= src[o + c] as u32;
                    }
                    cnt -= 1;
                }
                let add = x + r;
                if add < w {
                    let o = row + (add * 4) as usize;
                    for c in 0..4 {
                        acc[c] += src[o + c] as u32;
                    }
                    cnt += 1;
                }
            }
            let o = row + (x * 4) as usize;
            for c in 0..4 {
                dst[o + c] = (acc[c] / cnt) as u8;
            }
        }
    }
}

fn box_v(src: &[u8], dst: &mut [u8], w: i32, h: i32, r: i32) {
    let stride = (w * 4) as usize;
    for x in 0..w {
        let col = (x * 4) as usize;
        let mut acc = [0u32; 4];
        let mut cnt = 0u32;
        let last = r.min(h - 1);
        for y in 0..=last {
            let o = (y as usize) * stride + col;
            for c in 0..4 {
                acc[c] += src[o + c] as u32;
            }
            cnt += 1;
        }
        for y in 0..h {
            if y > 0 {
                let rem = y - 1 - r;
                if rem >= 0 {
                    let o = (rem as usize) * stride + col;
                    for c in 0..4 {
                        acc[c] -= src[o + c] as u32;
                    }
                    cnt -= 1;
                }
                let add = y + r;
                if add < h {
                    let o = (add as usize) * stride + col;
                    for c in 0..4 {
                        acc[c] += src[o + c] as u32;
                    }
                    cnt += 1;
                }
            }
            let o = (y as usize) * stride + col;
            for c in 0..4 {
                dst[o + c] = (acc[c] / cnt) as u8;
            }
        }
    }
}

/// 壁纸原图（BGRA，来自 `SPI_GETDESKWALLPAPER` 指向的文件，由外部解码成 raw）
pub struct Wall {
    pub bgra: Vec<u8>,
    pub w: u32,
    pub h: u32,
}

fn px(wall: &Wall, x: i32, y: i32) -> [u8; 4] {
    let x = x.clamp(0, wall.w as i32 - 1) as usize;
    let y = y.clamp(0, wall.h as i32 - 1) as usize;
    let o = (y * wall.w as usize + x) * 4;
    [wall.bgra[o], wall.bgra[o + 1], wall.bgra[o + 2], wall.bgra[o + 3]]
}

fn sample_bilinear(wall: &Wall, u: f32, v: f32) -> [u8; 4] {
    let x = u - 0.5;
    let y = v - 0.5;
    let x0 = x.floor() as i32;
    let y0 = y.floor() as i32;
    let fx = x - x0 as f32;
    let fy = y - y0 as f32;
    let a = px(wall, x0, y0);
    let b = px(wall, x0 + 1, y0);
    let c = px(wall, x0, y0 + 1);
    let d = px(wall, x0 + 1, y0 + 1);
    let mut out = [0u8; 4];
    for i in 0..4 {
        let top = a[i] as f32 * (1.0 - fx) + b[i] as f32 * fx;
        let bot = c[i] as f32 * (1.0 - fx) + d[i] as f32 * fx;
        out[i] = (top * (1.0 - fy) + bot * fy).round().clamp(0.0, 255.0) as u8;
    }
    out
}

/// 复刻 card.html 的 `.wall`：壁纸按「填充」cover 到本显示器 → 取卡片所在区域（含 pad）→ 模糊。
/// 壁纸像素坐标 = (屏幕点 + offset) / k，与 CSS background-size/position 的解算等价。
#[allow(clippy::too_many_arguments)]
pub fn from_wallpaper(
    wall: &Wall,
    mon: (f32, f32),
    pos: (f32, f32),
    size: (u32, u32),
    pad: f32,
    sigma: f32,
) -> Capture {
    let (iw, ih) = (wall.w as f32, wall.h as f32);
    let k = (mon.0 / iw).max(mon.1 / ih);
    let ox = (iw * k - mon.0) / 2.0;
    let oy = (ih * k - mon.1) / 2.0;
    let out_w = (size.0 as f32 + 2.0 * pad).round() as u32;
    let out_h = (size.1 as f32 + 2.0 * pad).round() as u32;
    let mut bgra = vec![0u8; (out_w * out_h * 4) as usize];
    for j in 0..out_h {
        for i in 0..out_w {
            let sx = pos.0 - pad + i as f32;
            let sy = pos.1 - pad + j as f32;
            let c = sample_bilinear(wall, (sx + ox) / k, (sy + oy) / k);
            let o = ((j * out_w + i) * 4) as usize;
            bgra[o..o + 4].copy_from_slice(&c);
        }
    }
    let mut cap = Capture { bgra, w: out_w, h: out_h };
    blur(&mut cap, sigma);
    cap
}
