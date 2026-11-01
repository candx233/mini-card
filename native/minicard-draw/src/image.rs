//! B4 图片：WIC 解码（音乐封面 data URL / 文件）→ 预乘 BGRA → D2D 位图 + 圆角裁剪。
//!
//! 为什么用 WIC 而不是自己解 PNG：JPG/PNG/WebP(系统扩展) 都得吃，自己解一遍等于把
//! 格式解析、色彩管理、EXIF 方向全重做一遍；WIC 是系统组件，跟着系统更新。
//!
//! 口径：统一转 **32bppPBGRA**（预乘）——D2D 合成交换链是预乘的，直通 alpha 的位图
//! 上屏会在边缘出现白边/暗边（预乘与直通混用的经典症状）。

use std::cell::RefCell;

use windows::core::{Interface, Result, GUID};
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::Direct2D::{ID2D1Bitmap1, ID2D1Geometry};
use windows::Win32::Graphics::Imaging::{
    IWICBitmapFrameDecode, IWICBitmapSource, IWICFormatConverter, IWICImagingFactory, IWICStream,
    CLSID_WICImagingFactory2, GUID_WICPixelFormat32bppPBGRA, WICBitmapDitherTypeNone,
    WICBitmapPaletteTypeCustom, WICDecodeMetadataCacheOnLoad,
};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER, IStream};
use minicard_shell::gfx::Gfx;

/// 解出来的图：预乘 BGRA，行间无 padding。
#[derive(Clone, Debug)]
pub struct Image {
    pub w: u32,
    pub h: u32,
    pub bgra: Vec<u8>,
}

impl Image {
    pub fn stride(&self) -> usize {
        self.w as usize * 4
    }

    /// 取一个像素（预乘 BGRA）。
    pub fn px(&self, x: u32, y: u32) -> (u8, u8, u8, u8) {
        let i = (y as usize * self.w as usize + x as usize) * 4;
        (self.bgra[i], self.bgra[i + 1], self.bgra[i + 2], self.bgra[i + 3])
    }
}

thread_local! {
    static FACTORY: RefCell<Option<IWICImagingFactory>> = const { RefCell::new(None) };
}

fn factory() -> Result<IWICImagingFactory> {
    if let Some(f) = FACTORY.with(|c| c.borrow().clone()) {
        return Ok(f);
    }
    let f: IWICImagingFactory =
        unsafe { CoCreateInstance(&CLSID_WICImagingFactory2, None, CLSCTX_INPROC_SERVER)? };
    FACTORY.with(|c| *c.borrow_mut() = Some(f.clone()));
    Ok(f)
}

/// 从内存字节解码（PNG / JPG / 系统装了编码器的其它格式）。
///
/// 注意：`IWICStream::InitializeFromMemory` 只**借**这块内存，解码期间 `bytes` 必须活着 ——
/// 所以这里全程用切片，函数返回时流也一起销毁。
pub fn decode(bytes: &[u8]) -> Result<Image> {
    if bytes.is_empty() {
        return Err(windows::core::Error::from_thread());
    }
    let f = factory()?;
    unsafe {
        let stream: IWICStream = f.CreateStream()?;
        stream.InitializeFromMemory(bytes)?;
        let st: IStream = stream.cast()?;
        let dec = f.CreateDecoderFromStream(&st, std::ptr::null(), WICDecodeMetadataCacheOnLoad)?;
        let frame: IWICBitmapFrameDecode = dec.GetFrame(0)?;
        let conv: IWICFormatConverter = f.CreateFormatConverter()?;
        let src: IWICBitmapSource = frame.cast()?;
        conv.Initialize(
            &src,
            &GUID_WICPixelFormat32bppPBGRA as *const GUID,
            WICBitmapDitherTypeNone,
            None,
            0.0,
            WICBitmapPaletteTypeCustom,
        )?;
        let mut w = 0u32;
        let mut h = 0u32;
        conv.GetSize(&mut w, &mut h)?;
        if w == 0 || h == 0 {
            return Err(windows::core::Error::from_thread());
        }
        let stride = w * 4;
        let mut bgra = vec![0u8; (stride as usize) * (h as usize)];
        conv.CopyPixels(std::ptr::null(), stride, &mut bgra)?;
        Ok(Image { w, h, bgra })
    }
}

/// 从文件解码。
pub fn decode_file(path: &std::path::Path) -> Result<Image> {
    let bytes = std::fs::read(path).map_err(|_| windows::core::Error::from_thread())?;
    decode(&bytes)
}

/// 音乐卡收到的封面形态：`data:image/png;base64,…`。
pub fn decode_data_url(url: &str) -> Result<Image> {
    let raw = data_url_payload(url).ok_or_else(windows::core::Error::from_thread)?;
    let bytes = base64_decode(raw).ok_or_else(windows::core::Error::from_thread)?;
    decode(&bytes)
}

/// 取出 data URL 的 base64 载荷（不是 data URL / 不是 base64 就返回 None）。
pub fn data_url_payload(url: &str) -> Option<&str> {
    let rest = url.strip_prefix("data:")?;
    let (meta, payload) = rest.split_once(',')?;
    if !meta.contains(";base64") {
        return None;
    }
    Some(payload)
}

/// 标准 base64 解码（与 `minicard_core::base64_encode` 对偶）。容忍换行与缺省 padding。
pub fn base64_decode(s: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => (c - b'A') as u32,
            b'a'..=b'z' => (c - b'a') as u32 + 26,
            b'0'..=b'9' => (c - b'0') as u32 + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        })
    }
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let mut acc: u32 = 0;
    let mut bits = 0;
    let mut pad = 0;
    for &c in s.as_bytes() {
        if c == b'\n' || c == b'\r' || c == b' ' || c == b'\t' {
            continue;
        }
        if c == b'=' {
            pad += 1;
            continue;
        }
        let v = val(c)?;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((acc >> bits) & 0xFF) as u8);
        }
    }
    // 合法的 padding 只能是 1..=2，且残余位必须不足一个字节
    if pad > 2 || bits >= 6 {
        return None;
    }
    Some(out)
}

/// CSS `object-fit: cover`：把源图按比例放大到填满目标框、居中裁切 → 返回源矩形。
pub fn cover_src(img: &Image, dst_w: f32, dst_h: f32) -> D2D_RECT_F {
    cover_src_px(img.w as f32, img.h as f32, dst_w, dst_h)
}

/// 同 [`cover_src`]，但只用像素尺寸（位图已经上传、`Image` 不在了的时候用）。
pub fn cover_src_px(iw: f32, ih: f32, dst_w: f32, dst_h: f32) -> D2D_RECT_F {
    if iw <= 0.0 || ih <= 0.0 || dst_w <= 0.0 || dst_h <= 0.0 {
        return D2D_RECT_F { left: 0.0, top: 0.0, right: iw.max(0.0), bottom: ih.max(0.0) };
    }
    let scale = (dst_w / iw).max(dst_h / ih);
    let (sw, sh) = (dst_w / scale, dst_h / scale);
    let (left, top) = ((iw - sw) / 2.0, (ih - sh) / 2.0);
    D2D_RECT_F { left, top, right: left + sw, bottom: top + sh }
}

/// 上传成 D2D 位图（预乘 BGRA）。
pub fn bitmap(g: &Gfx, img: &Image) -> Result<ID2D1Bitmap1> {
    g.bitmap_from_bgra(&img.bgra, img.w, img.h)
}

/// 按源矩形画到目标矩形（不裁剪；圆角交给 `draw_round`）。
pub fn draw(g: &Gfx, bmp: &ID2D1Bitmap1, src: D2D_RECT_F, dst: D2D_RECT_F) -> Result<()> {
    unsafe {
        g.d2d.DrawBitmap(
            bmp,
            Some(&dst),
            1.0,
            windows::Win32::Graphics::Direct2D::D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC,
            Some(&src),
            None,
        )
    };
    Ok(())
}

/// 圆角裁剪后画（音乐卡封面：图缘直接到卡片圆角，不能有边框/缝隙）。
///
/// 用几何遮罩做层裁剪而不是「先画图再用四个角盖住」：遮罩走 D2D 的抗锯齿，
/// 圆角边缘是真正的亚像素过渡（盖角会在浅色壁纸上露出台阶）。
pub fn draw_round(
    g: &Gfx,
    bmp: &ID2D1Bitmap1,
    src: D2D_RECT_F,
    dst: D2D_RECT_F,
    radius: f32,
) -> Result<()> {
    let geo = unsafe {
        g.factory.CreateRoundedRectangleGeometry(&windows::Win32::Graphics::Direct2D::D2D1_ROUNDED_RECT {
            rect: dst,
            radiusX: radius,
            radiusY: radius,
        })?
    };
    let mask: ID2D1Geometry = geo.cast()?;
    unsafe {
        let layer = g.d2d.CreateLayer(None)?;
        let params = windows::Win32::Graphics::Direct2D::D2D1_LAYER_PARAMETERS1 {
            contentBounds: D2D_RECT_F {
                left: -1.0e6,
                top: -1.0e6,
                right: 1.0e6,
                bottom: 1.0e6,
            },
            geometricMask: std::mem::ManuallyDrop::new(Some(mask)),
            maskAntialiasMode: windows::Win32::Graphics::Direct2D::D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
            maskTransform: windows_numerics::Matrix3x2 {
                M11: 1.0,
                M12: 0.0,
                M21: 0.0,
                M22: 1.0,
                M31: 0.0,
                M32: 0.0,
            },
            opacity: 1.0,
            opacityBrush: std::mem::ManuallyDrop::new(None),
            layerOptions: windows::Win32::Graphics::Direct2D::D2D1_LAYER_OPTIONS1_NONE,
        };
        g.d2d.PushLayer(&params, &layer);
        let r = g.d2d.DrawBitmap(
            bmp,
            Some(&dst),
            1.0,
            windows::Win32::Graphics::Direct2D::D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC,
            Some(&src),
            None,
        );
        g.d2d.PopLayer();
        r
    };
    Ok(())
}


/// 旋转着画（音乐卡封面转动）。圆角遮罩与位图**在同一个变换下**推入层，
/// 所以遮罩跟着转 —— 否则转起来会看到「圆角不转、图像在方框里转」的穿帮。
pub fn draw_round_rotated(
    g: &Gfx,
    bmp: &ID2D1Bitmap1,
    src: D2D_RECT_F,
    dst: D2D_RECT_F,
    radius: f32,
    angle_rad: f32,
) -> Result<()> {
    let center = windows_numerics::Vector2 {
        X: (dst.left + dst.right) / 2.0,
        Y: (dst.top + dst.bottom) / 2.0,
    };
    unsafe {
        let mut old = windows_numerics::Matrix3x2::default();
        g.d2d.GetTransform(&mut old);
        let rot = crate::paint::rotation_rad(angle_rad, center.X, center.Y);
        let m = rot * old;
        g.d2d.SetTransform(&m);
        let r = draw_round(g, bmp, src, dst, radius);
        g.d2d.SetTransform(&old);
        r
    }
}

/// 把一张图按角度画到 dst（不裁圆角，给需要方形旋转的场合）。
pub fn draw_rotated(
    g: &Gfx,
    bmp: &ID2D1Bitmap1,
    src: D2D_RECT_F,
    dst: D2D_RECT_F,
    angle_rad: f32,
) -> Result<()> {
    let center = windows_numerics::Vector2 {
        X: (dst.left + dst.right) / 2.0,
        Y: (dst.top + dst.bottom) / 2.0,
    };
    unsafe {
        let mut old = windows_numerics::Matrix3x2::default();
        g.d2d.GetTransform(&mut old);
        let m = crate::paint::rotation_rad(angle_rad, center.X, center.Y) * old;
        g.d2d.SetTransform(&m);
        let r = draw(g, bmp, src, dst);
        g.d2d.SetTransform(&old);
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_往返() {
        use minicard_core::base64_encode;
        for len in 0..=8usize {
            let data: Vec<u8> = (0..len).map(|i| (i * 37 + 11) as u8).collect();
            let enc = base64_encode(&data);
            let dec = base64_decode(&enc).unwrap_or_else(|| panic!("解不出 len={len}"));
            assert_eq!(dec, data, "len={len} 往返不一致（编码 {enc}）");
        }
        // 一段真 PNG 头（含 + 与 / 的编码路径）
        let png = [0x89u8, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0xFE, 0xFF];
        let enc = base64_encode(&png);
        assert_eq!(base64_decode(&enc).unwrap(), png);
    }

    #[test]
    fn base64_容忍空白与非法字符() {
        assert_eq!(base64_decode("QUJD").unwrap(), b"ABC");
        assert_eq!(base64_decode("QU\nJD").unwrap(), b"ABC");
        assert_eq!(base64_decode("QUJD\n").unwrap(), b"ABC");
        assert_eq!(base64_decode("QUI=").unwrap(), b"AB");
        assert_eq!(base64_decode("QQ==").unwrap(), b"A");
        assert!(base64_decode("!!").is_none(), "非法字符应报错而不是静默产出空");
        assert!(base64_decode("Q").is_none(), "残余位不足一个字节也算错");
    }

    #[test]
    fn data_url_只认_base64() {
        assert_eq!(
            data_url_payload("data:image/png;base64,QUJD"),
            Some("QUJD")
        );
        assert_eq!(
            data_url_payload("data:image/jpeg;base64,AA=="),
            Some("AA==")
        );
        // 非 base64（URL 编码）不认
        assert_eq!(data_url_payload("data:text/plain,hello"), None);
        assert_eq!(data_url_payload("http://x/y.png"), None);
        assert_eq!(data_url_payload("QUJD"), None);
    }

    fn img(w: u32, h: u32) -> Image {
        Image { w, h, bgra: vec![0; (w * h * 4) as usize] }
    }

    #[test]
    fn cover_填满并居中裁切() {
        // 正方形图进 2:1 的框：左右不裁、上下裁（源矩形高 = 宽/2）
        let s = cover_src(&img(100, 100), 200.0, 100.0);
        assert!((s.left - 0.0).abs() < 0.01 && (s.right - 100.0).abs() < 0.01, "宽不裁 {:?}", s);
        assert!((s.top - 25.0).abs() < 0.01 && (s.bottom - 75.0).abs() < 0.01, "高居中裁 {:?}", s);

        // 宽图进正方形框：上下不裁、左右裁
        let s = cover_src(&img(200, 100), 100.0, 100.0);
        assert!((s.left - 50.0).abs() < 0.01 && (s.right - 150.0).abs() < 0.01, "{:?}", s);
        assert!((s.top - 0.0).abs() < 0.01 && (s.bottom - 100.0).abs() < 0.01, "{:?}", s);

        // 长宽比一致时一个像素都不裁
        let s = cover_src(&img(80, 40), 240.0, 120.0);
        assert!(
            (s.left).abs() < 0.01 && (s.top).abs() < 0.01 && (s.right - 80.0).abs() < 0.01,
            "{:?}",
            s
        );
    }

    /// 端到端：走**生产路径**（core 的 base64 编码 → `data:image/png;base64,…` → decode），
    /// 而不是手工拼 base64 字符串。2×2 红/绿/蓝/黄 PNG 就压在下面这行十六进制里。
    #[test]
    fn data_url_端到端解码() {
        // WIC 走 CoCreateInstance，测试进程里先把 COM 起起来（已初始化会返回 S_FALSE，忽略即可）
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(
                None,
                windows::Win32::System::Com::COINIT_APARTMENTTHREADED,
            );
        }
        const PNG: &[u8] = &[
            0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x02, 0x08, 0x02, 0x00, 0x00,
            0x00, 0xfd, 0xd4, 0x9a, 0x73, 0x00, 0x00, 0x00, 0x15, 0x49, 0x44, 0x41, 0x54, 0x78,
            0xda, 0x05, 0xc1, 0x01, 0x01, 0x00, 0x00, 0x00, 0x80, 0x10, 0xff, 0x4f, 0x17, 0x42,
            0x50, 0x19, 0x1e, 0xef, 0x04, 0xfc, 0x15, 0xda, 0x49, 0x30, 0x00, 0x00, 0x00, 0x00,
            0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
        ];
        let url = format!("data:image/png;base64,{}", minicard_core::base64_encode(PNG));
        let img = decode_data_url(&url).expect("data URL 应能解码");
        assert_eq!((img.w, img.h), (2, 2), "尺寸不对");
        // 预乘 BGRA：2×2 四色。注意 BGRA 顺序，且这是不透明图（a=255）
        assert_eq!(img.px(0, 0), (0, 0, 255, 255), "左上应红");
        assert_eq!(img.px(1, 0), (0, 255, 0, 255), "右上应绿");
        assert_eq!(img.px(0, 1), (255, 0, 0, 255), "左下应蓝");
        assert_eq!(img.px(1, 1), (0, 255, 255, 255), "右下应黄");
    }

    #[test]
    fn 空字节与坏数据要报错() {
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(
                None,
                windows::Win32::System::Com::COINIT_APARTMENTTHREADED,
            );
        }
        assert!(decode(&[]).is_err(), "空输入应报错");
        assert!(decode(b"not an image").is_err(), "垃圾数据应报错");
        assert!(decode_data_url("data:image/png;base64,bm90IGFuIGltYWdl").is_err());
    }

    #[test]
    fn 旋转按弧度进_九十度就是转四分之一圈() {
        let m = crate::paint::rotation_rad(std::f32::consts::FRAC_PI_2, 0.0, 0.0);
        // D2D 行向量口径、y 向下：θ=90° → M11=cos=0、M12=sin=1、M21=-1、M22=0
        assert!(m.M11.abs() < 1e-5, "M11={}", m.M11);
        assert!((m.M12 - 1.0).abs() < 1e-5, "M12={}（≈1 说明真的是 90°，不是 90/57.3）", m.M12);
        assert!((m.M21 + 1.0).abs() < 1e-5, "M21={}", m.M21);
        assert!(m.M22.abs() < 1e-5, "M22={}", m.M22);
        // 绕点旋转：中心点的位移应为 0
        let c = crate::paint::rotation_rad(std::f32::consts::FRAC_PI_2, 50.0, 80.0);
        // 把中心点 (50,80) 用行向量变换回去，应原地不动
        let (x, y) = (50.0, 80.0);
        let tx = x * c.M11 + y * c.M21 + c.M31;
        let ty = x * c.M12 + y * c.M22 + c.M32;
        assert!((tx - 50.0).abs() < 1e-4 && (ty - 80.0).abs() < 1e-4, "中心被移动了 ({tx},{ty})");
    }

    #[test]
    fn px_按行优先取() {
        let mut im = img(3, 2);
        im.bgra[0] = 1;
        im.bgra[(2 * 4) + 3] = 9; // 第 0 行第 2 像素的 alpha
        im.bgra[(3 * 4) + 1] = 7; // 第 1 行第 0 像素的 G
        assert_eq!(im.px(0, 0), (1, 0, 0, 0));
        assert_eq!(im.px(2, 0), (0, 0, 0, 9));
        assert_eq!(im.px(0, 1), (0, 7, 0, 0));
        assert_eq!(im.stride(), 12);
    }
}
/// 把圆角烘进预乘 BGRA 的 alpha（4×4 超采样）。
///
/// 位图上传后没法再套几何裁剪层（paint 里已经在用同一层做窗口圆角裁剪），
/// 所以封面这类"带圆角的图"在解码时就把角磨掉，画的时候直接铺 —— 一次开销，之后每帧零成本。
pub fn bake_round_alpha(img: &mut Image, radius: f32) {
    if radius <= 0.0 {
        return;
    }
    let (w, h) = (img.w as f32, img.h as f32);
    let r = radius.min(w / 2.0).min(h / 2.0);
    let stride = img.stride();
    // 圆角矩形 SDF（负值 = 内部）
    let sd = |x: f32, y: f32| -> f32 {
        let (cx, cy) = (x - w / 2.0, y - h / 2.0);
        let qx = cx.abs() - (w / 2.0 - r);
        let qy = cy.abs() - (h / 2.0 - r);
        let ax = qx.max(0.0);
        let ay = qy.max(0.0);
        (ax * ax + ay * ay).sqrt() + qx.max(qy).min(0.0) - r
    };
    for py in 0..img.h {
        for px in 0..img.w {
            let mut cover = 0.0f32;
            for sy in 0..4 {
                for sx in 0..4 {
                    let x = px as f32 + (sx as f32 + 0.5) / 4.0;
                    let y = py as f32 + (sy as f32 + 0.5) / 4.0;
                    if sd(x, y) <= 0.0 {
                        cover += 1.0 / 16.0;
                    }
                }
            }
            if cover >= 0.999 {
                continue;
            }
            let o = py as usize * stride + px as usize * 4;
            for c in 0..4 {
                let v = (img.bgra[o + c] as f32 * cover).round() as u8;
                img.bgra[o + c] = v;
            }
        }
    }
}

/// 按 `object-fit: cover` 把图缩到目标像素尺寸（双线性 + 居中裁切）。
///
/// 为什么要先缩再上传：圆角得在**目标尺寸**上烘（cover 裁切会切掉原图四角，
/// 在原图上烘的角根本不是画出来的那个角）。
pub fn resize_cover(img: &Image, dst_w: u32, dst_h: u32) -> Image {
    let (dw, dh) = (dst_w.max(1), dst_h.max(1));
    let src = cover_src_px(img.w as f32, img.h as f32, dw as f32, dh as f32);
    let (sx, sy) = (
        dw as f32 / (src.right - src.left).max(1e-3),
        dh as f32 / (src.bottom - src.top).max(1e-3),
    );
    let mut out = Image { bgra: vec![0u8; (dw * dh * 4) as usize], w: dw, h: dh };
    let stride = img.stride();
    for y in 0..dh {
        for x in 0..dw {
            let u = src.left + (x as f32 + 0.5) / sx - 0.5;
            let v = src.top + (y as f32 + 0.5) / sy - 0.5;
            let x0 = u.floor().max(0.0) as usize;
            let y0 = v.floor().max(0.0) as usize;
            let x1 = (x0 + 1).min(img.w as usize - 1);
            let y1 = (y0 + 1).min(img.h as usize - 1);
            let fx = (u - x0 as f32).clamp(0.0, 1.0);
            let fy = (v - y0 as f32).clamp(0.0, 1.0);
            let px = |xx: usize, yy: usize, c: usize| img.bgra[yy * stride + xx * 4 + c] as f32;
            let o = (y * dw + x) as usize * 4;
            for c in 0..4 {
                let a = px(x0, y0, c) * (1.0 - fx) + px(x1, y0, c) * fx;
                let b = px(x0, y1, c) * (1.0 - fx) + px(x1, y1, c) * fx;
                out.bgra[o + c] = (a * (1.0 - fy) + b * fy).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    out
}

#[cfg(test)]
mod cover_tests {
    use super::*;

    /// 纯色图 + 圆角烘 alpha：中心不透明、四角透明
    #[test]
    fn cover_resize_and_round_alpha() {
        // 预乘 BGRA，A=255（不透明）
        let mut bgra = vec![0u8; 300 * 300 * 4];
        for px in bgra.chunks_mut(4) {
            px[0] = 128;
            px[1] = 128;
            px[2] = 128;
            px[3] = 255;
        }
        let mut img = Image { w: 300, h: 300, bgra };
        let mut small = resize_cover(&img, 100, 100);
        assert_eq!((small.w, small.h), (100, 100));
        bake_round_alpha(&mut small, 12.0);
        let a = |x: usize, y: usize| small.bgra[y * small.stride() + x * 4 + 3];
        assert!(a(50, 50) > 250, "center must be opaque, got {}", a(50, 50));
        assert_eq!(a(0, 0), 0, "top-left corner must be cut");
        assert_eq!(a(99, 0), 0, "top-right corner must be cut");
        assert_eq!(a(0, 99), 0, "bottom-left corner must be cut");
        assert!(a(50, 2) > 200, "top edge middle should stay opaque");
        let _ = &mut img;
    }
}
