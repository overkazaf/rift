use std::collections::HashMap;
use std::path::PathBuf;

pub struct UserProfile {
    command_freq: HashMap<String, u32>,
    file_path: PathBuf,
}

impl UserProfile {
    pub fn new() -> Self {
        let file_path = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("rift")
            .join("user_profile.json");
        let mut profile = Self {
            command_freq: HashMap::new(),
            file_path,
        };
        profile.load();
        profile
    }

    pub fn track(&mut self, cmd: &str) {
        let base = cmd.split_whitespace().next().unwrap_or(cmd);
        if base.is_empty() {
            return;
        }
        *self.command_freq.entry(base.to_string()).or_insert(0) += 1;
        if self.command_freq.values().sum::<u32>() % 50 == 0 {
            self.save();
        }
    }

    pub fn summary(&self) -> String {
        let mut top: Vec<_> = self.command_freq.iter().collect();
        top.sort_by(|a, b| b.1.cmp(a.1));
        top.truncate(10);
        if top.is_empty() {
            return String::new();
        }
        top.iter()
            .map(|(k, v)| format!("{}({})", k, v))
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn save(&self) {
        if let Some(parent) = self.file_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let mut json = String::from("{\n");
        let entries: Vec<_> = self.command_freq.iter().collect();
        for (i, (k, v)) in entries.iter().enumerate() {
            if i > 0 {
                json.push_str(",\n");
            }
            json.push_str(&format!("  \"{}\": {}", k, v));
        }
        json.push_str("\n}");
        let _ = std::fs::write(&self.file_path, json);
    }

    fn load(&mut self) {
        let content = match std::fs::read_to_string(&self.file_path) {
            Ok(c) => c,
            Err(_) => return,
        };
        // Simple JSON object parser: {"key": number, ...}
        for line in content.lines() {
            let line = line.trim().trim_end_matches(',');
            if let Some((key_part, val_part)) = line.split_once(':') {
                let key = key_part.trim().trim_matches('"').to_string();
                if let Ok(val) = val_part.trim().parse::<u32>() {
                    if !key.is_empty() && !key.starts_with('{') && !key.starts_with('}') {
                        self.command_freq.insert(key, val);
                    }
                }
            }
        }
    }
}
