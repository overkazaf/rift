// Smart History Search — an fzf/atuin-style full-width bottom panel for
// browsing and fuzzy-filtering shell command history (Ctrl+R).
//
// Shell history files (~/.zsh_history, ~/.bash_history) don't record a
// per-command working directory or exit status, so `directory` and
// `exit_code` are populated as empty/`None` from file parsing — the fields
// exist so a richer source (e.g. in-session tracking) can enrich entries
// later without changing the data model.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::{Rgb, Theme};
use crate::renderer::font::FontManager;

/// Cap on how many raw (post-aggregation-input) history lines we keep —
/// keeps very large history files cheap to re-filter on every keystroke.
const MAX_PARSED_LINES: usize = 20_000;

pub struct HistorySearch {
    pub visible: bool,
    pub query: String,
    pub entries: Vec<HistoryEntry>,
    /// Indices into `entries`, ordered best-match-first by `recompute_filter`.
    pub filtered: Vec<usize>,
    pub selected: usize,
}

#[derive(Clone)]
pub struct HistoryEntry {
    pub command: String,
    /// Unix seconds when available (extended zsh history / bash `HISTTIMEFORMAT`).
    /// Falls back to a monotonically increasing sequence number (file order)
    /// when the shell doesn't record real timestamps — still sortable by
    /// recency, just not displayable as a real "time ago".
    pub timestamp: u64,
    pub frequency: u32,
    pub directory: String,
    pub exit_code: Option<i32>,
}

pub enum HistoryKey {
    Char(char),
    Backspace,
    Enter,
    Escape,
    Up,
    Down,
    Tab,
}

pub enum HistoryAction {
    /// Enter — run the selected command immediately.
    Execute(String),
    /// Tab — insert the selected command into the prompt without running it.
    Insert(String),
}

enum ShellKind {
    Zsh,
    Bash,
}

impl HistorySearch {
    pub fn new() -> Self {
        Self {
            visible: false,
            query: String::new(),
            entries: Vec::new(),
            filtered: Vec::new(),
            selected: 0,
        }
    }

    /// Open/close the panel. Reloads history from disk each time it opens,
    /// so newly-run commands show up without restarting rift.
    pub fn toggle(&mut self) {
        self.visible = !self.visible;
        if self.visible {
            self.query.clear();
            self.selected = 0;
            self.load_history();
            self.recompute_filter();
        }
    }

    pub fn handle_key(&mut self, key: HistoryKey) -> Option<HistoryAction> {
        match key {
            HistoryKey::Char(c) => {
                self.query.push(c);
                self.recompute_filter();
                None
            }
            HistoryKey::Backspace => {
                self.query.pop();
                self.recompute_filter();
                None
            }
            HistoryKey::Up => {
                self.selected = self.selected.saturating_sub(1);
                None
            }
            HistoryKey::Down => {
                if self.selected + 1 < self.filtered.len() {
                    self.selected += 1;
                }
                None
            }
            HistoryKey::Enter => {
                let cmd = self.selected_command()?;
                self.visible = false;
                Some(HistoryAction::Execute(cmd))
            }
            HistoryKey::Tab => {
                let cmd = self.selected_command()?;
                self.visible = false;
                Some(HistoryAction::Insert(cmd))
            }
            HistoryKey::Escape => {
                self.visible = false;
                None
            }
        }
    }

    fn selected_command(&self) -> Option<String> {
        let idx = *self.filtered.get(self.selected)?;
        self.entries.get(idx).map(|e| e.command.clone())
    }

    /// Read `~/.zsh_history` or `~/.bash_history` (whichever matches `$SHELL`,
    /// falling back to the other if that file doesn't exist), aggregating
    /// repeated commands into a single entry with a running `frequency`.
    pub fn load_history(&mut self) {
        self.entries.clear();

        let Some((path, kind)) = detect_shell_history_path() else {
            log::warn!("Smart History: no shell history file found (~/.zsh_history or ~/.bash_history)");
            return;
        };
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                log::warn!("Smart History: failed to read {}: {e}", path.display());
                return;
            }
        };

        let mut parsed = match kind {
            ShellKind::Zsh => parse_zsh_history(&content),
            ShellKind::Bash => parse_bash_history(&content),
        };

        // Keep only the most recent N logical lines (oldest-first order, so
        // drop from the front) to bound memory/CPU on huge history files.
        if parsed.len() > MAX_PARSED_LINES {
            let drop_n = parsed.len() - MAX_PARSED_LINES;
            parsed.drain(0..drop_n);
        }

        let mut index_of: HashMap<String, usize> = HashMap::new();
        let mut seq: u64 = 0;
        for (cmd, ts) in parsed {
            let cmd = cmd.trim();
            if cmd.is_empty() {
                continue;
            }
            seq += 1;
            let effective_ts = ts.unwrap_or(seq);

            if let Some(&idx) = index_of.get(cmd) {
                let entry = &mut self.entries[idx];
                entry.frequency += 1;
                if effective_ts >= entry.timestamp {
                    entry.timestamp = effective_ts;
                }
            } else {
                index_of.insert(cmd.to_string(), self.entries.len());
                self.entries.push(HistoryEntry {
                    command: cmd.to_string(),
                    timestamp: effective_ts,
                    frequency: 1,
                    directory: String::new(),
                    exit_code: None,
                });
            }
        }

        log::info!(
            "Smart History: loaded {} unique commands from {}",
            self.entries.len(),
            path.display()
        );
    }

    /// Re-rank `filtered` from `entries` against the current query.
    /// Empty query → most-recent-first (classic history browsing).
    /// Non-empty query → case-insensitive substring match, scored by
    /// prefix bonus + word-boundary bonus + frequency weighting.
    fn recompute_filter(&mut self) {
        let query_lower = self.query.to_lowercase();

        let mut scored: Vec<(usize, i64)> = if query_lower.is_empty() {
            self.entries
                .iter()
                .enumerate()
                .map(|(i, e)| (i, e.timestamp as i64))
                .collect()
        } else {
            self.entries
                .iter()
                .enumerate()
                .filter_map(|(i, e)| score_entry(e, &query_lower).map(|s| (i, s)))
                .collect()
        };

        scored.sort_by(|a, b| b.1.cmp(&a.1));
        self.filtered = scored.into_iter().map(|(i, _)| i).collect();
        self.selected = 0;
    }

    pub fn render(
        &self,
        buffer: &mut [u32],
        width: usize,
        height: usize,
        font: &mut FontManager,
        theme: &Theme,
    ) {
        use crate::ui::kit::{Ctx, PanelSpec, Rect, Tokens, Tone};
        if !self.visible {
            return;
        }
        let tk = Tokens::new(theme, font.cell_width, font.cell_height);
        let mut cx = Ctx::new(buffer, width, height, font, &tk);

        // Bottom sheet: title bar + search field + result rows + key hints.
        let chrome_h = cx.title_h() + cx.footer_h() + tk.input_h + tk.sp.sm + 2 * tk.sp.md;
        let max_total_h = (height * 3 / 5).max(tk.row_h * 10).min(height.saturating_sub(tk.row_h));
        let rows_budget = max_total_h.saturating_sub(chrome_h);
        let visible_rows = 10usize.min((rows_budget / tk.row_h).max(1)).max(1);
        let sheet_h = (chrome_h + visible_rows * tk.row_h).min(height);
        let sheet = cx.bottom_sheet(sheet_h + tk.sp.sm);
        let rect = Rect::new(sheet.x + tk.sp.sm, sheet.y, sheet.w.saturating_sub(2 * tk.sp.sm), sheet_h);

        let count = format!("{}/{} results", self.filtered.len(), self.entries.len());
        let spec = PanelSpec::new("History")
            .sub("smart search")
            .badge(&count, Tone::Neutral)
            .hints(&[("Enter", "run"), ("Tab", "insert"), ("Up/Down", "navigate"), ("Esc", "close")]);
        let body = cx.panel(rect, &spec);

        let input = Rect::new(body.x, body.y, body.w, tk.input_h);
        cx.text_input(input, &self.query, self.query.chars().count(), None, "Type to filter history...", true);
        let rows_area = Rect::new(body.x, body.y + tk.input_h + tk.sp.sm, body.w, body.h.saturating_sub(tk.input_h + tk.sp.sm));

        if self.entries.is_empty() {
            cx.empty_state(rows_area, "No shell history found", "Looked in ~/.zsh_history and ~/.bash_history");
            return;
        }
        if self.filtered.is_empty() {
            cx.empty_state(rows_area, "No matches", "");
            return;
        }

        let query_lower = self.query.to_lowercase();
        let vis = cx.rows_fit(rows_area.h);
        let scroll = crate::ui::kit::scroll_into_view(self.selected, 0, vis);
        let sb = if self.filtered.len() > vis { tk.sp.sm } else { 0 };
        let row_w = rows_area.w.saturating_sub(sb);

        for (row_i, &entry_idx) in self.filtered.iter().skip(scroll).take(vis).enumerate() {
            let Some(entry) = self.entries.get(entry_idx) else { continue };
            let is_sel = scroll + row_i == self.selected;
            let row = Rect::new(rows_area.x, rows_area.y + row_i * tk.row_h, row_w, tk.row_h);
            cx.row_bg(row, is_sel, false);

            // Right side: exit marker + time ago + frequency.
            let info = format!("{}  x{}", format_time_ago(entry.timestamp), entry.frequency);
            let mut right = row.right().saturating_sub(tk.sp.md);
            let iw = cx.tw(&info);
            cx.line(right.saturating_sub(iw), row.y, &info, tk.text_muted);
            right = right.saturating_sub(iw + tk.sp.md);
            if let Some(code) = entry.exit_code {
                let (mark, tone) = if code == 0 { ("ok", Tone::Success) } else { ("err", Tone::Danger) };
                let bw = cx.badge_w(mark);
                cx.badge_line(right.saturating_sub(bw), row.y, mark, tone);
                right = right.saturating_sub(bw + tk.sp.md);
            }

            let cmd_x = row.x + tk.sp.md + 2 * tk.scale;
            let mut cmd_w = right.saturating_sub(cmd_x);
            if !entry.directory.is_empty() {
                let dir = format!("({})", crate::ui::trunc(&entry.directory, 20));
                let dw = cx.tw(&dir);
                if cmd_w > dw + 20 * tk.cw {
                    cx.line(right.saturating_sub(dw), row.y, &dir, tk.text_faint);
                    cmd_w -= dw + tk.sp.md;
                }
            }
            let cmd_display = crate::ui::kit::ellipsize(&entry.command.replace('\n', " "), cx.cols(cmd_w));
            let base = if is_sel { tk.text } else { tk.text_muted };
            render_matched_line(&mut cx, &cmd_display, cmd_x, row.y, &query_lower, base, tk.accent);
        }
        cx.scrollbar(rows_area, self.filtered.len(), vis, scroll);
    }
}

/// Case-insensitive substring match scored by:
///  - prefix bonus (query starts the command)
///  - word-boundary bonus (query starts an argument)
///  - a small penalty for matches buried deeper in the line
///  - a small penalty for a lot of "extra" text around the match
///  - frequency weighting — commands run often rank higher ("smart" ranking)
fn score_entry(entry: &HistoryEntry, query_lower: &str) -> Option<i64> {
    if query_lower.is_empty() {
        return Some(entry.timestamp as i64);
    }
    let cmd_lower = entry.command.to_lowercase();
    let pos = cmd_lower.find(query_lower)?;

    let mut score: i64 = 1_000;
    if pos == 0 {
        score += 500;
    } else if cmd_lower.as_bytes().get(pos - 1) == Some(&b' ') {
        score += 200;
    } else {
        score -= pos as i64;
    }

    let extra_len = cmd_lower.chars().count() as i64 - query_lower.chars().count() as i64;
    score -= extra_len / 2;

    score += entry.frequency as i64 * 50;

    Some(score)
}

/// Render `text` with the first case-insensitive occurrence of `query_lower`
/// highlighted in `match_color`; everything else in `base_color`. `y` is the
/// top of a kit line box.
fn render_matched_line(
    cx: &mut crate::ui::kit::Ctx,
    text: &str,
    x: usize,
    y: usize,
    query_lower: &str,
    base_color: Rgb,
    match_color: Rgb,
) {
    if query_lower.is_empty() {
        cx.line(x, y, text, base_color);
        return;
    }

    let chars: Vec<char> = text.chars().collect();
    let lower_chars: Vec<char> = text.to_lowercase().chars().collect();
    let qchars: Vec<char> = query_lower.chars().collect();
    let qlen = qchars.len();

    let match_start = if qlen > 0 && lower_chars.len() == chars.len() && lower_chars.len() >= qlen {
        (0..=lower_chars.len() - qlen).find(|&i| lower_chars[i..i + qlen] == qchars[..])
    } else {
        None
    };

    let cw = cx.tk.cw;
    match match_start {
        Some(start) => {
            let end = start + qlen;
            let before: String = chars[..start].iter().collect();
            let matched: String = chars[start..end].iter().collect();
            let after: String = chars[end..].iter().collect();

            let mut px = x;
            cx.line(px, y, &before, base_color);
            px += before.chars().count() * cw;
            cx.line(px, y, &matched, match_color);
            px += matched.chars().count() * cw;
            cx.line(px, y, &after, base_color);
        }
        None => cx.line(x, y, text, base_color),
    }
}

/// Human "time ago" string. `timestamp` may be a real unix-seconds value or
/// a small file-order sequence number (see `HistoryEntry::timestamp`) — only
/// values that plausibly look like a real timestamp (post year-2001) are
/// rendered as a duration; otherwise we show a neutral dash.
fn format_time_ago(timestamp: u64) -> String {
    const MIN_PLAUSIBLE_UNIX_TS: u64 = 1_000_000_000; // 2001-09-09

    if timestamp < MIN_PLAUSIBLE_UNIX_TS {
        return "-".to_string();
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(timestamp);
    if timestamp > now {
        return "just now".to_string();
    }
    let diff = now - timestamp;
    match diff {
        0..=59 => "just now".to_string(),
        60..=3_599 => format!("{}m ago", diff / 60),
        3_600..=86_399 => format!("{}h ago", diff / 3_600),
        86_400..=604_799 => format!("{}d ago", diff / 86_400),
        604_800..=2_591_999 => format!("{}w ago", diff / 604_800),
        _ => format!("{}mo ago", diff / 2_592_000),
    }
}

/// Find the user's shell history file, preferring the shell named by
/// `$HISTFILE`/`$SHELL`, falling back to whichever of the two exists.
fn detect_shell_history_path() -> Option<(PathBuf, ShellKind)> {
    if let Ok(histfile) = std::env::var("HISTFILE") {
        let p = PathBuf::from(&histfile);
        if p.exists() {
            let kind = if histfile.contains("bash") {
                ShellKind::Bash
            } else {
                ShellKind::Zsh
            };
            return Some((p, kind));
        }
    }

    let home = dirs::home_dir()?;
    let preferred = shell_kind_from_env();
    let zsh_path = home.join(".zsh_history");
    let bash_path = home.join(".bash_history");

    match preferred {
        ShellKind::Zsh => {
            if zsh_path.exists() {
                return Some((zsh_path, ShellKind::Zsh));
            }
            if bash_path.exists() {
                return Some((bash_path, ShellKind::Bash));
            }
        }
        ShellKind::Bash => {
            if bash_path.exists() {
                return Some((bash_path, ShellKind::Bash));
            }
            if zsh_path.exists() {
                return Some((zsh_path, ShellKind::Zsh));
            }
        }
    }
    None
}

fn shell_kind_from_env() -> ShellKind {
    if let Ok(shell) = std::env::var("SHELL") {
        if shell.contains("bash") {
            return ShellKind::Bash;
        }
    }
    ShellKind::Zsh // macOS default since Catalina; also the more feature-rich format to prefer
}

/// Parse a zsh history file. Supports both plain (`command`) and extended
/// (`: <epoch>:<duration>;command`) formats, and joins `\`-continued
/// multi-line commands into a single logical entry.
fn parse_zsh_history(content: &str) -> Vec<(String, Option<u64>)> {
    let mut result = Vec::new();
    let mut pending: Option<String> = None;

    for raw_line in content.lines() {
        let line = match pending.take() {
            Some(prev) => format!("{prev}\n{raw_line}"),
            None => raw_line.to_string(),
        };

        if line.ends_with('\\') && !line.ends_with("\\\\") {
            let mut l = line;
            l.pop();
            pending = Some(l);
            continue;
        }

        if line.is_empty() {
            continue;
        }

        if let Some(rest) = line.strip_prefix(": ") {
            if let Some(semi) = rest.find(';') {
                let meta = &rest[..semi];
                let cmd = &rest[semi + 1..];
                if let Some(colon) = meta.find(':') {
                    if let Ok(ts) = meta[..colon].trim().parse::<u64>() {
                        result.push((cmd.to_string(), Some(ts)));
                        continue;
                    }
                }
            }
        }

        result.push((line, None));
    }

    result
}

/// Parse a bash history file. Supports plain lines and, when `HISTTIMEFORMAT`
/// is set, `#<epoch>` comment lines preceding the command they time-stamp.
fn parse_bash_history(content: &str) -> Vec<(String, Option<u64>)> {
    let mut result = Vec::new();
    let mut pending_ts: Option<u64> = None;

    for line in content.lines() {
        if let Some(rest) = line.strip_prefix('#') {
            if !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()) {
                pending_ts = rest.parse::<u64>().ok();
                continue;
            }
        }
        if line.trim().is_empty() {
            continue;
        }
        result.push((line.to_string(), pending_ts.take()));
    }

    result
}
