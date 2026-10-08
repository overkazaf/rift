use winit::event_loop::EventLoopProxy;

use super::pane::{Pane, PtyKind};
use super::tab::{MinSize, PaneNode, PaneRect, SplitDir, Tab};

/// A change to the tab list that outside indexes (e.g. the webview's tab)
/// must follow. Drained with [`WindowManager::take_tab_events`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabEvent {
    Closed(usize),
    Moved { from: usize, to: usize },
}

pub struct WindowManager {
    pub tabs: Vec<Tab>,
    pub active_tab: usize,
    next_pane_id: usize,
    proxy: EventLoopProxy<()>,
    /// Content area from the last `resize_all`; used for geometric decisions
    /// (e.g. which pane takes focus after a close).
    last_area: PaneRect,
    tab_events: Vec<TabEvent>,
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
            last_area: PaneRect { x: 0, y: 0, width: 0, height: 0 },
            tab_events: Vec::new(),
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
        let tab = Tab::new(pane);
        self.tabs.push(tab);
        self.active_tab = self.tabs.len() - 1;
        self.renumber_tabs();
    }

    pub fn renumber_tabs(&mut self) {
        for (i, tab) in self.tabs.iter_mut().enumerate() {
            if !tab.custom_title {
                tab.title = format!("Tab {}", i + 1);
            }
        }
    }

    /// Set a custom title; an empty name restores the automatic "Tab N".
    pub fn rename_tab(&mut self, idx: usize, name: &str) {
        let name = name.trim();
        if let Some(tab) = self.tabs.get_mut(idx) {
            if name.is_empty() {
                tab.custom_title = false;
                self.renumber_tabs();
            } else {
                tab.title = name.to_string();
                tab.custom_title = true;
            }
        }
    }

    /// Move tab `from` to position `to` (drag reorder). The active tab keeps
    /// following its content. Returns false for out-of-range or no-op moves.
    pub fn move_tab(&mut self, from: usize, to: usize) -> bool {
        let n = self.tabs.len();
        if from >= n || to >= n || from == to {
            return false;
        }
        let tab = self.tabs.remove(from);
        self.tabs.insert(to, tab);
        self.active_tab = crate::ui::tabbar::remap_after_move(self.active_tab, from, to);
        self.renumber_tabs();
        self.tab_events.push(TabEvent::Moved { from, to });
        true
    }

    /// Tab-list changes since the last call (oldest first).
    pub fn take_tab_events(&mut self) -> Vec<TabEvent> {
        std::mem::take(&mut self.tab_events)
    }

    pub fn close_current(&mut self) -> bool {
        let area = self.last_area;
        let should_close_tab = self.active_tab_mut().close_pane(area);
        if should_close_tab {
            self.tab_events.push(TabEvent::Closed(self.active_tab));
            self.tabs.remove(self.active_tab);
            if self.tabs.is_empty() {
                return true;
            }
            if self.active_tab >= self.tabs.len() {
                self.active_tab = self.tabs.len() - 1;
            }
            self.renumber_tabs();
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

    /// Split the active pane along `dir`; the new shell starts in the active
    /// pane's working directory (OSC 7) when known. Unzooms first. Refuses
    /// (returns false) when either half would drop below the minimum pane size.
    pub fn split_active(&mut self, dir: SplitDir, area: PaneRect, min: MinSize) -> bool {
        let idx = self.active_tab;
        self.tabs[idx].unzoom();
        if !self.tabs[idx].can_split(area, dir, min) {
            log::warn!("Split refused: pane would be smaller than {}x{} cells", super::tab::MIN_COLS, super::tab::MIN_ROWS);
            return false;
        }
        let (cols, rows, cwd) = {
            let p = self.tabs[idx].active_pane();
            let cwd = match p.pty {
                PtyKind::Local(_) => p.terminal.cwd.clone(),
                PtyKind::Ssh(_) => None, // remote path is meaningless locally
            };
            (p.terminal.cols, p.terminal.rows, cwd)
        };
        let id = self.alloc_id();
        let (c, r) = match dir {
            SplitDir::Horizontal => ((cols / 2).max(1), rows),
            SplitDir::Vertical => (cols, (rows / 2).max(1)),
        };
        let pane = Pane::new_in(id, c, r, self.proxy.clone(), cwd.as_deref());
        self.tabs[idx].split(dir, pane);
        true
    }

    /// Replace a freshly created single-pane tab's root with a restored
    /// layout. The existing pane becomes the first leaf; every other leaf is
    /// spawned in its saved working directory.
    pub fn restore_layout(&mut self, tab_idx: usize, layout: &PaneNode<Option<String>>, active: usize) {
        let proxy = self.proxy.clone();
        let mut next = self.next_pane_id;
        let Some(tab) = self.tabs.get_mut(tab_idx) else { return };
        if tab.pane_count() != 1 {
            return;
        }
        let old = std::mem::replace(&mut tab.root, PaneNode::Empty);
        let PaneNode::Leaf(first_pane) = old else {
            tab.root = old;
            return;
        };
        let (cols, rows) = (first_pane.terminal.cols, first_pane.terminal.rows);
        let mut first = Some(first_pane);
        tab.root = layout.map_leaves(&mut |cwd: &Option<String>| match first.take() {
            Some(p) => p,
            None => {
                let id = next;
                next += 1;
                Pane::new_in(id, (cols / 2).max(1), rows, proxy.clone(), cwd.as_deref())
            }
        });
        tab.active = active.min(tab.pane_count().saturating_sub(1));
        tab.zoomed = false;
        self.next_pane_id = next;
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
            for pane in tab.panes_mut() {
                if pane.process_output() {
                    changed = true;
                }
            }
        }
        changed
    }

    pub fn flush_all_responses(&mut self) {
        for tab in &mut self.tabs {
            for pane in tab.panes_mut() {
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

        self.last_area = area;

        for tab in &mut self.tabs {
            // Unzoomed tree geometry for every pane; the zoomed pane alone
            // gets the whole area (the others keep their tree size).
            let mut layouts = tab.tree_layouts(area);
            if tab.is_zoomed() {
                let active = tab.active;
                if let Some(l) = layouts.iter_mut().find(|(i, _)| *i == active) {
                    l.1 = area;
                }
            }
            for (pane, (_, rect)) in tab.panes_mut().into_iter().zip(layouts) {
                let cols = (rect.width / cell_width.max(1)).max(1);
                let rows = (rect.height / cell_height.max(1)).max(1);
                pane.resize(cols, rows);
            }
        }
    }

    pub fn tab_count(&self) -> usize {
        self.tabs.len()
    }

    #[allow(dead_code)]
    pub fn pane_count(&self) -> usize {
        self.active_tab().pane_count()
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
        self.tab_events.push(TabEvent::Closed(idx));
        self.tabs.remove(idx);
        if self.active_tab >= self.tabs.len() {
            self.active_tab = self.tabs.len() - 1;
        } else if self.active_tab > idx {
            self.active_tab -= 1;
        }
        self.renumber_tabs();
        false
    }

    pub fn switch_tab(&mut self, idx: usize) {
        if idx < self.tabs.len() {
            self.active_tab = idx;
        }
    }
}
