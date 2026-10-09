//! Local-first AI: model discovery, per-feature routing and the privacy ledger.
//!
//! * [`discovery`]: finds Ollama / LM Studio / llama.cpp / Jan / vLLM servers
//!   on loopback in a background thread and caches the answer.
//! * [`routing`]: decides which configured model serves which feature (auto-fix
//!   and `#` prefer a local model; chat uses the selected default).
//! * [`usage`]: counts the bytes sent to non-loopback endpoints (the "zero
//!   bytes left this machine" indicator and the Privacy Report).
//! * [`picker`]: the model list behind "AI: Select Model".

pub mod discovery;
pub mod picker;
pub mod routing;
pub mod usage;

pub use discovery::{LocalChoice, LocalServer};
pub use routing::{Route, Routing};

/// Which AI feature issued a request (routing key and ledger label).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Feature {
    Chat,
    Fix,
    Nl,
    Advisor,
    Teaching,
    Other,
}

impl Feature {
    pub fn label(self) -> &'static str {
        match self {
            Feature::Chat => "chat",
            Feature::Fix => "auto-fix",
            Feature::Nl => "# nl",
            Feature::Advisor => "advisor",
            Feature::Teaching => "teaching",
            Feature::Other => "other",
        }
    }
}

/// Ollama context window requested for every call. One fixed value avoids the
/// model being reloaded whenever two features ask for different sizes
/// (`RIFT_OLLAMA_NUM_CTX` overrides).
pub fn ollama_num_ctx() -> u32 {
    std::env::var("RIFT_OLLAMA_NUM_CTX")
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .filter(|n| (512..=131_072).contains(n))
        .unwrap_or(8192)
}

/// The model that should serve `feature` right now, or `None` when AI is not
/// allowed for it (no consent, no usable provider, or a strict
/// `*_provider = "local" | "cloud"` setting with no such model).
///
/// A remote model is only ever returned with a cloud opt-in; the cloud
/// candidate taken from an environment key needs the consent prompt first.
pub fn routed(app: &crate::app::App, feature: Feature) -> Option<crate::ai::LlmConfig> {
    use crate::ai::consent;
    let servers = discovery::servers();
    let default = &app.llm.config;
    let local = routing::local_candidate(default, app.llm.last_local.as_ref(), &servers);
    let cloud = if consent::cloud_ok(&app.config) { app.llm.cloud_candidate() } else { None };
    let cfg = routing::pick(feature, &app.config.ai_routing, default, local.as_ref(), cloud.as_ref())?;
    consent::allowed_config(&app.config, &cfg).then_some(cfg)
}
