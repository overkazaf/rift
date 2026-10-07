pub mod autocomplete;
pub mod backend;
pub mod context;
pub mod knowledge;
pub mod observer;
pub mod panel;
pub mod profile;

pub use autocomplete::Autocomplete;
pub use panel::{AiAction, AiPanel, AiPanelKey};

#[derive(Clone)]
pub struct LlmConfig {
    pub provider: String,
    pub model: String,
    pub api_url: String,
    pub api_key: Option<String>,
    pub enabled: bool,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            provider: "ollama".into(),
            model: "llama3.2".into(),
            api_url: "http://localhost:11434".into(),
            api_key: None,
            enabled: false,
        }
    }
}

impl LlmConfig {
    /// Resolve API key: config file > environment variable > None.
    /// Checks DEEPSEEK_API_KEY, OPENAI_API_KEY, ANTHROPIC_API_KEY based on provider/url.
    pub fn resolve_api_key(&mut self) {
        if self.api_key.is_some() { return; }

        let env_keys: &[&str] = if self.api_url.contains("deepseek") {
            &["DEEPSEEK_API_KEY", "OPENAI_API_KEY"]
        } else if self.api_url.contains("anthropic") {
            &["ANTHROPIC_API_KEY"]
        } else {
            &["OPENAI_API_KEY", "DEEPSEEK_API_KEY"]
        };

        for key in env_keys {
            if let Ok(val) = std::env::var(key) {
                if !val.is_empty() {
                    log::info!("LLM: using API key from ${key}");
                    self.api_key = Some(val);
                    self.enabled = true;
                    return;
                }
            }
        }
    }
}

pub struct Message {
    pub role: &'static str,
    pub content: String,
}

pub struct LlmManager {
    pub config: LlmConfig,
    pub profile: profile::UserProfile,
    pub knowledge: knowledge::KnowledgeBase,
}

impl LlmManager {
    pub fn new(config: LlmConfig) -> Self {
        Self {
            config,
            profile: profile::UserProfile::new(),
            knowledge: knowledge::KnowledgeBase::new(),
        }
    }

    pub fn ask(
        &self,
        question: &str,
        ctx: context::TermContext,
    ) -> std::sync::mpsc::Receiver<Result<String, String>> {
        let config = self.config.clone();
        let profile_summary = self.profile.summary();
        let question = question.to_string();

        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let result = backend::complete(&config, &question, &ctx, &profile_summary);
            let _ = tx.send(result);
        });
        rx
    }

    pub fn track_command(&mut self, cmd: &str) {
        self.profile.track(cmd);
    }
}
