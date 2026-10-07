#[allow(dead_code)]
use crate::terminal::grid::Cell;

#[derive(Clone, Copy)]
pub struct Selection {
    pub start: (usize, usize),
    pub end: (usize, usize),
    pub active: bool,
    pub dragging: bool,
}

impl Selection {
    pub fn new() -> Self {
        Self {
            start: (0, 0),
            end: (0, 0),
            active: false,
            dragging: false,
        }
    }

    pub fn start_at(&mut self, row: usize, col: usize) {
        self.start = (row, col);
        self.end = (row, col);
        self.active = true;
        self.dragging = true;
    }

    pub fn extend_to(&mut self, row: usize, col: usize) {
        if self.dragging {
            self.end = (row, col);
        }
    }

    pub fn finish(&mut self) {
        self.dragging = false;
        let (sr, sc) = self.start;
        let (er, ec) = self.end;
        if sr > er || (sr == er && sc > ec) {
            self.start = (er, ec);
            self.end = (sr, sc);
        }
        if self.start == self.end {
            self.active = false;
        }
    }

    pub fn clear(&mut self) {
        self.active = false;
        self.dragging = false;
    }

    pub fn contains(&self, row: usize, col: usize) -> bool {
        if !self.active {
            return false;
        }
        let ((sr, sc), (er, ec)) = self.normalized();
        if row < sr || row > er {
            return false;
        }
        if sr == er {
            return col >= sc && col <= ec;
        }
        if row == sr {
            return col >= sc;
        }
        if row == er {
            return col <= ec;
        }
        true
    }

    pub fn extract_text(&self, grid: &[Vec<Cell>]) -> String {
        if !self.active {
            return String::new();
        }
        let ((sr, sc), (er, ec)) = self.normalized();
        let mut text = String::new();
        for row in sr..=er {
            if row >= grid.len() {
                break;
            }
            let col_start = if row == sr { sc } else { 0 };
            let col_end = if row == er {
                (ec + 1).min(grid[row].len())
            } else {
                grid[row].len()
            };
            for col in col_start..col_end {
                text.push(grid[row][col].c);
            }
            if row < er {
                let trimmed = text.trim_end_matches(' ');
                text.truncate(trimmed.len());
                text.push('\n');
            }
        }
        text.trim_end().to_string()
    }

    fn normalized(&self) -> ((usize, usize), (usize, usize)) {
        let (sr, sc) = self.start;
        let (er, ec) = self.end;
        if sr > er || (sr == er && sc > ec) {
            ((er, ec), (sr, sc))
        } else {
            ((sr, sc), (er, ec))
        }
    }
}

pub fn copy_to_clipboard(text: &str) {
    if text.is_empty() {
        return;
    }
    #[cfg(target_os = "macos")]
    {
        use std::io::Write;
        if let Ok(mut child) = std::process::Command::new("pbcopy")
            .stdin(std::process::Stdio::piped())
            .spawn()
        {
            if let Some(ref mut stdin) = child.stdin {
                let _ = stdin.write_all(text.as_bytes());
            }
            let _ = child.wait();
        }
    }
    #[cfg(target_os = "linux")]
    {
        use std::io::Write;
        if let Ok(mut child) = std::process::Command::new("xclip")
            .args(["-selection", "clipboard"])
            .stdin(std::process::Stdio::piped())
            .spawn()
        {
            if let Some(ref mut stdin) = child.stdin {
                let _ = stdin.write_all(text.as_bytes());
            }
            let _ = child.wait();
        }
    }
    log::info!("Copied {} bytes to clipboard", text.len());
}

pub fn paste_from_clipboard() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        let output = std::process::Command::new("pbpaste").output().ok()?;
        if output.status.success() {
            return Some(String::from_utf8_lossy(&output.stdout).to_string());
        }
    }
    #[cfg(target_os = "linux")]
    {
        let output = std::process::Command::new("xclip")
            .args(["-selection", "clipboard", "-o"])
            .output()
            .ok()?;
        if output.status.success() {
            return Some(String::from_utf8_lossy(&output.stdout).to_string());
        }
    }
    None
}
