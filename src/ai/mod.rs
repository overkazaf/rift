pub mod advisor;
pub mod hub;
pub mod inline;
pub mod autocomplete;
pub mod backend;
pub mod chat;
pub mod consent;
pub mod context;
pub mod knowledge;
pub mod observer;
pub mod profile;

pub use advisor::Advisor;
pub use autocomplete::Autocomplete;

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
    /// Fill in the API key from the environment (config file wins).
    ///
    /// This only supplies the credential for an endpoint the user already
    /// opted into (`[llm]` in config.toml, or the cloud-AI consent prompt):
    /// it never sets `enabled`, and does nothing while AI is disabled, so a
    /// key lying around in the environment cannot switch cloud AI on.
    pub fn resolve_api_key(&mut self) {
        if self.api_key.is_some() || !self.enabled { return; }

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
            crate::wake::wake();
        });
        rx
    }

    pub fn track_command(&mut self, cmd: &str) {
        self.profile.track(cmd);
    }
}
