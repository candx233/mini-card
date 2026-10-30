//! 呈现层：D3D11 → DXGI 合成交换链（逐像素 alpha）→ DirectComposition 视觉树 → Direct2D 绘制。
//!
//! 与 §3.1 主路线一致：窗口 WS_EX_NOREDIRECTIONBITMAP（无重定向表面），内容由合成器直接合成，
//! 所以 alpha 是真的逐像素 alpha，不是分层窗口的整窗透明度。

use windows::core::{Interface, Result};
use windows_numerics::Matrix3x2;
use windows::Win32::Foundation::{HMODULE, HWND};
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_10_0, D3D_FEATURE_LEVEL_10_1,
    D3D_FEATURE_LEVEL_11_0,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAPPED_SUBRESOURCE,
    D3D11_MAP_READ, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::DirectComposition::{
    DCompositionCreateDevice2, IDCompositionDevice, IDCompositionTarget, IDCompositionVisual,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    IDXGIDevice, IDXGIFactory2, IDXGISurface, IDXGISwapChain1, DXGI_PRESENT,
    DXGI_SCALING_STRETCH, DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
    DXGI_USAGE_RENDER_TARGET_OUTPUT,
};

pub struct Gfx {
    pub d2d: ID2D1DeviceContext,
    #[allow(dead_code)]
    pub factory: ID2D1Factory1,
    device: ID3D11Device,
    ctx11: ID3D11DeviceContext,
    pub brush: ID2D1SolidColorBrush,
    pub swap: IDXGISwapChain1,
    target: ID2D1Bitmap1,
    _comp: IDCompositionDevice,
    _target: IDCompositionTarget,
    _visual: IDCompositionVisual,
    layer: ID2D1Layer,
    clip: ID2D1Geometry,
    pub w: f32, // DIPs
    pub h: f32,
    pub dpi: f32,
    /// 设了就把每帧的面板缓冲读回来落盘（BGRA 原始像素 + `<path>.txt` 里的 "宽x高"）
    pub dump: Option<std::path::PathBuf>,
    /// 连续帧序列目录 + 剩余帧数（动效取证用）
    pub dump_seq: Option<std::path::PathBuf>,
    pub dump_seq_left: std::cell::Cell<u32>,
    /// 上一帧耗时分解（毫秒）：D2D 绘制结束 / Present 等待
    pub t_end_raw: std::cell::Cell<f32>,
    pub t_present: std::cell::Cell<f32>,
}

impl Gfx {
    pub fn new(hwnd: HWND, w_px: u32, h_px: u32, dpi: f32, radius_dip: f32) -> Result<Self> {
        unsafe {
            let levels = [
                D3D_FEATURE_LEVEL_11_0,
                D3D_FEATURE_LEVEL_10_1,
                D3D_FEATURE_LEVEL_10_0,
            ];
            let mut device = None;
            let mut ctx11 = None;
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&levels),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut ctx11),
            )?;
            let device: ID3D11Device = device.unwrap();
            let ctx11: ID3D11DeviceContext = ctx11.unwrap();
            let dxgi_dev: IDXGIDevice = device.cast()?;
            let adapter = dxgi_dev.GetAdapter()?;
            let factory2: IDXGIFactory2 = adapter.GetParent()?;

            let desc = DXGI_SWAP_CHAIN_DESC1 {
                Width: w_px,
                Height: h_px,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                Stereo: false.into(),
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: 2,
                Scaling: DXGI_SCALING_STRETCH,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
                AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
                Flags: 0,
            };
            let swap = factory2.CreateSwapChainForComposition(&device, &desc, None)?;

            let factory: ID2D1Factory1 =
                D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let d2d_device = factory.CreateDevice(&dxgi_dev)?;
            let d2d = d2d_device.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)?;
            d2d.SetDpi(dpi, dpi);
            d2d.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            d2d.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);

            let surface: IDXGISurface = swap.GetBuffer(0)?;
            let props = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: dpi,
                dpiY: dpi,
                bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
                colorContext: Default::default(),
            };
            let target = d2d.CreateBitmapFromDxgiSurface(&surface, Some(&props))?;
            d2d.SetTarget(&target);

            let comp: IDCompositionDevice = DCompositionCreateDevice2(&dxgi_dev)?;
            let comp_target = comp.CreateTargetForHwnd(hwnd, true)?;
            let visual = comp.CreateVisual()?;
            visual.SetContent(&swap)?;
            comp_target.SetRoot(&visual)?;
            comp.Commit()?;

            let brush = d2d.CreateSolidColorBrush(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 1.0 }, None)?;
            let layer = d2d.CreateLayer(None)?;
            let w = w_px as f32 * 96.0 / dpi;
            let h = h_px as f32 * 96.0 / dpi;
            let clip_geo = factory.CreateRoundedRectangleGeometry(&D2D1_ROUNDED_RECT {
                rect: D2D_RECT_F { left: 0.0, top: 0.0, right: w, bottom: h },
                radiusX: radius_dip,
                radiusY: radius_dip,
            })?;
            let clip: ID2D1Geometry = clip_geo.cast()?;

            Ok(Self {
                d2d,
                device,
                ctx11,
                factory,
                brush,
                swap,
                target,
                _comp: comp,
                _target: comp_target,
                _visual: visual,
                layer,
                clip,
                w,
                h,
                dpi,
                dump: None,
                dump_seq: None,
                dump_seq_left: std::cell::Cell::new(0),
                t_end_raw: std::cell::Cell::new(0.0),
                t_present: std::cell::Cell::new(0.0),
            })
        }
    }

    pub fn begin(&self) -> Result<()> {
        unsafe {
            self.d2d.BeginDraw();
            self.d2d.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }));
            Ok(())
        }
    }

    pub fn end(&self) -> Result<()> {
        unsafe {
            let t0 = std::time::Instant::now();
            self.d2d.EndDraw(None, None)?;
            self.t_end_raw.set(t0.elapsed().as_secs_f32() * 1000.0);
            if let Some(p) = &self.dump {
                if let Err(e) = self.write_frame(p) {
                    crate::mlog!("读回失败: {e:?}");
                }
            }
            if let Some(dir) = &self.dump_seq {
                let left = self.dump_seq_left.get();
                if left > 0 {
                    let p = dir.join(format!("f{:03}.bgra", 400 - left));
                    if let Err(e) = self.write_frame(&p) {
                        crate::mlog!("序列读回失败: {e:?}");
                    }
                    self.dump_seq_left.set(left - 1);
                }
            }
            let t1 = std::time::Instant::now();
            self.swap.Present(1, DXGI_PRESENT(0)).ok()?;
            self.t_present.set(t1.elapsed().as_secs_f32() * 1000.0);
            Ok(())
        }
    }

    /// 把当前后台缓冲读回 CPU（staging texture），用于离屏取证（M1 的「渲成 PNG 比对」前置）
    fn write_frame(&self, path: &std::path::Path) -> Result<()> {
        unsafe {
            let tex: ID3D11Texture2D = self.swap.GetBuffer(0)?;
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            tex.GetDesc(&mut desc);
            let staging_desc = D3D11_TEXTURE2D_DESC {
                Width: desc.Width,
                Height: desc.Height,
                MipLevels: 1,
                ArraySize: 1,
                Format: desc.Format,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
            };
            let mut staging = None;
            self.device.CreateTexture2D(&staging_desc, None, Some(&mut staging))?;
            let staging = staging.unwrap();
            self.ctx11.CopyResource(&staging, &tex);
            let mut m = D3D11_MAPPED_SUBRESOURCE::default();
            self.ctx11.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut m))?;
            let (w, h) = (desc.Width as usize, desc.Height as usize);
            let mut out = vec![0u8; w * h * 4];
            for y in 0..h {
                let src = (m.pData as *const u8).add(y * m.RowPitch as usize);
                std::ptr::copy_nonoverlapping(src, out.as_mut_ptr().add(y * w * 4), w * 4);
            }
            self.ctx11.Unmap(&staging, 0);
            std::fs::write(path, &out)?;
            std::fs::write(path.with_extension("txt"), format!("{w}x{h}"))?;
            Ok(())
        }
    }

    pub fn push_clip(&self) -> Result<()> {
        unsafe {
            let params = D2D1_LAYER_PARAMETERS1 {
                contentBounds: D2D_RECT_F { left: 0.0, top: 0.0, right: self.w, bottom: self.h },
                geometricMask: core::mem::ManuallyDrop::new(Some(self.clip.clone())),
                maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
                maskTransform: Matrix3x2::identity(),
                opacity: 1.0,
                opacityBrush: core::mem::ManuallyDrop::new(None),
                layerOptions: D2D1_LAYER_OPTIONS1_NONE,
            };
            self.d2d.PushLayer(&params, &self.layer);
            Ok(())
        }
    }

    pub fn pop_clip(&self) {
        unsafe { self.d2d.PopLayer() };
    }

    /// 屏幕截图 → D2D 位图（dpi 与窗口一致，1 物理像素 = 1 DIP 的 96/dpi 倍）
    pub fn bitmap_from_bgra(&self, bgra: &[u8], w: u32, h: u32) -> Result<ID2D1Bitmap1> {
        unsafe {
            let props = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: self.dpi,
                dpiY: self.dpi,
                bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
                colorContext: Default::default(),
            };
            self.d2d.CreateBitmap(
                D2D_SIZE_U { width: w, height: h },
                Some(bgra.as_ptr() as *const core::ffi::c_void),
                w * 4,
                &props,
            )
        }
    }

    pub fn set_color(&self, c: D2D1_COLOR_F) -> Result<()> {
        unsafe { self.brush.SetColor(&c) }
        Ok(())
    }

    pub fn fill_round(&self, rect: D2D_RECT_F, radius: f32) -> Result<()> {
        unsafe {
            self.d2d.FillRoundedRectangle(
                &D2D1_ROUNDED_RECT { rect, radiusX: radius, radiusY: radius },
                &self.brush,
            );
            Ok(())
        }
    }

    pub fn stroke_round(&self, rect: D2D_RECT_F, radius: f32, width: f32) -> Result<()> {
        unsafe {
            self.d2d.DrawRoundedRectangle(
                &D2D1_ROUNDED_RECT { rect, radiusX: radius, radiusY: radius },
                &self.brush,
                width,
                None,
            );
            Ok(())
        }
    }

    #[allow(dead_code)]
    pub fn target_bitmap(&self) -> &ID2D1Bitmap1 {
        &self.target
    }
}
