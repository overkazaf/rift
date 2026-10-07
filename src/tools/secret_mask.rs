pub struct SecretMasker {
    pub enabled: bool,
    patterns: Vec<SecretPattern>,
}

struct SecretPattern {
    #[allow(dead_code)]
    name: &'static str,
    prefix: &'static str,
    min_len: usize,
}

impl SecretMasker {
    pub fn new() -> Self {
        Self {
            enabled: false,
            patterns: vec![
                SecretPattern { name: "openai", prefix: "sk-", min_len: 20 },
                SecretPattern { name: "github_pat", prefix: "ghp_", min_len: 20 },
                SecretPattern { name: "github_token", prefix: "gho_", min_len: 20 },
                SecretPattern { name: "github_app", prefix: "ghu_", min_len: 20 },
                SecretPattern { name: "github_fine", prefix: "github_pat_", min_len: 20 },
                SecretPattern { name: "aws_access", prefix: "AKIA", min_len: 16 },
                SecretPattern { name: "stripe_live", prefix: "sk_live_", min_len: 20 },
                SecretPattern { name: "stripe_test", prefix: "sk_test_", min_len: 20 },
                SecretPattern { name: "slack_bot", prefix: "xoxb-", min_len: 20 },
                SecretPattern { name: "slack_user", prefix: "xoxp-", min_len: 20 },
                SecretPattern { name: "npm_token", prefix: "npm_", min_len: 20 },
                SecretPattern { name: "pypi_token", prefix: "pypi-", min_len: 20 },
                SecretPattern { name: "deepseek", prefix: "sk-", min_len: 20 },
                SecretPattern { name: "anthropic", prefix: "sk-ant-", min_len: 20 },
                SecretPattern { name: "hf_token", prefix: "hf_", min_len: 20 },
            ],
        }
    }

    pub fn mask(&self, input: &str) -> Option<String> {
        if !self.enabled {
            return None;
        }

        let mut result = input.to_string();
        let mut masked = false;

        for pattern in &self.patterns {
            if let Some(pos) = result.find(pattern.prefix) {
                let start = pos;
                let mut end = pos + pattern.prefix.len();
                let chars: Vec<char> = result.chars().collect();
                while end < chars.len()
                    && (chars[end].is_alphanumeric() || chars[end] == '_' || chars[end] == '-')
                {
                    end += 1;
                }
                if end - start >= pattern.min_len {
                    let visible_len = pattern.prefix.len().min(4);
                    let visible_prefix = &result[start..start + visible_len];
                    let mask = format!("{visible_prefix}***");
                    result = format!("{}{}{}", &result[..start], mask, &result[end..]);
                    masked = true;
                }
            }
        }

        if let Some(r) = mask_key_value(&result, "password") {
            result = r;
            masked = true;
        }
        if let Some(r) = mask_key_value(&result, "secret") {
            result = r;
            masked = true;
        }
        if let Some(r) = mask_key_value(&result, "token") {
            result = r;
            masked = true;
        }
        if let Some(r) = mask_key_value(&result, "api_key") {
            result = r;
            masked = true;
        }

        if result.contains('.') {
            let snapshot = result.clone();
            let words: Vec<&str> = snapshot.split_whitespace().collect();
            for word in &words {
                if is_jwt(word) {
                    let jwt_masked = format!(
                        "{}...{}",
                        &word[..10.min(word.len())],
                        &word[word.len().saturating_sub(5)..]
                    );
                    result = result.replace(*word, &jwt_masked);
                    masked = true;
                }
            }
        }

        if masked { Some(result) } else { None }
    }

    pub fn toggle(&mut self) {
        self.enabled = !self.enabled;
        log::info!(
            "Secret masking: {}",
            if self.enabled { "ON" } else { "OFF" }
        );
    }
}

fn mask_key_value(input: &str, key: &str) -> Option<String> {
    let lower = input.to_lowercase();
    let key_lower = key.to_lowercase();
    for sep in &["=", ": ", "= "] {
        let pattern = format!("{key_lower}{sep}");
        if let Some(pos) = lower.find(&pattern) {
            let value_start = pos + key.len() + sep.len();
            let mut value_end = value_start;
            let chars: Vec<char> = input.chars().collect();
            while value_end < chars.len()
                && !chars[value_end].is_whitespace()
                && chars[value_end] != '"'
                && chars[value_end] != '\''
            {
                value_end += 1;
            }
            if value_end > value_start + 3 {
                let show = 3.min(value_end - value_start);
                let masked = format!(
                    "{}{}***{}",
                    &input[..value_start],
                    &input[value_start..value_start + show],
                    &input[value_end..]
                );
                return Some(masked);
            }
        }
    }
    None
}

fn is_jwt(s: &str) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() != 3 {
        return false;
    }
    parts.iter().all(|p| {
        p.len() > 10
            && p.chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == '-' || c == '=')
    })
}
