//! "MCP Activity" overlay (Command Palette): the log of what connected agents
//! asked Rift to do, newest first.

use super::{Activity, Outcome, Shared};

#[derive(Default)]
pub struct Overlay {
    pub visible: bool,
    scroll: usize,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OverlayKey {
    Escape,
    Up,
    Down,
    PageUp,
    PageDown,
}

/// "3s ago", "4m ago", "2h ago".
pub fn ago(secs: u64) -> String {
    match secs {
        0..=1 => "now".into(),
        2..=59 => format!("{secs}s ago"),
        60..=3599 => format!("{}m ago", secs / 60),
        _ => format!("{}h ago", secs / 3600),
    }
}

/// Header text: "MCP · 1 client" / "MCP · 2 clients" / "MCP · no clients".
pub fn indicator(clients: usize) -> String {
    match clients {
        0 => "MCP \u{b7} no clients".into(),
        1 => "MCP \u{b7} 1 client".into(),
        n => format!("MCP \u{b7} {n} clients"),
    }
}

impl Overlay {
    pub fn toggle(&mut self) {
        self.visible = !self.visible;
        self.scroll = 0;
    }

    pub fn handle_key(&mut self, key: OverlayKey, total: usize) {
        match key {
            OverlayKey::Escape => self.visible = false,
            OverlayKey::Up => self.scroll = self.scroll.saturating_sub(1),
            OverlayKey::Down => self.scroll = (self.scroll + 1).min(total.saturating_sub(1)),
            OverlayKey::PageUp => self.scroll = self.scroll.saturating_sub(10),
            OverlayKey::PageDown => self.scroll = (self.scroll + 10).min(total.saturating_sub(1)),
        }
    }

    #[cfg(test)]
    pub fn scroll(&self) -> usize {
        self.scroll
    }

    pub fn render(
        &self,
        buffer: &mut [u32],
        width: usize,
        height: usize,
        font: &mut crate::renderer::font::FontManager,
        theme: &crate::config::Theme,
        shared: Option<&Shared>,
        status: &str,
    ) {
        use crate::ui::kit::{Column, Ctx, PanelSpec, TableRow, Tokens, Tone, Width};
        if !self.visible {
            return;
        }
        let tk = Tokens::new(theme, font.cell_width, font.cell_height);
        let mut cx = Ctx::new(buffer, width, height, font, &tk);
        cx.backdrop(tk.backdrop);

        let entries: Vec<Activity> = shared.map(|s| s.recent(200)).unwrap_or_default();
        let clients = shared.map_or(0, |s| s.clients());
        let badge = indicator(clients);
        let want = cx.title_h() + cx.footer_h() + 2 * tk.sp.md + (entries.len().max(3) + 1) * tk.row_h;
        let rect = cx.centered_cols(96, want.min(height * 3 / 4));
        let spec = PanelSpec::new("MCP Activity")
            .badge(&badge, if clients > 0 { Tone::Accent } else { Tone::Neutral })
            .hints(&[("\u{2191}/\u{2193}", "scroll"), ("Esc", "close")]);
        let body = cx.panel(rect, &spec);
        if entries.is_empty() {
            let hint = format!("Server {status}. Add it to Claude Code with: claude mcp add rift -- rift mcp");
            cx.empty_state(body, "No tool calls yet", &hint);
            return;
        }
        let rows: Vec<TableRow> = entries
            .iter()
            .map(|a| {
                let tone = match a.outcome {
                    Outcome::Ok => Tone::Success,
                    Outcome::Error => Tone::Danger,
                    Outcome::Denied | Outcome::RateLimited => Tone::Warning,
                };
                TableRow::new(vec![
                    ago(a.at.elapsed().as_secs()),
                    format!("#{}", a.client),
                    a.tool.clone(),
                    a.summary.clone(),
                    format!("{} {}ms", a.outcome.label(), a.ms),
                ])
                .cell_tone(4, tone)
            })
            .collect();
        cx.table(
            body,
            &[
                Column::new("When", Width::Cols(8)),
                Column::new("Client", Width::Cols(6)),
                Column::new("Tool", Width::Cols(17)),
                Column::new("Detail", Width::Flex(1)),
                Column::new("Result", Width::Cols(18)),
            ],
            &rows,
            None,
            self.scroll,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_and_indicator_formatting() {
        assert_eq!(ago(0), "now");
        assert_eq!(ago(5), "5s ago");
        assert_eq!(ago(125), "2m ago");
        assert_eq!(ago(7300), "2h ago");
        assert_eq!(indicator(0), "MCP \u{b7} no clients");
        assert_eq!(indicator(1), "MCP \u{b7} 1 client");
        assert_eq!(indicator(3), "MCP \u{b7} 3 clients");
    }

    #[test]
    fn keys_scroll_within_bounds_and_escape_closes() {
        let mut o = Overlay::default();
        o.toggle();
        assert!(o.visible);
        o.handle_key(OverlayKey::Up, 5);
        assert_eq!(o.scroll(), 0);
        for _ in 0..10 {
            o.handle_key(OverlayKey::Down, 5);
        }
        assert_eq!(o.scroll(), 4);
        o.handle_key(OverlayKey::PageUp, 5);
        assert_eq!(o.scroll(), 0);
        o.handle_key(OverlayKey::Escape, 5);
        assert!(!o.visible);
    }
}
