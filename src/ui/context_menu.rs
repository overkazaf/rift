//! Right-click context menu: item model, layout, hit-testing, keyboard
//! navigation and software rendering. Execution of the chosen command lives
//! in `app::mouse` so this module stays free of app state.

use crate::config::Theme;
use crate::renderer::font::FontManager;
use crate::ui::{darken, draw_border, fill_rect, lighten, pack_rgb};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MenuCmd {
    Copy,
    Paste,
    SelectAll,
    ClearBuffer,
    SplitRight,
    SplitDown,
    ZoomPane,
    ClosePane,
    OpenLink(String),
    CopyLink(String),
    SearchWeb,
    AskAi,
}

#[derive(Clone, Debug)]
pub struct Item {
    pub label: String,
    pub hint: String,
    /// `None` makes this row a separator.
    pub cmd: Option<MenuCmd>,
    pub enabled: bool,
}

impl Item {
    fn new(label: &str, hint: &str, cmd: MenuCmd, enabled: bool) -> Self {
        Self { label: label.into(), hint: hint.into(), cmd: Some(cmd), enabled }
    }

    fn sep() -> Self {
        Self { label: String::new(), hint: String::new(), cmd: None, enabled: false }
    }

    pub fn is_separator(&self) -> bool {
        self.cmd.is_none()
    }

    fn selectable(&self) -> bool {
        self.cmd.is_some() && self.enabled
    }
}

fn mod_name() -> &'static str {
    if cfg!(target_os = "macos") { "Cmd" } else { "Ctrl" }
}

/// Items for the pane context menu.
pub fn build_items(has_selection: bool, link: Option<&str>, zoomed: bool) -> Vec<Item> {
    let m = mod_name();
    let mut v = vec![
        Item::new("Copy", &format!("{m}+C"), MenuCmd::Copy, has_selection),
        Item::new("Paste", &format!("{m}+V"), MenuCmd::Paste, true),
        Item::new("Select All", &format!("{m}+A"), MenuCmd::SelectAll, true),
        Item::new("Clear Buffer", &format!("{m}+Alt+K"), MenuCmd::ClearBuffer, true),
        Item::sep(),
        Item::new("Split Right", &format!("{m}+D"), MenuCmd::SplitRight, true),
        Item::new("Split Down", &format!("{m}+Shift+D"), MenuCmd::SplitDown, true),
        Item::new(
            if zoomed { "Unzoom Pane" } else { "Zoom Pane" },
            &format!("{m}+Shift+Enter"),
            MenuCmd::ZoomPane,
            true,
        ),
        Item::new("Close Pane", &format!("{m}+W"), MenuCmd::ClosePane, true),
    ];
    if let Some(url) = link {
        v.push(Item::sep());
        v.push(Item::new("Open Link in Browser", "", MenuCmd::OpenLink(url.to_string()), true));
        v.push(Item::new("Copy Link", "", MenuCmd::CopyLink(url.to_string()), true));
    }
    v.push(Item::sep());
    v.push(Item::new("Search Selection on Web", "", MenuCmd::SearchWeb, has_selection));
    v.push(Item::new("Ask AI about Selection", "", MenuCmd::AskAi, has_selection));
    v
}

/// Pixel rectangle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
}

impl Rect {
    pub fn contains(&self, px: usize, py: usize) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }
}

#[derive(Default)]
pub struct ContextMenu {
    pub visible: bool,
    anchor: (usize, usize),
    items: Vec<Item>,
    /// Highlighted row (hover or keyboard).
    pub sel: Option<usize>,
}

const PAD_X: usize = 12;
const PAD_Y: usize = 6;

impl ContextMenu {
    pub fn open(&mut self, x: usize, y: usize, items: Vec<Item>) {
        self.anchor = (x, y);
        self.items = items;
        self.sel = None;
        self.visible = true;
    }

    pub fn close(&mut self) {
        self.visible = false;
        self.items.clear();
        self.sel = None;
    }

    #[cfg(test)]
    pub fn items(&self) -> &[Item] {
        &self.items
    }

    fn item_h(ch: usize) -> usize {
        ch + 8
    }

    fn sep_h() -> usize {
        9
    }

    fn row_h(&self, i: usize, ch: usize) -> usize {
        if self.items[i].is_separator() { Self::sep_h() } else { Self::item_h(ch) }
    }

    /// Menu rectangle, kept inside the window (flips left/up near edges).
    pub fn rect(&self, cw: usize, ch: usize, win_w: usize, win_h: usize) -> Rect {
        let cols = self
            .items
            .iter()
            .map(|it| {
                let hint = if it.hint.is_empty() { 0 } else { crate::ui::tabbar::text_cols(&it.hint) + 3 };
                crate::ui::tabbar::text_cols(&it.label) + hint
            })
            .max()
            .unwrap_or(8);
        let w = (cols * cw + 2 * PAD_X).min(win_w);
        let h = (0..self.items.len()).map(|i| self.row_h(i, ch)).sum::<usize>() + 2 * PAD_Y;
        let h = h.min(win_h);
        let (ax, ay) = self.anchor;
        let x = if ax + w > win_w { ax.saturating_sub(w).min(win_w.saturating_sub(w)) } else { ax };
        let y = if ay + h > win_h { ay.saturating_sub(h).min(win_h.saturating_sub(h)) } else { ay };
        Rect { x, y, w, h }
    }

    /// Row rectangle (absolute) of item `i`.
    fn item_rect(&self, i: usize, r: Rect, ch: usize) -> Rect {
        let y = r.y + PAD_Y + (0..i).map(|k| self.row_h(k, ch)).sum::<usize>();
        Rect { x: r.x + 4, y, w: r.w.saturating_sub(8), h: self.row_h(i, ch) }
    }

    /// Item under the pixel, ignoring separators.
    pub fn item_at(&self, px: usize, py: usize, cw: usize, ch: usize, ww: usize, wh: usize) -> Option<usize> {
        let r = self.rect(cw, ch, ww, wh);
        if !r.contains(px, py) {
            return None;
        }
        (0..self.items.len()).find(|&i| !self.items[i].is_separator() && self.item_rect(i, r, ch).contains(px, py))
    }

    pub fn contains(&self, px: usize, py: usize, cw: usize, ch: usize, ww: usize, wh: usize) -> bool {
        self.visible && self.rect(cw, ch, ww, wh).contains(px, py)
    }

    /// Command of item `i` when it is enabled.
    pub fn command(&self, i: usize) -> Option<MenuCmd> {
        let it = self.items.get(i)?;
        if it.enabled { it.cmd.clone() } else { None }
    }

    /// Hover: highlight an enabled item (or clear).
    pub fn hover(&mut self, item: Option<usize>) -> bool {
        let new = item.filter(|&i| self.items.get(i).map_or(false, Item::selectable));
        let changed = new != self.sel;
        self.sel = new;
        changed
    }

    /// Move the keyboard highlight by `delta` (+1 / -1), skipping separators
    /// and disabled rows, wrapping around.
    pub fn move_sel(&mut self, delta: isize) {
        let n = self.items.len() as isize;
        if n == 0 || !self.items.iter().any(Item::selectable) {
            return;
        }
        let mut i = match self.sel {
            Some(s) => s as isize,
            None if delta > 0 => -1,
            None => n,
        };
        for _ in 0..n {
            i = (i + delta).rem_euclid(n);
            if self.items[i as usize].selectable() {
                self.sel = Some(i as usize);
                return;
            }
        }
    }

    /// Jump to the first / last selectable row.
    pub fn select_edge(&mut self, last: bool) {
        self.sel = None;
        self.move_sel(if last { -1 } else { 1 });
    }

    pub fn render(
        &self,
        buffer: &mut [u32],
        buf_w: usize,
        buf_h: usize,
        font: &mut FontManager,
        theme: &Theme,
    ) {
        if !self.visible || self.items.is_empty() {
            return;
        }
        let cw = font.cell_width;
        let ch = font.cell_height;
        let r = self.rect(cw, ch, buf_w, buf_h);

        // Soft shadow.
        for d in 1..=3usize {
            let a = 4 - d;
            for y in (r.y + d)..(r.y + r.h + d).min(buf_h) {
                for x in (r.x + d)..(r.x + r.w + d).min(buf_w) {
                    if r.contains(x, y) {
                        continue;
                    }
                    let i = y * buf_w + x;
                    let p = buffer[i];
                    let f = |c: u32| c - c * a as u32 / 24;
                    buffer[i] = (f((p >> 16) & 0xff) << 16) | (f((p >> 8) & 0xff) << 8) | f(p & 0xff);
                }
            }
        }

        let panel = pack_rgb(lighten(theme.bg, 14));
        let border = pack_rgb(lighten(theme.bg, 44));
        fill_rect(buffer, buf_w, r.x, r.y, r.w, r.h, panel);
        draw_border(buffer, buf_w, r.x, r.y, r.w, r.h, border);

        let hover_bg = pack_rgb(crate::ui::dim(theme.cursor, 0.45));
        let fg = theme.fg;
        let muted = crate::ui::dim(theme.fg, 0.55);
        let faint = crate::ui::dim(theme.fg, 0.32);
        for (i, it) in self.items.iter().enumerate() {
            let ir = self.item_rect(i, r, ch);
            if it.is_separator() {
                let y = ir.y + ir.h / 2;
                fill_rect(buffer, buf_w, ir.x + 4, y, ir.w.saturating_sub(8), 1, pack_rgb(darken(lighten(theme.bg, 40), 6)));
                continue;
            }
            let selected = self.sel == Some(i) && it.enabled;
            if selected {
                fill_rect(buffer, buf_w, ir.x, ir.y, ir.w, ir.h, hover_bg);
            }
            let ty = ir.y + (ir.h.saturating_sub(ch)) / 2;
            let color = if !it.enabled { faint } else { fg };
            crate::ui::tabbar::draw_text(buffer, buf_w, buf_h, font, &it.label, ir.x + PAD_X - 4, ty, ir.x + ir.w, color);
            if !it.hint.is_empty() {
                let hw = crate::ui::tabbar::text_cols(&it.hint) * cw;
                let hx = (ir.x + ir.w).saturating_sub(hw + PAD_X - 4);
                let hc = if !it.enabled { faint } else { muted };
                crate::ui::tabbar::draw_text(buffer, buf_w, buf_h, font, &it.hint, hx, ty, ir.x + ir.w, hc);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn menu(sel: bool, link: Option<&str>) -> ContextMenu {
        let mut m = ContextMenu::default();
        m.open(10, 10, build_items(sel, link, false));
        m
    }

    #[test]
    fn items_depend_on_context() {
        let no_sel = build_items(false, None, false);
        let copy = no_sel.iter().find(|i| i.cmd == Some(MenuCmd::Copy)).unwrap();
        assert!(!copy.enabled);
        assert!(no_sel.iter().all(|i| !matches!(i.cmd, Some(MenuCmd::OpenLink(_)))));

        let with = build_items(true, Some("https://a.b/c"), true);
        assert!(with.iter().any(|i| i.cmd == Some(MenuCmd::OpenLink("https://a.b/c".into()))));
        assert!(with.iter().any(|i| i.cmd == Some(MenuCmd::CopyLink("https://a.b/c".into()))));
        assert!(with.iter().find(|i| i.cmd == Some(MenuCmd::Copy)).unwrap().enabled);
        assert!(with.iter().any(|i| i.label == "Unzoom Pane"));
        // Never two separators in a row, never one at the ends.
        for items in [&no_sel, &with] {
            assert!(!items.first().unwrap().is_separator());
            assert!(!items.last().unwrap().is_separator());
            assert!(items.windows(2).all(|w| !(w[0].is_separator() && w[1].is_separator())));
        }
    }

    #[test]
    fn keyboard_navigation_skips_separators_and_disabled() {
        let mut m = menu(false, None);
        m.move_sel(1);
        // Copy is disabled -> first stop is Paste.
        assert_eq!(m.command(m.sel.unwrap()), Some(MenuCmd::Paste));
        m.move_sel(-1);
        // Wraps to the last enabled item (Search / Ask are disabled too).
        assert_eq!(m.command(m.sel.unwrap()), Some(MenuCmd::ClosePane));
        m.move_sel(1);
        assert_eq!(m.command(m.sel.unwrap()), Some(MenuCmd::Paste));
        for _ in 0..3 {
            m.move_sel(1);
        }
        assert_eq!(m.command(m.sel.unwrap()), Some(MenuCmd::SplitRight), "separator skipped");
        m.select_edge(true);
        assert_eq!(m.command(m.sel.unwrap()), Some(MenuCmd::ClosePane));
        m.select_edge(false);
        assert_eq!(m.command(m.sel.unwrap()), Some(MenuCmd::Paste));
    }

    #[test]
    fn geometry_stays_on_screen_and_hits_rows() {
        let mut m = menu(true, None);
        let (cw, ch, ww, wh) = (9, 18, 800, 600);
        let r = m.rect(cw, ch, ww, wh);
        assert_eq!((r.x, r.y), (10, 10));
        // Click in the first row hits item 0.
        let first = m.item_rect(0, r, ch);
        assert_eq!(m.item_at(first.x + 2, first.y + 2, cw, ch, ww, wh), Some(0));
        // Outside is not inside.
        assert!(!m.contains(r.x + r.w + 5, r.y, cw, ch, ww, wh) || !m.visible);
        // Near the bottom-right corner the menu flips inside the window.
        m.open(795, 595, build_items(true, None, false));
        let r = m.rect(cw, ch, ww, wh);
        assert!(r.x + r.w <= ww && r.y + r.h <= wh);
        assert!(r.x < 795 && r.y < 595);
    }

    #[test]
    fn separator_rows_are_not_hit() {
        let m = menu(true, None);
        let (cw, ch, ww, wh) = (9, 18, 800, 600);
        let r = m.rect(cw, ch, ww, wh);
        let sep = m.items().iter().position(Item::is_separator).unwrap();
        let sr = m.item_rect(sep, r, ch);
        assert_eq!(m.item_at(sr.x + 3, sr.y + 3, cw, ch, ww, wh), None);
    }

    #[test]
    fn hover_ignores_disabled() {
        let mut m = menu(false, None);
        assert!(!m.hover(Some(0))); // Copy disabled
        assert_eq!(m.sel, None);
        assert!(m.hover(Some(1)));
        assert_eq!(m.sel, Some(1));
        assert!(m.command(0).is_none());
    }
}
