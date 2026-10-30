//! 文字层：DirectWrite + 私有字体集（AddFontFile → 自建 font collection，不写注册表、不装系统字体）。
//!
//! 行盒口径按 CSS 复刻：line-height = size × lh；内容区 = (ascent+descent)×size；
//! baseline = (lineHeight − 内容区)/2 + ascent×size（half-leading）。这样 1.15 这种紧行高
//! 的字会像浏览器一样「略微溢出自己的行盒」，而不是被 DWrite 的默认行距顶下去。

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;

use windows::core::{Interface, Result, HSTRING, PCWSTR};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteFactory3, IDWriteFactory5, IDWriteFontCollection,
    IDWriteFontCollection1, IDWriteFontSetBuilder, IDWriteFontSetBuilder1, IDWriteFontSetBuilder2,
    IDWriteTextFormat, IDWriteTextLayout, IDWriteTypography, DWRITE_FACTORY_TYPE_SHARED,
    DWRITE_FONT_FEATURE, DWRITE_FONT_FEATURE_TAG_TABULAR_FIGURES, DWRITE_FONT_METRICS,
    DWRITE_TEXT_RANGE,
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT_NORMAL,
    DWRITE_TEXT_METRICS,
    DWRITE_LINE_SPACING_METHOD_UNIFORM, DWRITE_PARAGRAPH_ALIGNMENT_NEAR, DWRITE_TEXT_ALIGNMENT,
    DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_ALIGNMENT_TRAILING, DWRITE_WORD_WRAPPING_NO_WRAP,
};

pub struct TextEngine {
    pub factory: IDWriteFactory,
    pub collection: IDWriteFontCollection,
    pub family: HSTRING,
    /// 字体度量（单位 = em）
    pub ascent: f32,
    pub descent: f32,
    pub upem: u16,
    /// 等宽数字（.num 的 font-variant-numeric: tabular-nums）
    typo: Option<IDWriteTypography>,
    /// 缓存：文本格式（size/lineBox/align）与文本布局（align/text）—— 每帧重建 format/layout 是主要 CPU 开销
    fmt_cache: RefCell<HashMap<(u32, u32, u8), IDWriteTextFormat>>,
    lay_cache: RefCell<HashMap<(u8, String, u32, u32), IDWriteTextLayout>>,
}

impl TextEngine {
    pub fn new(font_path: &Path, family: &str) -> Result<Self> {
        unsafe {
            let factory: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let f5: IDWriteFactory5 = factory.cast()?;
            let b1: IDWriteFontSetBuilder1 = f5.CreateFontSetBuilder()?;
            let b2: IDWriteFontSetBuilder2 = b1.cast()?;
            b2.AddFontFile(&HSTRING::from(font_path.to_string_lossy().as_ref()))?;
            let b0: IDWriteFontSetBuilder = b2.cast()?;
            let set = b0.CreateFontSet()?;
            let f3: IDWriteFactory3 = factory.cast()?;
            let coll1: IDWriteFontCollection1 = f3.CreateFontCollectionFromFontSet(&set)?;
            let collection: IDWriteFontCollection = coll1.cast()?;

            let fam = HSTRING::from(family);
            let mut idx = 0u32;
            let mut exists = windows::core::BOOL(0);
            let _ = collection.FindFamilyName(&fam, &mut idx, &mut exists);
            if !exists.as_bool() {
                return Err(windows::core::Error::new(
                    windows::Win32::Foundation::E_FAIL,
                    format!("字体集里没有 family «{family}»"),
                ));
            }
            let family_obj = collection.GetFontFamily(idx)?;
            let font = family_obj.GetFirstMatchingFont(
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                DWRITE_FONT_STYLE_NORMAL,
            )?;
            let mut m = DWRITE_FONT_METRICS::default();
            font.GetMetrics(&mut m);
            let typo = factory.CreateTypography().ok();
            if let Some(tp) = &typo {
                let _ = tp.AddFontFeature(DWRITE_FONT_FEATURE {
                    nameTag: DWRITE_FONT_FEATURE_TAG_TABULAR_FIGURES,
                    parameter: 1,
                });
            }
            let upem = m.designUnitsPerEm.max(1);
            Ok(Self {
                factory,
                collection,
                family: fam,
                typo,
                fmt_cache: RefCell::new(HashMap::new()),
                lay_cache: RefCell::new(HashMap::new()),
                ascent: m.ascent as f32 / upem as f32,
                descent: m.descent as f32 / upem as f32,
                upem,
            })
        }
    }

    /// 建一个「行盒高度 / baseline 已钉死」的文本格式（CSS 复刻的关键）。
    pub fn format(
        &self,
        size: f32,
        line_box: f32,
        align: DWRITE_TEXT_ALIGNMENT,
    ) -> Result<IDWriteTextFormat> {
        unsafe {
            let fmt = self.factory.CreateTextFormat(
                &self.family,
                &self.collection,
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                size,
                &HSTRING::from("zh-CN"),
            )?;
            let content = (self.ascent + self.descent) * size;
            let baseline = (line_box - content) / 2.0 + self.ascent * size;
            fmt.SetLineSpacing(DWRITE_LINE_SPACING_METHOD_UNIFORM, line_box, baseline)?;
            fmt.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR)?;
            fmt.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
            fmt.SetTextAlignment(align)?;
            Ok(fmt)
        }
    }

    #[allow(dead_code)]
    pub fn leading(&self, size: f32, line_box: f32) -> Result<IDWriteTextFormat> {
        self.format(size, line_box, DWRITE_TEXT_ALIGNMENT_LEADING)
    }

    #[allow(dead_code)]
    pub fn layout(
        &self,
        fmt: &IDWriteTextFormat,
        text: &str,
        width: f32,
        height: f32,
    ) -> Result<IDWriteTextLayout> {
        self.layout_num(fmt, text, width, height, false)
    }

    /// tabular = 数字等宽（对应 CSS 的 font-variant-numeric: tabular-nums）
    pub fn layout_num(
        &self,
        fmt: &IDWriteTextFormat,
        text: &str,
        width: f32,
        height: f32,
        tabular: bool,
    ) -> Result<IDWriteTextLayout> {
        let wide: Vec<u16> = text.encode_utf16().collect();
        unsafe {
            let lay = self.factory.CreateTextLayout(&wide, fmt, width, height)?;
            if tabular {
                if let Some(tp) = &self.typo {
                    let _ = lay.SetTypography(tp, DWRITE_TEXT_RANGE {
                        startPosition: 0,
                        length: wide.len() as u32,
                    });
                }
            }
            Ok(lay)
        }
    }

    /// 带缓存的格式（同尺寸同对齐只建一次）
    pub fn format_cached(&self, size: f32, line_box: f32, align: u8) -> Result<IDWriteTextFormat> {
        let key = (size.to_bits(), line_box.to_bits(), align);
        if let Some(f) = self.fmt_cache.borrow().get(&key) {
            return Ok(f.clone());
        }
        let a = if align == 0 {
            DWRITE_TEXT_ALIGNMENT_LEADING
        } else {
            DWRITE_TEXT_ALIGNMENT_TRAILING
        };
        let f = self.format(size, line_box, a)?;
        self.fmt_cache.borrow_mut().insert(key, f.clone());
        Ok(f)
    }

    /// 带缓存的布局（文本没变就复用；表满就整表清空，避免每秒新文本把表撑爆）
    pub fn layout_cached(
        &self,
        align: u8,
        fmt: &IDWriteTextFormat,
        text: &str,
        width: f32,
        height: f32,
        tabular: bool,
    ) -> Result<IDWriteTextLayout> {
        let key = (align, text.to_owned(), width.to_bits(), height.to_bits());
        if let Some(l) = self.lay_cache.borrow().get(&key) {
            return Ok(l.clone());
        }
        let l = self.layout_num(fmt, text, width, height, tabular)?;
        let mut c = self.lay_cache.borrow_mut();
        if c.len() > 64 {
            c.clear();
        }
        c.insert(key, l.clone());
        Ok(l)
    }

    #[allow(dead_code)]
    pub fn family_pcwstr(&self) -> PCWSTR {
        PCWSTR(self.family.as_ptr())
    }
}

/// 布局层的度量来源：线上用的就是这一份（测试里换成假字体）。
///
/// 宽高口径与绘制一致 —— 同一个 `format_cached`（行盒已钉死）＋ `layout_cached`，
/// 所以「量出来的高」和「画出来的高」天然一致，不会差半个行距。
impl crate::layout::Measurer for TextEngine {
    fn text(&mut self, t: &crate::layout::TextSpec, max_w: f32) -> (f32, f32) {
        let fmt = match self.format_cached(t.size, t.line_h, 0) {
            Ok(f) => f,
            Err(_) => return (0.0, t.line_h),
        };
        // max_w <= 0 = 不限宽：给个足够大的盒子，让单行自然排开
        let (w, h) = if max_w > 0.0 {
            (max_w, 4096.0)
        } else {
            (4096.0, t.line_h * 1.5)
        };
        match self.layout_cached(0, &fmt, &t.s, w, h, false) {
            Ok(l) => unsafe {
                let mut m = DWRITE_TEXT_METRICS::default();
                match l.GetMetrics(&mut m) {
                    Ok(()) => (m.width, m.height),
                    Err(_) => (0.0, t.line_h),
                }
            },
            Err(_) => (0.0, t.line_h),
        }
    }

    fn ascent(&mut self, t: &crate::layout::TextSpec) -> f32 {
        self.ascent * t.size
    }
}
