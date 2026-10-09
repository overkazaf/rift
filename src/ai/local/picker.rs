//! "AI: Select Model": the list of models the user can switch to at runtime
//! (discovered local models plus the configured cloud model) and the switch.

use super::discovery::LocalServer;
use super::routing::is_local;
use crate::ai::consent::{self, Consent};
use crate::ai::LlmConfig;
use crate::app::App;

/// What selecting an entry switches to. Never carries an API key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelChoice {
    pub provider: String,
    pub model: String,
    pub api_url: String,
    pub local: bool,
}

/// One row of the picker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelOption {
    pub label: String,
    /// `Ollama - local` / `api.openai.com - cloud`.
    pub detail: String,
    pub active: bool,
    pub choice: ModelChoice,
}

fn same_model(a: &LlmConfig, c: &ModelChoice) -> bool {
    a.model == c.model && a.api_url.trim_end_matches('/') == c.api_url.trim_end_matches('/')
}

/// Pure: rows for `active`, the discovered `servers` and an optional cloud model.
/// Local models first (the private choice), then the cloud one.
pub fn options(active: &LlmConfig, servers: &[LocalServer], cloud: Option<&LlmConfig>) -> Vec<ModelOption> {
    let mut out = Vec::new();
    for s in servers {
        for m in s.chat_models() {
            let choice = ModelChoice { provider: s.kind.provider().into(), model: m.into(), api_url: s.base_url.clone(), local: true };
            out.push(ModelOption {
                label: m.to_string(),
                detail: format!("{} - local", s.kind.label()),
                active: active.enabled && same_model(active, &choice),
                choice,
            });
        }
    }
    // The active model may be a local one that is not (or no longer) discovered.
    if active.enabled && is_local(active) && !out.iter().any(|o| o.active) {
        let choice = ModelChoice { provider: active.provider.clone(), model: active.model.clone(), api_url: active.api_url.clone(), local: true };
        out.insert(0, ModelOption { label: active.model.clone(), detail: "configured - local".into(), active: true, choice });
    }
    if let Some(c) = cloud {
        let choice = ModelChoice { provider: c.provider.clone(), model: c.model.clone(), api_url: c.api_url.clone(), local: false };
        out.push(ModelOption {
            label: c.model.clone(),
            detail: format!("{} - cloud", super::usage::host_of(&c.api_url)),
            active: active.enabled && same_model(active, &choice),
            choice,
        });
    }
    out
}

/// Rows for the palette right now. Also kicks off a fresh discovery so the
/// next open sees servers started after Rift.
pub fn current_options(app: &App) -> Vec<ModelOption> {
    super::discovery::refresh();
    options(&app.llm.config, &super::discovery::servers(), app.llm.cloud_candidate().as_ref())
}

/// Switch to `choice`: live config, header, and persisted `[llm]`.
pub fn select(app: &mut App, choice: &ModelChoice) {
    if choice.local {
        let cfg = LlmConfig {
            provider: choice.provider.clone(),
            model: choice.model.clone(),
            api_url: choice.api_url.clone(),
            api_key: None,
            enabled: true,
        };
        if !matches!(app.config.ai_consent, Consent::Local | Consent::Cloud) && !app.config.llm_explicit {
            // Choosing a model on this machine is an explicit local opt-in.
            let lc = super::discovery::LocalChoice {
                provider: cfg.provider.clone(),
                model: cfg.model.clone(),
                api_url: cfg.api_url.clone(),
                server: "local",
            };
            consent::apply(app, Consent::Local, None, Some(&lc));
        } else {
            consent::set_active(app, cfg);
        }
        notify(app, &format!("AI model: {} (local - nothing leaves this machine)", choice.model));
        return;
    }
    // Cloud: only with an existing cloud opt-in; otherwise ask first.
    let Some(mut cfg) = app.llm.cloud_candidate().filter(|c| c.model == choice.model && c.api_url == choice.api_url) else {
        return;
    };
    if consent::cloud_ok(&app.config) {
        cfg.enabled = true;
        consent::set_active(app, cfg);
        notify(app, &format!("AI model: {} (cloud)", choice.model));
    } else if let Some(env) = consent::env_candidate() {
        let plan = consent::PromptPlan { local: None, env: Some(env) };
        crate::ui::confirm::show_ai_consent(app, plan);
    }
}

fn notify(app: &mut App, msg: &str) {
    app.chat.set_toast(msg);
    app.inline_ai.set_toast(msg);
    app.request_redraw();
}

#[cfg(test)]
mod tests {
    use super::super::discovery::ServerKind;
    use super::*;

    fn cfg(url: &str, provider: &str, model: &str) -> LlmConfig {
        LlmConfig { provider: provider.into(), model: model.into(), api_url: url.into(), api_key: None, enabled: true }
    }
    fn ollama() -> LocalServer {
        LocalServer { kind: ServerKind::Ollama, base_url: "http://127.0.0.1:11434".into(), models: vec!["qwen2.5-coder:7b".into(), "nomic-embed-text".into(), "llama3.2".into()] }
    }

    #[test]
    fn lists_local_chat_models_then_cloud_and_marks_active() {
        let cloud = cfg("https://api.deepseek.com", "deepseek", "deepseek-chat");
        let active = cfg("http://127.0.0.1:11434", "ollama", "llama3.2");
        let o = options(&active, &[ollama()], Some(&cloud));
        let labels: Vec<_> = o.iter().map(|x| x.label.as_str()).collect();
        assert_eq!(labels, ["qwen2.5-coder:7b", "llama3.2", "deepseek-chat"], "embedding models are hidden");
        assert_eq!(o.iter().filter(|x| x.active).count(), 1);
        assert!(o[1].active && o[1].choice.local && o[1].detail == "Ollama - local");
        assert!(!o[2].choice.local && o[2].detail == "api.deepseek.com - cloud");
        assert!(o.iter().all(|x| x.choice.api_url.starts_with("http")), "no key material in a choice");
    }

    #[test]
    fn cloud_active_and_undiscovered_local_active() {
        let cloud = cfg("https://api.openai.com", "openai", "gpt-4o-mini");
        let o = options(&cloud, &[], Some(&cloud));
        assert_eq!(o.len(), 1);
        assert!(o[0].active);
        // A configured local model whose server is down is still listed (and active).
        let o = options(&cfg("http://localhost:1234", "openai-compatible-local", "my-model"), &[], None);
        assert_eq!(o.len(), 1);
        assert!(o[0].active && o[0].choice.local);
        // Disabled AI marks nothing active.
        let mut off = cloud.clone();
        off.enabled = false;
        assert!(options(&off, &[], Some(&cloud)).iter().all(|x| !x.active));
    }
}
