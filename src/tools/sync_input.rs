use std::collections::HashSet;

pub struct SyncInput {
    active_panes: HashSet<usize>,
    pub enabled: bool,
}

impl SyncInput {
    pub fn new() -> Self {
        Self {
            active_panes: HashSet::new(),
            enabled: false,
        }
    }

    pub fn toggle(&mut self) {
        self.enabled = !self.enabled;
    }

    pub fn add_pane(&mut self, id: usize) {
        self.active_panes.insert(id);
    }

    pub fn remove_pane(&mut self, id: usize) {
        self.active_panes.remove(&id);
    }

    pub fn toggle_pane(&mut self, id: usize) {
        if self.active_panes.contains(&id) {
            self.active_panes.remove(&id);
        } else {
            self.active_panes.insert(id);
        }
    }

    pub fn should_broadcast(&self) -> bool {
        self.enabled && self.active_panes.len() > 1
    }

    pub fn active_panes(&self) -> &HashSet<usize> {
        &self.active_panes
    }
}
