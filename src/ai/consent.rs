//! One-time consent for cloud AI.
//!
//! An API key that merely sits in the environment (`OPENAI_API_KEY`, ...) must
//! never make Rift upload terminal output. AI is usable only when
//!
//! * the user wrote an `[llm]` section in config.toml (an explicit choice of
//!   endpoint), or
//! * the user answered the first-run prompt with "Enable" (`[ai] consent =
//!   "cloud"`) or "Local only" (`consent = "local"`, Ollama on localhost).
//!
//! Every AI entry point (chat/hub, inline `#`, Cmd+K, auto-fix) must check
//! [`allowed`] before building a request. The prompt itself is shown by
//! [`maybe_prompt`] and its answer applied by [`apply`].

use crate::ai::inline::llm_ready;
use crate::ai::LlmConfig;
use crate::app::App;
use crate::config::Config;

/// The user's answer to the consent prompt (`[ai] consent`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Consent {
    /// Never asked (or dismissed with Esc): ask again next launch.
    Unset,
    Cloud,
    Local,
    Declined,
}

impl Consent {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "cloud" => Some(Self::Cloud),
            "local" => Some(Self::Local),
            "declined" | "no" | "off" => Some(Self::Declined),
            "" | "unset" => Some(Self::Unset),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unset => "unset",
            Self::Cloud => "cloud",
            Self::Local => "local",
            Self::Declined => "declined",
        }
    }
}

/// A cloud provider whose API key was found in the environment.
#[derive(Clone, Debug)]
pub struct EnvCandidate {
    pub var: &'static str,
    pub provider: &'static str,
    pub api_url: &'static str,
    pub model: &'static str,
    pub key: String,
}

impl EnvCandidate {
    /// Host the terminal output would be sent to (for the prompt text).
    pub fn host(&self) -> &str {
        self.api_url.split("://").nth(1).unwrap_or(self.api_url).split('/').next().unwrap_or(self.api_url)
    }
}

/// First usable cloud key in the environment (OpenAI-compatible providers).
pub fn env_candidate() -> Option<EnvCandidate> {
    const CANDIDATES: &[(&str, &str, &str, &str)] = &[
        ("OPENAI_API_KEY", "openai", "https://api.openai.com", "gpt-4o-mini"),
        ("DEEPSEEK_API_KEY", "deepseek", "https://api.deepseek.com", "deepseek-chat"),
    ];
    for &(var, provider, api_url, model) in CANDIDATES {
        if let Ok(key) = std::env::var(var) {
            if !key.trim().is_empty() {
                return Some(EnvCandidate { var, provider, api_url, model, key });
            }
        }
    }
    None
}

/// Pure policy: may AI features run with this config? (`llm_ready` alone is
/// not enough: it only says a provider is reachable, not that the user opted in.)
pub fn allowed_config(cfg: &Config, llm: &LlmConfig) -> bool {
    llm_ready(llm) && (cfg.llm_explicit || matches!(cfg.ai_consent, Consent::Cloud | Consent::Local))
}

/// Stable gate for every AI entry point: true only when AI is configured or
/// consented to AND a provider is usable.
pub fn allowed(app: &App) -> bool {
    allowed_config(&app.config, &app.llm.config)
}

/// Should the first-run prompt be shown now?
pub fn should_prompt(cfg: &Config) -> Option<EnvCandidate> {
    if cfg.llm_explicit || cfg.ai_consent != Consent::Unset {
        return None;
    }
    env_candidate()
}

/// Queue the consent modal once per run when an env key is present and the
/// user has not decided yet.
pub fn maybe_prompt(app: &mut App) {
    if app.consent_checked {
        return;
    }
    app.consent_checked = true;
    if let Some(c) = should_prompt(&app.config) {
        crate::ui::confirm::show_ai_consent(app, c);
    }
}

/// Apply the user's choice: update the live config, persist via merge-save.
pub fn apply(app: &mut App, choice: Consent, env: Option<&EnvCandidate>) {
    app.config.ai_consent = choice;
    match choice {
        Consent::Cloud => {
            if let Some(e) = env {
                app.config.llm.provider = e.provider.into();
                app.config.llm.api_url = e.api_url.into();
                app.config.llm.model = e.model.into();
                app.config.llm.api_key = Some(e.key.clone());
                app.config.llm.enabled = true;
            }
            app.config.ai_auto_fix = true;
        }
        Consent::Local => {
            app.config.llm = LlmConfig::default();
            app.config.llm.enabled = true;
            app.config.ai_auto_fix = true;
        }
        Consent::Declined | Consent::Unset => {
            app.config.ai_auto_fix = false;
        }
    }
    app.llm.config = app.config.llm.clone();
    app.menubar.set_ai_checks(app.config.ai_auto_fix, app.config.ai_nl_hash);
    crate::config::toml::save_config(&app.config);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn llm(enabled: bool) -> LlmConfig {
        LlmConfig { provider: "openai".into(), model: "m".into(), api_url: "https://api.openai.com".into(), api_key: Some("k".into()), enabled }
    }

    #[test]
    fn nothing_is_allowed_without_config_or_consent() {
        let cfg = Config::default();
        assert!(!allowed_config(&cfg, &llm(true)), "an enabled provider alone is not an opt-in");
        assert!(!allowed_config(&cfg, &llm(false)));
    }

    #[test]
    fn explicit_llm_section_or_consent_allows() {
        let mut cfg = Config::default();
        cfg.llm_explicit = true;
        assert!(allowed_config(&cfg, &llm(true)));
        assert!(!allowed_config(&cfg, &llm(false)));
        let mut cfg = Config::default();
        cfg.ai_consent = Consent::Cloud;
        assert!(allowed_config(&cfg, &llm(true)));
        cfg.ai_consent = Consent::Declined;
        assert!(!allowed_config(&cfg, &llm(true)));
    }

    #[test]
    fn prompt_only_when_undecided_and_env_key_present() {
        let mut cfg = Config::default();
        cfg.ai_consent = Consent::Declined;
        assert!(should_prompt(&cfg).is_none());
        cfg.ai_consent = Consent::Unset;
        cfg.llm_explicit = true;
        assert!(should_prompt(&cfg).is_none());
    }

    #[test]
    fn consent_strings_round_trip() {
        for c in [Consent::Cloud, Consent::Local, Consent::Declined] {
            assert_eq!(Consent::parse(c.as_str()), Some(c));
        }
        assert_eq!(Consent::parse("maybe"), None);
    }

    #[test]
    fn host_is_extracted_from_url() {
        let e = EnvCandidate { var: "X", provider: "p", api_url: "https://api.deepseek.com/v1", model: "m", key: "k".into() };
        assert_eq!(e.host(), "api.deepseek.com");
    }
}
