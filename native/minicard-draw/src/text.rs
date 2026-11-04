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
    DWriteCreateFactory, DWRITE_FONT_WEIGHT, IDWriteTextLayout1, IDWriteFactory, IDWriteFactory3, IDWriteFactory5, IDWriteFontCollection,
    IDWriteFontCollection1, IDWriteFontFamily, IDWriteFontSetBuilder, IDWriteFontSetBuilder1, IDWriteFontSetBuilder2,
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
    /// 字集里实际有的字面：`(声明的 CSS 字重, 字体内部的 OS/2 字重)`，按 CSS 档升序。
    ///
    /// 这两个数**不是一回事**：MiSans 三份 TTF 的内部值是 330 / 380 / 520，而浏览器
    /// 按 `@font-face` 的声明值 400 / 500 / 600 选面。DWrite 只认内部值，所以选面
    /// 必须先按 CSS 算法挑「声明档」，再翻译回它认识的那个内部值。
    faces: Vec<(u16, u16)>,
    /// 等宽数字（.num 的 font-variant-numeric: tabular-nums）
    typo: Option<IDWriteTypography>,
    /// 缓存：文本格式（size/lineBox/align/mode）与文本布局（align/text/宽/高/mode）
    /// —— 每帧重建 format/layout 是主要 CPU 开销
    fmt_cache: RefCell<HashMap<(u32, u32, u8, u8, u16), IDWriteTextFormat>>,
    lay_cache: RefCell<HashMap<(u8, String, u32, u32, u8, u16, u32), IDWriteTextLayout>>,
}

/// 字体内部的 OS/2 字重 → **声明**的 CSS 字重（浏览器 `@font-face` 里写的那一档）。
///
/// MiSans 的三份 TTF 是自定义刻度，不是标准的 400/500/600：
///
/// | 文件                  | OS/2 usWeightClass | 浏览器里声明的档 |
/// |-----------------------|--------------------|------------------|
/// | `MiSans-Regular.ttf`  | 330                | 400              |
/// | `MiSans-Medium.ttf`   | 380                | 500              |
/// | `MiSans-Semibold.ttf` | 520                | 600              |
///
/// 浏览器只看 `@font-face` 的声明值来选面，DWrite 只看内部值 —— 之前直接拿内部值套
/// CSS 算法，`400` 就落到了 Medium（380）。这里把两个口径接上。
fn declared_css_weight(os2: u16) -> u16 {
    match os2 {
        // 标准刻度（系统字体、其它字族）：声明值就是内部值，原样返回
        100 | 200 | 300 | 400 | 500 | 600 | 700 | 800 | 900 => os2,
        // MiSans 的三个自定义档（340/390/530 这类微调值也落在同一档）
        301..=359 => 400,
        360..=469 => 500,
        470..=579 => 600,
        // 其余未知刻度：按最近的百位归档，落在 100–900 之间
        _ => ((os2 as f32 / 100.0).round() as u16 * 100).clamp(100, 900),
    }
}

/// 把 family 里的字面收成 `(声明的 CSS 字重, 内部 OS/2 字重)`，按声明档升序。
/// 同一档出现多个内部值时只留最小的那个（先到先得，稳定）。
fn collect_faces(family_obj: &IDWriteFontFamily) -> Vec<(u16, u16)> {
    let n = unsafe { family_obj.GetFontCount() };
    let mut v: Vec<(u16, u16)> = Vec::with_capacity(n as usize);
    for i in 0..n {
        if let Ok(f) = unsafe { family_obj.GetFont(i) } {
            let os2 = unsafe { f.GetWeight() }.0.clamp(1, 1000) as u16;
            v.push((declared_css_weight(os2), os2));
        }
    }
    v.sort_unstable();
    v.dedup_by_key(|f| f.0);
    v
}

impl TextEngine {
    fn from_collection(
        factory: IDWriteFactory,
        collection: IDWriteFontCollection,
        family: &str,
    ) -> Result<Self> {
        // 文字栅格化参数：每建一个 TextEngine 就把安装函数挂给外壳层，之后所有
        // `Gfx::new`（九张卡 / Dock / 菜单）都会自动套上，调用方不用管。
        minicard_shell::gfx::set_text_raster_hook(install_default_text_raster);
        unsafe {
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
            let faces = collect_faces(&family_obj);
            Ok(Self {
                factory,
                collection,
                family: fam,
                faces,
                typo,
                fmt_cache: RefCell::new(HashMap::new()),
                lay_cache: RefCell::new(HashMap::new()),
                ascent: m.ascent as f32 / upem as f32,
                descent: m.descent as f32 / upem as f32,
                upem,
            })
        }
    }

    /// 注册**多个字重**的字体文件（Regular / Medium / Semibold …）到同一个私有字集。
    ///
    /// 时钟的 4.5rem 时间用 600（Semibold）、歌名/日历标题也是 600；只装 Regular 时
    /// DWrite 会「合成加粗」（smear），字形与浏览器差一截，所以按字重装真字面。
    pub fn new_weights(files: &[&Path], family: &str) -> Result<Self> {
        unsafe {
            let factory: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let f5: IDWriteFactory5 = factory.cast()?;
            let b1: IDWriteFontSetBuilder1 = f5.CreateFontSetBuilder()?;
            let b2: IDWriteFontSetBuilder2 = b1.cast()?;
            for f in files {
                b2.AddFontFile(&HSTRING::from(f.to_string_lossy().as_ref()))?;
            }
            let b0: IDWriteFontSetBuilder = b2.cast()?;
            let set = b0.CreateFontSet()?;
            let f3: IDWriteFactory3 = factory.cast()?;
            let coll1: IDWriteFontCollection1 = f3.CreateFontCollectionFromFontSet(&set)?;
            let collection: IDWriteFontCollection = coll1.cast()?;
            Self::from_collection(factory, collection, family)
        }
    }

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
            let faces = collect_faces(&family_obj);
            Ok(Self {
                factory,
                collection,
                family: fam,
                faces,
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
        self.format_w(size, line_box, align, 400)
    }

    /// 按 CSS Fonts 4 的口径把「想要的字重」映射到**实际可用的字面**，返回的是
    /// 该字面在 TTF 里的内部 OS/2 值（`CreateTextFormat` 只认这个数）。
    ///
    /// 关键：CSS 的选面发生在**声明档**上，不是在内部值上。
    /// MiSans 的三份 TTF 内部是 330 / 380 / 520（见 `declared_css_weight`），但浏览器里
    /// 它们是 `@font-face` 声明的 400 / 500 / 600。拿内部值直接套 CSS 算法就会错：
    /// `400` 在内部值里「没有 ≥400 又 ≤500 的档」，于是往下落到 380（Medium）——
    /// 浏览器明明用的是 Regular（330）。Medium 的 tnum 数字每字宽 604/1000 em、Regular 是
    /// 594，长串「279 GB / 466 GB」因此宽出 1.4%（浏览器 111.23 / 自绘 112.78），
    /// 数字越往右错得越多。
    ///
    /// 所以这里先按声明档跑 CSS 算法（400→400、500→500、600→600），再翻译回内部值。
    fn css_weight(&self, want: u16) -> u16 {
        let fs = &self.faces;
        if fs.is_empty() {
            return want;
        }
        let w = want;
        // CSS Fonts 4 §5.2 的匹配顺序：
        //   <400 ：先 ≤w 降序，再 >w 升序
        //   400–500：先 [w,500] 升序，再 <w 降序，最后 >500 升序
        //   >500 ：先 ≥w 升序，再 <w 降序
        let pick = if (400..=500).contains(&w) {
            fs.iter()
                .filter(|f| f.0 >= w && f.0 <= 500)
                .min_by_key(|f| f.0)
                .or_else(|| fs.iter().filter(|f| f.0 < w).max_by_key(|f| f.0))
                .or_else(|| fs.iter().filter(|f| f.0 > 500).min_by_key(|f| f.0))
        } else if w < 400 {
            fs.iter()
                .filter(|f| f.0 <= w)
                .max_by_key(|f| f.0)
                .or_else(|| fs.iter().filter(|f| f.0 > w).min_by_key(|f| f.0))
        } else {
            fs.iter()
                .filter(|f| f.0 >= w)
                .min_by_key(|f| f.0)
                .or_else(|| fs.iter().filter(|f| f.0 < w).max_by_key(|f| f.0))
        };
        pick.map(|f| f.1).unwrap_or(w)
    }

    /// 带字重建格式（DWrite 的字重只能在 `CreateTextFormat` 时给，事后改不了）。
    pub fn format_w(
        &self,
        size: f32,
        line_box: f32,
        align: DWRITE_TEXT_ALIGNMENT,
        weight: u16,
    ) -> Result<IDWriteTextFormat> {
        unsafe {
            let face_w = self.css_weight(weight);
            if std::env::var("MC_DBG_WEIGHT").is_ok() {
                eprintln!("want {} → face {} / 字面 {:?}", weight, face_w, self.faces);
            }
            let fmt = self.factory.CreateTextFormat(
                &self.family,
                &self.collection,
                DWRITE_FONT_WEIGHT(face_w as i32),
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
        self.format_weight(size, line_box, align, mode, 400)
    }

    /// 带**字重**的格式。真字面必须在字集里（见 `new_weights`），否则 DWrite 会合成加粗。
    pub fn format_weight(
        &self,
        size: f32,
        line_box: f32,
        align: u8,
        mode: Mode,
        weight: u16,
    ) -> Result<IDWriteTextFormat> {
        let key = (size.to_bits(), line_box.to_bits(), align, mode as u8, weight);
        if let Some(f) = self.fmt_cache.borrow().get(&key) {
            return Ok(f.clone());
        }
        let a = match align {
            1 => DWRITE_TEXT_ALIGNMENT_TRAILING,
            2 => DWRITE_TEXT_ALIGNMENT_CENTER,
            _ => DWRITE_TEXT_ALIGNMENT_LEADING,
        };
        let f = self.format_w(size, line_box, a, weight)?;
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

    /// 按 `TextSpec` 建格式（含字重）。
    pub fn format_spec(&self, t: &crate::layout::TextSpec, mode: Mode) -> Result<IDWriteTextFormat> {
        self.format_weight(t.size, t.line_h, t.align, mode, t.weight)
    }

    /// 按 `TextSpec` 建布局（含字重与**字距**）。字距走 `IDWriteTextLayout1::SetCharacterSpacing`，
    /// 单位是 DIP（CSS 的 `letter-spacing: -0.02em` = -0.02 × fontSize）。
    pub fn layout_spec(
        &self,
        t: &crate::layout::TextSpec,
        width: f32,
        height: f32,
        mode: Mode,
    ) -> Result<IDWriteTextLayout> {
        // 键里必须带字号/行高/tabular：同串同盒不同字号会串（日历 35 格最易命中）。
        // 这里拼进字符串而不是扩元组，避免动到 layout_mode/layout_cached 共用的缓存类型。
        let composite = format!("{}|{}|{}|{}", t.s, t.size, t.line_h, t.tabular as u8);
        let key = (
            t.align,
            composite,
            width.to_bits(),
            height.to_bits(),
            mode as u8,
            t.weight,
            t.spacing_em.to_bits(),
        );
        if let Some(l) = self.lay_cache.borrow().get(&key) {
            return Ok(l.clone());
        }
        let fmt = self.format_spec(t, mode)?;
        let l = self.layout_num(&fmt, &t.s, width, height, t.tabular)?;
        if t.spacing_em != 0.0 {
            let px = t.spacing_em * t.size;
            let l1: IDWriteTextLayout1 = l.cast()?;
            unsafe {
                // CSS 的 letter-spacing 是「每个字符后面加」→ leading 0 / trailing px。
                // 两边都给 px 会少掉一整个字距（单字符宽度差 1.2px，整串差一倍）。
                l1.SetCharacterSpacing(
                    0.0,
                    px,
                    0.0,
                    DWRITE_TEXT_RANGE { startPosition: 0, length: t.s.encode_utf16().count() as u32 },
                )?;
            }
        }
        {
            let mut c = self.lay_cache.borrow_mut();
            if c.len() > 256 {
                c.clear();
            }
            c.insert(key, l.clone());
        }
        Ok(l)
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
        let key = (
            align,
            text.to_owned(),
            width.to_bits(),
            height.to_bits(),
            mode as u8,
            400u16,
            0u32,
        );
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
        // 旧调用点没有 TextSpec：包一个最小 spec 走同一条路径（字重 400、字距 0）
        let spec = crate::layout::TextSpec {
            s: text.to_string(),
            size: 0.0,
            line_h: 0.0,
            weight: 400,
            wrap: false,
            align,
            tabular,
            spacing_em: 0.0,
        };
        let _ = spec;
        let key = (align, text.to_owned(), width.to_bits(), height.to_bits(), Mode::Clip as u8, 400u16, 0u32);
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

    pub fn family_pcwstr(&self) -> PCWSTR {
        PCWSTR(self.family.as_ptr())
    }
}

impl TextEngine {
    /// 度量一段文字（按 `max_w` 换行后的宽高）。`Measurer` 的两个 impl 都走这里，
    /// 免得「只读引用」与「可变引用」两套实现漂移。
    pub fn measure(&self, t: &crate::layout::TextSpec, max_w: f32) -> (f32, f32) {
        let mode = if t.wrap && max_w > 0.0 { Mode::Wrap } else { Mode::Clip };
        let fmt = match self.format_spec(t, mode) {
            Ok(f) => f,
            Err(_) => return (0.0, t.line_h),
        };
        let (w, h) = if max_w > 0.0 { (max_w, 4096.0) } else { (4096.0, t.line_h * 1.5) };
        let _ = &fmt;
        match self.layout_spec(t, w, h, mode) {
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

    /// 自定义文字栅格化参数：`gamma` 与 `enhanced_contrast` 决定抗锯齿的中间调曲线。
    ///
    /// 出图默认走 [`DEFAULT_TEXT_GAMMA`]（见 `install_default_text_raster`），这里留给
    /// `--gamma/--contrast/--rmode` 做量化扫档用。
    pub fn rendering_params(
        &self,
        gamma: f32,
        enhanced_contrast: f32,
        rmode: u8,
    ) -> windows::core::Result<windows::Win32::Graphics::DirectWrite::IDWriteRenderingParams> {
        unsafe { custom_rendering_params(&self.factory, gamma, enhanced_contrast, rmode) }
    }

    /// 首行**基线到行盒顶**的距离。
    ///
    /// 不能直接用字体 ascent：行盒顶不等于 em 顶，中间隔着 half-leading
    /// `(line_height - (ascent+descent)·size) / 2`。`line-height:1` 时它是**负的**
    /// （MiSans 的字面框 1.326em 比行盒高），漏掉这一项会让基线对齐的兄弟节点
    /// 整体下移 6.2px（网络卡单位「MB/s」就是这么偏的）。
    pub fn ascent_of(&self, t: &crate::layout::TextSpec) -> f32 {
        let box_em = self.ascent + self.descent;
        let half_leading = (t.line_h - box_em * t.size) / 2.0;
        half_leading + self.ascent * t.size
    }

    /// 字体字面框（ascent, descent）的 em 比例 —— 行内元素（如单位 `<i>`）的盒子
    /// 高度是字面框而不是行高，卡片算度量时需要。
    pub fn font_box(&self) -> (f32, f32) {
        (self.ascent, self.descent)
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

    /// 装齐三个字面（Regular 330 / Medium 380 / Semibold 520）的引擎 —— 选面口径的用例要用。
    fn engine3() -> Option<TextEngine> {
        let dir = std::path::Path::new("../assets/fonts");
        let files: Vec<std::path::PathBuf> =
            ["MiSans-Regular.ttf", "MiSans-Medium.ttf", "MiSans-Semibold.ttf"]
                .iter()
                .map(|n| dir.join(n))
                .filter(|p| p.exists())
                .collect();
        if files.len() < 3 {
            eprintln!("跳过：三个字面不全（{}）", dir.display());
            return None;
        }
        let refs: Vec<&std::path::Path> = files.iter().map(|p| p.as_path()).collect();
        Some(TextEngine::new_weights(&refs, "MiSans").unwrap())
    }

    /// CSS 字重必须落到浏览器会用的那一面：400 → Regular(330)、500 → Medium(380)、
    /// 600 → Semibold(520)。400 若落到 Medium，tnum 数字每字宽就从 595 变 604/1000 em。
    #[test]
    fn css_字重映射到正确的内部字面() {
        let Some(te) = engine3() else { return };
        let pic = |w: u16| te.css_weight(w);
        assert_eq!(pic(400), 330, "CSS 400 应取 Regular(330)，字面表 {:?}", te.faces);
        assert_eq!(pic(500), 380, "CSS 500 应取 Medium(380)，字面表 {:?}", te.faces);
        assert_eq!(pic(600), 520, "CSS 600 应取 Semibold(520)，字面表 {:?}", te.faces);
        // 往下没有更细的面时取最近的 Regular（CSS：<400 先查 ≤w 降序，再 >w 升序）
        assert_eq!(pic(300), 330, "CSS 300 应取 Regular(330)");
        // 往上没有更粗的面时不能反而变细
        assert!(pic(700) >= pic(600), "CSS 700 不该比 600 更细");
    }

    /// 选面的**后果**：400 的等宽数字是 Regular 的 594/1000 em（Medium 是 604）。
    /// 只断言 `css_weight` 的返回值不够 —— DWrite 拿到真值才真的换了面。
    #[test]
    fn css_400_的等宽数字是_regular_的宽度() {
        let Some(te) = engine3() else { return };
        let size = 100.0;
        let w = |weight: u16| {
            let spec = crate::layout::TextSpec {
                s: "0123456789".into(),
                size,
                line_h: size,
                weight,
                wrap: false,
                align: 0,
                tabular: true,
                spacing_em: 0.0,
            };
            te.measure(&spec, 4096.0).0
        };
        let w400 = w(400);
        let w500 = w(500);
        // 10 位 × 0.594 em × 100px = 594px（Medium 是 604）
        assert!(
            (w400 - 594.0).abs() < 0.5,
            "CSS 400 的 10 位 tnum 数字宽 {w400:.2}，应 = 594（Regular 594/1000 em）；\
             落到 Medium 会是 604 —— 说明选面又错了"
        );
        assert!(
            (w500 - 604.0).abs() < 1.0,
            "CSS 500（Medium）的 10 位 tnum 数字宽 {w500:.2}，应 ≈ 604"
        );
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

/// CSS `text-shadow: 0 Ypx Rpx color` 的全部参数。
///
/// `.card` 上那条是 `0 0.833px 1.667px var(--tshadow)`：只往下挪 0.833px、模糊半径 1.667px。
/// 颜色走色板的 `--tshadow`（深色主题黑 45%、浅色主题白 60%），别写死黑色。
#[derive(Clone, Copy, Debug)]
pub struct TextShadow {
    /// 阴影色（alpha 是该次阴影的总不透明度）
    pub color: windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F,
    /// 3×3 抽样网格的步长（≈ CSS 模糊半径的一半，实测 0.8335 与参考图最贴）
    pub sigma: f32,
    /// 纵向偏移（CSS 的第二个参数）
    pub dy: f32,
}

impl TextShadow {
    /// `.card { text-shadow: 0 0.833px 1.667px var(--tshadow) }` 的几何部分 + 深色主题的影色。
    pub const CARD: Self = Self {
        color: windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 0.45,
        },
        sigma: 0.8335,
        dy: 0.833,
    };

    /// 只换影色（色板里的 `--tshadow`），几何仍按 CSS。
    pub fn with_color(self, color: windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F) -> Self {
        Self { color, ..self }
    }
}

/// 3×3 高斯核的 9 个抽样点 `(dx, dy, 归一化权重)`。
///
/// 硬边偏移副本在每条字形边缘都会差一大截（边缘渐变完全没有），所以阴影一律走这一套。
pub fn gauss_taps(sigma: f32) -> [(f32, f32, f32); 9] {
    let mut total = 0.0f32;
    let mut taps = [(0.0f32, 0.0f32, 0.0f32); 9];
    let mut k = 0;
    for i in -1..=1 {
        for j in -1..=1 {
            let (ox, oy) = (i as f32 * sigma, j as f32 * sigma);
            let wt = (-(ox * ox + oy * oy) / (2.0 * sigma * sigma)).exp();
            taps[k] = (ox, oy, wt);
            total += wt;
            k += 1;
        }
    }
    if total > 0.0 {
        for t in taps.iter_mut() {
            t.2 /= total;
        }
    }
    taps
}

/// 给**已经建好**的文本布局画一遍高斯阴影（不画正文）。
///
/// 滚动数字那种「每帧都在改字符」的场合复用它：布局已经在手，不必为了阴影再排版一次。
pub fn draw_shadow_taps(
    g: &minicard_shell::gfx::Gfx,
    lay: &IDWriteTextLayout,
    x: f32,
    y: f32,
    sh: &TextShadow,
) -> Result<()> {
    use windows::Win32::Graphics::Direct2D::D2D1_DRAW_TEXT_OPTIONS_NONE;
    use windows_numerics::Vector2;
    if sh.color.a <= 0.0 || sh.sigma <= 0.0 {
        return Ok(());
    }
    for (ox, oy, wt) in gauss_taps(sh.sigma) {
        let a = sh.color.a * wt;
        if a <= 0.001 {
            continue;
        }
        crate::paint::set_color(
            g,
            windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F { a, ..sh.color },
        )?;
        unsafe {
            g.d2d.DrawTextLayout(
                Vector2 {
                    X: x + ox,
                    Y: y + oy + sh.dy,
                },
                lay,
                &g.brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
            );
        }
    }
    Ok(())
}

/// CSS `text-shadow: 0 Ypx Rpx color` 的近似：R 是模糊半径（= 2σ），用 3×3 高斯核
/// 画 9 遍低透明度副本。注意**不是**硬边偏移 —— 硬边在每一条字形边缘都会差一大截。
#[allow(clippy::too_many_arguments)]
pub fn draw_shadowed(
    g: &minicard_shell::gfx::Gfx,
    spec: &crate::layout::TextSpec,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    mode: Mode,
    color: windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F,
    shadow_a: f32,
    sigma: f32,
    dy: f32,
    te: &TextEngine,
) -> Result<()> {
    use windows_numerics::Vector2;
    let lay = te.layout_spec(spec, w, h, mode)?;
    // 老三参版本（todo / 磁盘在用）保持黑影：几何与 α 都不变，逐字节等价
    let sh = TextShadow {
        color: windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: shadow_a,
        },
        sigma,
        dy,
    };
    draw_shadow_taps(g, &lay, x, y, &sh)?;
    crate::paint::set_color(g, color)?;
    unsafe {
        g.d2d.DrawTextLayout(
            Vector2 { X: x, Y: y },
            &lay,
            &g.brush,
            windows::Win32::Graphics::Direct2D::D2D1_DRAW_TEXT_OPTIONS_NONE,
        );
    }
    Ok(())
}

// ─────────────────────────── 文字栅格化参数（出图默认值） ───────────────────────────

/// 出图用的 gamma。
///
/// 实测（`--probe clock` 240×120、`--glass 0 --no-glass`，对 `_card_ref.py` 的真
/// WebView2 参考图；判据见 `docs/verify/minicard_center/_aa_edge.py`）：
///
/// | gamma | 平均差 | p95 | 墨量 | 边缘 10–90 宽度 p50 |
/// |-------|--------|-----|------|--------------------|
/// | 1.0   | 4.30   | 33  | 3614 | 1.419 |
/// | 1.4   | 3.82   | 27  | 3710 | 1.358 |
/// | 1.8（系统默认）| 3.56 | 24 | 3787 | 1.368 |
/// | **2.2** | **3.42** | **22** | **3848** | **1.330** |
///
/// DWrite 把 gamma 钳在 `[1.0, 2.2]`：给 2.6 / 3.0 得到与 2.2 **逐字节相同**的帧。
/// 参考图（Chrome/Skia）墨量 3995，2.2 比系统默认多 1.6% 墨，是这条路上能拿到的最肥一档。
pub const DEFAULT_TEXT_GAMMA: f32 = 2.2;

/// 出图用的渲染模式档位（0 = `NATURAL_SYMMETRIC`）。
///
/// 同一张卡实测：`NATURAL_SYMMETRIC` 3.42/22、`NATURAL` 4.24/28、`GDI_NATURAL` 4.50/33、
/// `OUTLINE` 4.05/28、`CLEARTYPE_*` 4.24~4.50/28~33 —— 灰阶抗锯齿下后者要么等价于
/// `NATURAL`，要么把边缘压得更硬（p50 1.33 → 1.12）。
pub const DEFAULT_TEXT_RMODE: u8 = 0;

/// 按档位建 `IDWriteRenderingParams`（gamma 与对比度都走 `IDWriteFactory1`）。
///
/// `enhanced_contrast` 只对 ClearType 有意义：GRAYSCALE 下扫 0 / 0.5 / 1.0 三档，
/// 墨量差 0.06%、边缘宽度差 0.002px（帧字节不同但肉眼与判据都不可见），所以出图不启用。
unsafe fn custom_rendering_params(
    factory: &IDWriteFactory,
    gamma: f32,
    enhanced_contrast: f32,
    rmode: u8,
) -> Result<windows::Win32::Graphics::DirectWrite::IDWriteRenderingParams> {
    use windows::Win32::Graphics::DirectWrite::{
        DWRITE_PIXEL_GEOMETRY_FLAT, DWRITE_RENDERING_MODE_ALIASED,
        DWRITE_RENDERING_MODE_CLEARTYPE_GDI_CLASSIC, DWRITE_RENDERING_MODE_CLEARTYPE_NATURAL,
        DWRITE_RENDERING_MODE_CLEARTYPE_NATURAL_SYMMETRIC, DWRITE_RENDERING_MODE_GDI_NATURAL,
        DWRITE_RENDERING_MODE_NATURAL, DWRITE_RENDERING_MODE_NATURAL_SYMMETRIC,
        DWRITE_RENDERING_MODE_OUTLINE,
    };
    let mode = match rmode {
        1 => DWRITE_RENDERING_MODE_NATURAL,
        2 => DWRITE_RENDERING_MODE_GDI_NATURAL,
        3 => DWRITE_RENDERING_MODE_ALIASED,
        4 => DWRITE_RENDERING_MODE_OUTLINE,
        5 => DWRITE_RENDERING_MODE_CLEARTYPE_NATURAL,
        6 => DWRITE_RENDERING_MODE_CLEARTYPE_GDI_CLASSIC,
        7 => DWRITE_RENDERING_MODE_CLEARTYPE_NATURAL_SYMMETRIC,
        _ => DWRITE_RENDERING_MODE_NATURAL_SYMMETRIC,
    };
    let f1: windows::Win32::Graphics::DirectWrite::IDWriteFactory1 = factory.cast()?;
    let p1 = f1.CreateCustomRenderingParams(
        gamma,
        enhanced_contrast,
        enhanced_contrast,
        0.0,
        DWRITE_PIXEL_GEOMETRY_FLAT,
        mode,
    )?;
    p1.cast()
}

/// 把出图默认参数套到一个 D2D 设备上下文上（由 `Gfx::new` 通过钩子调用）。
fn install_default_text_raster(
    d2d: &windows::Win32::Graphics::Direct2D::ID2D1DeviceContext,
) {
    // COM 指针不 Send，用 thread_local 缓存：每线程建一次参数对象，之后每窗只是套一遍。
    thread_local! {
        static RP: std::cell::RefCell<
            Option<windows::Win32::Graphics::DirectWrite::IDWriteRenderingParams>,
        > = const { std::cell::RefCell::new(None) };
    }
    RP.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.is_none() {
            let built = unsafe {
                DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)
                    .and_then(|f| custom_rendering_params(&f, DEFAULT_TEXT_GAMMA, 0.0, DEFAULT_TEXT_RMODE))
            };
            match built {
                Ok(rp) => *slot = Some(rp),
                Err(e) => {
                    minicard_shell::mlog!("文字栅格化参数创建失败（用系统默认）: {e:?}");
                    return;
                }
            }
        }
        if let Some(rp) = slot.as_ref() {
            unsafe {
                let _ = d2d.SetTextRenderingParams(rp);
            }
        }
    });
}
