//! Kit widgets. Every widget is a method on [`Ctx`].
//!
//! Conventions
//! * Rectangles are in absolute buffer pixels.
//! * Inline widgets (`badge`, `kbd_hint`, `line`, ...) take `y` = the top of a
//!   *line box* of height `tk.row_h` and centre themselves vertically in it.
//! * Anything that can overflow truncates with "…" rather than hard-cutting.

use super::draw::{ellipsize, BOTTOM, TOP};
use super::tokens::{contrast, mix, Tone};
use super::{Ctx, Rect};
use crate::config::Rgb;

// ---- panel -------------------------------------------------------------

/// Chrome description for [`Ctx::panel`].
#[derive(Clone, Copy)]
pub struct PanelSpec<'a> {
    pub title: &'a str,
    pub subtitle: &'a str,
    pub badge: Option<(&'a str, Tone)>,
    pub hints: &'a [(&'a str, &'a str)],
    /// Append an "Esc close" hint to the footer when none is given.
    pub close_hint: bool,
    /// Tone for the panel border (severity); `None` = neutral border.
    pub edge: Option<Tone>,
}

impl<'a> PanelSpec<'a> {
    pub fn new(title: &'a str) -> Self {
        Self { title, subtitle: "", badge: None, hints: &[], close_hint: true, edge: None }
    }
    pub fn sub(mut self, s: &'a str) -> Self { self.subtitle = s; self }
    pub fn badge(mut self, text: &'a str, tone: Tone) -> Self { self.badge = Some((text, tone)); self }
    pub fn hints(mut self, h: &'a [(&'a str, &'a str)]) -> Self { self.hints = h; self }
    pub fn edge(mut self, t: Tone) -> Self { self.edge = Some(t); self }
    /// Do not auto-append the "Esc close" footer hint.
    pub fn no_close(mut self) -> Self { self.close_hint = false; self }
    /// Footer hints including the automatic "Esc close" (unless an "Esc" hint exists).
    pub fn footer_hints(&self) -> Vec<(&'a str, &'a str)> {
        let mut v: Vec<(&'a str, &'a str)> = self.hints.to_vec();
        if self.close_hint && !v.iter().any(|h| h.0 == "Esc") {
            v.push(("Esc", "close"));
        }
        v
    }
}

fn round8(v: usize, unit: usize) -> usize {
    v.div_ceil(unit) * unit
}

impl<'a> Ctx<'a> {
    /// Height of the panel title bar.
    pub fn title_h(&self) -> usize {
        round8(self.tk.row_h + self.tk.sp.sm, 4 * self.tk.scale)
    }

    /// Height of the footer band (when hints are present).
    pub fn footer_h(&self) -> usize {
        self.tk.row_h + self.tk.sp.xs
    }

    /// Draw a modal panel (shadow, border, title bar, footer hints) and
    /// return the padded content rectangle.
    pub fn panel(&mut self, r: Rect, spec: &PanelSpec) -> Rect {
        let tk = self.tk;
        self.shadow(r, tk.radius);
        let edge = spec.edge.map(|t| tk.tone(t)).unwrap_or(tk.border_strong);
        self.fill_rrect(r, tk.radius, edge);
        let inner = Rect::new(r.x + 1, r.y + 1, r.w.saturating_sub(2), r.h.saturating_sub(2));
        let irad = tk.radius.saturating_sub(1);
        let title_h = self.title_h().min(inner.h);
        let (tb, body) = inner.split_top(title_h);
        self.fill_rrect_ex(tb, irad, tk.surface_alt, 255, TOP);
        self.fill_rrect_ex(body, irad, tk.surface, 255, BOTTOM);
        self.hline(inner.x, body.y, inner.w, tk.border);

        // ---- title bar (title left, optional badge right)
        let pad = tk.sp.lg;
        let mut right = r.right().saturating_sub(pad);
        if let Some((text, tone)) = spec.badge {
            let bw = self.badge_w(text);
            self.badge(right.saturating_sub(bw), tb.y + 1, text, tone, tb.h);
            right = right.saturating_sub(bw + tk.sp.md);
        }
        let ty = self.text_y(tb.y + 1, tb.h);
        let avail = right.saturating_sub(r.x + pad);
        let title = ellipsize(spec.title, self.cols(avail));
        let tw = self.tw(&title);
        self.text(r.x + pad, ty, &title, tk.accent);
        if !spec.subtitle.is_empty() {
            let sx = r.x + pad + tw + tk.sp.md;
            if sx + 4 * tk.cw < right {
                self.text_fit(sx, ty, right - sx, spec.subtitle, tk.text_muted);
            }
        }

        // ---- footer: key hints; "Esc close" is appended unless already given
        let hints = spec.footer_hints();
        if !hints.is_empty() {
            let fh = self.footer_h().min(body.h);
            let fr = Rect::new(inner.x, inner.bottom() - fh, inner.w, fh);
            self.fill_rrect_ex(fr, irad, tk.surface_alt, 255, BOTTOM);
            self.hline(fr.x, fr.y, fr.w, tk.border);
            self.hint_row(r.x + pad, fr.y + 1, r.w.saturating_sub(2 * pad), fh.saturating_sub(1), &hints);
        }

        self.panel_content(r, !spec.footer_hints().is_empty())
    }

    /// Content rectangle [`panel`](Self::panel) returns for panel rect `r`
    /// (pure layout; lets other code align with a panel without drawing it).
    pub fn panel_content(&self, r: Rect, has_hints: bool) -> Rect {
        let tk = self.tk;
        let top = r.y + self.title_h().min(r.h.saturating_sub(2));
        let bottom = if has_hints { r.bottom().saturating_sub(self.footer_h()) } else { r.bottom() };
        let cy = top + tk.sp.md;
        Rect::new(r.x + tk.sp.lg, cy, r.w.saturating_sub(2 * tk.sp.lg), bottom.saturating_sub(cy + tk.sp.md))
    }

    /// X coordinate where a panel title bar's right-hand controls begin,
    /// i.e. the right edge available to extra badges.
    pub fn title_right_edge(&self, r: Rect) -> usize {
        r.right().saturating_sub(self.tk.sp.lg)
    }

    // ---- keyboard hints ----------------------------------------------------

    fn kbd_chip_h(&self) -> usize {
        self.tk.ch + self.tk.sp.xs
    }

    fn kbd_chip_w(&self, key: &str) -> usize {
        self.tw(key) + 2 * (self.tk.sp.xs + 2 * self.tk.scale)
    }

    /// Draw a key cap at `x`, vertically centred in `[y, y+h)`. Returns width.
    pub fn kbd_chip(&mut self, x: usize, y: usize, h: usize, key: &str) -> usize {
        let tk = self.tk;
        let w = self.kbd_chip_w(key);
        let ch = self.kbd_chip_h();
        let r = Rect::new(x, y + h.saturating_sub(ch) / 2, w, ch);
        self.fill_rrect(r, tk.radius_sm, tk.border_strong);
        self.fill_rrect(r.inset(1, 1), tk.radius_sm.saturating_sub(1), tk.elevated);
        self.text(x + tk.sp.xs + 2 * tk.scale, r.y + ch.saturating_sub(tk.ch) / 2, key, tk.text);
        w
    }

    /// `[Key] label` pair. `y` is the top of a `row_h` line box. Returns width.
    pub fn kbd_hint(&mut self, x: usize, y: usize, key: &str, label: &str) -> usize {
        let tk = self.tk;
        let kw = self.kbd_chip(x, y, tk.row_h, key);
        let lx = x + kw + tk.sp.sm;
        let ty = self.text_y(y, tk.row_h);
        self.text(lx, ty, label, tk.text_muted);
        kw + tk.sp.sm + self.tw(label)
    }

    /// Width [`kbd_hint`] would occupy.
    pub fn kbd_hint_w(&self, key: &str, label: &str) -> usize {
        self.kbd_chip_w(key) + self.tk.sp.sm + self.tw(label)
    }

    /// A row of key hints separated by `lg`; hints that do not fit are dropped.
    pub fn hint_row(&mut self, x: usize, y: usize, max_w: usize, h: usize, hints: &[(&str, &str)]) {
        let gap = self.tk.sp.lg;
        let mut cx = x;
        let line_y = y + h.saturating_sub(self.tk.row_h) / 2;
        for (k, l) in hints {
            let w = self.kbd_hint_w(k, l);
            if cx + w > x + max_w {
                break;
            }
            self.kbd_hint(cx, line_y, k, l);
            cx += w + gap;
        }
    }

    // ---- text line helpers ---------------------------------------------------

    /// Draw one line of text centred in a `row_h` line box at `y`.
    pub fn line(&mut self, x: usize, y: usize, s: &str, c: Rgb) {
        let ty = self.text_y(y, self.tk.row_h);
        self.text(x, ty, s, c);
    }

    /// Like [`line`](Self::line) but truncated to `max_w` with "…".
    pub fn line_fit(&mut self, x: usize, y: usize, max_w: usize, s: &str, c: Rgb) -> usize {
        let ty = self.text_y(y, self.tk.row_h);
        self.text_fit(x, ty, max_w, s, c)
    }

    /// Section heading: muted caption with a rule extending to the right.
    pub fn section(&mut self, x: usize, y: usize, w: usize, title: &str) {
        let tk = self.tk;
        let t = title.to_uppercase();
        let tw = self.line_fit(x, y, w, &t, tk.text_muted);
        let rx = x + tw + tk.sp.sm;
        if rx + tk.sp.lg < x + w {
            self.hline(rx, y + tk.row_h / 2, x + w - rx, tk.border);
        }
    }

    /// `key  value` pair; key column is `key_cols` characters wide.
    pub fn kv(&mut self, x: usize, y: usize, w: usize, key_cols: usize, key: &str, value: &str, tone: Option<Tone>) {
        let tk = self.tk;
        let kw = key_cols * tk.cw;
        self.line_fit(x, y, kw.min(w), key, tk.text_muted);
        let vc = tone.map(|t| tk.tone(t)).unwrap_or(tk.text);
        let vx = x + kw + tk.sp.sm;
        if vx < x + w {
            self.line_fit(vx, y, x + w - vx, value, vc);
        }
    }

    // ---- divider -----------------------------------------------------------

    pub fn divider(&mut self, x: usize, y: usize, w: usize) {
        self.hline(x, y, w, self.tk.border);
    }

    pub fn vdivider(&mut self, x: usize, y: usize, h: usize) {
        self.vline(x, y, h, self.tk.border);
    }

    // ---- badge -------------------------------------------------------------

    pub fn badge_w(&self, text: &str) -> usize {
        self.tw(text) + 2 * self.tk.sp.sm
    }

    fn badge_h(&self) -> usize {
        self.tk.ch + self.tk.sp.xs
    }

    /// Pill badge at `x`, centred in `[y, y+h)`. Returns its width.
    pub fn badge(&mut self, x: usize, y: usize, text: &str, tone: Tone, h: usize) -> usize {
        let tk = self.tk;
        let w = self.badge_w(text);
        let bh = self.badge_h();
        let r = Rect::new(x, y + h.saturating_sub(bh) / 2, w, bh);
        let fg = tk.tone(tone);
        self.fill_rrect_ex(r, bh / 2, fg, 46, super::draw::ALL);
        self.text(x + tk.sp.sm, r.y + (bh - tk.ch) / 2, text, fg);
        w
    }

    /// Badge on a `row_h` line box.
    pub fn badge_line(&mut self, x: usize, y: usize, text: &str, tone: Tone) -> usize {
        self.badge(x, y, text, tone, self.tk.row_h)
    }

    // ---- button ------------------------------------------------------------

    pub fn button_w(&self, label: &str) -> usize {
        self.tw(label) + 2 * self.tk.sp.lg
    }

    pub fn button(&mut self, r: Rect, label: &str, kind: ButtonKind, state: ButtonState) {
        let tk = self.tk;
        let disabled = state == ButtonState::Disabled;
        let (fill, border, fg) = match kind {
            ButtonKind::Primary => {
                let on = if contrast((0, 0, 0), tk.accent) > contrast((255, 255, 255), tk.accent) { (0, 0, 0) } else { (255, 255, 255) };
                (tk.accent, tk.accent, on)
            }
            ButtonKind::Secondary => (tk.surface_alt, tk.border_strong, tk.text),
            ButtonKind::Danger => (mix(tk.surface, tk.danger, 0.18), tk.danger, tk.danger),
        };
        let (fill, border, fg) = if disabled {
            (tk.surface_alt, tk.border, tk.text_faint)
        } else {
            (fill, border, fg)
        };
        self.fill_rrect(r, tk.radius_sm, border);
        self.fill_rrect(r.inset(1, 1), tk.radius_sm.saturating_sub(1), fill);
        if state == ButtonState::Focused {
            let ring = Rect::new(r.x.saturating_sub(2), r.y.saturating_sub(2), r.w + 4, r.h + 4);
            self.stroke_rrect(ring, tk.radius_sm + 2, 1, tk.accent);
        }
        let ty = self.text_y(r.y, r.h);
        self.text_center(r.x + tk.sp.sm, ty, r.w.saturating_sub(2 * tk.sp.sm), label, fg);
    }

    // ---- text input --------------------------------------------------------

    /// Single-line text field. `cursor`/`selection` are character indices.
    pub fn text_input(
        &mut self,
        r: Rect,
        text: &str,
        cursor: usize,
        selection: Option<(usize, usize)>,
        placeholder: &str,
        focused: bool,
    ) {
        let tk = self.tk;
        let edge = if focused { tk.accent } else { tk.border_strong };
        self.fill_rrect(r, tk.radius_sm, edge);
        self.fill_rrect(r.inset(1, 1), tk.radius_sm.saturating_sub(1), tk.field);
        let pad = tk.sp.md;
        let tx = r.x + pad;
        let ty = r.y + r.h.saturating_sub(tk.ch) / 2;
        let cols = self.cols(r.w.saturating_sub(2 * pad + 2 * tk.scale)).max(1);
        let chars: Vec<char> = text.chars().collect();
        let cursor = cursor.min(chars.len());
        let start = if cursor + 1 > cols { cursor + 1 - cols } else { 0 };
        let end = (start + cols).min(chars.len());
        if chars.is_empty() {
            if !placeholder.is_empty() {
                self.text_fit(tx, ty, r.w.saturating_sub(2 * pad), placeholder, tk.text_faint);
            }
        } else {
            if let Some((a, b)) = selection {
                let (a, b) = (a.min(b).max(start), a.max(b).min(end));
                if b > a {
                    let sr = Rect::new(tx + (a - start) * tk.cw, ty, (b - a) * tk.cw, tk.ch);
                    self.fill(sr, tk.selection);
                }
            }
            let shown: String = chars[start..end].iter().collect();
            self.text(tx, ty, &shown, tk.text);
        }
        if focused {
            let cx = tx + (cursor - start) * tk.cw;
            self.fill(Rect::new(cx, ty, (tk.scale).max(1) * 2, tk.ch), tk.accent);
        }
    }

    // ---- rows & lists ------------------------------------------------------

    /// Row background: selected rows get a soft accent fill and a left bar.
    pub fn row_bg(&mut self, r: Rect, selected: bool, hover: bool) {
        let tk = self.tk;
        if selected {
            self.fill_rrect(r, tk.radius_sm, tk.accent_soft);
            let bar = Rect::new(r.x + tk.scale, r.y + tk.sp.xs, 3 * tk.scale, r.h.saturating_sub(2 * tk.sp.xs));
            self.fill_rrect(bar, tk.scale + 1, tk.accent);
        } else if hover {
            self.fill_rrect(r, tk.radius_sm, tk.surface_alt);
        }
    }

    /// Number of whole rows that fit in `h` pixels.
    pub fn rows_fit(&self, h: usize) -> usize {
        (h / self.tk.row_h).max(1)
    }

    /// Draw a vertical scrollbar thumb at the right edge of `r` when needed.
    pub fn scrollbar(&mut self, r: Rect, total: usize, visible: usize, scroll: usize) {
        if total <= visible || r.h == 0 {
            return;
        }
        let tk = self.tk;
        let w = 3 * tk.scale;
        let x = r.right().saturating_sub(w);
        let thumb_h = (r.h * visible / total).max(2 * tk.sp.sm).min(r.h);
        let max_scroll = total - visible;
        let ty = r.y + (r.h - thumb_h) * scroll.min(max_scroll) / max_scroll.max(1);
        self.fill_rrect(Rect::new(x, ty, w, thumb_h), w / 2, tk.border_strong);
    }

    /// Selectable list. Returns the number of visible rows.
    pub fn list(&mut self, r: Rect, items: &[ListItem], selected: Option<usize>, scroll: usize, hover: Option<usize>) -> usize {
        let tk = self.tk;
        let vis = self.rows_fit(r.h);
        let scroll = scroll.min(items.len().saturating_sub(vis));
        let sb = if items.len() > vis { tk.sp.sm } else { 0 };
        let rw = r.w.saturating_sub(sb);
        for (n, item) in items.iter().skip(scroll).take(vis).enumerate() {
            let idx = scroll + n;
            let row = Rect::new(r.x, r.y + n * tk.row_h, rw, tk.row_h);
            let sel = selected == Some(idx);
            self.row_bg(row, sel, hover == Some(idx));
            let fg = match item.tone {
                Some(t) => tk.tone(t),
                None if item.dim => tk.text_muted,
                None => tk.text,
            };
            let mut right = row.right().saturating_sub(tk.sp.md);
            if !item.meta.is_empty() {
                let meta = ellipsize(item.meta, self.cols(rw / 3));
                let mw = self.tw(&meta);
                self.line(right.saturating_sub(mw), row.y, &meta, tk.text_muted);
                right = right.saturating_sub(mw + tk.sp.md);
            }
            let lx = row.x + tk.sp.md + tk.scale * 2;
            if right > lx {
                self.line_fit(lx, row.y, right - lx, item.label, fg);
            }
        }
        self.scrollbar(r, items.len(), vis, scroll);
        vis
    }

    // ---- table -------------------------------------------------------------

    /// Table with a header row. Returns the number of visible body rows.
    pub fn table(&mut self, r: Rect, cols: &[Column], rows: &[TableRow], selected: Option<usize>, scroll: usize) -> usize {
        let tk = self.tk;
        let gap = tk.sp.md;
        let pad = tk.sp.md;
        let inner_w = r.w.saturating_sub(2 * pad + gap * cols.len().saturating_sub(1) + tk.sp.sm);
        let fixed: usize = cols.iter().map(|c| if let Width::Cols(n) = c.width { n * tk.cw } else { 0 }).sum();
        let flex_total: usize = cols.iter().map(|c| if let Width::Flex(f) = c.width { f as usize } else { 0 }).sum();
        let free = inner_w.saturating_sub(fixed);
        let widths: Vec<usize> = cols
            .iter()
            .map(|c| match c.width {
                Width::Cols(n) => n * tk.cw,
                Width::Flex(f) => if flex_total == 0 { 0 } else { free * f as usize / flex_total },
            })
            .collect();

        // header
        let mut x = r.x + pad + 2 * tk.scale;
        for (c, w) in cols.iter().zip(&widths) {
            let t = ellipsize(&c.title.to_uppercase(), self.cols(*w));
            let tx = if c.align == Align::Right { x + w.saturating_sub(self.tw(&t)) } else { x };
            self.line(tx, r.y, &t, tk.text_muted);
            x += w + gap;
        }
        self.divider(r.x, r.y + tk.row_h - 1, r.w);

        let body = Rect::new(r.x, r.y + tk.row_h, r.w, r.h.saturating_sub(tk.row_h));
        let vis = self.rows_fit(body.h);
        let vis = if body.h < tk.row_h { 0 } else { vis };
        let scroll = scroll.min(rows.len().saturating_sub(vis));
        let sb = if rows.len() > vis { tk.sp.sm } else { 0 };
        for (n, row) in rows.iter().skip(scroll).take(vis).enumerate() {
            let idx = scroll + n;
            let rr = Rect::new(body.x, body.y + n * tk.row_h, body.w.saturating_sub(sb), tk.row_h);
            self.row_bg(rr, selected == Some(idx), false);
            let mut x = rr.x + pad + 2 * tk.scale;
            for (i, (c, w)) in cols.iter().zip(&widths).enumerate() {
                let cell = row.cells.get(i).map(|s| s.as_str()).unwrap_or("");
                let t = ellipsize(cell, self.cols(*w));
                let tx = if c.align == Align::Right { x + w.saturating_sub(self.tw(&t)) } else { x };
                let col = row.tone_for(i).map(|t| tk.tone(t)).unwrap_or(tk.text);
                self.line(tx, rr.y, &t, col);
                x += w + gap;
            }
        }
        self.scrollbar(body, rows.len(), vis, scroll);
        vis
    }

    // ---- progress ----------------------------------------------------------

    /// Horizontal progress bar; `frac` in 0..=1.
    pub fn progress(&mut self, r: Rect, frac: f32, tone: Tone) {
        let tk = self.tk;
        let h = (6 * tk.scale).min(r.h).max(2);
        let y = r.y + (r.h - h) / 2;
        let track = Rect::new(r.x, y, r.w, h);
        self.fill_rrect(track, h / 2, tk.border);
        let fw = ((r.w as f32) * frac.clamp(0.0, 1.0)).round() as usize;
        if fw > 0 {
            self.fill_rrect(Rect::new(r.x, y, fw.max(h), h), h / 2, tk.tone(if tone == Tone::Neutral { Tone::Accent } else { tone }));
        }
    }

    // ---- sparkline -----------------------------------------------------------

    /// Filled area sparkline over `r`: `data` is resampled to the width and
    /// scaled so `max` fills the height. A faint baseline anchors it.
    pub fn sparkline(&mut self, r: Rect, data: &[f32], max: f32, c: Rgb) {
        if r.w == 0 || r.h == 0 {
            return;
        }
        let base = r.bottom() - 1;
        self.hline(r.x, base, r.w, self.tk.border);
        for (i, hh) in super::draw::spark_heights(data, r.w, r.h - 1, max).into_iter().enumerate() {
            if hh == 0 {
                continue;
            }
            let x = r.x + i;
            self.fill_a(Rect::new(x, base - hh, 1, hh), c, 60);
            self.put(x, base - hh, c, 255);
        }
    }

    // ---- toast -------------------------------------------------------------

    /// Toast anchored at the bottom centre of the screen.
    pub fn toast(&mut self, tone: Tone, msg: &str) -> Rect {
        let tk = self.tk;
        let w = self.toast_w(msg, self.w.saturating_sub(2 * tk.sp.xl));
        let x = (self.w.saturating_sub(w)) / 2;
        let y = self.h.saturating_sub(tk.row_h + tk.sp.sm * 2 + tk.sp.xl + self.footer_h());
        self.toast_at(x, y, w, tone, msg)
    }

    /// Width a toast needs for `msg`, capped at `max_w`.
    pub fn toast_w(&self, msg: &str, max_w: usize) -> usize {
        let tk = self.tk;
        (self.tw(msg) + 2 * tk.sp.lg + 3 * tk.scale + tk.sp.sm).min(max_w)
    }

    /// Toast with explicit position/width. Returns its rectangle.
    pub fn toast_at(&mut self, x: usize, y: usize, w: usize, tone: Tone, msg: &str) -> Rect {
        let tk = self.tk;
        let h = tk.row_h + tk.sp.md;
        let r = Rect::new(x, y, w, h);
        self.shadow(r, tk.radius);
        self.fill_rrect(r, tk.radius, tk.border_strong);
        self.fill_rrect(r.inset(1, 1), tk.radius.saturating_sub(1), tk.elevated);
        let bar = Rect::new(r.x + tk.sp.sm, r.y + tk.sp.sm, 3 * tk.scale, h - 2 * tk.sp.sm);
        self.fill_rrect(bar, tk.scale + 1, if tone == Tone::Neutral { tk.border_strong } else { tk.tone(tone) });
        let tx = bar.right() + tk.sp.sm;
        let ty = self.text_y(r.y, h);
        self.text_fit(tx, ty, r.right().saturating_sub(tx + tk.sp.md), msg, tk.text);
        r
    }

    /// Bottom-centre toast with an animated spinner (async operations).
    pub fn spinner_toast(&mut self, msg: &str, elapsed: f32) -> Rect {
        let tk = self.tk;
        let w = (self.tw(msg) + 2 * tk.sp.lg + 3 * tk.cw).min(self.w.saturating_sub(2 * tk.sp.xl));
        let h = tk.row_h + tk.sp.md;
        let r = Rect::new(self.w.saturating_sub(w) / 2, self.h.saturating_sub(h + tk.sp.xl + tk.row_h), w, h);
        self.shadow(r, tk.radius);
        self.fill_rrect(r, tk.radius, tk.border_strong);
        self.fill_rrect(r.inset(1, 1), tk.radius.saturating_sub(1), tk.elevated);
        let ty = self.text_y(r.y, h);
        let sx = r.x + tk.sp.lg;
        self.text(sx, ty, &crate::ui::spinner_char(elapsed).to_string(), tk.accent);
        self.text_fit(sx + 2 * tk.cw, ty, r.right().saturating_sub(sx + 2 * tk.cw + tk.sp.lg), msg, tk.text);
        r
    }

    // ---- empty state -------------------------------------------------------

    pub fn empty_state(&mut self, r: Rect, msg: &str, hint: &str) {
        let tk = self.tk;
        let lines = if hint.is_empty() { 1 } else { 2 };
        let total = lines * tk.row_h;
        let y = r.y + r.h.saturating_sub(total) / 2;
        self.text_center(r.x, self.text_y(y, tk.row_h), r.w, msg, tk.text_muted);
        if !hint.is_empty() {
            self.text_center(r.x, self.text_y(y + tk.row_h, tk.row_h), r.w, hint, tk.text_faint);
        }
    }

    /// Tab strip on a `row_h` line box: active tab accent + underline, others
    /// muted. Returns the x extent used.
    pub fn tabs(&mut self, x: usize, y: usize, max_w: usize, labels: &[&str], active: usize) -> usize {
        let tk = self.tk;
        let mut cx = x;
        for (i, l) in labels.iter().enumerate() {
            let tw = self.tw(l);
            if cx + tw > x + max_w {
                break;
            }
            let on = i == active;
            self.line(cx, y, l, if on { tk.accent } else { tk.text_muted });
            if on {
                let uy = y + tk.row_h - 2 * tk.scale;
                self.fill(Rect::new(cx, uy, tw, 2 * tk.scale), tk.accent);
            }
            cx += tw + tk.sp.lg;
        }
        cx - x
    }

    /// Floating, non-modal container (search bar, popups): shadow + border +
    /// elevated fill. Returns the padded inner rectangle.
    pub fn float(&mut self, r: Rect) -> Rect {
        let tk = self.tk;
        self.shadow(r, tk.radius);
        self.fill_rrect(r, tk.radius, tk.border_strong);
        self.fill_rrect(r.inset(1, 1), tk.radius.saturating_sub(1), tk.elevated);
        r.inset(tk.sp.sm, tk.sp.sm)
    }

    /// Draw an inline code/preview block (field background, rounded).
    pub fn well(&mut self, r: Rect) {
        let tk = self.tk;
        self.fill_rrect(r, tk.radius_sm, tk.border);
        self.fill_rrect(r.inset(1, 1), tk.radius_sm.saturating_sub(1), tk.field);
    }
}

/// Keep `selected` inside the `[scroll, scroll+visible)` window.
pub fn scroll_into_view(selected: usize, scroll: usize, visible: usize) -> usize {
    if visible == 0 {
        return 0;
    }
    if selected < scroll {
        selected
    } else if selected >= scroll + visible {
        selected + 1 - visible
    } else {
        scroll
    }
}

#[derive(Clone, Copy)]
pub struct ListItem<'a> {
    pub label: &'a str,
    pub meta: &'a str,
    pub tone: Option<Tone>,
    pub dim: bool,
}

impl<'a> ListItem<'a> {
    pub fn new(label: &'a str) -> Self { Self { label, meta: "", tone: None, dim: false } }
    pub fn meta(mut self, m: &'a str) -> Self { self.meta = m; self }
    pub fn tone(mut self, t: Tone) -> Self { self.tone = Some(t); self }
    pub fn dim(mut self) -> Self { self.dim = true; self }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ButtonKind { Primary, Secondary, Danger }

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ButtonState { Normal, Focused, Disabled }

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Align { Left, Right }

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Width {
    /// Fixed width in character cells.
    Cols(usize),
    /// Share of the remaining space.
    Flex(u8),
}

#[derive(Clone, Copy)]
pub struct Column<'a> {
    pub title: &'a str,
    pub width: Width,
    pub align: Align,
}

impl<'a> Column<'a> {
    pub fn new(title: &'a str, width: Width) -> Self { Self { title, width, align: Align::Left } }
    pub fn right(mut self) -> Self { self.align = Align::Right; self }
}

pub struct TableRow {
    pub cells: Vec<String>,
    /// Tone applied to the first cell (status/marker column).
    pub tone: Option<Tone>,
    /// Extra per-cell tones as `(column, tone)`.
    pub cell_tones: Vec<(usize, Tone)>,
}

impl TableRow {
    pub fn new<S: Into<String>>(cells: Vec<S>) -> Self {
        Self { cells: cells.into_iter().map(Into::into).collect(), tone: None, cell_tones: Vec::new() }
    }
    pub fn tone(mut self, t: Tone) -> Self { self.tone = Some(t); self }
    pub fn cell_tone(mut self, col: usize, t: Tone) -> Self { self.cell_tones.push((col, t)); self }
    fn tone_for(&self, col: usize) -> Option<Tone> {
        self.cell_tones.iter().find(|(c, _)| *c == col).map(|(_, t)| *t).or(if col == 0 { self.tone } else { None })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Theme;
    use crate::renderer::font::FontManager;
    use crate::ui::kit::Tokens;

    #[test]
    fn center_scroll_clamps() {
        assert_eq!(center_scroll(0, 100, 10), 0);
        assert_eq!(center_scroll(50, 100, 10), 45);
        assert_eq!(center_scroll(99, 100, 10), 90);
        assert_eq!(center_scroll(3, 5, 10), 0);
    }

    #[test]
    fn scroll_window_follows_selection() {
        assert_eq!(scroll_into_view(0, 0, 5), 0);
        assert_eq!(scroll_into_view(7, 0, 5), 3);
        assert_eq!(scroll_into_view(2, 4, 5), 2);
        assert_eq!(scroll_into_view(6, 4, 5), 4);
        assert_eq!(scroll_into_view(3, 0, 0), 0);
    }

    /// Smoke-test: drawing every widget into a small buffer never panics,
    /// even when the buffer is far smaller than the requested rects.
    #[test]
    fn widgets_clip_instead_of_panicking() {
        let Some(path) = ["/System/Library/Fonts/Menlo.ttc", "/System/Library/Fonts/Monaco.ttf", "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf"]
            .into_iter()
            .find(|p| std::path::Path::new(p).exists())
        else {
            return;
        };
        let mut font = FontManager::new(path, 14.0);
        let tk = Tokens::new(&Theme::catppuccin_mocha(), font.cell_width, font.cell_height);
        let (w, h) = (120usize, 90usize);
        let mut buf = vec![0u32; w * h];
        let mut cx = Ctx::new(&mut buf, w, h, &mut font, &tk);
        cx.backdrop(0.5);
        let body = cx.panel(Rect::new(10, 10, 400, 400), &PanelSpec::new("A very long title that must truncate").sub("sub").badge("beta", Tone::Warning).hints(&[("Esc", "close"), ("Enter", "ok")]));
        cx.list(body, &[ListItem::new("x").meta("y"); 30], Some(2), 5, None);
        cx.text_input(Rect::new(0, 0, 300, 40), "hello world", 5, Some((1, 4)), "ph", true);
        cx.button(Rect::new(5, 5, 200, 30), "OK", ButtonKind::Primary, ButtonState::Focused);
        cx.table(Rect::new(0, 0, 500, 300), &[Column::new("a", Width::Flex(1)), Column::new("b", Width::Cols(4)).right()], &[TableRow::new(vec!["1", "2"])], Some(0), 0);
        cx.toast(Tone::Danger, "boom");
        cx.progress(Rect::new(0, 0, 100, 10), 0.5, Tone::Success);
        cx.empty_state(Rect::new(0, 0, 100, 80), "nothing", "try again");
    }
}

/// Stateless scroll offset that keeps `selected` roughly centred in a window
/// of `visible` rows over `total` items.
pub fn center_scroll(selected: usize, total: usize, visible: usize) -> usize {
    if visible == 0 || total <= visible {
        return 0;
    }
    selected.saturating_sub(visible / 2).min(total - visible)
}
