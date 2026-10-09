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

// Packed per-cell flag bits (see `Cell::flags`).
const F_BOLD: u16 = 1 << 0;
const F_DIM: u16 = 1 << 1;
const F_ITALIC: u16 = 1 << 2;
const F_UNDERLINE: u16 = 1 << 3;
const F_REVERSE: u16 = 1 << 4;
const F_HIDDEN: u16 = 1 << 5;
const F_STRIKE: u16 = 1 << 6;
const F_OVERLINE: u16 = 1 << 7;
const F_BLINK: u16 = 1 << 8;
const UL_SHIFT: u16 = 9; // 3 bits: UnderlineStyle
const UL_MASK: u16 = 0b111 << UL_SHIFT;
/// Only meaningful on the last cell of a row: the row soft-wrapped.
const F_WRAP: u16 = 1 << 12;
/// End-of-row spacer left when a wide cluster did not fit (a '\0' cell that
/// is *not* the right half of a wide glyph).
const F_SPACER: u16 = 1 << 13;
/// Everything that is a rendition attribute (excludes WRAP / SPACER).
const ATTR_MASK: u16 = F_WRAP.wrapping_sub(1);

impl Attrs {
    /// Effective underline shape, folding in the legacy `underline` flag.
    pub fn ul_style(&self) -> UnderlineStyle {
        match self.underline_style {
            UnderlineStyle::None if self.underline => UnderlineStyle::Single,
            s => s,
        }
    }

    /// Pack the rendition flags (not the underline color) into cell bits.
    #[inline]
    pub fn bits(&self) -> u16 {
        (self.bold as u16 * F_BOLD)
            | (self.dim as u16 * F_DIM)
            | (self.italic as u16 * F_ITALIC)
            | (self.underline as u16 * F_UNDERLINE)
            | (self.reverse as u16 * F_REVERSE)
            | (self.hidden as u16 * F_HIDDEN)
            | (self.strikethrough as u16 * F_STRIKE)
            | (self.overline as u16 * F_OVERLINE)
            | (self.blink as u16 * F_BLINK)
            | ((self.underline_style as u16) << UL_SHIFT)
    }

    /// Unpack rendition flags (underline color left `None`).
    #[inline]
    pub fn from_bits(f: u16) -> Self {
        Self {
            bold: f & F_BOLD != 0,
            dim: f & F_DIM != 0,
            italic: f & F_ITALIC != 0,
            underline: f & F_UNDERLINE != 0,
            reverse: f & F_REVERSE != 0,
            hidden: f & F_HIDDEN != 0,
            strikethrough: f & F_STRIKE != 0,
            overline: f & F_OVERLINE != 0,
            blink: f & F_BLINK != 0,
            underline_style: ul_from_bits((f & UL_MASK) >> UL_SHIFT),
            underline_color: None,
        }
    }
}

#[inline]
fn ul_from_bits(b: u16) -> UnderlineStyle {
    match b {
        1 => UnderlineStyle::Single,
        2 => UnderlineStyle::Double,
        3 => UnderlineStyle::Curly,
        4 => UnderlineStyle::Dotted,
        5 => UnderlineStyle::Dashed,
        _ => UnderlineStyle::None,
    }
}

/// One terminal cell, 16 bytes.
///
/// Rendition attributes live in `flags` (bitfield) and everything rare
/// (hyperlink id, grapheme-cluster extras, SGR 58 underline colour) is interned
/// process-wide into a single `u16` id, `x` (`0` = none of them). Use the
/// accessor methods rather than the raw fields.
#[derive(Clone, Copy)]
pub struct Cell {
    /// Base character of the grapheme cluster. `'\0'` marks the right half
    /// (continuation) of a wide cluster, or the spacer left at the end of a
    /// row when a wide cluster did not fit.
    pub c: char,
    pub fg: Color,
    pub bg: Color,
    flags: u16,
    x: u16,
}

impl Default for Cell {
    #[inline]
    fn default() -> Self {
        Self { c: ' ', fg: Color::Default, bg: Color::Default, flags: 0, x: 0 }
    }
}

impl Cell {
    #[inline]
    pub fn blank_with(fg: Color, bg: Color) -> Self {
        Self { c: ' ', fg, bg, flags: 0, x: 0 }
    }

    /// Unstyled cell showing `c`.
    #[inline]
    pub fn plain(c: char) -> Self {
        Self { c, ..Self::default() }
    }

    /// Cell for `c` with the given pen (colors, packed attr bits, extra id).
    #[inline]
    pub fn with_pen(c: char, fg: Color, bg: Color, attr_bits: u16, x: u16) -> Self {
        Self { c, fg, bg, flags: attr_bits & ATTR_MASK, x }
    }

    /// Wide-cluster continuation / end-of-row spacer.
    #[inline]
    pub fn is_continuation(&self) -> bool {
        self.c == '\0'
    }

    /// True for a default blank: space, default colors, no attributes, no
    /// wrap flag, no link / cluster / underline colour.
    #[inline]
    pub fn is_default_blank(&self) -> bool {
        self.non_default_bits() == 0
    }

    /// Zero iff the cell is a default blank (branch-free so row scans and
    /// OR-reductions over several cells stay cheap).
    #[inline(always)]
    fn non_default_bits(&self) -> u64 {
        ((self.c as u32 ^ 0x20) as u64)
            | (self.flags as u64) << 32
            | (self.x as u64) << 48
            | (!matches!(self.fg, Color::Default)) as u64
            | (!matches!(self.bg, Color::Default)) as u64
    }

    // ── rendition flags ──
    #[inline] pub fn bold(&self) -> bool { self.flags & F_BOLD != 0 }
    #[inline] pub fn dim(&self) -> bool { self.flags & F_DIM != 0 }
    #[inline] pub fn italic(&self) -> bool { self.flags & F_ITALIC != 0 }
    #[inline] pub fn underline(&self) -> bool { self.flags & F_UNDERLINE != 0 }
    #[inline] pub fn reverse(&self) -> bool { self.flags & F_REVERSE != 0 }
    #[inline] pub fn hidden(&self) -> bool { self.flags & F_HIDDEN != 0 }
    #[inline] pub fn strikethrough(&self) -> bool { self.flags & F_STRIKE != 0 }
    #[inline] pub fn overline(&self) -> bool { self.flags & F_OVERLINE != 0 }
    #[inline] pub fn blink(&self) -> bool { self.flags & F_BLINK != 0 }

    /// Effective underline shape (folds in the legacy underline flag).
    #[inline]
    pub fn ul_style(&self) -> UnderlineStyle {
        match ul_from_bits((self.flags & UL_MASK) >> UL_SHIFT) {
            UnderlineStyle::None if self.flags & F_UNDERLINE != 0 => UnderlineStyle::Single,
            s => s,
        }
    }

    /// Raw rendition bits (attributes only, no wrap/spacer): equal bits and
    /// equal [`Cell::extra_id`] mean equal visual style.
    #[inline]
    pub fn attr_bits(&self) -> u16 {
        self.flags & ATTR_MASK
    }

    /// Full attribute set including the (interned) underline colour.
    pub fn attrs(&self) -> Attrs {
        let mut a = Attrs::from_bits(self.flags);
        if self.x != 0 {
            a.underline_color = with_extra(self.x, |e| e.ulc).flatten();
        }
        a
    }

    /// Replace the rendition attributes (keeps wrap / spacer, link, cluster).
    pub fn set_attrs(&mut self, a: &Attrs) {
        self.flags = (self.flags & !ATTR_MASK) | a.bits();
        if self.x != 0 || a.underline_color.is_some() {
            let e = extra_entry(self.x).unwrap_or_default();
            self.x = intern_extra(&e.cluster, e.link, a.underline_color).unwrap_or(self.x);
        }
    }

    /// Modify the attributes in place: `cell.update_attrs(|a| a.bold = true)`.
    pub fn update_attrs(&mut self, f: impl FnOnce(&mut Attrs)) {
        let mut a = self.attrs();
        f(&mut a);
        self.set_attrs(&a);
    }

    /// SGR 58 underline colour (`None` = follow the foreground).
    #[inline]
    pub fn underline_color(&self) -> Option<Color> {
        if self.x == 0 { None } else { with_extra(self.x, |e| e.ulc).flatten() }
    }

    // ── wrap / spacer ──
    #[inline] pub fn wrap(&self) -> bool { self.flags & F_WRAP != 0 }
    #[inline]
    pub fn set_wrap(&mut self, on: bool) {
        if on { self.flags |= F_WRAP } else { self.flags &= !F_WRAP }
    }
    /// End-of-row spacer ('\0' that is not a wide glyph's right half).
    #[inline] pub fn is_spacer(&self) -> bool { self.flags & F_SPACER != 0 }
    #[inline]
    pub fn set_spacer(&mut self, on: bool) {
        if on { self.flags |= F_SPACER } else { self.flags &= !F_SPACER }
    }

    // ── interned extras: hyperlink id, cluster extras, underline colour ──
    /// Interned id of (link, cluster, underline colour); `0` = none of them.
    #[inline]
    pub fn extra_id(&self) -> u16 {
        self.x
    }
    #[inline]
    pub fn set_extra_id(&mut self, x: u16) {
        self.x = x;
    }
    /// OSC 8 hyperlink id (index into `Terminal::hyperlinks`); `0` = none.
    #[inline]
    pub fn link(&self) -> u16 {
        if self.x == 0 { 0 } else { with_extra(self.x, |e| e.link).unwrap_or(0) }
    }
    pub fn set_link(&mut self, link: u16) {
        if self.x == 0 && link == 0 {
            return;
        }
        let e = extra_entry(self.x).unwrap_or_default();
        self.x = intern_extra(&e.cluster, link, e.ulc).unwrap_or(self.x);
    }
    /// Does the cell carry grapheme-cluster extras (combining marks, ...)?
    #[inline]
    pub fn has_cluster(&self) -> bool {
        self.x != 0 && with_extra(self.x, |e| !e.cluster.is_empty()).unwrap_or(false)
    }
    /// Drop the cluster extras (keeps link / underline colour).
    pub fn clear_cluster(&mut self) {
        if self.x != 0 {
            let e = extra_entry(self.x).unwrap_or_default();
            self.x = intern_extra("", e.link, e.ulc).unwrap_or(0);
        }
    }
    /// Replace the cluster extras with `extra` (keeps link / underline colour).
    pub fn set_cluster(&mut self, extra: &str) -> bool {
        let e = if self.x != 0 { extra_entry(self.x).unwrap_or_default() } else { Extra::default() };
        match intern_extra(extra, e.link, e.ulc) {
            Some(x) => {
                self.x = x;
                true
            }
            None => false,
        }
    }
    /// The extra code points of the cluster (empty when none).
    pub fn extra(&self) -> String {
        if self.x == 0 { String::new() } else { extra_entry(self.x).map(|e| e.cluster).unwrap_or_default() }
    }

    /// Append this cell's full text (base char + cluster extras) to `out`.
    /// Continuation cells append nothing.
    pub fn push_text(&self, out: &mut String) {
        if self.c == '\0' {
            return;
        }
        out.push(self.c);
        if self.x != 0 {
            with_extra(self.x, |e| out.push_str(&e.cluster));
        }
    }

    /// Full text of the cell (base + combining marks / ZWJ sequence).
    pub fn text(&self) -> String {
        let mut s = String::new();
        self.push_text(&mut s);
        s
    }
}

/// Length of `row` without trailing default blanks (how much of it a
/// scrollback row needs to keep).
#[inline]
pub fn trimmed_len(row: &[Cell]) -> usize {
    let mut end = row.len();
    while end > 0 && row[end - 1].is_default_blank() {
        end -= 1;
    }
    end
}

/// Compact a row for scrollback storage: drop trailing default blanks and
/// release the spare capacity. Readers treat missing cells as default blanks.
pub fn trim_row(row: &mut Vec<Cell>) {
    row.truncate(trimmed_len(row));
    row.shrink_to_fit();
}

// ── Screen grid with per-row extent hints ──

/// The visible screen: rows of cells plus, per row, an upper bound on how much
/// of it can be non-blank (`hi[r]`: every cell at index `>= hi[r]` is a default
/// blank). The hint lets `scroll_up` store a row in scrollback without scanning
/// (and pulling through the cache) the blank tail.
///
/// Reading is free (`Deref` to `Vec<Vec<Cell>>`). Any *untracked* mutable access
/// (`DerefMut`, indexing for write, `iter_mut`, ...) just marks the hints stale;
/// they are rebuilt by one scan the next time they are needed. The terminal's hot
/// paths use the tracked helpers instead, which keep the hints exact.
#[derive(Default)]
pub struct Grid {
    pub(super) rows: Vec<Vec<Cell>>,
    pub(super) hi: Vec<u16>,
    pub(super) dirty: bool,
}

impl Grid {
    /// A blank `cols` x `rows` screen.
    pub fn blank(cols: usize, rows: usize) -> Self {
        Self { rows: vec![vec![Cell::default(); cols]; rows], hi: vec![0; rows], dirty: false }
    }

    /// Take the rows out (consumes the hints).
    pub fn into_rows(self) -> Vec<Vec<Cell>> {
        self.rows
    }

    /// Rebuild the hints if untracked mutation made them stale.
    #[inline]
    pub(super) fn refresh_hints(&mut self) {
        if self.dirty {
            self.hi = self.rows.iter().map(|r| trimmed_len(r) as u16).collect();
            self.dirty = false;
        }
    }

    /// Record that row `r` may now have non-blank cells below index `end`.
    #[inline]
    pub(super) fn raise(&mut self, r: usize, end: usize) {
        if let Some(h) = self.hi.get_mut(r) {
            if (*h as usize) < end {
                *h = end as u16;
            }
        }
    }

    /// Fill all of row `r` with `blank`, keeping the hint exact.
    #[inline]
    pub(super) fn fill_row(&mut self, r: usize, blank: Cell) {
        self.rows[r].fill(blank);
        let n = if blank.is_default_blank() { 0 } else { self.rows[r].len() };
        if let Some(h) = self.hi.get_mut(r) {
            *h = n as u16;
        }
    }

    /// Fill `rows[r][a..b]` with `blank`, keeping the hint an upper bound.
    #[inline]
    pub(super) fn fill_span(&mut self, r: usize, a: usize, b: usize, blank: Cell) {
        self.rows[r][a..b].fill(blank);
        if !blank.is_default_blank() {
            self.raise(r, b);
        }
    }
}

impl From<Vec<Vec<Cell>>> for Grid {
    fn from(rows: Vec<Vec<Cell>>) -> Self {
        let hi = vec![0; rows.len()];
        Self { rows, hi, dirty: true }
    }
}

impl std::ops::Deref for Grid {
    type Target = Vec<Vec<Cell>>;
    #[inline]
    fn deref(&self) -> &Vec<Vec<Cell>> {
        &self.rows
    }
}

impl std::ops::DerefMut for Grid {
    /// Untracked mutation: the hints can no longer be trusted.
    #[inline]
    fn deref_mut(&mut self) -> &mut Vec<Vec<Cell>> {
        self.dirty = true;
        &mut self.rows
    }
}

impl<'a> IntoIterator for &'a Grid {
    type Item = &'a Vec<Cell>;
    type IntoIter = std::slice::Iter<'a, Vec<Cell>>;
    fn into_iter(self) -> Self::IntoIter {
        self.rows.iter()
    }
}

impl<'a> IntoIterator for &'a mut Grid {
    type Item = &'a mut Vec<Cell>;
    type IntoIter = std::slice::IterMut<'a, Vec<Cell>>;
    fn into_iter(self) -> Self::IntoIter {
        self.dirty = true;
        self.rows.iter_mut()
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

// ── Extras interner ──
//
// Cells stay `Copy` and small: the rare per-cell data (hyperlink id, extra code
// points of a grapheme cluster, SGR 58 underline colour) is interned
// process-wide as a tuple and referenced by one `u16` id. Id 0 is the empty
// tuple (no link, no cluster, no underline colour). Typical sessions use a few
// dozen distinct tuples; once the table is full further extras are dropped
// (the base character still renders).

#[derive(Clone, Default)]
pub(crate) struct Extra {
    pub cluster: String,
    pub link: u16,
    pub ulc: Option<Color>,
}

struct Interner {
    items: Vec<Extra>,
    map: std::collections::HashMap<(Box<str>, u16, u32), u16>,
}

fn interner() -> &'static std::sync::Mutex<Interner> {
    static I: std::sync::OnceLock<std::sync::Mutex<Interner>> = std::sync::OnceLock::new();
    I.get_or_init(|| {
        std::sync::Mutex::new(Interner { items: vec![Extra::default()], map: std::collections::HashMap::new() })
    })
}

impl Color {
    /// Compact, injective encoding (0 = Default).
    fn pack(self) -> u32 {
        match self {
            Color::Default => 0,
            Color::Indexed(n) => 1 << 24 | n as u32,
            Color::Rgb(r, g, b) => 2 << 24 | (r as u32) << 16 | (g as u32) << 8 | b as u32,
        }
    }
}

/// Intern a (cluster extras, link, underline colour) tuple; `None` when the
/// table is full.
pub(crate) fn intern_extra(cluster: &str, link: u16, ulc: Option<Color>) -> Option<u16> {
    if cluster.is_empty() && link == 0 && ulc.is_none() {
        return Some(0);
    }
    let packed = ulc.map_or(0, |c| match c {
        // `Some(Default)` must differ from `None`.
        Color::Default => 3 << 24,
        c => c.pack(),
    });
    let mut g = interner().lock().ok()?;
    if let Some(&id) = g.map.get(&(Box::from(cluster), link, packed)) {
        return Some(id);
    }
    if g.items.len() >= u16::MAX as usize {
        return None;
    }
    let id = g.items.len() as u16;
    g.items.push(Extra { cluster: cluster.to_string(), link, ulc });
    g.map.insert((Box::from(cluster), link, packed), id);
    Some(id)
}

/// Run `f` on the interned tuple without cloning it.
pub(crate) fn with_extra<R>(id: u16, f: impl FnOnce(&Extra) -> R) -> Option<R> {
    let g = interner().lock().ok()?;
    g.items.get(id as usize).map(f)
}

pub(crate) fn extra_entry(id: u16) -> Option<Extra> {
    if id == 0 {
        return Some(Extra::default());
    }
    let g = interner().lock().ok()?;
    g.items.get(id as usize).cloned()
}
