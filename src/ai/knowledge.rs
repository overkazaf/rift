use std::path::PathBuf;

pub struct KnowledgeBase {
    cache_dir: PathBuf,
}

impl KnowledgeBase {
    pub fn new() -> Self {
        let dir = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("rift")
            .join("knowledge");
        let _ = std::fs::create_dir_all(&dir);
        Self { cache_dir: dir }
    }

    pub fn cache_response(&self, question: &str, answer: &str) {
        let hash = simple_hash(question);
        let path = self.cache_dir.join(format!("{hash:016x}.json"));
        let json = format!(
            r#"{{"q":"{}","a":"{}","t":{}}}"#,
            escape(question),
            escape(answer),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
        );
        let _ = std::fs::write(path, json);
    }

    pub fn lookup(&self, question: &str) -> Option<String> {
        let hash = simple_hash(question);
        let path = self.cache_dir.join(format!("{hash:016x}.json"));
        let content = std::fs::read_to_string(path).ok()?;
        extract_field(&content, "a")
    }
}

fn simple_hash(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

fn extract_field(json: &str, key: &str) -> Option<String> {
    let pattern = format!("\"{}\":\"", key);
    let start = json.find(&pattern)? + pattern.len();
    let rest = &json[start..];
    let mut end = 0;
    let mut escaped = false;
    for (i, c) in rest.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if c == '\\' {
            escaped = true;
            continue;
        }
        if c == '"' {
            end = i;
            break;
        }
    }
    Some(
        rest[..end]
            .replace("\\n", "\n")
            .replace("\\\"", "\"")
            .replace("\\\\", "\\"),
    )
}
