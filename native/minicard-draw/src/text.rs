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
    DWRITE_TEXT_METRICS, DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER,
    DWRITE_WORD_WRAPPING_WRAP,
    DWRITE_LINE_SPACING_METHOD_UNIFORM, DWRITE_PARAGRAPH_ALIGNMENT_NEAR, DWRITE_TEXT_ALIGNMENT,
    DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_ALIGNMENT_TRAILING, DWRITE_WORD_WRAPPING_NO_WRAP,
};

/// 排版模式：对应 CSS 的 `white-space` / `text-overflow` 组合。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// `white-space:nowrap`（不换行、不省略；溢出靠裁剪收）
    Clip = 0,
    /// `white-space:normal`（自动换行）
    Wrap = 1,
    /// `nowrap + text-overflow:ellipsis`（末尾三点）
    Ellipsis = 2,
}

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
    /// 缓存：文本格式（size/lineBox/align/mode）与文本布局（align/text/宽/高/mode）
    /// —— 每帧重建 format/layout 是主要 CPU 开销
    fmt_cache: RefCell<HashMap<(u32, u32, u8, u8), IDWriteTextFormat>>,
    lay_cache: RefCell<HashMap<(u8, String, u32, u32, u8), IDWriteTextLayout>>,
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
        self.format_mode(size, line_box, align, Mode::Clip)
    }

    /// 按 CSS 的 `white-space` / `text-overflow` 组合取格式：
    /// - `Clip`：`white-space:nowrap`（溢出画出去，靠裁剪收）
    /// - `Wrap`：`white-space:normal`
    /// - `Ellipsis`：`nowrap + text-overflow:ellipsis`（末尾三点，粒度 = 字符）
    pub fn format_mode(
        &self,
        size: f32,
        line_box: f32,
        align: u8,
        mode: Mode,
    ) -> Result<IDWriteTextFormat> {
        let key = (size.to_bits(), line_box.to_bits(), align, mode as u8);
        if let Some(f) = self.fmt_cache.borrow().get(&key) {
            return Ok(f.clone());
        }
        let a = match align {
            1 => DWRITE_TEXT_ALIGNMENT_TRAILING,
            2 => DWRITE_TEXT_ALIGNMENT_CENTER,
            _ => DWRITE_TEXT_ALIGNMENT_LEADING,
        };
        let f = self.format(size, line_box, a)?;
        unsafe {
            match mode {
                Mode::Clip => {}
                Mode::Wrap => f.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP)?,
                Mode::Ellipsis => {
                    // 粒度用「字符」而不是「词」：中文没有空格，按词粒度会整段消失
                    let sign = self.factory.CreateEllipsisTrimmingSign(&f)?;
                    f.SetTrimming(
                        &DWRITE_TRIMMING {
                            granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
                            delimiter: 0,
                            delimiterCount: 0,
                        },
                        &sign,
                    )?;
                }
            }
        }
        self.fmt_cache.borrow_mut().insert(key, f.clone());
        Ok(f)
    }

    /// 带模式的布局（缓存键含模式与包围盒，缺一个都会串——见 `布局缓存键必须含宽度`）。
    #[allow(clippy::too_many_arguments)]
    pub fn layout_mode(
        &self,
        align: u8,
        fmt: &IDWriteTextFormat,
        text: &str,
        width: f32,
        height: f32,
        tabular: bool,
        mode: Mode,
    ) -> Result<IDWriteTextLayout> {
        let key = (align, text.to_owned(), width.to_bits(), height.to_bits(), mode as u8);
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
        self.layout_mode(align, fmt, text, width, height, tabular, Mode::Clip)
    }

    #[allow(dead_code)]
    pub fn family_pcwstr(&self) -> PCWSTR {
        PCWSTR(self.family.as_ptr())
    }
}

impl TextEngine {
    /// 度量一段文字（按 `max_w` 换行后的宽高）。`Measurer` 的两个 impl 都走这里，
    /// 免得「只读引用」与「可变引用」两套实现漂移。
    pub fn measure(&self, t: &crate::layout::TextSpec, max_w: f32) -> (f32, f32) {
        let mode = if t.wrap && max_w > 0.0 { Mode::Wrap } else { Mode::Clip };
        let fmt = match self.format_mode(t.size, t.line_h, t.align, mode) {
            Ok(f) => f,
            Err(_) => return (0.0, t.line_h),
        };
        let (w, h) = if max_w > 0.0 { (max_w, 4096.0) } else { (4096.0, t.line_h * 1.5) };
        match self.layout_mode(t.align, &fmt, &t.s, w, h, t.tabular, mode) {
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

    pub fn ascent_of(&self, t: &crate::layout::TextSpec) -> f32 {
        self.ascent * t.size
    }
}

/// 绘制层手里只有 `&TextEngine`（没有所有权），没必要为「可变性」把所有调用点改 `&mut`。
impl crate::layout::Measurer for &TextEngine {
    fn text(&mut self, t: &crate::layout::TextSpec, max_w: f32) -> (f32, f32) {
        TextEngine::measure(self, t, max_w)
    }
    fn ascent(&mut self, t: &crate::layout::TextSpec) -> f32 {
        TextEngine::ascent_of(self, t)
    }
}

/// 拥有所有权的 `TextEngine` 也能当度量器（测试与需要 `&mut dyn Measurer` 的场合）。
impl crate::layout::Measurer for TextEngine {
    fn text(&mut self, t: &crate::layout::TextSpec, max_w: f32) -> (f32, f32) {
        self.measure(t, max_w)
    }
    fn ascent(&mut self, t: &crate::layout::TextSpec) -> f32 {
        self.ascent_of(t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine() -> Option<TextEngine> {
        let p = std::path::Path::new("../assets/fonts/MiSans-Regular.ttf");
        if !p.exists() {
            eprintln!("跳过：字体不在 {}（先跑 assets/fonts/build_misans_ttf.py）", p.display());
            return None;
        }
        Some(TextEngine::new(p, "MiSans").unwrap())
    }

    fn metrics(l: &IDWriteTextLayout) -> (f32, f32, u32) {
        unsafe {
            let mut m = DWRITE_TEXT_METRICS::default();
            l.GetMetrics(&mut m).unwrap();
            (m.width, m.height, m.lineCount)
        }
    }

    /// 缓存键必须含宽度：同一段文字在同一对齐下，两个可用宽度要拿到各自的布局。
    ///
    /// 这条不是理论洁癖 —— `align=1`（trailing）的绘制是「按布局盒右缘右对齐」，
    /// 性能卡的百分比就是这么画的：先被度量按 4096 宽建过一次，再按 220 宽绘制时若
    /// 命中旧布局，数字会被右对齐到 4096 的位置，整块飞出卡片（看着像「数值消失了」）。
    #[test]
    fn 布局缓存键必须含宽度() {
        let Some(te) = engine() else { return };
        let fmt = te.format_cached(13.125, 15.09375, 1).unwrap();
        let wide = te.layout_cached(1, &fmt, "27%", 4096.0, 20.0, true).unwrap();
        let narrow = te.layout_cached(1, &fmt, "27%", 60.0, 20.0, true).unwrap();
        let (w_wide, w_narrow) = unsafe { (wide.GetMaxWidth(), narrow.GetMaxWidth()) };
        assert!((w_wide - 4096.0).abs() < 0.01, "宽盒子布局宽 = {w_wide}");
        assert!(
            (w_narrow - 60.0).abs() < 0.01,
            "窄盒子拿到旧布局（宽 {w_narrow}）= 缓存键漏了宽度"
        );
    }

    const LONG: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZ";
    const SIZE: f32 = 13.125;
    const LH: f32 = 15.09375;

    #[test]
    fn 不换行模式永远单行且宽度溢出() {
        let Some(te) = engine() else { return };
        let fmt = te.format_mode(SIZE, LH, 0, Mode::Clip).unwrap();
        let l = te.layout_mode(0, &fmt, LONG, 60.0, LH, false, Mode::Clip).unwrap();
        let (w, _, lines) = metrics(&l);
        assert_eq!(lines, 1, "nowrap 必须单行");
        assert!(w > 60.0, "溢出时报告的自然宽应大于盒宽，实得 {w}");
    }

    #[test]
    fn 换行模式按盒宽折行且高等于行数乘行高() {
        let Some(te) = engine() else { return };
        let fmt = te.format_mode(SIZE, LH, 0, Mode::Wrap).unwrap();
        let l = te.layout_mode(0, &fmt, LONG, 60.0, 400.0, false, Mode::Wrap).unwrap();
        let (w, h, lines) = metrics(&l);
        assert!(lines >= 2, "60px 宽的盒子装不下 26 个字母，行数 {lines}");
        assert!(w <= 60.5, "换行后宽度不能超出盒宽，实得 {w}");
        assert!(
            (h - lines as f32 * LH).abs() < 0.6,
            "高 {h} 应 ≈ 行数 {lines} × 行高 {LH}"
        );
    }

    #[test]
    fn 中文按字符折行() {
        let Some(te) = engine() else { return };
        let fmt = te.format_mode(SIZE, LH, 0, Mode::Wrap).unwrap();
        // 中文没有空格：按词粒度会整段不折，必须按字符
        let l = te.layout_mode(0, &fmt, "迷你卡片测试歌名", 40.0, 400.0, false, Mode::Wrap).unwrap();
        let (_, _, lines) = metrics(&l);
        assert!(lines >= 2, "40px 宽装不下 8 个汉字，行数 {lines}");
    }

    #[test]
    fn 省略号把宽度收进盒内且比裁剪模式窄() {
        let Some(te) = engine() else { return };
        let clip_fmt = te.format_mode(SIZE, LH, 0, Mode::Clip).unwrap();
        let ell_fmt = te.format_mode(SIZE, LH, 0, Mode::Ellipsis).unwrap();
        let clip = te.layout_mode(0, &clip_fmt, LONG, 60.0, LH, false, Mode::Clip).unwrap();
        let ell = te.layout_mode(0, &ell_fmt, LONG, 60.0, LH, false, Mode::Ellipsis).unwrap();
        let (wc, _, lc) = metrics(&clip);
        let (we, _, le) = metrics(&ell);
        assert_eq!(lc, 1);
        assert_eq!(le, 1, "省略号不换行");
        assert!(we <= 60.5, "省略号后的宽度必须收在盒内，实得 {we}");
        assert!(we < wc - 8.0, "省略号版 {we} 应明显窄于裁剪版 {wc}（至少省一个字符宽）");
    }

    #[test]
    fn 放得下时不加省略号() {
        let Some(te) = engine() else { return };
        let clip_fmt = te.format_mode(SIZE, LH, 0, Mode::Clip).unwrap();
        let ell_fmt = te.format_mode(SIZE, LH, 0, Mode::Ellipsis).unwrap();
        let clip = te.layout_mode(0, &clip_fmt, "AB", 200.0, LH, false, Mode::Clip).unwrap();
        let ell = te.layout_mode(0, &ell_fmt, "AB", 200.0, LH, false, Mode::Ellipsis).unwrap();
        let (wc, _, _) = metrics(&clip);
        let (we, _, _) = metrics(&ell);
        assert!((wc - we).abs() < 0.01, "放得下时两模式同宽：{wc} vs {we}");
        assert!(wc < 60.0, "AB 自然宽应远小于 200，实得 {wc}");
    }

    #[test]
    fn 度量区分_nowrap_与_wrap() {
        let Some(mut te) = engine() else { return };
        use crate::layout::Measurer;
        // nowrap：宽度就是自然宽（父容器负责裁），高恒为一行
        let nowrap = crate::layout::TextSpec::new(LONG, SIZE, LH);
        let (w_clip, h_clip) = Measurer::text(&mut te, &nowrap, 60.0);
        assert!(w_clip > 200.0, "nowrap 不该被盒宽压窄，实得 {w_clip}");
        assert!((h_clip - LH).abs() < 0.6, "nowrap 高应是一行，实得 {h_clip}");
        // wrap：宽收在盒内，高按行数长上去
        let mut wrap = crate::layout::TextSpec::new(LONG, SIZE, LH);
        wrap.wrap = true;
        let (w_wrap, h_wrap) = Measurer::text(&mut te, &wrap, 60.0);
        assert!(w_wrap <= 60.5, "wrap 宽应收到盒内，实得 {w_wrap}");
        assert!(h_wrap > LH * 1.5, "wrap 高应长到多行，实得 {h_wrap}");
    }
}
