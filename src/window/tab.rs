use super::pane::Pane;

#[derive(Clone, Copy)]
pub enum Layout {
    Single,
    SplitH(f32),
    SplitV(f32),
}

#[derive(Clone, Copy)]
pub struct PaneRect {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
}

pub struct Tab {
    pub title: String,
    pub panes: Vec<Pane>,
    pub layout: Layout,
    pub active: usize,
}

impl Tab {
    pub fn new(pane: Pane) -> Self {
        Self {
            title: "shell".to_string(),
            panes: vec![pane],
            layout: Layout::Single,
            active: 0,
        }
    }

    pub fn active_pane(&self) -> &Pane {
        &self.panes[self.active]
    }

    pub fn active_pane_mut(&mut self) -> &mut Pane {
        &mut self.panes[self.active]
    }

    pub fn layouts(&self, area: PaneRect) -> Vec<(usize, PaneRect, bool)> {
        let n = self.panes.len();
        match n {
            0 => vec![],
            1 => vec![(0, area, self.active == 0)],
            2 => {
                let border = 1;
                match self.layout {
                    Layout::SplitH(ratio) => {
                        let left_w = (area.width as f32 * ratio) as usize;
                        let right_w = area.width.saturating_sub(left_w + border);
                        vec![
                            (0, PaneRect { x: area.x, y: area.y, width: left_w, height: area.height }, self.active == 0),
                            (1, PaneRect { x: area.x + left_w + border, y: area.y, width: right_w, height: area.height }, self.active == 1),
                        ]
                    }
                    Layout::SplitV(ratio) => {
                        let top_h = (area.height as f32 * ratio) as usize;
                        let bot_h = area.height.saturating_sub(top_h + border);
                        vec![
                            (0, PaneRect { x: area.x, y: area.y, width: area.width, height: top_h }, self.active == 0),
                            (1, PaneRect { x: area.x, y: area.y + top_h + border, width: area.width, height: bot_h }, self.active == 1),
                        ]
                    }
                    _ => {
                        let half = area.width / 2;
                        vec![
                            (0, PaneRect { x: area.x, y: area.y, width: half, height: area.height }, self.active == 0),
                            (1, PaneRect { x: area.x + half + 1, y: area.y, width: area.width.saturating_sub(half + 1), height: area.height }, self.active == 1),
                        ]
                    }
                }
            }
            n => {
                // Auto grid: cols = ceil(sqrt(n)), rows = ceil(n / cols)
                let cols = (n as f32).sqrt().ceil() as usize;
                let rows = (n + cols - 1) / cols;
                let gap = 1;
                let cell_w = (area.width.saturating_sub(gap * (cols - 1))) / cols;
                let cell_h = (area.height.saturating_sub(gap * (rows - 1))) / rows;
                let mut result = Vec::with_capacity(n);
                for i in 0..n {
                    let col = i % cols;
                    let row = i / cols;
                    result.push((i, PaneRect {
                        x: area.x + col * (cell_w + gap),
                        y: area.y + row * (cell_h + gap),
                        width: cell_w,
                        height: cell_h,
                    }, i == self.active));
                }
                result
            }
        }
    }

    pub fn split_h(&mut self, new_pane: Pane) {
        self.panes.push(new_pane);
        self.active = self.panes.len() - 1;
        if self.panes.len() == 2 {
            self.layout = Layout::SplitH(0.5);
        }
        // 3+ panes: layouts() handles auto-grid
    }

    pub fn split_v(&mut self, new_pane: Pane) {
        self.panes.push(new_pane);
        self.active = self.panes.len() - 1;
        if self.panes.len() == 2 {
            self.layout = Layout::SplitV(0.5);
        }
    }

    pub fn close_pane(&mut self) -> bool {
        if self.panes.len() <= 1 {
            return true;
        }
        self.panes.remove(self.active);
        if self.active >= self.panes.len() {
            self.active = self.panes.len() - 1;
        }
        if self.panes.len() == 1 {
            self.layout = Layout::Single;
        }
        false
    }

    pub fn focus_next(&mut self) {
        if !self.panes.is_empty() {
            self.active = (self.active + 1) % self.panes.len();
        }
    }

    pub fn focus_prev(&mut self) {
        if !self.panes.is_empty() {
            self.active = self.active.checked_sub(1).unwrap_or(self.panes.len() - 1);
        }
    }

    pub fn display_title(&self) -> &str {
        let pane = self.active_pane();
        if let Some(t) = pane.title() {
            if !t.is_empty() { return t; }
        }
        &self.title
    }
}
