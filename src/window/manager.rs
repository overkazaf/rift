use winit::event_loop::EventLoopProxy;

use super::pane::Pane;
use super::tab::{PaneRect, Tab};

pub struct WindowManager {
    pub tabs: Vec<Tab>,
    pub active_tab: usize,
    next_pane_id: usize,
    proxy: EventLoopProxy<()>,
}

impl WindowManager {
    pub fn new(proxy: EventLoopProxy<()>, cols: usize, rows: usize) -> Self {
        let pane = Pane::new(0, cols, rows, proxy.clone());
        let tab = Tab::new(pane);
        Self {
            tabs: vec![tab],
            active_tab: 0,
            next_pane_id: 1,
            proxy,
        }
    }

    fn alloc_id(&mut self) -> usize {
        let id = self.next_pane_id;
        self.next_pane_id += 1;
        id
    }

    pub fn active_tab(&self) -> &Tab {
        &self.tabs[self.active_tab]
    }

    pub fn active_tab_mut(&mut self) -> &mut Tab {
        &mut self.tabs[self.active_tab]
    }

    pub fn active_pane(&self) -> &Pane {
        self.active_tab().active_pane()
    }

    pub fn active_pane_mut(&mut self) -> &mut Pane {
        self.active_tab_mut().active_pane_mut()
    }

    pub fn new_tab(&mut self, cols: usize, rows: usize) {
        let id = self.alloc_id();
        let pane = Pane::new(id, cols, rows, self.proxy.clone());
        let tab_num = self.tabs.len() + 1;
        let mut tab = Tab::new(pane);
        tab.title = format!("Tab {tab_num}");
        self.tabs.push(tab);
        self.active_tab = self.tabs.len() - 1;
    }

    pub fn close_current(&mut self) -> bool {
        let should_close_tab = self.active_tab_mut().close_pane();
        if should_close_tab {
            self.tabs.remove(self.active_tab);
            if self.tabs.is_empty() {
                return true;
            }
            if self.active_tab >= self.tabs.len() {
                self.active_tab = self.tabs.len() - 1;
            }
        }
        false
    }

    pub fn next_tab(&mut self) {
        if !self.tabs.is_empty() {
            self.active_tab = (self.active_tab + 1) % self.tabs.len();
        }
    }

    pub fn prev_tab(&mut self) {
        if !self.tabs.is_empty() {
            self.active_tab = self.active_tab.checked_sub(1).unwrap_or(self.tabs.len() - 1);
        }
    }

    pub fn split_h(&mut self, cols: usize, rows: usize) {
        let id = self.alloc_id();
        let pane = Pane::new(id, cols / 2, rows, self.proxy.clone());
        self.active_tab_mut().split_h(pane);
    }

    pub fn split_v(&mut self, cols: usize, rows: usize) {
        let id = self.alloc_id();
        let pane = Pane::new(id, cols, rows / 2, self.proxy.clone());
        self.active_tab_mut().split_v(pane);
    }

    pub fn focus_next_pane(&mut self) {
        self.active_tab_mut().focus_next();
    }

    pub fn focus_prev_pane(&mut self) {
        self.active_tab_mut().focus_prev();
    }

    pub fn process_all_output(&mut self) -> bool {
        let mut changed = false;
        for tab in &mut self.tabs {
            for pane in &mut tab.panes {
                if pane.process_output() {
                    changed = true;
                }
            }
        }
        changed
    }

    pub fn flush_all_responses(&mut self) {
        for tab in &mut self.tabs {
            for pane in &mut tab.panes {
                pane.flush_responses();
            }
        }
    }

    pub fn tab_bar_info(&self) -> Vec<(&str, bool)> {
        self.tabs.iter().enumerate().map(|(i, tab)| {
            (tab.display_title(), i == self.active_tab)
        }).collect()
    }

    pub fn pane_layouts(&self, area: PaneRect) -> Vec<(usize, PaneRect, bool)> {
        self.active_tab().layouts(area)
    }

    pub fn resize_all(&mut self, cell_width: usize, cell_height: usize, width: u32, height: u32, tab_bar_height: usize) {
        let content_h = (height as usize).saturating_sub(tab_bar_height);
        let area = PaneRect { x: 0, y: tab_bar_height, width: width as usize, height: content_h };

        for tab in &mut self.tabs {
            let layouts = tab.layouts(area);
            for (idx, rect, _) in layouts {
                if idx < tab.panes.len() {
                    let cols = (rect.width / cell_width).max(1);
                    let rows = (rect.height / cell_height).max(1);
                    tab.panes[idx].resize(cols, rows);
                }
            }
        }
    }

    pub fn tab_count(&self) -> usize {
        self.tabs.len()
    }

    #[allow(dead_code)]
    pub fn pane_count(&self) -> usize {
        self.active_tab().panes.len()
    }

    pub fn get_proxy(&self) -> EventLoopProxy<()> {
        self.proxy.clone()
    }

    pub fn alloc_pane_id(&mut self) -> usize {
        self.alloc_id()
    }

    pub fn add_ssh_tab(&mut self, pane: Pane, title: &str) {
        let mut tab = Tab::new(pane);
        tab.title = title.to_string();
        self.tabs.push(tab);
        self.active_tab = self.tabs.len() - 1;
    }

    pub fn close_tab_at(&mut self, idx: usize) -> bool {
        if idx >= self.tabs.len() {
            return false;
        }
        if self.tabs.len() <= 1 {
            return true;
        }
        self.tabs.remove(idx);
        if self.active_tab >= self.tabs.len() {
            self.active_tab = self.tabs.len() - 1;
        } else if self.active_tab > idx {
            self.active_tab -= 1;
        }
        false
    }

    pub fn switch_tab(&mut self, idx: usize) {
        if idx < self.tabs.len() {
            self.active_tab = idx;
        }
    }
}
