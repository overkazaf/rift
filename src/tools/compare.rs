use crate::config::Theme;
use crate::renderer::font::FontManager;
use crate::tools::diff::{diff, DiffLine, DiffResult};
use crate::window::pane::Pane;

pub struct CompareView {
    pub visible: bool,
    panels: Vec<PaneOutput>,
    diff_result: Option<DiffResult>,
    scroll: usize,
}

struct PaneOutput {
    title: String,
    lines: Vec<String>,
}

impl CompareView {
    pub fn new() -> Self {
        Self {
            visible: false,
            panels: Vec::new(),
            diff_result: None,
            scroll: 0,
        }
    }

    pub fn collect_and_compare(&mut self, panes: &[&Pane]) {
        self.panels.clear();
        self.diff_result = None;
        self.scroll = 0;

        for pane in panes {
            let title = pane.title().unwrap_or("pane").to_string();
            let lines = extract_lines(&pane.terminal, 40);
            self.panels.push(PaneOutput { title, lines });
        }

        if self.panels.len() >= 2 {
            let text_a = self.panels[0].lines.join("\n");
            let text_b = self.panels[1].lines.join("\n");
            self.diff_result = Some(diff(&text_a, &text_b));
        }

        self.visible = true;
    }

    pub fn handle_key(&mut self, key: CompareKey) {
        match key {
            CompareKey::Escape => self.visible = false,
            CompareKey::Up => self.scroll = self.scroll.saturating_sub(1),
            CompareKey::Down => self.scroll += 1,
        }
    }

    pub fn render(
        &self,
        buffer: &mut [u32],
        width: usize,
        height: usize,
        font: &mut FontManager,
        theme: &Theme,
    ) {
        use crate::ui::kit::{Ctx, PanelSpec, Rect, Tokens, Tone};
        if !self.visible { return; }
        let tk = Tokens::new(theme, font.cell_width, font.cell_height);
        let mut cx = Ctx::new(buffer, width, height, font, &tk);
        cx.backdrop(tk.backdrop);

        let rect = cx.centered(96, usize::MAX / 4, 94);
        let total = self.diff_result.as_ref().map(|d| d.lines.len()).unwrap_or(0);
        let changed = self.diff_result.as_ref().map(|d| d.lines.iter().filter(|l| !matches!(l, DiffLine::Same(_))).count()).unwrap_or(0);
        let badge = format!("{} changed", changed);
        let spec = PanelSpec::new("Output Compare")
            .sub("pane A vs pane B")
            .badge(&badge, if changed > 0 { Tone::Warning } else { Tone::Success })
            .hints(&[("Up/Down", "scroll"), ("Esc", "close")]);
        let body = cx.panel(rect, &spec);

        if self.panels.len() < 2 {
            cx.empty_state(body, "Need 2+ panes to compare", "Split the window with Ctrl+Shift+D");
            return;
        }

        let gap = tk.sp.lg;
        let half = body.w.saturating_sub(gap) / 2;
        let left_x = body.x;
        let right_x = body.x + half + gap;
        cx.line_fit(left_x, body.y, half, &self.panels[0].title, tk.accent);
        cx.line_fit(right_x, body.y, half, &self.panels[1].title, tk.accent);
        cx.divider(body.x, body.y + tk.row_h, body.w);

        let list = Rect::new(body.x, body.y + tk.row_h + tk.sp.xs, body.w, body.h.saturating_sub(tk.row_h + tk.sp.xs));
        cx.vdivider(body.x + half + gap / 2, list.y, list.h);
        if let Some(ref dr) = self.diff_result {
            // Line height is tight (one text line plus 2px) so more of the diff fits.
            let line_h = tk.ch + 2 * tk.scale;
            let max_vis = (list.h / line_h).max(1);
            let scroll = self.scroll.min(total.saturating_sub(max_vis));
            let max_chars = half.saturating_sub(tk.sp.sm) / tk.cw;

            for (n, line) in dr.lines.iter().skip(scroll).take(max_vis).enumerate() {
                let y = list.y + n * line_h;
                let ty = y + tk.scale;
                match line {
                    DiffLine::Same(s) => {
                        let t = crate::ui::kit::ellipsize(s, max_chars);
                        cx.text(left_x + tk.sp.xs, ty, &t, tk.text_muted);
                        cx.text(right_x + tk.sp.xs, ty, &t, tk.text_muted);
                    }
                    DiffLine::Removed(s) => {
                        cx.fill_a(Rect::new(left_x, y, half, line_h), tk.danger, 40);
                        let t = crate::ui::kit::ellipsize(&format!("- {}", s), max_chars);
                        cx.text(left_x + tk.sp.xs, ty, &t, tk.danger);
                    }
                    DiffLine::Added(s) => {
                        cx.fill_a(Rect::new(right_x, y, half, line_h), tk.success, 40);
                        let t = crate::ui::kit::ellipsize(&format!("+ {}", s), max_chars);
                        cx.text(right_x + tk.sp.xs, ty, &t, tk.success);
                    }
                }
            }
            cx.scrollbar(list, total, max_vis, scroll);
        }
    }
}

pub enum CompareKey {
    Up, Down, Escape,
}

fn extract_lines(terminal: &crate::terminal::Terminal, max_lines: usize) -> Vec<String> {
    terminal.grid.iter().rev().take(max_lines).collect::<Vec<_>>().into_iter().rev()
        .map(|row| row.iter().map(|c| c.c).collect::<String>().trim_end().to_string())
        .collect()
}


#[cfg(test)]
mod qa_tests {
    use super::*;
    use crate::ui::kit::gallery::qa::each_theme;

    #[test]
    fn renders_diff_and_needs_two_panes() {
        let a = "one\ntwo\nthree\nsame";
        let b = "one\nTWO\nthree\nsame";
        let mut c = CompareView::new();
        c.visible = true;
        c.panels = vec![
            PaneOutput { title: "zsh - left".into(), lines: vec![] },
            PaneOutput { title: "zsh - right".into(), lines: vec![] },
        ];
        c.diff_result = Some(diff(a, b));
        each_theme("compare", |b, w, h, f, t| c.render(b, w, h, f, t));
        c.panels.truncate(1);
        each_theme("compare-one", |b, w, h, f, t| c.render(b, w, h, f, t));
    }
}
