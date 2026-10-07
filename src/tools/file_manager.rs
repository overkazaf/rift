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
        if !self.visible {
            return;
        }

        let cw = font.cell_width;
        let ch = font.cell_height;

        let panel_w = (width * 3 / 10).max(20 * cw);
        let panel_h = height;

        let bg = crate::ui::darken(theme.bg, 10);
        let bg_px = crate::ui::pack_rgb(bg);
        crate::ui::fill_rect(buffer, width, 0, 0, panel_w, panel_h, bg_px);

        let border = crate::ui::dim(theme.cursor, 0.3);
        let border_px = crate::ui::pack_rgb(border);
        for y in 0..panel_h {
            crate::ui::set_px(buffer, width, y, panel_w - 1, border_px);
        }

        let pad = 8;
        let max_chars = (panel_w - pad * 2) / cw;
        let mut ty = 8;

        let dir_name = self
            .cwd
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| self.cwd.display().to_string());
        crate::ui::render_text(
            buffer,
            width,
            font,
            crate::ui::trunc(&dir_name, max_chars),
            pad,
            ty,
            theme.cursor,
        );
        ty += ch + 6;

        let sep_color = crate::ui::dim(theme.fg, 0.15);
        let sep_px = crate::ui::pack_rgb(sep_color);
        for x in pad..panel_w.saturating_sub(pad) {
            crate::ui::set_px(buffer, width, ty, x, sep_px);
        }
        ty += 6;

        for (i, entry) in self.entries.iter().enumerate() {
            if ty + ch >= panel_h.saturating_sub(20) {
                break;
            }
            let is_selected = i == self.selected;

            if is_selected {
                let hl = crate::ui::lighten(bg, 12);
                let hl_px = crate::ui::pack_rgb(hl);
                if ty >= 2 {
                    crate::ui::fill_rect(buffer, width, 2, ty - 2, panel_w - 4, ch + 4, hl_px);
                }
                let accent_px = crate::ui::pack_rgb(theme.cursor);
                for y in ty..ty + ch {
                    crate::ui::set_px(buffer, width, y, 2, accent_px);
                    crate::ui::set_px(buffer, width, y, 3, accent_px);
                }
            }

            let (icon, icon_color) = if entry.is_dir {
                ("/", theme.cursor)
            } else {
                let ext = entry.name.rsplit('.').next().unwrap_or("");
                let color = match ext {
                    "rs" => (243, 139, 168),
                    "py" => (137, 180, 250),
                    "js" | "ts" => (249, 226, 175),
                    "md" => (148, 226, 213),
                    "toml" | "json" | "yaml" => (166, 227, 161),
                    _ => crate::ui::dim(theme.fg, 0.5),
                };
                ("-", color)
            };
            crate::ui::render_text(buffer, width, font, icon, pad, ty, icon_color);

            let name_color = if is_selected {
                theme.fg
            } else {
                crate::ui::dim(theme.fg, 0.7)
            };
            let name_display = crate::ui::trunc(&entry.name, max_chars.saturating_sub(4));
            crate::ui::render_text(buffer, width, font, name_display, pad + 2 * cw, ty, name_color);

            if !entry.is_dir && entry.size > 0 {
                let size_str = format_size(entry.size);
                let sx = panel_w.saturating_sub(size_str.len() * cw + pad);
                crate::ui::render_text(
                    buffer,
                    width,
                    font,
                    &size_str,
                    sx,
                    ty,
                    crate::ui::dim(theme.fg, 0.3),
                );
            }

            ty += ch + 2;
        }

        let count = format!("{} items", self.entries.len());
        let count_y = panel_h.saturating_sub(ch + 8);
        crate::ui::render_text(
            buffer,
            width,
            font,
            &count,
            pad,
            count_y,
            crate::ui::dim(theme.fg, 0.3),
        );
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
