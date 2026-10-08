use std::collections::HashMap;
use std::path::PathBuf;

pub struct Autocomplete {
    path_commands: Vec<String>,
    command_hints: HashMap<&'static str, &'static str>,
    history: Vec<String>,
    suggestions: Vec<Suggestion>,
    selected: usize,
    pub visible: bool,
    input_buffer: String,
}

pub struct Suggestion {
    pub text: String,
    pub kind: SuggestionKind,
    pub description: String,
}

pub enum SuggestionKind {
    Command,
    File,
    Directory,
    History,
}

impl Autocomplete {
    pub fn new() -> Self {
        let mut ac = Self {
            path_commands: Vec::new(),
            command_hints: HashMap::new(),
            history: Vec::new(),
            suggestions: Vec::new(),
            selected: 0,
            visible: false,
            input_buffer: String::new(),
        };
        ac.scan_path();
        ac.load_builtin_hints();
        ac.load_history();
        ac
    }

    fn scan_path(&mut self) {
        if let Ok(path_var) = std::env::var("PATH") {
            let mut seen = std::collections::HashSet::new();
            for dir in path_var.split(':') {
                if let Ok(entries) = std::fs::read_dir(dir) {
                    for entry in entries.flatten() {
                        if let Ok(name) = entry.file_name().into_string() {
                            if seen.insert(name.clone()) {
                                self.path_commands.push(name);
                            }
                        }
                    }
                }
            }
            self.path_commands.sort();
        }
    }

    fn load_builtin_hints(&mut self) {
        let hints: &[(&str, &str)] = &[
            ("ls", "列出目录内容"),
            ("cd", "切换目录"),
            ("pwd", "显示当前目录"),
            ("cat", "查看文件内容"),
            ("grep", "搜索文本模式"),
            ("find", "查找文件"),
            ("mkdir", "创建目录"),
            ("rm", "删除文件"),
            ("cp", "复制文件"),
            ("mv", "移动/重命名文件"),
            ("chmod", "修改文件权限"),
            ("chown", "修改文件所有者"),
            ("tar", "压缩/解压归档"),
            ("ssh", "SSH 远程连接"),
            ("scp", "SSH 文件传输"),
            ("rsync", "增量文件同步"),
            ("curl", "HTTP 请求工具"),
            ("wget", "下载文件"),
            ("git", "版本控制"),
            ("docker", "容器管理"),
            ("kubectl", "Kubernetes CLI"),
            ("brew", "macOS 包管理"),
            ("npm", "Node.js 包管理"),
            ("cargo", "Rust 包管理/构建"),
            ("python3", "Python 3 解释器"),
            ("node", "Node.js 运行时"),
            ("make", "构建工具"),
            ("vim", "终端文本编辑器"),
            ("nano", "简易文本编辑器"),
            ("top", "进程监控"),
            ("htop", "交互式进程监控"),
            ("ps", "显示进程列表"),
            ("kill", "终止进程"),
            ("ping", "网络连通测试"),
            ("traceroute", "网络路径追踪"),
            ("netstat", "网络连接状态"),
            ("lsof", "列出打开的文件"),
            ("df", "磁盘空间使用"),
            ("du", "目录空间占用"),
            ("head", "查看文件开头"),
            ("tail", "查看文件末尾"),
            ("wc", "统计行/词/字节"),
            ("sort", "排序"),
            ("uniq", "去重"),
            ("awk", "文本处理"),
            ("sed", "流编辑器"),
            ("xargs", "构建命令参数"),
            ("env", "显示/设置环境变量"),
            ("export", "导出环境变量"),
            ("alias", "设置命令别名"),
            ("history", "命令历史"),
            ("man", "查看手册"),
            ("which", "查找命令位置"),
            ("whoami", "当前用户名"),
            ("date", "日期时间"),
            ("echo", "输出文本"),
            ("printf", "格式化输出"),
            ("tee", "同时输出到文件和终端"),
            ("diff", "文件差异对比"),
            ("patch", "应用补丁"),
            ("base64", "Base64 编解码"),
            ("openssl", "加密/证书工具"),
            ("jq", "JSON 处理"),
            ("yq", "YAML 处理"),
            ("ffmpeg", "音视频处理"),
            ("rift", "你正在使用的终端!"),
        ];
        for &(cmd, desc) in hints {
            self.command_hints.insert(cmd, desc);
        }
    }

    fn load_history(&mut self) {
        let history_paths = [
            dirs::home_dir().map(|h| h.join(".zsh_history")),
            dirs::home_dir().map(|h| h.join(".bash_history")),
        ];
        for path in history_paths.iter().flatten() {
            if let Ok(content) = std::fs::read_to_string(path) {
                for line in content.lines().rev().take(500) {
                    let cmd = if line.contains(';') {
                        line.split(';').last().unwrap_or(line)
                    } else {
                        line
                    };
                    let cmd = cmd.trim();
                    if !cmd.is_empty() && cmd.len() > 1 {
                        self.history.push(cmd.to_string());
                    }
                }
                break;
            }
        }
    }

    pub fn update(&mut self, input: &str) {
        self.input_buffer = input.to_string();
        self.suggestions.clear();
        self.selected = 0;

        let input = input.trim();
        if input.is_empty() {
            self.visible = false;
            return;
        }

        let parts: Vec<&str> = input.split_whitespace().collect();
        let is_first_word = parts.len() <= 1 && !input.ends_with(' ');

        if is_first_word {
            let prefix = parts.first().copied().unwrap_or("");
            let prefix_lower = prefix.to_lowercase();

            for cmd in &self.path_commands {
                if cmd.to_lowercase().starts_with(&prefix_lower) && cmd != prefix {
                    let desc = self.command_hints.get(cmd.as_str())
                        .map(|d| d.to_string())
                        .unwrap_or_default();
                    self.suggestions.push(Suggestion {
                        text: cmd.clone(),
                        kind: SuggestionKind::Command,
                        description: desc,
                    });
                }
                if self.suggestions.len() >= 8 { break; }
            }

            let mut seen = std::collections::HashSet::new();
            for cmd in &self.history {
                if cmd.to_lowercase().starts_with(&prefix_lower)
                    && cmd != prefix
                    && seen.insert(cmd.clone())
                {
                    self.suggestions.push(Suggestion {
                        text: cmd.clone(),
                        kind: SuggestionKind::History,
                        description: "history".to_string(),
                    });
                }
                if self.suggestions.len() >= 12 { break; }
            }
        } else {
            let last_word = parts.last().copied().unwrap_or("");
            if let Some(suggestions) = self.complete_path(last_word) {
                self.suggestions = suggestions;
            }
        }

        self.visible = !self.suggestions.is_empty();
    }

    fn complete_path(&self, partial: &str) -> Option<Vec<Suggestion>> {
        let (dir, prefix) = if partial.contains('/') {
            let last_slash = partial.rfind('/').unwrap_or(0);
            let dir = if last_slash == 0 { "/" } else { &partial[..last_slash] };
            let prefix = &partial[last_slash + 1..];
            (PathBuf::from(dir), prefix.to_string())
        } else {
            (PathBuf::from("."), partial.to_string())
        };

        let entries = std::fs::read_dir(&dir).ok()?;
        let mut results = Vec::new();
        let prefix_lower = prefix.to_lowercase();

        for entry in entries.flatten() {
            if let Ok(name) = entry.file_name().into_string() {
                if name.to_lowercase().starts_with(&prefix_lower) {
                    let is_dir = entry.file_type().map_or(false, |t| t.is_dir());
                    let display = if partial.contains('/') {
                        let base = &partial[..partial.rfind('/').unwrap_or(0) + 1];
                        format!("{}{}{}", base, name, if is_dir { "/" } else { "" })
                    } else {
                        format!("{}{}", name, if is_dir { "/" } else { "" })
                    };
                    results.push(Suggestion {
                        text: display,
                        kind: if is_dir { SuggestionKind::Directory } else { SuggestionKind::File },
                        description: if is_dir { "dir".into() } else { "file".into() },
                    });
                }
                if results.len() >= 8 { break; }
            }
        }
        Some(results)
    }

    pub fn select_next(&mut self) {
        if !self.suggestions.is_empty() {
            self.selected = (self.selected + 1) % self.suggestions.len();
        }
    }

    pub fn select_prev(&mut self) {
        if !self.suggestions.is_empty() {
            self.selected = self.selected.checked_sub(1)
                .unwrap_or(self.suggestions.len() - 1);
        }
    }

    pub fn accept(&mut self) -> Option<String> {
        if self.visible && self.selected < self.suggestions.len() {
            let text = self.suggestions[self.selected].text.clone();
            self.visible = false;
            Some(text)
        } else {
            None
        }
    }

    pub fn dismiss(&mut self) {
        self.visible = false;
    }

    pub fn suggestions(&self) -> &[Suggestion] {
        &self.suggestions
    }

    pub fn selected_index(&self) -> usize {
        self.selected
    }

    pub fn render(
        &self,
        buffer: &mut [u32],
        buf_width: usize,
        buf_height: usize,
        font: &mut crate::renderer::font::FontManager,
        theme: &crate::config::Theme,
        cursor_x: usize,
        cursor_y: usize,
    ) {
        use crate::ui::kit::{scroll_into_view, Ctx, ListItem, Rect, Tokens, Tone};
        if !self.visible || self.suggestions.is_empty() { return; }
        let tk = Tokens::new(theme, font.cell_width, font.cell_height);
        let mut cx = Ctx::new(buffer, buf_width, buf_height, font, &tk);

        let max_items = 8.min(self.suggestions.len());
        let popup_h = max_items * tk.row_h + 2 * tk.sp.sm;

        // Width: widest "icon text   description" among the visible window.
        let scroll = scroll_into_view(self.selected, 0, max_items);
        let window = &self.suggestions[scroll..(scroll + max_items).min(self.suggestions.len())];
        let want_cols = window.iter()
            .map(|s| 2 + s.text.chars().count() + if s.description.is_empty() { 0 } else { 3 + s.description.chars().count() })
            .max()
            .unwrap_or(20);
        let popup_w = (want_cols * tk.cw + 4 * tk.sp.md + 2 * tk.sp.sm)
            .clamp(24 * tk.cw, (buf_width / 2).max(24 * tk.cw))
            .min(buf_width);

        let px = cursor_x.min(buf_width.saturating_sub(popup_w));
        let py = if cursor_y + tk.ch + popup_h + tk.sp.sm < buf_height {
            cursor_y + tk.ch + tk.sp.xs
        } else {
            cursor_y.saturating_sub(popup_h + tk.sp.xs)
        };

        let inner = cx.float(Rect::new(px, py, popup_w, popup_h));
        let labels: Vec<String> = window.iter().map(|s| {
            let icon = match s.kind {
                SuggestionKind::Command => '>',
                SuggestionKind::File => '-',
                SuggestionKind::Directory => '/',
                SuggestionKind::History => '*',
            };
            format!("{} {}", icon, s.text)
        }).collect();
        let items: Vec<ListItem> = window.iter().zip(&labels).map(|(s, l)| {
            let it = ListItem::new(l).meta(&s.description);
            match s.kind {
                SuggestionKind::Directory => it.tone(Tone::Accent),
                SuggestionKind::History => it.dim(),
                _ => it,
            }
        }).collect();
        cx.list(inner, &items, Some(self.selected - scroll), 0, None);
    }
}

#[cfg(test)]
mod qa_tests {
    use super::*;
    use crate::ui::kit::gallery::qa::each_theme;

    #[test]
    fn renders_popup() {
        let mut a = Autocomplete::new();
        a.visible = true;
        let mk = |t: &str, k: SuggestionKind, d: &str| Suggestion { text: t.into(), kind: k, description: d.into() };
        a.suggestions = vec![
            mk("git status", SuggestionKind::History, ""),
            mk("grep", SuggestionKind::Command, "search text patterns"),
            mk("src/", SuggestionKind::Directory, ""),
            mk("Cargo.toml", SuggestionKind::File, "1.2K"),
        ];
        a.selected = 1;
        each_theme("autocomplete", |b, w, h, f, t| a.render(b, w, h, f, t, 200, 300));
    }
}
