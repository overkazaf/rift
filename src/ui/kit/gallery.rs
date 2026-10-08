//! UI Gallery: every kit widget on one page, for visual QA across themes.
//!
//! Open from the Help menu ("UI Gallery") or start Rift with `RIFT_UI_GALLERY=1`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Once;

use super::{
    scroll_into_view, ButtonKind, ButtonState, Column, Ctx, ListItem, PanelSpec, Rect, TableRow, Tokens, Tone, Width,
};
use crate::config::Theme;
use crate::renderer::font::FontManager;

static VISIBLE: AtomicBool = AtomicBool::new(false);
static ENV_INIT: Once = Once::new();

fn init_from_env() {
    ENV_INIT.call_once(|| {
        if std::env::var("RIFT_UI_GALLERY").map(|v| v != "0" && !v.is_empty()).unwrap_or(false) {
            VISIBLE.store(true, Ordering::Relaxed);
        }
    });
}

pub fn visible() -> bool {
    init_from_env();
    VISIBLE.load(Ordering::Relaxed)
}

pub fn set_visible(v: bool) {
    init_from_env();
    VISIBLE.store(v, Ordering::Relaxed);
}

pub fn toggle() {
    set_visible(!visible());
}

/// Render the gallery page.
pub fn render(buffer: &mut [u32], w: usize, h: usize, font: &mut FontManager, theme: &Theme) {
    if !visible() {
        return;
    }
    let tk = Tokens::new(theme, font.cell_width, font.cell_height);
    let mut cx = Ctx::new(buffer, w, h, font, &tk);
    cx.backdrop(tk.backdrop);
    let rect = cx.centered(94, 1180, 92);
    let spec = PanelSpec::new("UI Gallery")
        .sub("kit widgets / tokens")
        .badge(theme.name, Tone::Accent)
        .hints(&[("Esc", "close"), ("Up/Down", "navigate"), ("Enter", "select")]);
    let body = cx.panel(rect, &spec);
    let gap = tk.sp.xl;
    let colw = body.w.saturating_sub(gap) / 2;
    let left = Rect::new(body.x, body.y, colw, body.h);
    let right = Rect::new(body.x + colw + gap, body.y, colw, body.h);
    cx.vdivider(body.x + colw + gap / 2, body.y, body.h);

    let rh = tk.row_h;
    // ---------------- left column
    let mut y = left.y;
    cx.section(left.x, y, left.w, "Tokens");
    y += rh;
    let swatches: [(&str, _); 10] = [
        ("surface", tk.surface),
        ("alt", tk.surface_alt),
        ("elev", tk.elevated),
        ("border", tk.border_strong),
        ("accent", tk.accent),
        ("soft", tk.accent_soft),
        ("danger", tk.danger),
        ("warn", tk.warning),
        ("ok", tk.success),
        ("sel", tk.selection),
    ];
    let sw = (left.w / swatches.len()).max(tk.cw * 4);
    for (i, (name, c)) in swatches.iter().enumerate() {
        let x = left.x + i * sw;
        if x + sw > left.right() {
            break;
        }
        cx.fill_rrect(Rect::new(x, y, sw - tk.sp.xs, rh), tk.radius_sm, *c);
        cx.stroke_rrect(Rect::new(x, y, sw - tk.sp.xs, rh), tk.radius_sm, 1, tk.border);
        cx.text_fit(x + 2, y + rh + tk.sp.xs, sw - tk.sp.xs, name, tk.text_muted);
    }
    y += rh * 2 + tk.sp.sm;

    if y + 5 * rh <= left.bottom() {
        cx.section(left.x, y, left.w, "Typography");
        y += rh;
        cx.line_fit(left.x, y, left.w, "Primary text  The quick brown fox", tk.text);
        y += rh;
        cx.line_fit(left.x, y, left.w, "Muted text  secondary information", tk.text_muted);
        y += rh;
        cx.line_fit(left.x, y, left.w, "Faint text  placeholders and disabled", tk.text_faint);
        y += rh;
        cx.line_fit(left.x, y, left.w, "Truncation: this is a very long line of text that will certainly be cut with an ellipsis at the edge", tk.text);
        y += rh + tk.sp.sm;
    }

    let bh = tk.button_h;
    if y + rh + bh <= left.bottom() {
    cx.section(left.x, y, left.w, "Buttons");
    y += rh;
    let mut bx = left.x;
    for (label, kind, st) in [
        ("Primary", ButtonKind::Primary, ButtonState::Normal),
        ("Secondary", ButtonKind::Secondary, ButtonState::Normal),
        ("Focused", ButtonKind::Secondary, ButtonState::Focused),
        ("Delete", ButtonKind::Danger, ButtonState::Normal),
        ("Disabled", ButtonKind::Secondary, ButtonState::Disabled),
    ] {
        let bw = cx.button_w(label);
        if bx + bw > left.right() {
            break;
        }
        cx.button(Rect::new(bx, y, bw, bh), label, kind, st);
        bx += bw + tk.sp.md;
    }
    y += bh + tk.sp.md;
    }

    if y + 2 * rh <= left.bottom() {
        cx.section(left.x, y, left.w, "Badges");
        y += rh;
        let mut bx = left.x;
        for (t, tone) in [
            ("neutral", Tone::Neutral),
            ("accent", Tone::Accent),
            ("success", Tone::Success),
            ("warning", Tone::Warning),
            ("danger", Tone::Danger),
        ] {
            let bw = cx.badge_w(t);
            if bx + bw > left.right() {
                break;
            }
            cx.badge_line(bx, y, t, tone);
            bx += bw + tk.sp.sm;
        }
        y += rh + tk.sp.sm;
    }

    if y + rh + 2 * tk.input_h + tk.sp.sm <= left.bottom() {
        cx.section(left.x, y, left.w, "Inputs");
        y += rh;
        cx.text_input(Rect::new(left.x, y, left.w, tk.input_h), "selected text here", 18, Some((0, 8)), "Search...", true);
        y += tk.input_h + tk.sp.sm;
        cx.text_input(Rect::new(left.x, y, left.w, tk.input_h), "", 0, None, "Placeholder when empty", false);
        y += tk.input_h + tk.sp.md;
    }

    if y + 3 * rh <= left.bottom() {
        cx.section(left.x, y, left.w, "Progress");
        y += rh;
        for (f, tone) in [(0.72, Tone::Accent), (0.35, Tone::Warning), (0.95, Tone::Danger)] {
            cx.progress(Rect::new(left.x, y, left.w, rh / 2 + tk.sp.xs), f, tone);
            y += rh / 2 + tk.sp.sm;
        }
        y += tk.sp.sm;
    }

    if y + 2 * rh <= left.bottom() {
        cx.section(left.x, y, left.w, "Toasts");
        y += rh;
    }
    for (tone, msg) in [(Tone::Success, "Saved preferences"), (Tone::Warning, "Slow command detected"), (Tone::Danger, "Connection refused")] {
        if y + rh + tk.sp.md + tk.sp.sm > left.bottom() {
            break;
        }
        let tw = cx.toast_w(msg, left.w);
        cx.toast_at(left.x, y, tw, tone, msg);
        y += rh + tk.sp.md + tk.sp.sm;
    }

    // ---------------- right column
    let mut y = right.y;
    let items = [
        ListItem::new("Selected row with accent bar").meta("Enter"),
        ListItem::new("Regular row").meta("12 items"),
        ListItem::new("Dimmed row").dim(),
        ListItem::new("Success tone").tone(Tone::Success).meta("ok"),
        ListItem::new("Warning tone").tone(Tone::Warning).meta("!"),
        ListItem::new("Danger tone with a very long label that must truncate nicely").tone(Tone::Danger).meta("err"),
    ];
    let lh = rh * items.len();
    cx.section(right.x, y, right.w, "List");
    y += rh;
    let shown = items.len().min(right.bottom().saturating_sub(y) / rh);
    cx.list(Rect::new(right.x, y, right.w, rh * shown), &items, Some(0), scroll_into_view(0, 0, items.len()), Some(1));
    y += lh + tk.sp.md;

    let cols = [
        Column::new("Name", Width::Flex(2)),
        Column::new("State", Width::Cols(8)),
        Column::new("CPU", Width::Cols(6)).right(),
        Column::new("Mem", Width::Cols(7)).right(),
    ];
    let rows = vec![
        TableRow::new(vec!["rift", "running", "3.2%", "212M"]),
        TableRow::new(vec!["postgres", "running", "0.4%", "1.2G"]),
        TableRow::new(vec!["worker-with-a-very-long-service-name", "failed", "0.0%", "0M"]).tone(Tone::Danger),
        TableRow::new(vec!["redis", "idle", "0.1%", "48M"]),
    ];
    let th = rh * (rows.len() + 1);
    if y + rh + th <= right.bottom() {
        cx.section(right.x, y, right.w, "Table");
        y += rh;
        cx.table(Rect::new(right.x, y, right.w, th), &cols, &rows, Some(1), 0);
        y += th + tk.sp.md;
    }

    if y + 2 * rh <= right.bottom() {
        cx.section(right.x, y, right.w, "Key hints");
        y += rh;
        cx.hint_row(right.x, y, right.w, rh, &[("Enter", "open"), ("Tab", "switch"), ("Ctrl+K", "palette"), ("Esc", "close")]);
        y += rh + tk.sp.sm;
    }

    if y + 4 * rh <= right.bottom() {
        cx.section(right.x, y, right.w, "Empty state");
        y += rh;
        let es = Rect::new(right.x, y, right.w, (right.bottom() - y).min(rh * 3));
        cx.well(es);
        cx.empty_state(es, "Nothing here yet", "Press n to create one");
    }
}

#[cfg(test)]
pub(crate) mod qa {
    //! Visual-QA helpers: render to a PPM when `RIFT_QA_OUT` names a directory.
    use crate::config::Theme;
    use crate::renderer::font::FontManager;

    pub fn font() -> Option<FontManager> {
        let path = ["/System/Library/Fonts/Menlo.ttc", "/System/Library/Fonts/Monaco.ttf", "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf"]
            .into_iter()
            .find(|p| std::path::Path::new(p).exists())?;
        let size = std::env::var("RIFT_QA_FONT").ok().and_then(|v| v.parse().ok()).unwrap_or(16.0);
        Some(FontManager::new(path, size))
    }

    pub fn themes() -> Vec<Theme> {
        vec![
            Theme::rift_neon(), Theme::catppuccin_mocha(), Theme::hacker_green(), Theme::dracula(), Theme::nord(),
            Theme::solarized_dark(), Theme::tokyo_night(), Theme::cyberpunk(), Theme::gruvbox(), Theme::monokai(),
        ]
    }

    /// A fake terminal-ish background so overlays are judged over content.
    pub fn background(theme: &Theme, w: usize, h: usize, font: &mut FontManager) -> Vec<u32> {
        let mut buf = vec![crate::ui::pack_rgb(theme.bg); w * h];
        let ch = font.cell_height;
        for row in 0..h / ch {
            let line = format!("user@host ~/project $ cargo build --release && ./target/release/rift --verbose {}", row);
            crate::ui::render_text(&mut buf, w, font, &line, 8, row * ch, theme.fg);
        }
        buf
    }

    /// Render an overlay in every theme over a fake terminal background.
    /// Asserts the overlay actually painted something, and (when `RIFT_QA_OUT`
    /// is set) dumps a PPM per theme for visual inspection.
    pub fn each_theme(name: &str, mut f: impl FnMut(&mut [u32], usize, usize, &mut FontManager, &Theme)) {
        let Some(mut font) = font() else { return };
        let (w, h) = (1400usize, 900usize);
        for th in themes() {
            let bg = background(&th, w, h, &mut font);
            let mut buf = bg.clone();
            f(&mut buf, w, h, &mut font, &th);
            assert!(buf != bg, "{name}: overlay painted nothing in theme {}", th.name);
            dump(name, &th, &buf, w, h);
        }
    }

    pub fn dump(name: &str, theme: &Theme, buf: &[u32], w: usize, h: usize) {
        let Ok(dir) = std::env::var("RIFT_QA_OUT") else { return };
        let mut out = format!("P6\n{} {}\n255\n", w, h).into_bytes();
        for px in buf {
            out.push((px >> 16) as u8);
            out.push((px >> 8) as u8);
            out.push(*px as u8);
        }
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(format!("{}/{}-{}.ppm", dir, name, theme.name), out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gallery_renders_in_every_theme() {
        let Some(mut font) = qa::font() else { return };
        let (w, h) = (1500usize, 1000usize);
        set_visible(true);
        for th in qa::themes() {
            let mut buf = qa::background(&th, w, h, &mut font);
            render(&mut buf, w, h, &mut font, &th);
            qa::dump("gallery", &th, &buf, w, h);
        }
        set_visible(false);
    }
}
