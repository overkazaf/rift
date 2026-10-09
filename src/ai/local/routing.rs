//! Per-feature provider routing.
//!
//! `[ai] fix_provider / nl_provider / chat_provider = "local" | "cloud" | "default"`.
//! Unset means: auto-fix and `#` prefer a local model when one is available
//! (cheap and private), chat uses the selected default model.
//!
//! `"local"` and `"cloud"` are strict: when no such provider exists the feature
//! gets no model (and so sends nothing) rather than silently falling back to
//! the other side.

use super::discovery::{self, LocalServer};
use super::usage::is_loopback_url;
use super::Feature;
use crate::ai::LlmConfig;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    Local,
    Cloud,
    Default,
}

impl Route {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "local" => Some(Self::Local),
            "cloud" => Some(Self::Cloud),
            "default" => Some(Self::Default),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Cloud => "cloud",
            Self::Default => "default",
        }
    }
}

/// The three `[ai] *_provider` settings (`None` = not set).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Routing {
    pub fix: Option<Route>,
    pub nl: Option<Route>,
    pub chat: Option<Route>,
}

impl Routing {
    pub fn get(&self, f: Feature) -> Option<Route> {
        match f {
            Feature::Fix => self.fix,
            Feature::Nl => self.nl,
            Feature::Chat => self.chat,
            _ => None,
        }
    }
}

pub fn is_local(c: &LlmConfig) -> bool {
    is_loopback_url(&c.api_url)
}

/// The local model to use when `default` is not already local: the user's
/// last local selection if its server is still up, else the first discovered.
pub fn local_candidate(default: &LlmConfig, last_local: Option<&LlmConfig>, servers: &[LocalServer]) -> Option<LlmConfig> {
    if is_local(default) {
        return Some(default.clone());
    }
    if let Some(l) = last_local {
        if servers.iter().any(|s| s.base_url.trim_end_matches('/') == l.api_url.trim_end_matches('/')) {
            return Some(l.clone());
        }
    }
    discovery::first_choice(servers).map(|c| c.to_config())
}

/// Pure routing decision. `None` = no eligible model: send nothing.
pub fn pick(
    feature: Feature,
    routing: &Routing,
    default: &LlmConfig,
    local: Option<&LlmConfig>,
    cloud: Option<&LlmConfig>,
) -> Option<LlmConfig> {
    let pref = routing.get(feature);
    let default_local = is_local(default);
    match pref {
        // Unset: fix / # prefer local when available, everything else uses the default.
        None => match feature {
            Feature::Fix | Feature::Nl => local.cloned().or_else(|| Some(default.clone())),
            _ => Some(default.clone()),
        },
        Some(Route::Default) => Some(default.clone()),
        Some(Route::Local) => {
            if default_local {
                Some(default.clone())
            } else {
                local.cloned()
            }
        }
        Some(Route::Cloud) => {
            if !default_local {
                Some(default.clone())
            } else {
                cloud.cloned()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(url: &str, provider: &str, model: &str) -> LlmConfig {
        LlmConfig { provider: provider.into(), model: model.into(), api_url: url.into(), api_key: None, enabled: true }
    }
    fn cloud() -> LlmConfig {
        let mut c = cfg("https://api.openai.com", "openai", "gpt-4o-mini");
        c.api_key = Some("k".into());
        c
    }
    fn local() -> LlmConfig {
        cfg("http://127.0.0.1:11434", "ollama", "qwen2.5-coder:7b")
    }

    #[test]
    fn parse_round_trip() {
        for r in [Route::Local, Route::Cloud, Route::Default] {
            assert_eq!(Route::parse(r.as_str()), Some(r));
        }
        assert_eq!(Route::parse(" LOCAL "), Some(Route::Local));
        assert_eq!(Route::parse("auto"), None);
    }

    #[test]
    fn fix_and_nl_prefer_local_chat_uses_default() {
        let r = Routing::default();
        let (c, l) = (cloud(), local());
        assert_eq!(pick(Feature::Fix, &r, &c, Some(&l), None).unwrap().model, "qwen2.5-coder:7b");
        assert_eq!(pick(Feature::Nl, &r, &c, Some(&l), None).unwrap().model, "qwen2.5-coder:7b");
        assert_eq!(pick(Feature::Chat, &r, &c, Some(&l), None).unwrap().model, "gpt-4o-mini");
        assert_eq!(pick(Feature::Advisor, &r, &c, Some(&l), None).unwrap().model, "gpt-4o-mini");
        // No local server: fix falls back to the default (the user's own choice).
        assert_eq!(pick(Feature::Fix, &r, &c, None, None).unwrap().model, "gpt-4o-mini");
    }

    #[test]
    fn explicit_prefs_are_honoured_and_strict() {
        let (c, l) = (cloud(), local());
        let r = Routing { fix: Some(Route::Default), nl: Some(Route::Local), chat: Some(Route::Cloud) };
        assert_eq!(pick(Feature::Fix, &r, &c, Some(&l), None).unwrap().model, "gpt-4o-mini");
        assert_eq!(pick(Feature::Nl, &r, &c, Some(&l), None).unwrap().model, "qwen2.5-coder:7b");
        // local requested, none exists: nothing is sent.
        assert!(pick(Feature::Nl, &r, &c, None, None).is_none());
        // cloud requested while the default is local: the cloud model, or nothing.
        assert_eq!(pick(Feature::Chat, &r, &l, Some(&l), Some(&c)).unwrap().model, "gpt-4o-mini");
        assert!(pick(Feature::Chat, &r, &l, Some(&l), None).is_none());
        // already-local default satisfies "local"; already-cloud default satisfies "cloud".
        assert_eq!(pick(Feature::Nl, &r, &l, None, None).unwrap().model, "qwen2.5-coder:7b");
        assert_eq!(pick(Feature::Chat, &r, &c, None, None).unwrap().model, "gpt-4o-mini");
    }

    #[test]
    fn local_candidate_prefers_default_then_last_then_discovered() {
        let srv = LocalServer {
            kind: discovery::ServerKind::Ollama,
            base_url: "http://127.0.0.1:11434".into(),
            models: vec!["a".into(), "b".into()],
        };
        let c = cloud();
        assert_eq!(local_candidate(&local(), None, &[]).unwrap().model, "qwen2.5-coder:7b");
        let mut last = local();
        last.model = "b".into();
        assert_eq!(local_candidate(&c, Some(&last), std::slice::from_ref(&srv)).unwrap().model, "b");
        // last selection's server is gone -> first discovered
        assert_eq!(local_candidate(&c, Some(&last), &[LocalServer { base_url: "http://127.0.0.1:1234".into(), ..srv.clone() }]).unwrap().api_url, "http://127.0.0.1:1234");
        assert!(local_candidate(&c, None, &[]).is_none());
    }
}
