//! 音乐卡：`web/card.html` 的 `.card-music` / `.mc-*` 搬到 D2D（240×120）。
//!
//! 左封面（100×100 圆角 12 的 135° 强调色渐变 + 音符）· 右列（歌名 / 歌手 / 进度条 + 时间 /
//! 三个按钮）。图标全是 SVG 路径原文（音符、上一首、播放/暂停、下一首），走 `Path::svg`。

use windows::core::Result;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::{
    ID2D1Bitmap1, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_INTERPOLATION_MODE_LINEAR,
};
use windows_numerics::Vector2;

use minicard_draw::color::Palette;
use minicard_draw::gradient;
use minicard_draw::layout::{layout, Align, Dim, Dir, Justify, Node, Placed, TextSpec};
use minicard_draw::paint::{fill_round, set_color, stroke_path, stroke_round};
use minicard_draw::path::Path;
use minicard_draw::text::{Mode as TextMode, TextEngine};
use minicard_shell::gfx::Gfx;

#[derive(Clone, Debug)]
pub struct Music {
    pub title: String,
    pub artist: String,
    pub playing: bool,
    pub position_ms: u64,
    pub duration_ms: u64,
}

impl Music {
    pub fn pct(&self) -> f32 {
        if self.duration_ms == 0 {
            return 0.0;
        }
        let pos = self.position_ms.min(self.duration_ms);
        (pos as f32 / self.duration_ms as f32 * 100.0).clamp(0.0, 100.0)
    }

    /// `renderMusic` 里的 `posTxt`：分:秒，两位
    pub fn pos_text(&self) -> String {
        let secs = (self.position_ms.min(self.duration_ms.max(self.position_ms)) / 1000) as u64;
        format!("{:02}:{:02}", secs / 60, secs % 60)
    }
}

/// 图标路径（viewBox 24）
fn icon(name: &str) -> &'static str {
    match name {
        "note" => "M9 18V6.2l9.5-2v11.6M9 18a2.4 2.4 0 1 1-4.8 0 2.4 2.4 0 0 1 4.8 0M18.5 15.8a2.4 2.4 0 1 1-4.8 0 2.4 2.4 0 0 1 4.8 0",
        "prev" => "M6.4 6.1v11.8M14.7 8Q18.5 5.4 18.5 10L18.5 14Q18.5 18.6 14.7 16L12.6 14.6Q8.8 12 12.6 9.4L14.7 8Z",
        "play" => "M12.9 7.8Q9 5.4 9 10L9 14Q9 18.6 12.9 16.2L16 14.4Q19.9 12 16 9.6L12.9 7.8Z",
        "pause" => "M9.2 6.4v11.2M14.8 6.4v11.2",
        _ => "M17.6 6.1v11.8M9.3 8Q5.5 5.4 5.5 10L5.5 14Q5.5 18.6 9.3 16L11.4 14.6Q15.2 12 11.4 9.4L9.3 8Z",
    }
}

pub struct Tokens {
    pub w: f32,
    pub h: f32,
    pub r: f32,
    pub radius: f32,
    pub pad: f32,
    pub body_gap: f32,
    pub cover_r: f32,
    pub note_sz: f32,
    pub note_stroke: f32,
    pub meta_gap: f32,
    pub meta_mb: f32,
    pub title_fs: f32,
    pub title_lh: f32,
    pub artist_fs: f32,
    pub artist_lh: f32,
    pub prog_gap: f32,
    pub prog_mb: f32,
    pub bar_h: f32,
    pub time_fs: f32,
    pub time_lh: f32,
    pub acts_gap: f32,
    pub btn_w: f32,
    pub btn_h: f32,
    pub btn_r: f32,
    pub btn_ic: f32,
    pub btn_stroke: f32,
}

impl Tokens {
    pub fn music(w: f32, h: f32) -> Self {
        let k = (w / 240.0).min(h / 120.0).clamp(0.4, 2.5);
        let r = 16.0 * k;
        Self {
            w,
            h,
            r,
            radius: 1.0 * r,
            pad: 0.625 * r,
            body_gap: 0.625 * r,
            cover_r: 0.75 * r,
            note_sz: 1.75 * r,
            note_stroke: 1.8,
            meta_gap: 0.3125 * r,
            meta_mb: 0.4375 * r,
            title_fs: 1.0 * r,
            title_lh: 1.0 * r * 1.25,
            artist_fs: 0.8125 * r,
            artist_lh: 0.8125 * r * 1.25,
            prog_gap: 0.375 * r,
            prog_mb: 0.5 * r,
            bar_h: 0.3125 * r,   // `.mc-prog .bar` 把通条覆写成 0.3125rem（5px）
            time_fs: 0.625 * r,
            time_lh: 0.625 * r,
            acts_gap: 0.25 * r,
            btn_w: 2.125 * r,
            btn_h: 1.875 * r,
            btn_r: 0.5 * r,
            btn_ic: 1.375 * r,
            btn_stroke: 2.2,
        }
    }
}

pub fn paint(
    g: &Gfx,
    te: &TextEngine,
    pal: &Palette,
    t: &Tokens,
    m: &Music,
    glass: Option<(&ID2D1Bitmap1, D2D_RECT_F)>,
    // 真专辑封面（圆角已烘进 alpha）；None = 渐变 + 音符占位
    cover: Option<&ID2D1Bitmap1>,
) -> Result<()> {
    let tree = tree(t, m);
    let mut mm: &TextEngine = te;
    let placed = layout(&tree, (t.w, t.h), &mut mm);

    unsafe {
        g.begin()?;
        g.push_clip()?;
        if let Some((bmp, rect)) = glass {
            g.d2d.DrawBitmap(bmp, Some(&rect), 1.0, D2D1_INTERPOLATION_MODE_LINEAR, None, None);
        }
        let full = D2D_RECT_F { left: 0.0, top: 0.0, right: t.w, bottom: t.h };
        set_color(g, pal.veil)?;
        fill_round(g, full, t.radius)?;
        set_color(g, pal.border)?;
        stroke_round(
            g,
            D2D_RECT_F { left: 0.5, top: 0.5, right: t.w - 0.5, bottom: t.h - 0.5 },
            (t.radius - 0.5).max(0.0),
            1.0,
        )?;
        paint_tree(g, te, pal, t, m, &placed, cover)?;
        g.pop_clip();
        g.end()?;
        Ok(())
    }
}

fn text_node(s: &str, size: f32, lh: f32, weight: u16) -> Node {
    Node::text(TextSpec {
        s: s.to_string(),
        size,
        line_h: lh,
        weight,
        wrap: false,
        align: 0,
        tabular: true,
        spacing_em: 0.0,
    })
}

fn tree(t: &Tokens, m: &Music) -> Node {
    let cover = Node::new(Dir::Row)
        .id(9001)
        .style(move |s| s.size(Dim::Px(t.h - 2.0 * t.pad), Dim::Px(t.h - 2.0 * t.pad)));

    let meta = Node::new(Dir::Column)
        .style(move |s| {
            s.gap(t.meta_gap)
                .mar([0.0, 0.0, t.meta_mb, 0.0])
                .align(Align::Stretch)
        })
        .child(
            text_node(
                if m.title.is_empty() { "—" } else { &m.title },
                t.title_fs,
                t.title_lh,
                600,
            )
            .id(9101)
            .style(|s| s.size(Dim::Pct(1.0), Dim::Px(t.title_lh))),
        )
        .child(
            text_node(
                if m.artist.is_empty() { "—" } else { &m.artist },
                t.artist_fs,
                t.artist_lh,
                400,
            )
            .id(9102)
            .style(|s| s.size(Dim::Pct(1.0), Dim::Px(t.artist_lh))),
        );

    let prog = Node::new(Dir::Row)
        .style(move |s| {
            s.size(Dim::Pct(1.0), Dim::Auto)
                .align(Align::Center)
                .gap(t.prog_gap)
                .mar([0.0, 0.0, t.prog_mb, 0.0])
        })
        .child(
            Node::new(Dir::Row)
                .id(9200)
                .style(move |s| s.grow(1.0).basis(0.0).size(Dim::Auto, Dim::Px(t.bar_h))),
        )
        .child(text_node(&m.pos_text(), t.time_fs, t.time_lh, 400).id(9201));

    let mut acts = Node::new(Dir::Row).style(move |s| s.gap(t.acts_gap).align(Align::Center));
    for i in 0..3u32 {
        acts = acts.child(
            Node::new(Dir::Row)
                .id(9300 + i)
                .style(move |s| s.size(Dim::Px(t.btn_w), Dim::Px(t.btn_h))),
        );
    }

    let col = Node::new(Dir::Column)
        // `.mc-col` 是 `align-self: stretch`：高度撑满 100，内容在内部居中
        .style(|s| {
            s.grow(1.0)
                .basis(0.0)
                .justify(Justify::Center)
                .align(Align::Stretch)
                .self_align(Align::Stretch)
        })
        .child(meta)
        .child(prog)
        .child(acts);

    Node::new(Dir::Column)
        .style(move |s| {
            s.size(Dim::Px(t.w), Dim::Px(t.h))
                .pad([t.pad, t.pad, t.pad, t.pad])
        })
        .child(
            Node::new(Dir::Row)
                .style(move |s| {
                    s.grow(1.0)
                        .basis(0.0)
                        .align(Align::Center)
                        .gap(t.body_gap)
                })
                .child(cover)
                .child(col),
        )
}

fn paint_tree(
    g: &Gfx,
    te: &TextEngine,
    pal: &Palette,
    t: &Tokens,
    m: &Music,
    pl: &Placed,
    cover: Option<&ID2D1Bitmap1>,
) -> Result<()> {
    // 封面：有真专辑图就画图，否则渐变 + 音符占位
    if pl.id == 9001 {
        let r = pl.rect;
        let rect = D2D_RECT_F { left: r.x, top: r.y, right: r.right(), bottom: r.bottom() };
        if let Some(bmp) = cover {
            // 圆角已经烘进位图的 alpha（见 live 的 `bake_round_cover`），这里直接 cover 铺满
            let (iw, ih) = unsafe {
                let sz = bmp.GetPixelSize();
                (sz.width as f32, sz.height as f32)
            };
            let src = minicard_draw::image::cover_src_px(iw, ih, r.w, r.h);
            minicard_draw::image::draw(g, bmp, src, rect)?;
            return Ok(());
        }
        let (grad, stops) = gradient::linear_two(
            135.0,
            rect,
            D2D1_COLOR_F { a: 0.72, ..pal.accent },
            D2D1_COLOR_F { a: 0.26, ..pal.accent },
        );
        gradient::fill_round(g, rect, t.cover_r, &grad, &stops)?;
        // 音符居中
        let sz = t.note_sz;
        let s = sz / 24.0;
        let cx = r.x + (r.w - sz) / 2.0;
        let cy = r.y + (r.h - sz) / 2.0;
        let p = Path::svg(icon("note")).scale(cx, cy, s);
        set_color(g, D2D1_COLOR_F { a: 0.80, ..pal.fg })?;
        stroke_path(g, &p, t.note_stroke * s, true)?;
        return Ok(());
    }

    // 进度条：底 + 填充
    if pl.id == 9200 {
        let r = pl.rect;
        let rad = t.bar_h / 2.0;
        set_color(g, pal.track)?;
        fill_round(
            g,
            D2D_RECT_F { left: r.x, top: r.y, right: r.right(), bottom: r.bottom() },
            rad,
        )?;
        let w = r.w * m.pct() / 100.0;
        if w > 0.0 {
            set_color(g, pal.fill)?;
            fill_round(
                g,
                D2D_RECT_F { left: r.x, top: r.y, right: r.x + w, bottom: r.bottom() },
                rad,
            )?;
        }
        return Ok(());
    }

    // 按钮：吸附整像素的 1px 描边 + 居中图标
    if (9300..9400).contains(&pl.id) {
        let i = pl.id - 9300;
        let px = pl.rect.x.round();
        let py = pl.rect.y.round();
        let pw = pl.rect.w.round();
        let ph = pl.rect.h.round();
        set_color(g, pal.badge_border)?;
        stroke_round(
            g,
            D2D_RECT_F { left: px + 0.5, top: py + 0.5, right: px + pw - 0.5, bottom: py + ph - 0.5 },
            t.btn_r,
            1.0,
        )?;
        let name = match i {
            0 => "prev",
            1 => {
                if m.playing {
                    "pause"
                } else {
                    "play"
                }
            }
            _ => "next",
        };
        let sz = t.btn_ic;
        let s = sz / 24.0;
        let cx = px + (pw - sz) / 2.0;
        let cy = py + (ph - sz) / 2.0;
        let p = Path::svg(icon(name)).scale(cx, cy, s);
        set_color(g, pal.fg)?;
        stroke_path(g, &p, t.btn_stroke * s, true)?;
        return Ok(());
    }

    if let Some(spec) = pl.text() {
        let color = match pl.id {
            9102 => pal.muted,
            9201 => pal.muted,
            _ => pal.fg,
        };
        set_color(g, color)?;
        let mode = if spec.size == t.title_fs || spec.size == t.artist_fs {
            TextMode::Ellipsis
        } else {
            TextMode::Clip
        };
        let lay = te.layout_spec(spec, pl.rect.w, spec.line_h + 6.0, mode)?;
        unsafe {
            g.d2d.DrawTextLayout(
                Vector2 { X: pl.rect.x, Y: pl.rect.y },
                &lay,
                &g.brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
            );
        }
    }
    for c in &pl.children {
        paint_tree(g, te, pal, t, m, c, cover)?;
    }
    Ok(())
}

pub fn dump_layout(p: &Placed, depth: usize) {
    let pad = " ".repeat(depth * 2);
    let role = if let Some(s) = p.text() {
        format!("text {:?}", s.s)
    } else {
        "box".to_string()
    };
    minicard_shell::mlog!(
        "{}id={:<5} {:<22} x={:7.2} y={:7.2} w={:7.2} h={:7.2}",
        pad,
        p.id,
        role,
        p.rect.x,
        p.rect.y,
        p.rect.w,
        p.rect.h
    );
    for c in &p.children {
        dump_layout(c, depth + 1);
    }
}

pub fn tree_hook(t: &Tokens, m: &Music) -> Node {
    tree(t, m)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 进度与时间文本() {
        let m = Music {
            title: "t".into(),
            artist: "a".into(),
            playing: true,
            position_ms: 61_000,
            duration_ms: 240_000,
        };
        assert!((m.pct() - 25.4166).abs() < 0.01, "pct {}", m.pct());
        assert_eq!(m.pos_text(), "01:01");
        // 超出时长要夹住
        let over = Music { position_ms: 999_000, ..m.clone() };
        assert!((over.pct() - 100.0).abs() < 0.001);
        assert_eq!(over.pos_text(), "16:39");
        // 时长为 0 不能除零
        let zero = Music { duration_ms: 0, ..m };
        assert_eq!(zero.pct(), 0.0);
    }
}

/// 音乐卡可点区域
pub enum Hit {
    Prev,
    Play,
    Next,
}

pub fn hit(te: &TextEngine, t: &Tokens, m_: &Music, x: f32, y: f32) -> Option<Hit> {
    let tr = tree(t, m_);
    let mut m: &TextEngine = te;
    let pl = minicard_draw::layout::layout(&tr, (t.w, t.h), &mut m);
    match minicard_draw::layout::hit_id(&pl, x, y)? {
        9300 => Some(Hit::Prev),
        9301 => Some(Hit::Play),
        9302 => Some(Hit::Next),
        _ => None,
    }
}
