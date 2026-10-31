//! B1 布局：场景树 + flex 子集（row/column、gap、padding、margin、grow、justify、align）。
//!
//! 只做九张卡真实用到的那一档，不做通用布局引擎：
//! - 尺寸三态 `Px / Pct / Auto`；`Pct` 按父节点**内容盒**解析（CSS 口径）。
//! - 主轴：`grow` 分摊剩余空间（basis 为 0 时即等分），没 grow 就按 `justify` 摆放。
//! - 交叉轴：`Stretch` 撑满（仅当子节点该轴是 `Auto`，与 CSS 一致），否则按 `Align` 落位。
//! - 文字叶子不自带排版知识，宽高由 `Measurer` 给（线上是 DWrite，测试里是假的）。
//!
//! 与 CSS 的对应：`.perf-rows{display:flex;flex-direction:column}` + `.perf-row{flex:1}` +
//! `.perf-row{justify-content:center}` 这套写法，在 `tests` 里逐条对过数值（见文件末）。

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Dim {
    Px(f32),
    /// 相对父节点内容盒的百分比（`0.5` = 50%）
    Pct(f32),
    Auto,
}

impl Dim {
    fn resolve(self, base: f32) -> Option<f32> {
        match self {
            Dim::Px(v) => Some(v),
            Dim::Pct(p) => Some(base * p),
            Dim::Auto => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Dir {
    Row,
    Column,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Align {
    Start,
    Center,
    End,
    /// 撑满交叉轴（子节点该轴为 `Auto` 时生效）
    Stretch,
    /// 按首行基线对齐（文字叶子才参与，容器按顶对齐）
    Baseline,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Justify {
    Start,
    Center,
    End,
    /// 首尾贴边、间隔均分
    Between,
}

#[derive(Clone, Copy, Debug)]
pub struct Style {
    pub dir: Dir,
    pub gap: f32,
    /// 顺时针：上 右 下 左（与 CSS `padding: t r b l` 同序）
    pub pad: [f32; 4],
    /// 同上，写在盒外
    pub mar: [f32; 4],
    pub w: Dim,
    pub h: Dim,
    pub grow: f32,
    /// 主轴基准尺寸；`None` 时用自身内容尺寸（CSS `flex-basis:auto`）
    pub basis: Option<f32>,
    /// 作用在自己孩子身上（CSS `align-items`）；默认 `Stretch` 与 CSS 同值
    pub align: Align,
    /// 自己在父容器里的交叉轴对齐（CSS `align-self`）；`None` = 继承父的 `align`
    pub self_align: Option<Align>,
    pub justify: Justify,
}

impl Style {
    pub fn new(dir: Dir) -> Self {
        Self {
            dir,
            gap: 0.0,
            pad: [0.0; 4],
            mar: [0.0; 4],
            w: Dim::Auto,
            h: Dim::Auto,
            grow: 0.0,
            basis: None,
            align: Align::Stretch,
            self_align: None,
            justify: Justify::Start,
        }
    }
    pub fn pad(mut self, v: [f32; 4]) -> Self {
        self.pad = v;
        self
    }
    pub fn mar(mut self, v: [f32; 4]) -> Self {
        self.mar = v;
        self
    }
    pub fn gap(mut self, v: f32) -> Self {
        self.gap = v;
        self
    }
    pub fn size(mut self, w: Dim, h: Dim) -> Self {
        self.w = w;
        self.h = h;
        self
    }
    pub fn grow(mut self, v: f32) -> Self {
        self.grow = v;
        self
    }
    pub fn basis(mut self, v: f32) -> Self {
        self.basis = Some(v);
        self
    }
    pub fn align(mut self, a: Align) -> Self {
        self.align = a;
        self
    }
    pub fn self_align(mut self, a: Align) -> Self {
        self.self_align = Some(a);
        self
    }
    pub fn justify(mut self, j: Justify) -> Self {
        self.justify = j;
        self
    }
}

/// 文字叶子：只描述「要排什么」，怎么排交给 `Measurer`。
#[derive(Clone, Debug)]
pub struct TextSpec {
    pub s: String,
    pub size: f32,
    /// CSS `line-height` 的绝对值（设计 px，不是倍数）
    pub line_h: f32,
    /// 100..=900
    pub weight: u16,
    /// 是否允许自动换行（CSS `white-space:normal`）；默认 false = nowrap
    pub wrap: bool,
    /// 文本在盒内的水平对齐：0 = leading（CSS `text-align:left`），1 = trailing（`right`）
    pub align: u8,
    /// 等宽数字（CSS `font-variant-numeric: tabular-nums`）
    pub tabular: bool,
}

impl TextSpec {
    pub fn new(s: impl Into<String>, size: f32, line_h: f32) -> Self {
        Self { s: s.into(), size, line_h, weight: 400, wrap: false, align: 0, tabular: false }
    }
}

#[derive(Clone, Debug)]
pub enum Content {
    None,
    Text(TextSpec),
}

#[derive(Clone, Debug)]
pub struct Node {
    pub style: Style,
    /// 业务标识（绘制层用它把节点映射回数据；布局本身不用）
    pub id: u32,
    pub content: Content,
    pub children: Vec<Node>,
}

impl Node {
    pub fn new(dir: Dir) -> Self {
        Self { style: Style::new(dir), id: 0, content: Content::None, children: Vec::new() }
    }
    pub fn text(t: TextSpec) -> Self {
        Self {
            style: Style::new(Dir::Row),
            id: 0,
            content: Content::Text(t),
            children: Vec::new(),
        }
    }
    pub fn text_s(s: impl Into<String>, size: f32, line_h: f32) -> Self {
        Self::text(TextSpec::new(s, size, line_h))
    }
    pub fn id(mut self, id: u32) -> Self {
        self.id = id;
        self
    }
    pub fn style(mut self, f: impl FnOnce(Style) -> Style) -> Self {
        self.style = f(self.style);
        self
    }
    /// 文字在盒内右对齐（CSS `text-align:right`）—— 数值列用
    pub fn align_text(mut self, a: u8) -> Self {
        if let Content::Text(t) = &mut self.content {
            t.align = a;
        }
        self
    }
    /// 等宽数字（`tabular-nums`）
    pub fn tabular(mut self, on: bool) -> Self {
        if let Content::Text(t) = &mut self.content {
            t.tabular = on;
        }
        self
    }
    /// 允许换行（对应 CSS `white-space:normal`）
    pub fn wrap(mut self, on: bool) -> Self {
        if let Content::Text(t) = &mut self.content {
            t.wrap = on;
        }
        self
    }
    pub fn weight(mut self, w: u16) -> Self {
        if let Content::Text(t) = &mut self.content {
            t.weight = w;
        }
        self
    }
    pub fn child(mut self, c: Node) -> Self {
        self.children.push(c);
        self
    }
    pub fn children_of(mut self, cs: impl IntoIterator<Item = Node>) -> Self {
        self.children.extend(cs);
        self
    }
    /// 该节点是叶子（无子节点且非文字）——布局上按 0×0 处理。
    fn is_leaf(&self) -> bool {
        self.children.is_empty()
    }
    fn mar_w(&self) -> f32 {
        self.style.mar[1] + self.style.mar[3]
    }
    fn mar_h(&self) -> f32 {
        self.style.mar[0] + self.style.mar[2]
    }
    fn pad_w(&self) -> f32 {
        self.style.pad[1] + self.style.pad[3]
    }
    fn pad_h(&self) -> f32 {
        self.style.pad[0] + self.style.pad[2]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }
    pub fn right(&self) -> f32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }
}

/// 排好版的树：几何 + 原内容，绘制层照着画。
#[derive(Clone, Debug)]
pub struct Placed {
    pub rect: Rect,
    pub id: u32,
    pub content: Content,
    pub style: Style,
    pub children: Vec<Placed>,
}

impl Placed {
    pub fn text(&self) -> Option<&TextSpec> {
        match &self.content {
            Content::Text(t) => Some(t),
            Content::None => None,
        }
    }
    /// 深度优先遍历（先自己再孩子）。
    pub fn walk(&self, f: &mut impl FnMut(&Placed)) {
        f(self);
        for c in &self.children {
            c.walk(f);
        }
    }
}

/// 文字度量：线上实现吃 DWrite，测试里是等宽假字体。
pub trait Measurer {
    /// 按 `max_w` 换行后的 (宽, 高)；`max_w <= 0` 表示不换行。
    fn text(&mut self, t: &TextSpec, max_w: f32) -> (f32, f32);
    /// 首行基线到行盒顶的距离（`Align::Baseline` 用）。
    fn ascent(&mut self, t: &TextSpec) -> f32;
}

/// 布局入口：`size` 是根节点内容盒（= 卡片尺寸）。
pub fn layout(root: &Node, size: (f32, f32), m: &mut dyn Measurer) -> Placed {
    let rect = Rect::new(0.0, 0.0, size.0, size.1);
    place(root, rect, m)
}

/// 量自身内容尺寸（不含 margin）。
fn measure(n: &Node, avail_w: f32, avail_h: f32, m: &mut dyn Measurer) -> (f32, f32) {
    if let Content::Text(t) = &n.content {
        let max_w = n.style.w.resolve(avail_w).unwrap_or(avail_w).max(0.0);
        let (w, h) = m.text(t, max_w);
        return (w, h);
    }
    if n.is_leaf() {
        return (0.0, 0.0);
    }
    let inner_w = (avail_w - n.pad_w()).max(0.0);
    let inner_h = (avail_h - n.pad_h()).max(0.0);
    let mut main = 0.0f32;
    let mut cross: f32 = 0.0;
    for (i, c) in n.children.iter().enumerate() {
        if i > 0 {
            main += n.style.gap;
        }
        // 子节点主轴给「不限」，交叉轴给父内容盒（CSS 里是子自己撑开父）
        let (cw, ch) = match n.style.dir {
            Dir::Row => measure(c, 0.0, inner_h, m),
            Dir::Column => measure(c, inner_w, 0.0, m),
        };
        let (mw, mh) = (cw + c.mar_w(), ch + c.mar_h());
        match n.style.dir {
            Dir::Row => {
                main += mw;
                cross = cross.max(mh);
            }
            Dir::Column => {
                main += mh;
                cross = cross.max(mw);
            }
        }
    }
    let (w, h) = match n.style.dir {
        Dir::Row => (main + n.pad_w(), cross + n.pad_h()),
        Dir::Column => (cross + n.pad_w(), main + n.pad_h()),
    };
    (w, h)
}

fn place(n: &Node, rect: Rect, m: &mut dyn Measurer) -> Placed {
    let mut out = Placed {
        rect,
        id: n.id,
        content: n.content.clone(),
        style: n.style,
        children: Vec::new(),
    };
    if n.children.is_empty() {
        return out;
    }

    // 内容盒
    let inner = Rect::new(
        rect.x + n.style.pad[3],
        rect.y + n.style.pad[0],
        (rect.w - n.pad_w()).max(0.0),
        (rect.h - n.pad_h()).max(0.0),
    );
    let row = n.style.dir == Dir::Row;
    let (inner_main, inner_cross) = if row { (inner.w, inner.h) } else { (inner.h, inner.w) };

    // 每个子节点的主轴基准 / 交叉尺寸
    struct Slot {
        main: f32,
        cross: f32,
        grow: f32,
    }
    let mut slots: Vec<Slot> = Vec::with_capacity(n.children.len());
    for c in &n.children {
        let (iw, ih) = measure(c, inner.w, inner.h, m);
        let (intr_main, intr_cross) = if row { (iw, ih) } else { (ih, iw) };
        let main = c
            .style
            .basis
            .or_else(|| if row { c.style.w.resolve(inner.w) } else { c.style.h.resolve(inner.h) })
            .unwrap_or(intr_main);
        let cross_dim = if row { c.style.h } else { c.style.w };
        let cross = match (c.style.align, cross_dim) {
            _ if cross_dim != Dim::Auto => cross_dim.resolve(inner_cross).unwrap_or(intr_cross),
            _ => intr_cross,
        };
        slots.push(Slot { main, cross, grow: c.style.grow });
    }

    let gaps = n.style.gap * (n.children.len() as f32 - 1.0).max(0.0);
    let used: f32 = slots.iter().map(|s| s.main).sum::<f32>() + gaps;
    let mut free = inner_main - used;

    // grow：只分正剩余；`basis:0 + grow:1` 即等分
    let total_grow: f32 = slots.iter().map(|s| s.grow).sum();
    if total_grow > 0.0 && free > 0.0 {
        let share = free;
        for s in slots.iter_mut() {
            if s.grow > 0.0 {
                s.main += share * s.grow / total_grow;
            }
        }
        free = 0.0;
    }

    // justify：剩余空间的摆放
    let (mut cursor, extra_gap) = match n.style.justify {
        Justify::Start => (inner_main * 0.0, 0.0),
        Justify::Center => (free / 2.0, 0.0),
        Justify::End => (free.max(0.0), 0.0),
        Justify::Between => {
            let n_gap = (n.children.len() as f32 - 1.0).max(1.0);
            (0.0, (free / n_gap).max(0.0))
        }
    };

    // Baseline 对齐需要先知道整行最大 ascent
    let max_asc = if n.style.align == Align::Baseline {
        n.children
            .iter()
            .filter_map(|c| match &c.content {
                Content::Text(t) => Some(m.ascent(t)),
                _ => None,
            })
            .fold(0.0f32, f32::max)
    } else {
        0.0
    };

    for (i, c) in n.children.iter().enumerate() {
        let s = &slots[i];
        let mar = c.style.mar;
        cursor += mar_main(&mar, row);
        if i > 0 {
            cursor += n.style.gap + extra_gap;
        }

        // 交叉轴：尺寸与落位。`align-self` 缺省才看父的 `align-items`（CSS 口径）
        let al = c.style.self_align.unwrap_or(n.style.align);
        let cross_dim = if row { c.style.h } else { c.style.w };
        let stretch = al == Align::Stretch && cross_dim == Dim::Auto;
        let cross_size = if stretch { inner_cross } else { s.cross };
        let cross_pos = if stretch {
            0.0
        } else {
            match al {
                Align::Start | Align::Stretch => 0.0,
                Align::Center => (inner_cross - cross_size) / 2.0,
                Align::End => inner_cross - cross_size,
                Align::Baseline => match &c.content {
                    Content::Text(t) => max_asc - m.ascent(t),
                    _ => 0.0,
                },
            }
        } + mar_cross_start(&mar, row);

        let child_rect = if row {
            Rect::new(
                inner.x + cursor,
                inner.y + cross_pos,
                s.main,
                cross_size,
            )
        } else {
            Rect::new(
                inner.x + cross_pos,
                inner.y + cursor,
                cross_size,
                s.main,
            )
        };
        out.children.push(place(c, child_rect, m));
        cursor += s.main + mar_main_end(&mar, row);
    }
    out
}

fn mar_main(m: &[f32; 4], row: bool) -> f32 {
    if row {
        m[3]
    } else {
        m[0]
    }
}
fn mar_main_end(m: &[f32; 4], row: bool) -> f32 {
    if row {
        m[1]
    } else {
        m[2]
    }
}
fn mar_cross_start(m: &[f32; 4], row: bool) -> f32 {
    if row {
        m[0]
    } else {
        m[3]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 等宽假字体：宽 = 0.6×字号×字数，行高 = line_h，ascent = 0.88×字号。
    struct Fake;
    impl Measurer for Fake {
        fn text(&mut self, t: &TextSpec, max_w: f32) -> (f32, f32) {
            let natural = 0.6 * t.size * t.s.chars().count() as f32;
            let w = if max_w > 0.0 { natural.min(max_w) } else { natural };
            let lines = if max_w > 0.0 && natural > max_w {
                (natural / max_w).ceil()
            } else {
                1.0
            };
            (w, lines * t.line_h)
        }
        fn ascent(&mut self, t: &TextSpec) -> f32 {
            0.88 * t.size
        }
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.01
    }
    fn assert_close(a: f32, b: f32, what: &str) {
        assert!(close(a, b), "{what}: 实得 {a}，期望 {b}");
    }

    /// 性能卡 240×120 的 k/r 档（与 `Tokens::perf` 同源）
    struct K;
    impl K {
        const R: f32 = 10.0; // k = 120/192 = 0.625 → 根字号 10
        const PAD_TB: f32 = 4.375;
        const PAD_LR: f32 = 10.0;
        const ROWS_PB: f32 = 2.0;
        const GAP: f32 = 2.5;
        const LAB: f32 = 13.125;
        const LINE_H: f32 = 15.09375;
        const BAR_H: f32 = 9.375;
    }

    /// `.card-perf > .perf-rows > .perf-row×(n) > (line > lab + pct) + bar`
    fn perf_tree(n_rows: usize) -> Node {
        let rows = (0..n_rows).map(|i| {
            let line = Node::new(Dir::Row)
                .style(|s| s.gap(K::GAP).align(Align::Stretch).self_align(Align::Baseline))
                .child(Node::text_s("CPU", K::LAB, K::LINE_H).weight(500))
                .child(Node::text_s(format!("{}%", 10 + i), K::LAB, K::LINE_H));
            let bar = Node::new(Dir::Row).style(|s| {
                s.size(Dim::Pct(1.0), Dim::Px(K::BAR_H))
            });
            Node::new(Dir::Column)
                .style(|s| s.grow(1.0).basis(0.0).justify(Justify::Center).gap(K::GAP))
                .child(line)
                .child(bar)
        });
        Node::new(Dir::Column)
            .style(|s| {
                s.size(Dim::Px(240.0), Dim::Px(120.0))
                    .pad([K::PAD_TB, K::PAD_LR, K::PAD_TB, K::PAD_LR])
            })
            .child(
                Node::new(Dir::Column)
                    .style(|s| {
                        s.grow(1.0)
                            .basis(0.0)
                            .pad([0.0, 0.0, K::ROWS_PB, 0.0])
                    })
                    .children_of(rows),
            )
    }

    #[test]
    fn 性能卡_行等分与内容居中与手写几何一致() {
        let t = perf_tree(3);
        let p = layout(&t, (240.0, 120.0), &mut Fake);
        let rows = &p.children[0];

        // .perf-rows 吃掉 (120 - 2×4.375) = 111.25，其中 pb 2.0 是光学补偿
        assert_close(rows.rect.h, 111.25, "rows 高");
        assert_close(rows.rect.w, 220.0, "rows 宽");
        assert_close(rows.children[0].rect.h, 109.25 / 3.0, "row 高");

        // 第三行 y：4.375 + 2×(109.25/3)
        let row3 = &rows.children[2];
        assert_close(row3.rect.y, 4.375 + 2.0 * (109.25 / 3.0), "第三行 y");

        // 行内 justify:center → 内容高 26.96875，上余量 4.723958
        let content_h = K::LINE_H + K::GAP + K::BAR_H;
        let row_h = 109.25 / 3.0;
        let line = &row3.children[0];
        let bar = &row3.children[1];
        assert_close(line.rect.y - row3.rect.y, (row_h - content_h) / 2.0, "行内上余量");
        assert_close(line.rect.h, K::LINE_H, "文字行高");
        assert_close(bar.rect.y - row3.rect.y, (row_h - content_h) / 2.0 + K::LINE_H + K::GAP, "条顶");
        assert_close(bar.rect.h, K::BAR_H, "条高");

        // 条宽 = 内容盒宽 220（Pct(1.0) 按父内容盒解析）
        assert_close(bar.rect.w, 220.0, "条宽");
        // 文字叶子横向贴左（align:Start）
        assert_close(line.rect.x, 10.0, "行左缘");
        assert_close(line.children[0].rect.x, 10.0, "lab 左缘");
    }

    #[test]
    fn grow_按比例分剩余空间() {
        let t = Node::new(Dir::Row)
            .style(|s| s.size(Dim::Px(100.0), Dim::Px(20.0)).gap(4.0))
            .child(Node::new(Dir::Row).style(|s| s.grow(1.0).basis(0.0)))
            .child(Node::new(Dir::Row).style(|s| s.grow(3.0).basis(0.0)));
        let p = layout(&t, (100.0, 20.0), &mut Fake);
        assert_close(p.children[0].rect.w, (100.0 - 4.0) * 0.25, "1 份");
        assert_close(p.children[1].rect.w, (100.0 - 4.0) * 0.75, "3 份");
        assert_close(p.children[1].rect.x, (100.0 - 4.0) * 0.25 + 4.0, "第二块 x");
    }

    #[test]
    fn justify_四档() {
        let mk = |j: Justify| {
            Node::new(Dir::Row)
                .style(|s| s.size(Dim::Px(100.0), Dim::Px(20.0)).justify(j))
                .child(Node::new(Dir::Row).style(|s| s.size(Dim::Px(20.0), Dim::Px(10.0))))
                .child(Node::new(Dir::Row).style(|s| s.size(Dim::Px(20.0), Dim::Px(10.0))))
        };
        let p = layout(&mk(Justify::Start), (100.0, 20.0), &mut Fake);
        assert_close(p.children[0].rect.x, 0.0, "start");
        let p = layout(&mk(Justify::Center), (100.0, 20.0), &mut Fake);
        assert_close(p.children[0].rect.x, 30.0, "center");
        let p = layout(&mk(Justify::End), (100.0, 20.0), &mut Fake);
        assert_close(p.children[0].rect.x, 60.0, "end");
        let p = layout(&mk(Justify::Between), (100.0, 20.0), &mut Fake);
        assert_close(p.children[0].rect.x, 0.0, "between 首");
        assert_close(p.children[1].rect.x, 80.0, "between 末");
    }

    #[test]
    fn align_stretch_与_center() {
        let t = Node::new(Dir::Column)
            .style(|s| s.size(Dim::Px(100.0), Dim::Px(40.0)).align(Align::Stretch))
            .child(Node::new(Dir::Row).style(|s| s.size(Dim::Px(30.0), Dim::Px(40.0))));
        let p = layout(&t, (100.0, 40.0), &mut Fake);
        // 显式宽 30 → 不撑
        assert_close(p.children[0].rect.w, 30.0, "显式宽不撑");

        let t = Node::new(Dir::Column)
            .style(|s| s.size(Dim::Px(100.0), Dim::Px(40.0)).align(Align::Center))
            .child(Node::new(Dir::Row).style(|s| s.size(Dim::Px(30.0), Dim::Px(10.0))));
        let p = layout(&t, (100.0, 40.0), &mut Fake);
        assert_close(p.children[0].rect.x, 35.0, "居中");
        // 子自己声明的 align-self 优先于父的 align-items
        let t = Node::new(Dir::Column)
            .style(|s| s.size(Dim::Px(100.0), Dim::Px(40.0)).align(Align::Stretch))
            .child(
                Node::new(Dir::Row)
                    .style(|s| {
                        s.size(Dim::Px(30.0), Dim::Px(10.0))
                            .self_align(Align::End)
                    }),
            );
        let p = layout(&t, (100.0, 40.0), &mut Fake);
        assert_close(p.children[0].rect.x, 70.0, "align-self:end");
    }

    #[test]
    fn 百分比按父内容盒解析() {
        let t = Node::new(Dir::Column)
            .style(|s| {
                s.size(Dim::Px(200.0), Dim::Px(100.0))
                    .pad([10.0, 20.0, 10.0, 20.0])
            })
            .child(
                Node::new(Dir::Row)
                    .style(|s| s.size(Dim::Pct(0.5), Dim::Px(10.0))),
            );
        let p = layout(&t, (200.0, 100.0), &mut Fake);
        assert_close(p.children[0].rect.w, 80.0, "160 的 50%");
        assert_close(p.children[0].rect.x, 20.0, "左内边距");
    }

    #[test]
    fn margin_累加进主轴且参与基准() {
        let t = Node::new(Dir::Column)
            .style(|s| s.size(Dim::Px(100.0), Dim::Px(100.0)).gap(5.0))
            .child(Node::new(Dir::Row).style(|s| s.size(Dim::Px(10.0), Dim::Px(10.0))))
            .child(
                Node::new(Dir::Row)
                    .style(|s| s.size(Dim::Px(10.0), Dim::Px(10.0)).mar([3.0, 0.0, 7.0, 0.0])),
            );
        let p = layout(&t, (100.0, 100.0), &mut Fake);
        // 第二块 y = 10(第一块) + 5(gap) + 3(上边距)
        assert_close(p.children[1].rect.y, 18.0, "第二块 y");
    }

    #[test]
    fn 文字按最大宽换行并撑高() {
        let t = Node::new(Dir::Column)
            .style(|s| s.size(Dim::Px(100.0), Dim::Px(200.0)))
            .child(Node::text_s("ABCDEFGHIJ", 10.0, 12.0)); // 自然宽 60
        let p = layout(&t, (100.0, 200.0), &mut Fake);
        assert_close(p.children[0].rect.h, 12.0, "单行");

        // 限宽 30 → 2 行
        let t = Node::new(Dir::Column)
            .style(|s| s.size(Dim::Px(30.0), Dim::Px(200.0)))
            .child(Node::text_s("ABCDEFGHIJ", 10.0, 12.0));
        let p = layout(&t, (30.0, 200.0), &mut Fake);
        assert_close(p.children[0].rect.h, 24.0, "两行");
    }

    #[test]
    fn 嵌套行列与_auto_尺寸自撑() {
        let inner = Node::new(Dir::Row)
            .style(|s| s.gap(4.0))
            .child(Node::text_s("AB", 10.0, 12.0))
            .child(Node::text_s("CDE", 10.0, 12.0));
        // 默认 align 是 Stretch（CSS 同值）→ 列里的行撑满 200
        let t = Node::new(Dir::Column)
            .style(|s| s.size(Dim::Px(200.0), Dim::Px(200.0)))
            .child(inner.clone());
        let p = layout(&t, (200.0, 200.0), &mut Fake);
        assert_close(p.children[0].rect.w, 200.0, "stretch 撑满");
        assert_close(p.children[0].rect.h, 12.0, "row 内容高");
        // 里面两个文字按内容排：第二块 x = 18 + 4
        assert_close(p.children[0].children[1].rect.x, 16.0, "第二个文字 x（AB=12 + gap 4）");

        // align:Start 时按内容自撑
        let t = Node::new(Dir::Column)
            .style(|s| {
                s.size(Dim::Px(200.0), Dim::Px(200.0))
                    .align(Align::Start)
            })
            .child(inner);
        let p = layout(&t, (200.0, 200.0), &mut Fake);
        // 12 + 4 + 18 = 34（Fake：0.6×10×字数）
        assert_close(p.children[0].rect.w, 34.0, "row 内容宽");
    }
}
