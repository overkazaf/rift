use super::{Config, Rgb};
use crate::effects::EffectKind;
use std::collections::HashMap;
use std::path::PathBuf;

pub fn save_config(config: &Config) {
    let path = dirs::home_dir()
        .unwrap_or_default()
        .join(".config/rift/config.toml");
    let s = config_to_toml(config);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match std::fs::write(&path, &s) {
        Ok(_) => log::info!("Config saved: {}", path.display()),
        Err(e) => log::error!("Save config failed: {e}"),
    }
}

/// Serialize the config to TOML (inverse of `parse_toml_config`).
pub fn config_to_toml(config: &Config) -> String {
    let mut s = String::new();
    s.push_str("[general]\n");
    s.push_str(&format!("font_size = {:.1}\n", config.font_size));
    s.push_str(&format!("cols = {}\n", config.cols));
    s.push_str(&format!("rows = {}\n", config.rows));
    s.push_str(&format!("opacity = {:.2}\n", config.opacity));
    s.push_str(&format!("theme = \"{}\"\n", config.theme_name));
    if let Some(ref family) = config.font_family {
        s.push_str(&format!("font_family = \"{family}\"\n"));
    }
    if let Some(ref path) = config.font_path {
        s.push_str(&format!("font_path = \"{path}\"\n"));
    }
    s.push_str(&format!("effect = \"{}\"\n", config.effect.map_or("none", |k| k.name())));
    s.push_str(&format!("effect_intensity = {:.2}\n", config.effect_intensity));
    s.push_str(&format!("startup_animation = {}\n", config.startup_animation));
    s.push_str("\n[ai]\n");
    s.push_str(&format!("auto_fix = {}\n", config.ai_auto_fix));
    s.push_str(&format!("nl_hash = {}\n", config.ai_nl_hash));
    if config.llm.enabled {
        s.push_str("\n[llm]\n");
        s.push_str(&format!("provider = \"{}\"\n", config.llm.provider));
        s.push_str(&format!("model = \"{}\"\n", config.llm.model));
        s.push_str(&format!("api_url = \"{}\"\n", config.llm.api_url));
    }
    s
}

pub fn load_config() -> Config {
    let path = config_path();
    if let Ok(content) = std::fs::read_to_string(&path) {
        log::info!("Loaded config: {}", path.display());
        parse_toml_config(&content)
    } else {
        Config::default()
    }
}

fn config_path() -> PathBuf {
    if let Some(home) = dirs::home_dir() {
        // Primary: ~/.config/rift/config.toml
        let rift = home.join(".config").join("rift").join("config.toml");
        if rift.exists() { return rift; }
        // Fallback: ~/.config/rterm/config.toml (backward compat)
        let rterm = home.join(".config").join("rterm").join("config.toml");
        if rterm.exists() { return rterm; }
    }
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("rift")
        .join("config.toml")
}

fn parse_toml_config(content: &str) -> Config {
    let map = parse_toml(content);
    let mut config = Config::default();

    // Config files that predate the rift-neon default keep their old look.
    if get_str(&map, "general", "theme").is_none() {
        if let Some(theme) = Config::theme_by_name(super::LEGACY_THEME) {
            config.theme = theme;
            config.theme_name = super::LEGACY_THEME.to_string();
        }
    }
    if let Some(v) = get_str(&map, "general", "theme") {
        if let Some(theme) = Config::theme_by_name(&v) {
            config.theme = theme;
            config.theme_name = v;
        } else {
            log::warn!("Unknown theme '{}', using default", v);
        }
    }

    if let Some(v) = get_float(&map, "general", "font_size") {
        config.font_size = v;
    }
    if let Some(v) = get_int(&map, "general", "cols") {
        config.cols = v as u16;
    }
    if let Some(v) = get_int(&map, "general", "rows") {
        config.rows = v as u16;
    }
    if let Some(v) = get_float(&map, "general", "opacity") {
        config.opacity = v.clamp(0.1, 1.0);
    }
    if let Some(v) = get_str(&map, "general", "font_family") {
        config.font_family = Some(v);
    }
    if let Some(v) = get_str(&map, "general", "font_path") {
        config.font_path = Some(v);
    }

    if let Some(v) = get_str(&map, "general", "effect").or_else(|| get_str(&map, "", "effect")) {
        match EffectKind::parse_setting(&v) {
            Some(kind) => config.effect = kind,
            None => log::warn!("Unknown effect '{v}' (valid: crt, glitch, neon, matrix, amber, hologram, none)"),
        }
    }
    if let Some(v) = get_float(&map, "general", "effect_intensity").or_else(|| get_float(&map, "", "effect_intensity")) {
        config.effect_intensity = v.clamp(0.0, 1.0);
    }
    if let Some(v) = get_int(&map, "general", "startup_animation").or_else(|| get_int(&map, "", "startup_animation")) {
        config.startup_animation = v != 0;
    }

    if let Some(fg) = get_rgb(&map, "theme.custom", "fg") {
        config.theme.fg = fg;
    }
    if let Some(bg) = get_rgb(&map, "theme.custom", "bg") {
        config.theme.bg = bg;
    }
    if let Some(cursor) = get_rgb(&map, "theme.custom", "cursor") {
        config.theme.cursor = cursor;
    }

    if let Some(v) = get_int(&map, "ai", "auto_fix") {
        config.ai_auto_fix = v != 0;
    }
    if let Some(v) = get_int(&map, "ai", "nl_hash") {
        config.ai_nl_hash = v != 0;
    }

    // LLM config
    if let Some(v) = get_str(&map, "llm", "provider") {
        config.llm.provider = v;
        config.llm.enabled = true;
    }
    if let Some(v) = get_str(&map, "llm", "model") {
        config.llm.model = v;
    }
    if let Some(v) = get_str(&map, "llm", "api_url") {
        config.llm.api_url = v;
    }
    if let Some(v) = get_str(&map, "llm", "api_key") {
        config.llm.api_key = Some(v);
    }

    config.llm.resolve_api_key();

    config
}

type TomlMap = HashMap<String, HashMap<String, TomlValue>>;

#[derive(Debug, Clone)]
enum TomlValue {
    Str(String),
    Float(f64),
    Int(i64),
    Array(Vec<TomlValue>),
}

fn parse_toml(content: &str) -> TomlMap {
    let mut map = TomlMap::new();
    let mut section = String::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') { continue; }
        if line.starts_with('[') && line.ends_with(']') {
            section = line[1..line.len() - 1].trim().to_string();
            continue;
        }
        if let Some((key, val)) = line.split_once('=') {
            let key = key.trim().to_string();
            let val = val.trim();
            let parsed = parse_value(val);
            map.entry(section.clone()).or_default().insert(key, parsed);
        }
    }
    map
}

fn parse_value(s: &str) -> TomlValue {
    let s = s.trim();
    if (s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')) {
        return TomlValue::Str(s[1..s.len() - 1].to_string());
    }
    if s.starts_with('[') && s.ends_with(']') {
        let inner = &s[1..s.len() - 1];
        let items: Vec<TomlValue> = inner.split(',').map(|v| parse_value(v.trim())).collect();
        return TomlValue::Array(items);
    }
    if s.contains('.') {
        if let Ok(f) = s.parse::<f64>() { return TomlValue::Float(f); }
    }
    if let Ok(i) = s.parse::<i64>() { return TomlValue::Int(i); }
    if s == "true" { return TomlValue::Int(1); }
    if s == "false" { return TomlValue::Int(0); }
    TomlValue::Str(s.to_string())
}

fn get_str(map: &TomlMap, section: &str, key: &str) -> Option<String> {
    match map.get(section)?.get(key)? {
        TomlValue::Str(s) => Some(s.clone()),
        _ => None,
    }
}

fn get_float(map: &TomlMap, section: &str, key: &str) -> Option<f32> {
    match map.get(section)?.get(key)? {
        TomlValue::Float(f) => Some(*f as f32),
        TomlValue::Int(i) => Some(*i as f32),
        _ => None,
    }
}

fn get_int(map: &TomlMap, section: &str, key: &str) -> Option<i64> {
    match map.get(section)?.get(key)? {
        TomlValue::Int(i) => Some(*i),
        TomlValue::Float(f) => Some(*f as i64),
        _ => None,
    }
}

fn get_rgb(map: &TomlMap, section: &str, key: &str) -> Option<Rgb> {
    match map.get(section)?.get(key)? {
        TomlValue::Array(arr) if arr.len() == 3 => {
            let r = match &arr[0] { TomlValue::Int(v) => *v as u8, _ => return None };
            let g = match &arr[1] { TomlValue::Int(v) => *v as u8, _ => return None };
            let b = match &arr[2] { TomlValue::Int(v) => *v as u8, _ => return None };
            Some((r, g, b))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ai_toggles_default_on_and_parse() {
        let c = parse_toml_config("[general]\nfont_size = 14.0\n");
        assert!(c.ai_auto_fix && c.ai_nl_hash);
        let c = parse_toml_config("[ai]\nauto_fix = false\nnl_hash = true\n");
        assert!(!c.ai_auto_fix && c.ai_nl_hash);
    }

    #[test]
    fn effect_settings_round_trip() {
        for kind in EffectKind::ALL.map(Some).into_iter().chain([None]) {
            let mut c = Config::default();
            c.effect = kind;
            c.effect_intensity = 0.35;
            c.startup_animation = false;
            let back = parse_toml_config(&config_to_toml(&c));
            assert_eq!(back.effect, kind);
            assert!((back.effect_intensity - 0.35).abs() < 1e-6);
            assert!(!back.startup_animation);
            assert_eq!(back.theme_name, c.theme_name);
        }
    }

    #[test]
    fn effect_parsing_defaults_clamps_and_rejects_junk() {
        let c = parse_toml_config("[general]\nfont_size = 14.0\n");
        assert_eq!(c.effect, None);
        assert!((c.effect_intensity - 0.6).abs() < 1e-6);
        assert!(c.startup_animation);
        let c = parse_toml_config("[general]\neffect = \"hologram\"\neffect_intensity = 7\n");
        assert_eq!(c.effect, Some(EffectKind::Hologram));
        assert_eq!(c.effect_intensity, 1.0);
        let c = parse_toml_config("effect = \"crt\"\n");
        assert_eq!(c.effect, Some(EffectKind::Crt), "top-level key accepted");
        let c = parse_toml_config("[general]\neffect = \"pixelate\"\n");
        assert_eq!(c.effect, None, "removed effects are ignored");
    }

    #[test]
    fn default_theme_only_applies_to_new_configs() {
        assert_eq!(Config::default().theme_name, "rift-neon");
        let c = parse_toml_config("[general]\nfont_size = 14.0\n");
        assert_eq!(c.theme_name, "catppuccin-mocha", "existing config without theme keeps legacy look");
        let c = parse_toml_config("[general]\ntheme = \"nord\"\n");
        assert_eq!(c.theme_name, "nord");
        let c = parse_toml_config("[general]\ntheme = \"rift-neon\"\n");
        assert_eq!(c.theme.name, "rift-neon");
    }
}
