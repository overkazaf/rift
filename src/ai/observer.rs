use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// AI Observer — passive terminal usage tracker with privacy-first design.
///
/// Safety guarantees:
/// - All data stored locally only (~/.config/rift/observer/)
/// - Disabled by default, requires explicit opt-in
/// - Sensitive commands (sudo/passwd/ssh-keygen/export) never recorded
/// - Only command names and frequencies stored, never arguments
/// - SecretMasker filters API keys/tokens/passwords from any recorded data
/// - User can view collected data and delete it at any time
/// - LLM analysis only on explicit user request, with sanitized summary
pub struct Observer {
    pub enabled: bool,

    command_freq: HashMap<String, u32>,
    command_sequences: Vec<Vec<String>>,
    current_sequence: Vec<String>,

    hourly_activity: [u32; 24],
    daily_commands: u32,

    error_count: u32,
    common_errors: HashMap<String, u32>,

    project_dirs: HashMap<String, u32>,
    current_dir: String,

    total_output_bytes: u64,

    secret_masker: crate::tools::secret_mask::SecretMasker,

    data_dir: PathBuf,
    last_save: Instant,
    save_interval: Duration,
}

impl Observer {
    pub fn new() -> Self {
        let data_dir = dirs::home_dir()
            .unwrap_or_default()
            .join(".config/rift/observer");
        let mut obs = Self {
            enabled: false,
            command_freq: HashMap::new(),
            command_sequences: Vec::new(),
            current_sequence: Vec::new(),
            hourly_activity: [0; 24],
            daily_commands: 0,
            error_count: 0,
            common_errors: HashMap::new(),
            project_dirs: HashMap::new(),
            current_dir: String::new(),
            total_output_bytes: 0,
            secret_masker: crate::tools::secret_mask::SecretMasker::new(),
            data_dir,
            last_save: Instant::now(),
            save_interval: Duration::from_secs(300),
        };
        obs.secret_masker.enabled = true;
        obs.load();
        obs
    }

    pub fn toggle(&mut self) {
        self.enabled = !self.enabled;
        log::info!("AI Observer: {}", if self.enabled { "ON" } else { "OFF" });
    }

    /// Record a user command. Filters sensitive data automatically.
    pub fn on_command(&mut self, raw_command: &str) {
        if !self.enabled { return; }

        let command = self.secret_masker.mask(raw_command)
            .unwrap_or_else(|| raw_command.to_string());

        let cmd_name = command.split_whitespace().next().unwrap_or("").to_string();
        if cmd_name.is_empty() || Self::is_sensitive_command(&cmd_name) {
            return;
        }

        *self.command_freq.entry(cmd_name.clone()).or_insert(0) += 1;
        self.daily_commands += 1;

        let hour = (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() / 3600 % 24) as usize;
        self.hourly_activity[hour] += 1;

        self.current_sequence.push(cmd_name);
        if self.current_sequence.len() > 10 {
            self.current_sequence.remove(0);
        }

        if self.last_save.elapsed() >= self.save_interval {
            self.save();
        }
    }

    /// Track output volume and error patterns. Never stores raw output.
    pub fn on_output(&mut self, line: &str) {
        if !self.enabled { return; }
        self.total_output_bytes += line.len() as u64;

        if line.contains("error") || line.contains("Error") {
            let error_type = Self::classify_error(line);
            *self.common_errors.entry(error_type).or_insert(0) += 1;
            self.error_count += 1;
        }
    }

    pub fn on_dir_change(&mut self, dir: &str) {
        if !self.enabled { return; }
        self.current_dir = dir.to_string();
        *self.project_dirs.entry(dir.to_string()).or_insert(0) += 1;
    }

    /// Generate a human-readable summary of observed patterns.
    pub fn generate_summary(&self) -> String {
        let mut s = String::from("=== Rift AI Observer Summary ===\n\n");

        s.push_str("## Top Commands\n");
        let mut cmds: Vec<_> = self.command_freq.iter().collect();
        cmds.sort_by(|a, b| b.1.cmp(a.1));
        for (cmd, count) in cmds.iter().take(15) {
            s.push_str(&format!("  {}: {} times\n", cmd, count));
        }

        s.push_str("\n## Activity Pattern\n");
        let peak = self.hourly_activity.iter().enumerate()
            .max_by_key(|(_, &c)| c).map(|(h, _)| h).unwrap_or(0);
        s.push_str(&format!("  Peak hour: {}:00 (UTC)\n", peak));
        s.push_str(&format!("  Commands today: {}\n", self.daily_commands));

        if !self.common_errors.is_empty() {
            s.push_str("\n## Common Errors\n");
            let mut errs: Vec<_> = self.common_errors.iter().collect();
            errs.sort_by(|a, b| b.1.cmp(a.1));
            for (err, count) in errs.iter().take(5) {
                s.push_str(&format!("  {}: {} times\n", err, count));
            }
        }

        if !self.project_dirs.is_empty() {
            s.push_str("\n## Frequent Directories\n");
            let mut dirs: Vec<_> = self.project_dirs.iter().collect();
            dirs.sort_by(|a, b| b.1.cmp(a.1));
            for (dir, count) in dirs.iter().take(5) {
                s.push_str(&format!("  {}: {} visits\n", dir, count));
            }
        }

        s.push_str(&format!("\nOutput processed: {:.1}MB\n",
            self.total_output_bytes as f64 / 1_048_576.0));
        s
    }

    /// Build an LLM prompt from the sanitized summary.
    pub fn request_llm_analysis(&self) -> String {
        let summary = self.generate_summary();
        format!(
            "Based on this terminal usage summary, provide personalized suggestions:\n\
             1. Workflow optimizations (alias suggestions, tool recommendations)\n\
             2. Common error prevention tips\n\
             3. Productivity patterns (good habits to reinforce)\n\
             Keep suggestions specific and actionable.\n\n{summary}"
        )
    }

    /// Delete all collected data.
    pub fn clear_all_data(&mut self) {
        self.command_freq.clear();
        self.command_sequences.clear();
        self.current_sequence.clear();
        self.hourly_activity = [0; 24];
        self.daily_commands = 0;
        self.error_count = 0;
        self.common_errors.clear();
        self.project_dirs.clear();
        self.total_output_bytes = 0;
        let _ = std::fs::remove_dir_all(&self.data_dir);
        log::info!("AI Observer: all data cleared");
    }

    /// Transparency: list exactly what data is stored.
    pub fn data_inventory(&self) -> Vec<String> {
        vec![
            format!("Commands tracked: {} unique", self.command_freq.len()),
            format!("Total commands: {}", self.daily_commands),
            format!("Error types: {}", self.common_errors.len()),
            format!("Directories: {}", self.project_dirs.len()),
            format!("Output processed: {:.1}MB", self.total_output_bytes as f64 / 1_048_576.0),
            format!("Data location: {}", self.data_dir.display()),
            "Note: No raw command content or arguments stored".into(),
            "Note: Sensitive commands (sudo/passwd/ssh-keygen) excluded".into(),
            "Note: All data is local only, never sent externally".into(),
        ]
    }

    // ── Private ──

    fn is_sensitive_command(cmd: &str) -> bool {
        matches!(cmd,
            "sudo" | "passwd" | "ssh-keygen" | "gpg" | "openssl" |
            "mysql" | "psql" | "mongo" | "redis-cli" |
            "aws" | "gcloud" | "az" |
            "export" | "env" | "printenv" |
            "login" | "su" | "doas"
        )
    }

    fn classify_error(line: &str) -> String {
        if line.contains("not found") { "command_not_found".into() }
        else if line.contains("ermission denied") { "permission".into() }
        else if line.contains("No such file") { "file_not_found".into() }
        else if line.contains("syntax error") { "syntax".into() }
        else if line.contains("onnection refused") { "network".into() }
        else if line.contains("ut of memory") { "memory".into() }
        else if line.contains("egmentation fault") { "segfault".into() }
        else { "other".into() }
    }

    fn save(&mut self) {
        self.last_save = Instant::now();
        let _ = std::fs::create_dir_all(&self.data_dir);

        let mut data = String::new();
        data.push_str(&format!("daily_commands={}\n", self.daily_commands));
        data.push_str(&format!("error_count={}\n", self.error_count));
        data.push_str(&format!("output_bytes={}\n", self.total_output_bytes));

        data.push_str("\n[commands]\n");
        for (cmd, count) in &self.command_freq {
            data.push_str(&format!("{}={}\n", cmd, count));
        }

        data.push_str("\n[errors]\n");
        for (err, count) in &self.common_errors {
            data.push_str(&format!("{}={}\n", err, count));
        }

        data.push_str("\n[dirs]\n");
        for (dir, count) in &self.project_dirs {
            data.push_str(&format!("{}={}\n", dir, count));
        }

        data.push_str("\n[hours]\n");
        for (h, &count) in self.hourly_activity.iter().enumerate() {
            if count > 0 {
                data.push_str(&format!("{}={}\n", h, count));
            }
        }

        let path = self.data_dir.join("stats.txt");
        let _ = std::fs::write(&path, &data);
    }

    fn load(&mut self) {
        let path = self.data_dir.join("stats.txt");
        let Ok(content) = std::fs::read_to_string(&path) else { return };

        let mut section = "";
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() { continue; }
            if line.starts_with('[') && line.ends_with(']') {
                section = &line[1..line.len() - 1];
                continue;
            }
            let Some((key, val)) = line.split_once('=') else { continue };
            let key = key.trim();
            let val = val.trim();

            match section {
                "" => match key {
                    "daily_commands" => self.daily_commands = val.parse().unwrap_or(0),
                    "error_count" => self.error_count = val.parse().unwrap_or(0),
                    "output_bytes" => self.total_output_bytes = val.parse().unwrap_or(0),
                    _ => {}
                },
                "commands" => {
                    if let Ok(c) = val.parse::<u32>() {
                        self.command_freq.insert(key.to_string(), c);
                    }
                }
                "errors" => {
                    if let Ok(c) = val.parse::<u32>() {
                        self.common_errors.insert(key.to_string(), c);
                    }
                }
                "dirs" => {
                    if let Ok(c) = val.parse::<u32>() {
                        self.project_dirs.insert(key.to_string(), c);
                    }
                }
                "hours" => {
                    if let Ok(h) = key.parse::<usize>() {
                        if h < 24 {
                            self.hourly_activity[h] = val.parse().unwrap_or(0);
                        }
                    }
                }
                _ => {}
            }
        }
    }
}
