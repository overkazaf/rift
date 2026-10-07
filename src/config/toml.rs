use super::{Config, Rgb};
use std::collections::HashMap;
use std::path::PathBuf;

pub fn save_config(config: &Config) {
    let path = dirs::home_dir()
        .unwrap_or_default()
        .join(".config/rift/config.toml");
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
    if config.llm.enabled {
        s.push_str("\n[llm]\n");
        s.push_str(&format!("provider = \"{}\"\n", config.llm.provider));
        s.push_str(&format!("model = \"{}\"\n", config.llm.model));
        s.push_str(&format!("api_url = \"{}\"\n", config.llm.api_url));
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match std::fs::write(&path, &s) {
        Ok(_) => log::info!("Config saved: {}", path.display()),
        Err(e) => log::error!("Save config failed: {e}"),
    }
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

    if let Some(fg) = get_rgb(&map, "theme.custom", "fg") {
        config.theme.fg = fg;
    }
    if let Some(bg) = get_rgb(&map, "theme.custom", "bg") {
        config.theme.bg = bg;
    }
    if let Some(cursor) = get_rgb(&map, "theme.custom", "cursor") {
        config.theme.cursor = cursor;
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
