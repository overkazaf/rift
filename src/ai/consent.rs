//! One-time consent for cloud AI.
//!
//! An API key that merely sits in the environment (`OPENAI_API_KEY`, ...) must
//! never make Rift upload terminal output. AI is usable only when
//!
//! * the user wrote an `[llm]` section in config.toml (an explicit choice of
//!   endpoint), or
//! * the user answered the first-run prompt with "Enable" (`[ai] consent =
//!   "cloud"`) or "Local only" (`consent = "local"`, a model server on this
//!   machine: the discovered one, else Ollama on localhost).
//!
//! Every AI entry point (chat/hub, inline `#`, Cmd+K, auto-fix) must check
//! [`allowed`] before building a request. The prompt itself is shown by
//! [`maybe_prompt`] and its answer applied by [`apply`].

use crate::ai::inline::llm_ready;
use crate::ai::local::{self, LocalChoice, LocalServer};
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

/// May this config send data off the machine? Only with the cloud consent or
/// an explicit non-local `[llm]` section ([`Config::cloud_opt_in`]); a "local" consent never reaches a remote host.
pub fn cloud_ok(cfg: &Config) -> bool {
    cfg.cloud_opt_in
}

/// Pure policy: may AI features run with this config? (`llm_ready` alone is
/// not enough: it only says a provider is reachable, not that the user opted in.)
pub fn allowed_config(cfg: &Config, llm: &LlmConfig) -> bool {
    llm_ready(llm)
        && (cfg.llm_explicit || matches!(cfg.ai_consent, Consent::Cloud | Consent::Local))
        && (local::routing::is_local(llm) || cloud_ok(cfg))
}

/// Stable gate for every AI entry point: true only when AI is configured or
/// consented to AND a provider is usable.
pub fn allowed(app: &App) -> bool {
    allowed_config(&app.config, &app.llm.config)
}

/// What the first-run prompt offers.
#[derive(Clone, Debug)]
pub struct PromptPlan {
    /// A running local model server with at least one model.
    pub local: Option<LocalChoice>,
    /// A cloud key found in the environment.
    pub env: Option<EnvCandidate>,
}

/// One answer the prompt can give (index-aligned with its buttons).
#[derive(Clone, Debug)]
pub enum ConsentOption {
    /// `None` = "Local only" without a discovered server (Ollama defaults).
    Local(Option<LocalChoice>),
    Cloud(EnvCandidate),
    Declined,
}

impl PromptPlan {
    /// Buttons, their answers and the default selection. A found local model
    /// is the default; otherwise "Not now" is.
    pub fn options(&self) -> (Vec<(&'static str, ConsentOption)>, usize) {
        let mut v: Vec<(&'static str, ConsentOption)> = Vec::new();
        if let Some(l) = &self.local {
            v.push(("Use local model", ConsentOption::Local(Some(l.clone()))));
            if let Some(e) = &self.env {
                v.push(("Enable cloud", ConsentOption::Cloud(e.clone())));
            }
            v.push(("Not now", ConsentOption::Declined));
            (v, 0)
        } else {
            if let Some(e) = &self.env {
                v.push(("Enable", ConsentOption::Cloud(e.clone())));
            }
            v.push(("Local only", ConsentOption::Local(None)));
            v.push(("Not now", ConsentOption::Declined));
            let d = v.len() - 1;
            (v, d)
        }
    }
}

/// Pure: what to ask, given the config, the discovered servers and the env.
pub fn plan_prompt(cfg: &Config, servers: &[LocalServer], env: Option<EnvCandidate>) -> Option<PromptPlan> {
    if cfg.llm_explicit || cfg.ai_consent != Consent::Unset {
        return None;
    }
    let local = local::discovery::first_choice(servers);
    if local.is_none() && env.is_none() {
        return None;
    }
    Some(PromptPlan { local, env })
}

/// Should the first-run prompt be shown now?
pub fn should_prompt(cfg: &Config) -> Option<PromptPlan> {
    plan_prompt(cfg, &local::discovery::servers(), env_candidate())
}

/// Queue the consent modal once per run when there is something to offer
/// (a local model server or a cloud key) and the user has not decided yet.
/// Waits for the background discovery so the modal can name the local model.
pub fn maybe_prompt(app: &mut App) {
    if app.consent_checked {
        return;
    }
    if app.config.llm_explicit || app.config.ai_consent != Consent::Unset {
        app.consent_checked = true;
        return;
    }
    local::discovery::start();
    if !local::discovery::finished() {
        return; // woken again when the probes land
    }
    app.consent_checked = true;
    if let Some(plan) = should_prompt(&app.config) {
        crate::ui::confirm::show_ai_consent(app, plan);
    }
}

/// Make `cfg` the active model: live config, chat header, and a persisted
/// `[llm]` (provider/model/api_url; the API key is never written).
pub fn set_active(app: &mut App, cfg: LlmConfig) {
    if local::routing::is_local(&cfg) {
        app.llm.last_local = Some(cfg.clone());
    } else {
        app.llm.cloud = Some(cfg.clone());
    }
    app.config.llm = cfg.clone();
    app.config.llm_persist = true;
    app.llm.config = cfg;
    crate::ai::chat::refresh_header(app);
    crate::config::toml::save_config(&app.config);
}

/// Apply the user's choice: update the live config, persist via merge-save.
pub fn apply(app: &mut App, choice: Consent, env: Option<&EnvCandidate>, local: Option<&LocalChoice>) {
    app.config.ai_consent = choice;
    match choice {
        Consent::Cloud => {
            app.config.cloud_opt_in = true;
            if let Some(e) = env {
                app.config.llm.provider = e.provider.into();
                app.config.llm.api_url = e.api_url.into();
                app.config.llm.model = e.model.into();
                app.config.llm.api_key = Some(e.key.clone());
                app.config.llm.enabled = true;
                app.llm.cloud = Some(app.config.llm.clone());
            }
            app.config.ai_auto_fix = true;
        }
        Consent::Local => {
            match local {
                Some(l) => {
                    app.config.llm = l.to_config();
                    // Persist [llm] so the chosen model survives a restart.
                    app.config.llm_persist = true;
                    app.llm.last_local = Some(app.config.llm.clone());
                }
                None => {
                    app.config.llm = LlmConfig::default();
                    app.config.llm.enabled = true;
                }
            }
            app.config.ai_auto_fix = true;
        }
        Consent::Declined | Consent::Unset => {
            app.config.ai_auto_fix = false;
        }
    }
    app.llm.config = app.config.llm.clone();
    app.menubar.set_ai_checks(app.config.ai_auto_fix, app.config.ai_nl_hash);
    crate::ai::chat::refresh_header(app);
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
        cfg.cloud_opt_in = true; // set by the loader for a non-local [llm]
        assert!(allowed_config(&cfg, &llm(true)));
        assert!(!allowed_config(&cfg, &llm(false)));
        let mut cfg = Config::default();
        cfg.ai_consent = Consent::Cloud;
        cfg.cloud_opt_in = true;
        assert!(allowed_config(&cfg, &llm(true)));
        cfg.ai_consent = Consent::Declined;
        assert!(!allowed_config(&cfg, &llm(true)));
    }

    fn server(model: &str) -> LocalServer {
        LocalServer { kind: local::discovery::ServerKind::Ollama, base_url: "http://127.0.0.1:11434".into(), models: vec![model.into()] }
    }
    fn env() -> EnvCandidate {
        EnvCandidate { var: "OPENAI_API_KEY", provider: "openai", api_url: "https://api.openai.com", model: "gpt-4o-mini", key: "k".into() }
    }

    #[test]
    fn prompt_only_when_undecided_and_something_to_offer() {
        let mut cfg = Config::default();
        cfg.ai_consent = Consent::Declined;
        assert!(plan_prompt(&cfg, &[server("m")], Some(env())).is_none());
        cfg.ai_consent = Consent::Unset;
        cfg.llm_explicit = true;
        assert!(plan_prompt(&cfg, &[server("m")], Some(env())).is_none());
        let cfg = Config::default();
        assert!(plan_prompt(&cfg, &[], None).is_none(), "no server and no key: nothing to ask");
        assert!(plan_prompt(&cfg, &[], Some(env())).is_some());
        assert!(plan_prompt(&cfg, &[server("m")], None).is_some());
    }

    #[test]
    fn local_model_is_the_default_and_cloud_needs_an_env_key() {
        let cfg = Config::default();
        // Local server found, no env key: local + decline only, local highlighted.
        let plan = plan_prompt(&cfg, &[server("qwen2.5-coder:7b")], None).unwrap();
        let (opts, def) = plan.options();
        assert_eq!(opts.iter().map(|o| o.0).collect::<Vec<_>>(), ["Use local model", "Not now"]);
        assert_eq!(def, 0);
        assert!(matches!(&opts[0].1, ConsentOption::Local(Some(l)) if l.model == "qwen2.5-coder:7b" && l.provider == "ollama"));
        assert!(!opts.iter().any(|o| matches!(o.1, ConsentOption::Cloud(_))), "no cloud option without a key");
        // Both: cloud is offered, local still the default.
        let plan = plan_prompt(&cfg, &[server("m")], Some(env())).unwrap();
        let (opts, def) = plan.options();
        assert_eq!(opts.iter().map(|o| o.0).collect::<Vec<_>>(), ["Use local model", "Enable cloud", "Not now"]);
        assert_eq!(def, 0);
        // Env key only: the safe "Not now" stays the default.
        let plan = plan_prompt(&cfg, &[], Some(env())).unwrap();
        let (opts, def) = plan.options();
        assert_eq!(opts.iter().map(|o| o.0).collect::<Vec<_>>(), ["Enable", "Local only", "Not now"]);
        assert_eq!(def, 2);
    }

    #[test]
    fn local_consent_never_reaches_a_remote_host() {
        let mut cfg = Config::default();
        cfg.ai_consent = Consent::Local;
        assert!(!allowed_config(&cfg, &llm(true)), "cloud endpoint under a local-only consent");
        let local_cfg = LlmConfig { provider: "ollama".into(), model: "m".into(), api_url: "http://127.0.0.1:11434".into(), api_key: None, enabled: true };
        assert!(allowed_config(&cfg, &local_cfg));
        cfg.ai_consent = Consent::Declined;
        assert!(!allowed_config(&cfg, &local_cfg));
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
