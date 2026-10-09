#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Color {
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

/// Underline shape (SGR 4:n). `None` defers to the legacy `Attrs::underline` bool.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub enum UnderlineStyle {
    #[default]
    None,
    Single,
    Double,
    Curly,
    Dotted,
    Dashed,
}

#[derive(Clone, Copy, Default)]
pub struct Attrs {
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub reverse: bool,
    pub hidden: bool,
    pub strikethrough: bool,
    pub overline: bool,
    /// SGR 5/6 (slow/rapid blink). Stored; the renderer may ignore it.
    pub blink: bool,
    /// Underline shape; `None` + `underline == true` means a single underline.
    pub underline_style: UnderlineStyle,
    /// SGR 58 underline color; `None` = use the foreground.
    pub underline_color: Option<Color>,
}

impl Attrs {
    /// Effective underline shape, folding in the legacy `underline` flag.
    pub fn ul_style(&self) -> UnderlineStyle {
        match self.underline_style {
            UnderlineStyle::None if self.underline => UnderlineStyle::Single,
            s => s,
        }
    }
}

#[derive(Clone, Copy)]
pub struct Cell {
    /// Base character of the grapheme cluster. `'\0'` marks the right half
    /// (continuation) of a wide cluster, or the spacer left at the end of a
    /// row when a wide cluster did not fit.
    pub c: char,
    pub fg: Color,
    pub bg: Color,
    pub attrs: Attrs,
    /// Interned id of the cluster's extra code points (combining marks, VS16,
    /// ZWJ continuations, skin tones, ...); `0` = none. Use [`Cell::text`].
    pub ext: u16,
    /// OSC 8 hyperlink id (index into `Terminal::hyperlinks`); `0` = none.
    pub link: u16,
    /// Only meaningful on the last cell of a row: the row soft-wrapped into
    /// the next one (auto-wrap), so both belong to one logical line.
    pub wrap: bool,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            c: ' ',
            fg: Color::Default,
            bg: Color::Default,
            attrs: Attrs::default(),
            ext: 0,
            link: 0,
            wrap: false,
        }
    }
}

impl Cell {
    pub fn blank_with(fg: Color, bg: Color) -> Self {
        Self { c: ' ', fg, bg, attrs: Attrs::default(), ..Self::default() }
    }

    /// Wide-cluster continuation / end-of-row spacer.
    #[inline]
    pub fn is_continuation(&self) -> bool {
        self.c == '\0'
    }

    /// Append this cell's full text (base char + cluster extras) to `out`.
    /// Continuation cells append nothing.
    pub fn push_text(&self, out: &mut String) {
        if self.c == '\0' {
            return;
        }
        out.push(self.c);
        if self.ext != 0 {
            if let Some(x) = cluster_extra(self.ext) {
                out.push_str(&x);
            }
        }
    }

    /// Full text of the cell (base + combining marks / ZWJ sequence).
    pub fn text(&self) -> String {
        let mut s = String::new();
        self.push_text(&mut s);
        s
    }

    /// The extra code points of the cluster (empty when none).
    pub fn extra(&self) -> String {
        if self.ext == 0 { String::new() } else { cluster_extra(self.ext).unwrap_or_default() }
    }
}

/// Text of a row of cells: continuation cells skipped, clusters expanded.
pub fn cells_text(cells: &[Cell]) -> String {
    let mut s = String::with_capacity(cells.len());
    for c in cells {
        c.push_text(&mut s);
    }
    s
}

// ── Cluster interner ──
//
// Cells stay `Copy` and small: the extra code points of a grapheme cluster are
// interned process-wide and referenced by a `u16` id. Typical sessions use a
// few dozen distinct clusters; once the table is full further extras are
// dropped (the base character still renders).

struct Interner {
    items: Vec<Box<str>>,
    map: std::collections::HashMap<Box<str>, u16>,
}

fn interner() -> &'static std::sync::Mutex<Interner> {
    static I: std::sync::OnceLock<std::sync::Mutex<Interner>> = std::sync::OnceLock::new();
    I.get_or_init(|| {
        std::sync::Mutex::new(Interner {
            items: vec![Box::from("")],
            map: std::collections::HashMap::new(),
        })
    })
}

/// Intern a cluster-extra string; `None` when the table is full.
pub fn intern_cluster(extra: &str) -> Option<u16> {
    if extra.is_empty() {
        return Some(0);
    }
    let mut g = interner().lock().ok()?;
    if let Some(&id) = g.map.get(extra) {
        return Some(id);
    }
    if g.items.len() >= u16::MAX as usize {
        return None;
    }
    let id = g.items.len() as u16;
    let b: Box<str> = Box::from(extra);
    g.items.push(b.clone());
    g.map.insert(b, id);
    Some(id)
}

pub fn cluster_extra(id: u16) -> Option<String> {
    let g = interner().lock().ok()?;
    g.items.get(id as usize).map(|s| s.to_string())
}
