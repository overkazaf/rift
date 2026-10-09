//! Change Review rendering: the review overlay and the per-pane chip.
//! Everything uses the UI kit (`Tokens` / `Ctx`); no hard-coded palette.

use super::diff::{FileDiff, FileStatus, LineKind, Row};
use super::{Focus, Review, TurnKind, ViewState};
use crate::config::Theme;
use crate::renderer::font::FontManager;
use crate::ui::kit::{mix, scroll_into_view, Ctx, ListItem, PanelSpec, Rect, Tokens, Tone};
use crate::window::manager::WindowManager;
use crate::window::tab::PaneRect;

/// Make arbitrary file text safe to draw: tabs to spaces, control chars visible.
pub fn clean(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\t' => out.push_str("    "),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => out.push('\u{b7}'),
            c => out.push(c),
        }
    }
    out
}

fn ago(secs: u64) -> String {
    crate::mcp::overlay::ago(secs)
}

/// `…tail` if `s` is longer than `cols`: for paths the file name matters most.
fn tail_fit(s: &str, cols: usize) -> String {
    let n = s.chars().count();
    if n <= cols || cols < 2 {
        return s.to_string();
    }
    let tail: String = s.chars().skip(n - (cols - 1)).collect();
    format!("\u{2026}{tail}")
}

fn status_tone(s: FileStatus) -> Tone {
    match s {
        FileStatus::Added | FileStatus::Copied => Tone::Success,
        FileStatus::Deleted => Tone::Danger,
        FileStatus::Renamed => Tone::Accent,
        FileStatus::Modified => Tone::Warning,
    }
}

fn digits(n: u32) -> usize {
    n.max(1).to_string().len()
}

fn max_line_no(f: &FileDiff) -> u32 {
    f.hunks.iter().map(|h| (h.old_start + h.old_len).max(h.new_start + h.new_len)).max().unwrap_or(0)
}

impl Review {
    pub fn render(&mut self, buffer: &mut [u32], width: usize, height: usize, font: &mut FontManager, theme: &Theme) {
        if !self.ui.visible {
            return;
        }
        let tk = Tokens::new(theme, font.cell_width, font.cell_height);
        let mut cx = Ctx::new(buffer, width, height, font, &tk);
        cx.backdrop(tk.backdrop);

        // ── header strings ──
        let repo_name = self
            .ui
            .range
            .as_ref()
            .and_then(|r| r.repo.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_default();
        let range_label = self.ui.range.as_ref().map(|r| r.label.clone()).unwrap_or_default();
        let sub = match (repo_name.is_empty(), range_label.is_empty()) {
            (true, true) => String::new(),
            (false, true) => repo_name,
            (true, false) => range_label,
            (false, false) => format!("{repo_name} \u{b7} {range_label}"),
        };
        let (badge, tone) = match &self.ui.view {
            ViewState::Ready(d) => {
                let (f, a, r) = d.totals();
                let t = format!("{f} file{} +{a} -{r}{}", if f == 1 { "" } else { "s" }, if d.truncated { " (truncated)" } else { "" });
                (t, if f == 0 { Tone::Neutral } else { Tone::Accent })
            }
            ViewState::Loading => ("loading\u{2026}".to_string(), Tone::Neutral),
            ViewState::Error(_) => ("error".to_string(), Tone::Danger),
            _ => (String::new(), Tone::Neutral),
        };
        let hints: [(&str, &str); 9] = [
            ("j/k", "file"),
            ("n/p", "hunk"),
            ("Tab", "turns"),
            ("r", "revert file"),
            ("R", "revert all"),
            ("a", "accept"),
            ("c", "copy"),
            ("i", "ask AI"),
            ("Esc", "close"),
        ];
        let mut spec = PanelSpec::new("Change Review").sub(&sub).hints(&hints);
        if !badge.is_empty() {
            spec = spec.badge(&badge, tone);
        }
        let rect = cx.centered(96, 4000, 92);
        let body = cx.panel(rect, &spec);

        // ── columns ──
        let left_w = (body.w / 3).clamp(26 * tk.cw, 46 * tk.cw).min(body.w / 2);
        let gap = tk.sp.md;
        let left = Rect::new(body.x, body.y, left_w, body.h);
        let right = Rect::new(body.x + left_w + gap, body.y, body.w.saturating_sub(left_w + gap), body.h);
        cx.vdivider(body.x + left_w + gap / 2, body.y, body.h);

        self.render_left(&mut cx, left);
        self.render_right(&mut cx, right);

        if let Some(t) = &self.toast {
            let msg = t.msg.clone();
            let tone = t.tone;
            cx.toast(tone, &msg);
        }
    }

    fn render_left(&mut self, cx: &mut Ctx, area: Rect) {
        let tk = cx.tk;
        // Turns
        let title = if self.ui.focus == Focus::Turns { "> Turns" } else { "Turns" };
        cx.section(area.x, area.y, area.w, title);
        let mut labels: Vec<(String, String)> = vec![("All since start".into(), String::new())];
        if let Some(log) = self.logs.get(&self.ui.pane) {
            for t in &log.turns {
                let meta = match (&t.summary, t.kind, t.finished) {
                    (Some(s), _, _) => format!("{} f {}", s.files, s.short()),
                    (None, TurnKind::Agent, false) => "running".to_string(),
                    (None, _, _) => ago(t.at.elapsed().as_secs()),
                };
                labels.push((t.label.clone(), meta));
            }
        }
        let items: Vec<ListItem> = labels.iter().map(|(l, m)| ListItem::new(l).meta(m)).collect();
        let max_rows = ((area.h / 3) / tk.row_h).max(3);
        let rows = items.len().min(max_rows);
        let list_rect = Rect::new(area.x, area.y + tk.row_h, area.w, rows * tk.row_h);
        self.ui.turn_scroll = scroll_into_view(self.ui.sel_pos, self.ui.turn_scroll, rows);
        cx.list(list_rect, &items, Some(self.ui.sel_pos), self.ui.turn_scroll, None);

        // Files
        let fy = list_rect.bottom() + tk.sp.sm;
        let title = if self.ui.focus == Focus::Files { "> Files" } else { "Files" };
        cx.section(area.x, fy, area.w, title);
        let frect = Rect::new(area.x, fy + tk.row_h, area.w, area.bottom().saturating_sub(fy + tk.row_h));
        match &self.ui.view {
            ViewState::Ready(d) if !d.files.is_empty() => {
                let vis = cx.rows_fit(frect.h);
                self.ui.file_scroll = scroll_into_view(self.ui.file, self.ui.file_scroll, vis);
                let cols = cx.cols(frect.w).saturating_sub(18);
                let labels: Vec<(String, String, Tone)> = d
                    .files
                    .iter()
                    .map(|f| {
                        let name = if f.status == FileStatus::Renamed { format!("{} (from {})", f.new_path, f.old_path) } else { f.new_path.clone() };
                        let meta = if f.binary { "bin".to_string() } else { format!("+{} -{}", f.added, f.removed) };
                        (format!("{} {}", f.status.letter(), tail_fit(&name, cols.max(8))), meta, status_tone(f.status))
                    })
                    .collect();
                let items: Vec<ListItem> = labels.iter().map(|(l, m, t)| ListItem::new(l).meta(m).tone(*t)).collect();
                cx.list(frect, &items, Some(self.ui.file), self.ui.file_scroll, None);
            }
            ViewState::Ready(_) => cx.empty_state(frect, "No changes", ""),
            _ => {}
        }
    }

    fn render_right(&mut self, cx: &mut Ctx, area: Rect) {
        let tk = cx.tk;
        match &self.ui.view {
            ViewState::Idle | ViewState::Loading => cx.empty_state(area, "Loading changes\u{2026}", ""),
            ViewState::Message(m, h) => cx.empty_state(area, m, h),
            ViewState::Error(e) => cx.empty_state(area, "Could not compute the diff", e),
            ViewState::Ready(d) => {
                let Some(f) = d.files.get(self.ui.file) else {
                    let hint = if d.truncated { "The diff was cut at 8 MB" } else { "The working tree matches the checkpoint" };
                    cx.empty_state(area, "No changes in this range", hint);
                    return;
                };
                // Header: status badge + path + counts
                let mut x = area.x;
                x += cx.badge_line(x, area.y, &f.status.letter().to_string(), status_tone(f.status)) + tk.sp.sm;
                let counts = if f.binary { "binary".to_string() } else { format!("+{} -{}", f.added, f.removed) };
                let cw_counts = cx.tw(&counts);
                let path = f.display_path();
                let mut extra = String::new();
                if let Some(s) = f.similarity {
                    extra = format!("  {s}% similar");
                }
                if let Some(m) = &f.mode_note {
                    extra.push_str(&format!("  {m}"));
                }
                let avail = area.right().saturating_sub(x + cw_counts + tk.sp.md);
                let drawn = cx.line_fit(x, area.y, avail, &format!("{path}{extra}"), tk.text);
                let _ = drawn;
                cx.text_right(area.right(), cx.text_y(area.y, tk.row_h), &counts, if f.binary { tk.text_muted } else { tk.success });
                cx.divider(area.x, area.y + tk.row_h, area.w);

                let diff_area = Rect::new(area.x, area.y + tk.row_h + 1 + tk.sp.xs, area.w, area.h.saturating_sub(tk.row_h + 1 + tk.sp.xs));
                if f.hunks.is_empty() {
                    let msg = if f.binary {
                        "Binary file \u{2014} no textual diff"
                    } else if f.status == FileStatus::Renamed {
                        "Renamed without content changes"
                    } else if f.mode_note.is_some() {
                        "File mode changed"
                    } else {
                        "No textual changes"
                    };
                    cx.empty_state(diff_area, msg, "");
                    return;
                }
                self.draw_diff(cx, diff_area);
            }
        }
    }

    fn draw_diff(&mut self, cx: &mut Ctx, area: Rect) {
        let ViewState::Ready(d) = &self.ui.view else { return };
        let Some(f) = d.files.get(self.ui.file) else { return };
        let paint = draw_file_diff(cx, area, f, self.ui.scroll, self.ui.limit);
        self.ui.page = paint.page;
        self.ui.scroll = paint.scroll;
    }

    /// Chips (and the toast, when the overlay is closed) over the panes of the active tab.
    pub fn draw_chips(
        &mut self,
        wm: &WindowManager,
        font: &mut FontManager,
        theme: &Theme,
        buffer: &mut [u32],
        width: usize,
        height: usize,
        area: PaneRect,
        shortcut: &str,
    ) {
        self.chip_rects.clear();
        let show_toast = self.toast.is_some() && !self.ui.visible;
        if self.chips.is_empty() && !show_toast {
            return;
        }
        let tk = Tokens::new(theme, font.cell_width, font.cell_height);
        let mut cx = Ctx::new(buffer, width, height, font, &tk);
        let tab = wm.active_tab();
        for (idx, pr, _) in wm.pane_layouts(area) {
            let Some(pane) = tab.pane(idx) else { continue };
            let Some(chip) = self.chips.get(&pane.id) else { continue };
            let s = chip.summary;
            let key = if shortcut.is_empty() { String::new() } else { format!(" {shortcut}") };
            let long = format!("{} file{} changed \u{b7} {} \u{b7} Review{key}", s.files, if s.files == 1 { "" } else { "s" }, s.short());
            let short = format!("{} file{} \u{b7} Review", s.files, if s.files == 1 { "" } else { "s" });
            let pad = tk.sp.md;
            let avail = pr.width.saturating_sub(2 * tk.sp.lg);
            let text = if cx.tw(&long) + 2 * pad <= avail { long } else { short };
            let w = cx.tw(&text) + 2 * pad;
            if w > avail || pr.height < 3 * tk.row_h {
                continue;
            }
            let h = tk.row_h + tk.sp.xs;
            let r = Rect::new(pr.x + pr.width - w - tk.sp.lg, pr.y + pr.height - h - tk.sp.md, w, h);
            cx.shadow(r, tk.radius);
            cx.fill_rrect(r, tk.radius, tk.border_strong);
            cx.fill_rrect(r.inset(1, 1), tk.radius.saturating_sub(1), tk.elevated);
            cx.text(r.x + pad, cx.text_y(r.y, r.h), &text, tk.accent);
            self.chip_rects.push((pane.id, r));
        }
        if show_toast {
            if let Some(t) = &self.toast {
                let (msg, tone) = (t.msg.clone(), t.tone);
                cx.toast(tone, &msg);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review::diff::parse_unified;
    use crate::review::Range;

    #[test]
    fn clean_makes_text_drawable() {
        assert_eq!(clean("a\tb"), "a    b");
        assert_eq!(clean("x\u{1b}[31my"), "x\u{b7}[31my");
        assert!(tail_fit("src/very/long/path/file.rs", 12).ends_with("file.rs"));
        assert_eq!(tail_fit("short", 12), "short");
    }

    fn sample() -> Review {
        let t = "diff --git a/src/lib.rs b/src/lib.rs\nindex 1..2 100644\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,3 +1,4 @@ fn main()\n one\n-two\n+TWO\n+2.5\n three\ndiff --git a/new.txt b/new.txt\nnew file mode 100644\n--- /dev/null\n+++ b/new.txt\n@@ -0,0 +1 @@\n+hi\\tthere\ndiff --git a/img.png b/img.png\nBinary files a/img.png and b/img.png differ\n";
        let mut rv = Review::default();
        rv.ui.visible = true;
        rv.ui.limit = 10;
        rv.ui.range = Some(Range { repo: "/tmp/demo".into(), base: "abc".into(), target: None, label: "changes in turn 1".into() });
        rv.ui.view = ViewState::Ready(Box::new(parse_unified(t, false)));
        rv
    }

    #[test]
    fn overlay_renders_in_every_state_without_panicking() {
        use crate::ui::kit::gallery::qa::each_theme;
        let mut rv = sample();
        each_theme("review-ready", |b, w, h, f, t| rv.render(b, w, h, f, t));
        rv.ui.file = 1;
        each_theme("review-new", |b, w, h, f, t| rv.render(b, w, h, f, t));
        rv.ui.file = 2;
        each_theme("review-binary", |b, w, h, f, t| rv.render(b, w, h, f, t));
        rv.ui.limit = 3; // exercises the "show more" row
        rv.ui.file = 0;
        each_theme("review-capped", |b, w, h, f, t| rv.render(b, w, h, f, t));
        rv.ui.view = ViewState::Loading;
        each_theme("review-loading", |b, w, h, f, t| rv.render(b, w, h, f, t));
        rv.ui.view = ViewState::Message("No checkpoint for this pane".into(), "Run Review: Mark Checkpoint".into());
        each_theme("review-msg", |b, w, h, f, t| rv.render(b, w, h, f, t));
        rv.ui.view = ViewState::Error("git timed out".into());
        each_theme("review-error", |b, w, h, f, t| rv.render(b, w, h, f, t));
        rv.ui.view = ViewState::Ready(Box::new(Default::default()));
        each_theme("review-empty", |b, w, h, f, t| rv.render(b, w, h, f, t));
    }
}

/// What [`draw_file_diff`] settled on.
#[derive(Clone, Copy, Debug)]
pub struct DiffPaint {
    /// Scroll offset after clamping.
    pub scroll: usize,
    /// Rows visible in `area` (the page size).
    pub page: usize,
}

/// Paint one file's diff (gutter with line numbers, add/delete tint, hunk
/// headers, scroll bar) into `area`. Shared by the review overlay and the
/// best-of-N compare view.
pub(crate) fn draw_file_diff(cx: &mut Ctx, area: Rect, f: &FileDiff, scroll: usize, limit: usize) -> DiffPaint {
    let tk = cx.tk;
    let vis = (area.h / tk.row_h).max(1);
    let total = f.row_count();
    let shown = total.min(limit.max(1));
    let hidden = total - shown;
    let content_rows = shown + usize::from(hidden > 0);
    let max_scroll = content_rows.saturating_sub(vis);
    let scroll = scroll.min(max_scroll);

    let nd = digits(max_line_no(f)).max(3);
    let gut_cols = 2 * nd + 3;
    let gut_w = gut_cols * tk.cw;
    let sb = if content_rows > vis { tk.sp.sm } else { 0 };
    let row_w = area.w.saturating_sub(sb);
    let base = tk.surface;
    let add_bg = mix(base, tk.success, 0.16);
    let del_bg = mix(base, tk.danger, 0.16);
    let add_gut = mix(base, tk.success, 0.30);
    let del_gut = mix(base, tk.danger, 0.30);
    let hunk_bg = mix(base, tk.accent, 0.12);

    let take = vis.min(shown.saturating_sub(scroll));
    let rows = f.rows(scroll, take);
    for (n, row) in rows.iter().enumerate() {
        let y = area.y + n * tk.row_h;
        let rr = Rect::new(area.x, y, row_w, tk.row_h);
        match row {
            Row::Hunk(_, h) => {
                cx.fill(rr, hunk_bg);
                cx.line_fit(area.x + tk.sp.sm, y, row_w.saturating_sub(tk.sp.md), &clean(&h.header()), tk.accent);
            }
            Row::Line(l) => {
                let (bg, gutbg, fg_mark, mark) = match l.kind {
                    LineKind::Add => (Some(add_bg), add_gut, tk.success, '+'),
                    LineKind::Del => (Some(del_bg), del_gut, tk.danger, '-'),
                    LineKind::Context => (None, base, tk.text_faint, ' '),
                    LineKind::NoNewline => (None, base, tk.text_faint, '\\'),
                };
                if let Some(bg) = bg {
                    cx.fill(rr, bg);
                }
                cx.fill(Rect::new(area.x, y, gut_w.min(row_w), tk.row_h), gutbg);
                let ty = cx.text_y(y, tk.row_h);
                let num = |v: Option<u32>| v.map_or(" ".repeat(nd), |v| format!("{v:>nd$}"));
                cx.text(area.x + tk.cw / 2, ty, &num(l.old_no), tk.text_faint);
                cx.text(area.x + tk.cw / 2 + (nd + 1) * tk.cw, ty, &num(l.new_no), tk.text_faint);
                cx.text(area.x + (2 * nd + 2) * tk.cw, ty, &mark.to_string(), fg_mark);
                let tx = area.x + gut_w + tk.cw / 2;
                let text = if l.kind == LineKind::NoNewline { clean(&l.text) } else { clean(&l.text) };
                let fg = if l.kind == LineKind::NoNewline { tk.text_faint } else { tk.text };
                if tx < area.x + row_w {
                    cx.text_fit(tx, ty, area.x + row_w - tx, &text, fg);
                }
            }
        }
    }
    if hidden > 0 && scroll + take >= shown {
        let n = scroll + take - scroll;
        if n < vis {
            let y = area.y + n * tk.row_h;
            let msg = format!("\u{2026} {hidden} more rows hidden \u{2014} press m to show more");
            cx.line_fit(area.x + gut_w, y, row_w.saturating_sub(gut_w), &msg, tk.text_muted);
        }
    }
    cx.scrollbar(area, content_rows, vis, scroll);
    DiffPaint { scroll, page: vis }
}
