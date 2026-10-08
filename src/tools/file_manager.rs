use std::path::PathBuf;

pub struct FileManager {
    pub visible: bool,
    entries: Vec<FileEntry>,
    selected: usize,
    cwd: PathBuf,
    scroll: usize,
}

struct FileEntry {
    name: String,
    is_dir: bool,
    size: u64,
}

impl FileManager {
    pub fn new() -> Self {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
        let mut fm = Self {
            visible: false,
            entries: Vec::new(),
            selected: 0,
            cwd,
            scroll: 0,
        };
        fm.refresh();
        fm
    }

    pub fn toggle(&mut self) {
        self.visible = !self.visible;
        if self.visible {
            self.refresh();
        }
    }

    pub fn refresh(&mut self) {
        self.entries.clear();
        if self.cwd.parent().is_some() {
            self.entries.push(FileEntry {
                name: "..".into(),
                is_dir: true,
                size: 0,
            });
        }

        if let Ok(read_dir) = std::fs::read_dir(&self.cwd) {
            let mut dirs = Vec::new();
            let mut files = Vec::new();

            for entry in read_dir.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with('.') {
                    continue;
                }
                let meta = entry.metadata().ok();
                let is_dir = meta.as_ref().map_or(false, |m| m.is_dir());
                let size = meta.as_ref().map_or(0, |m| m.len());

                let fe = FileEntry { name, is_dir, size };
                if is_dir {
                    dirs.push(fe);
                } else {
                    files.push(fe);
                }
            }

            dirs.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
            files.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
            self.entries.extend(dirs);
            self.entries.extend(files);
        }
        self.selected = 0;
        self.scroll = 0;
    }

    pub fn handle_key(&mut self, key: FileManagerKey) -> Option<FileManagerAction> {
        match key {
            FileManagerKey::Escape => {
                self.visible = false;
                None
            }
            FileManagerKey::Up => {
                self.selected = self.selected.saturating_sub(1);
                None
            }
            FileManagerKey::Down => {
                if self.selected + 1 < self.entries.len() {
                    self.selected += 1;
                }
                None
            }
            FileManagerKey::Enter => {
                if let Some(entry) = self.entries.get(self.selected) {
                    if entry.is_dir {
                        if entry.name == ".." {
                            if let Some(parent) = self.cwd.parent() {
                                self.cwd = parent.to_path_buf();
                            }
                        } else {
                            self.cwd = self.cwd.join(&entry.name);
                        }
                        self.refresh();
                        None
                    } else {
                        let path = self.cwd.join(&entry.name).display().to_string();
                        Some(FileManagerAction::OpenFile(path))
                    }
                } else {
                    None
                }
            }
            FileManagerKey::Char(_) => None,
        }
    }

    pub fn render(
        &self,
        buffer: &mut [u32],
        width: usize,
        height: usize,
        font: &mut crate::renderer::font::FontManager,
        theme: &crate::config::Theme,
    ) {
        use crate::ui::kit::{center_scroll, Ctx, ListItem, PanelSpec, Side, Tokens, Tone};
        if !self.visible {
            return;
        }
        let tk = Tokens::new(theme, font.cell_width, font.cell_height);
        let mut cx = Ctx::new(buffer, width, height, font, &tk);

        let panel_w = (width * 3 / 10).max(28 * tk.cw);
        let rect = cx.side_panel(Side::Left, panel_w).inset(tk.sp.sm, tk.sp.sm);
        let dir_name = self
            .cwd
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| self.cwd.display().to_string());
        let count = format!("{} items", self.entries.len());
        let spec = PanelSpec::new("Files")
            .sub(&dir_name)
            .badge(&count, Tone::Neutral)
            .hints(&[("Up/Down", "move"), ("Enter", "open"), ("Esc", "close")]);
        let body = cx.panel(rect, &spec);

        if self.entries.is_empty() {
            cx.empty_state(body, "Empty directory", "");
            return;
        }
        let labels: Vec<String> = self
            .entries
            .iter()
            .map(|e| if e.is_dir && e.name != ".." { format!("{}/", e.name) } else { e.name.clone() })
            .collect();
        let sizes: Vec<String> = self
            .entries
            .iter()
            .map(|e| if !e.is_dir && e.size > 0 { format_size(e.size) } else { String::new() })
            .collect();
        let items: Vec<ListItem> = self
            .entries
            .iter()
            .enumerate()
            .map(|(i, e)| {
                let it = ListItem::new(&labels[i]).meta(&sizes[i]);
                if e.is_dir { it.tone(Tone::Accent) } else { it }
            })
            .collect();
        let vis = cx.rows_fit(body.h);
        let scroll = center_scroll(self.selected, items.len(), vis);
        cx.list(body, &items, Some(self.selected), scroll, None);
    }
}

pub enum FileManagerKey {
    Up,
    Down,
    Enter,
    Escape,
    Char(char),
}

pub enum FileManagerAction {
    OpenFile(String),
}

fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{}B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.0}K", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1}M", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.1}G", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}
