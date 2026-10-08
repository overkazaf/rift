use crate::config::{Rgb, Theme};
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
        if !self.visible { return; }

        let cw = font.cell_width;
        let ch = font.cell_height;

        for px in buffer.iter_mut() {
            let r = ((*px >> 16) & 0xff) / 3;
            let g = ((*px >> 8) & 0xff) / 3;
            let b = (*px & 0xff) / 3;
            *px = (r << 16) | (g << 8) | b;
        }

        let pad = 16;
        let panel_w = width.saturating_sub(pad * 2);
        let panel_h = height.saturating_sub(pad * 2);
        let px0 = pad;
        let py0 = pad;

        let bg = lighten(theme.bg, 6);
        fill_rect(buffer, width, px0, py0, panel_w, panel_h, pack(bg.0, bg.1, bg.2));

        let border = dim(theme.cursor, 0.4);
        let bp = pack(border.0, border.1, border.2);
        for x in px0..px0 + panel_w { set_px(buffer, width, py0, x, bp); set_px(buffer, width, py0 + panel_h - 1, x, bp); }
        for y in py0..py0 + panel_h { set_px(buffer, width, y, px0, bp); set_px(buffer, width, y, px0 + panel_w - 1, bp); }

        let mut cy = py0 + 10;
        render_text(buffer, width, font, "Output Compare", px0 + 16, cy, theme.cursor);
        cy += ch + 8;

        if self.panels.len() < 2 {
            render_text(buffer, width, font, "Need 2+ panes to compare (Ctrl+Shift+D to split)", px0 + 16, cy, dim(theme.fg, 0.5));
            let hy = py0 + panel_h - ch - 10;
            render_text(buffer, width, font, "Esc: close", px0 + 16, hy, dim(theme.fg, 0.3));
            return;
        }

        let half = (panel_w - 32) / 2;
        let left_x = px0 + 16;
        let right_x = px0 + 16 + half + 8;

        render_text(buffer, width, font, trunc(&self.panels[0].title, half / cw), left_x, cy, theme.cursor);
        render_text(buffer, width, font, trunc(&self.panels[1].title, half / cw), right_x, cy, theme.cursor);
        cy += ch + 4;

        let sep_px = pack(dim(theme.fg, 0.15).0, dim(theme.fg, 0.15).1, dim(theme.fg, 0.15).2);
        for x in (px0 + 12)..(px0 + panel_w - 12) { set_px(buffer, width, cy, x, sep_px); }
        cy += 6;

        if let Some(ref dr) = self.diff_result {
            let max_vis = (panel_h - (cy - py0) - ch - 20) / (ch + 2);
            let total = dr.lines.len();
            let scroll = self.scroll.min(total.saturating_sub(max_vis));
            let max_chars = half / cw;

            for line in dr.lines.iter().skip(scroll).take(max_vis) {
                if cy + ch >= py0 + panel_h - ch - 10 { break; }
                match line {
                    DiffLine::Same(s) => {
                        let t = trunc(s, max_chars);
                        render_text(buffer, width, font, t, left_x, cy, dim(theme.fg, 0.5));
                        render_text(buffer, width, font, t, right_x, cy, dim(theme.fg, 0.5));
                    }
                    DiffLine::Removed(s) => {
                        let red: Rgb = (240, 80, 80);
                        fill_rect(buffer, width, left_x - 4, cy, half, ch, pack(60, 20, 20));
                        render_text(buffer, width, font, &format!("- {}", trunc(s, max_chars.saturating_sub(2))), left_x, cy, red);
                    }
                    DiffLine::Added(s) => {
                        let green: Rgb = (80, 240, 80);
                        fill_rect(buffer, width, right_x - 4, cy, half, ch, pack(20, 50, 20));
                        render_text(buffer, width, font, &format!("+ {}", trunc(s, max_chars.saturating_sub(2))), right_x, cy, green);
                    }
                }
                cy += ch + 2;
            }

            if total > max_vis {
                let info = format!("{}/{}", scroll + 1, total);
                render_text(buffer, width, font, &info, px0 + panel_w - 16 - info.len() * cw, py0 + 10, dim(theme.fg, 0.3));
            }
        }

        let div_x = px0 + 16 + half + 2;
        let dv_px = pack(dim(theme.cursor, 0.3).0, dim(theme.cursor, 0.3).1, dim(theme.cursor, 0.3).2);
        for y in (py0 + ch + 20)..(py0 + panel_h - ch - 10) { set_px(buffer, width, y, div_x, dv_px); }

        render_text(buffer, width, font, "Up/Down: scroll  Esc: close", px0 + 16, py0 + panel_h - ch - 10, dim(theme.fg, 0.3));
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

use crate::ui::{render_text, set_px, fill_rect, pack, lighten, dim, trunc};
