use std::path::PathBuf;

#[derive(Clone)]
pub struct Snippet {
    pub name: String,
    pub command: String,
    pub tags: Vec<String>,
    pub description: String,
}

pub struct SnippetManager {
    snippets: Vec<Snippet>,
    file_path: PathBuf,
}

impl SnippetManager {
    pub fn new() -> Self {
        let file_path = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("rift")
            .join("snippets.json");
        let mut mgr = Self { snippets: Vec::new(), file_path };
        mgr.load();
        mgr
    }

    pub fn add(&mut self, snippet: Snippet) {
        self.snippets.push(snippet);
        self.save();
    }

    pub fn remove(&mut self, index: usize) {
        if index < self.snippets.len() {
            self.snippets.remove(index);
            self.save();
        }
    }

    pub fn search(&self, query: &str) -> Vec<(usize, &Snippet)> {
        let query_lower = query.to_lowercase();
        self.snippets
            .iter()
            .enumerate()
            .filter(|(_, s)| {
                s.name.to_lowercase().contains(&query_lower)
                    || s.command.to_lowercase().contains(&query_lower)
                    || s.description.to_lowercase().contains(&query_lower)
                    || s.tags.iter().any(|t| t.to_lowercase().contains(&query_lower))
            })
            .collect()
    }

    pub fn list(&self) -> &[Snippet] {
        &self.snippets
    }

    fn save(&self) {
        if let Some(parent) = self.file_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let mut json = String::from("[\n");
        for (i, s) in self.snippets.iter().enumerate() {
            if i > 0 {
                json.push_str(",\n");
            }
            json.push_str(&format!(
                "  {{\"name\":\"{}\",\"command\":\"{}\",\"tags\":[{}],\"description\":\"{}\"}}",
                escape_json(&s.name),
                escape_json(&s.command),
                s.tags
                    .iter()
                    .map(|t| format!("\"{}\"", escape_json(t)))
                    .collect::<Vec<_>>()
                    .join(","),
                escape_json(&s.description)
            ));
        }
        json.push_str("\n]");
        let _ = std::fs::write(&self.file_path, json);
    }

    fn load(&mut self) {
        if let Ok(content) = std::fs::read_to_string(&self.file_path) {
            self.snippets = parse_snippets_json(&content);
        }
    }
}

fn escape_json(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

fn parse_snippets_json(json: &str) -> Vec<Snippet> {
    let mut result = Vec::new();
    for obj in json.split('{').skip(1) {
        if let Some(end) = obj.find('}') {
            let obj = &obj[..end];
            let name = extract_field(obj, "name").unwrap_or_default();
            let command = extract_field(obj, "command").unwrap_or_default();
            let description = extract_field(obj, "description").unwrap_or_default();
            let tags = extract_array(obj, "tags");
            if !name.is_empty() {
                result.push(Snippet {
                    name,
                    command,
                    tags,
                    description,
                });
            }
        }
    }
    result
}

fn extract_field(obj: &str, key: &str) -> Option<String> {
    let pattern = format!("\"{}\":\"", key);
    let start = obj.find(&pattern)? + pattern.len();
    let rest = &obj[start..];
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

fn extract_array(obj: &str, key: &str) -> Vec<String> {
    let pattern = format!("\"{}\":[", key);
    let Some(start) = obj.find(&pattern) else {
        return Vec::new();
    };
    let rest = &obj[start + pattern.len()..];
    let Some(end) = rest.find(']') else {
        return Vec::new();
    };
    let inner = &rest[..end];
    inner
        .split('"')
        .enumerate()
        .filter(|(i, _)| i % 2 == 1)
        .map(|(_, s)| s.to_string())
        .collect()
}
