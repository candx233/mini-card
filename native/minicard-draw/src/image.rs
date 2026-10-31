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
    let (iw, ih) = (img.w as f32, img.h as f32);
    if iw <= 0.0 || ih <= 0.0 || dst_w <= 0.0 || dst_h <= 0.0 {
        return D2D_RECT_F { left: 0.0, top: 0.0, right: iw, bottom: ih };
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
