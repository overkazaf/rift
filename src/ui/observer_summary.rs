//! AI Observer summary overlay, drawn with the UI kit.

use crate::config::Theme;
use crate::renderer::font::FontManager;
use crate::ui::kit::{Ctx, PanelSpec, Tokens, Tone};

/// Render the observer summary modal. `enabled` is the observer on/off state.
pub fn render(
    buffer: &mut [u32],
    w: usize,
    h: usize,
    font: &mut FontManager,
    theme: &Theme,
    summary: &str,
    enabled: bool,
) {
    let tk = Tokens::new(theme, font.cell_width, font.cell_height);
    let mut cx = Ctx::new(buffer, w, h, font, &tk);
    cx.backdrop(tk.backdrop);

    let lines: Vec<&str> = summary.lines().collect();
    let want_h = cx.title_h() + cx.footer_h() + tk.sp.md * 2 + (lines.len() + 1) * tk.row_h;
    let rect = cx.centered_cols(56, want_h);
    let (badge, tone) = if enabled { ("ON", Tone::Success) } else { ("OFF", Tone::Neutral) };
    let body = cx.panel(
        rect,
        &PanelSpec::new("AI Observer").badge(badge, tone).hints(&[("Esc", "close"), ("V", "toggle observer")]),
    );

    let visible = body.h / tk.row_h;
    if lines.is_empty() {
        cx.empty_state(body, "No activity observed yet", "");
        return;
    }
    for (i, line) in lines.iter().take(visible).enumerate() {
        let y = body.y + i * tk.row_h;
        let (text, color) = if let Some(h) = line.strip_prefix("##") {
            (h.trim_start_matches('#').trim(), tk.accent)
        } else if line.starts_with("  ") {
            (line.trim_start(), tk.text_muted)
        } else {
            (*line, tk.text)
        };
        let indent = if line.starts_with("  ") { tk.sp.md } else { 0 };
        cx.line_fit(body.x + indent, y, body.w.saturating_sub(indent), text, color);
    }
    if lines.len() > visible {
        cx.scrollbar(body, lines.len(), visible, 0);
    }
}
