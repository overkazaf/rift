use crate::terminal::grid::Cell;
use std::time::{Duration, Instant};

pub struct TimeWarp {
    snapshots: Vec<Snapshot>,
    max_snapshots: usize,
    write_idx: usize,
    count: usize,
    last_capture: Instant,
    min_interval: Duration,
}

struct Snapshot {
    grid: Vec<Vec<Cell>>,
    cursor_row: usize,
    cursor_col: usize,
    timestamp: Instant,
}

impl TimeWarp {
    pub fn new(max_snapshots: usize) -> Self {
        Self {
            snapshots: Vec::with_capacity(max_snapshots.min(1024)),
            max_snapshots,
            write_idx: 0,
            count: 0,
            last_capture: Instant::now(),
            min_interval: Duration::from_millis(100),
        }
    }

    pub fn capture(&mut self, grid: &[Vec<Cell>], cursor_row: usize, cursor_col: usize) {
        let now = Instant::now();
        if now.duration_since(self.last_capture) < self.min_interval {
            return;
        }
        self.last_capture = now;

        let snapshot = Snapshot {
            grid: grid.to_vec(),
            cursor_row,
            cursor_col,
            timestamp: now,
        };

        if self.snapshots.len() < self.max_snapshots {
            self.snapshots.push(snapshot);
        } else {
            self.snapshots[self.write_idx] = snapshot;
        }
        self.write_idx = (self.write_idx + 1) % self.max_snapshots;
        self.count = (self.count + 1).min(self.max_snapshots);
    }

    pub fn get(&self, steps_back: usize) -> Option<(&[Vec<Cell>], usize, usize, Duration)> {
        if steps_back >= self.count {
            return None;
        }
        let idx = (self.write_idx + self.max_snapshots - 1 - steps_back) % self.max_snapshots;
        let snap = &self.snapshots[idx];
        Some((&snap.grid, snap.cursor_row, snap.cursor_col, snap.timestamp.elapsed()))
    }

    pub fn snapshot_count(&self) -> usize {
        self.count
    }
}

pub struct TimeWarpBrowser {
    pub active: bool,
    pub position: usize,
}

impl TimeWarpBrowser {
    pub fn new() -> Self {
        Self {
            active: false,
            position: 0,
        }
    }

    pub fn enter(&mut self) {
        self.active = true;
        self.position = 0;
    }

    pub fn exit(&mut self) {
        self.active = false;
        self.position = 0;
    }

    pub fn step_back(&mut self, max: usize) {
        if self.position < max.saturating_sub(1) {
            self.position += 1;
        }
    }

    pub fn step_forward(&mut self) {
        self.position = self.position.saturating_sub(1);
    }

    pub fn jump_back(&mut self, n: usize, max: usize) {
        self.position = (self.position + n).min(max.saturating_sub(1));
    }

    pub fn jump_forward(&mut self, n: usize) {
        self.position = self.position.saturating_sub(n);
    }
}
