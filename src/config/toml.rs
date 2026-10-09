use super::{Config, Osc52Policy, Rgb};
use crate::ai::consent::Consent;
use crate::effects::EffectKind;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::OnceLock;

/// `--config PATH`: used for both loading and saving.
static CONFIG_PATH_OVERRIDE: OnceLock<PathBuf> = OnceLock::new();

pub fn set_config_path(p: PathBuf) {
    let _ = CONFIG_PATH_OVERRIDE.set(p);
}

pub fn clamp_font_size(v: f32) -> f32 {
    if v.is_finite() { v.clamp(super::FONT_SIZE_RANGE.0, super::FONT_SIZE_RANGE.1) } else { Config::default().font_size }
}
pub fn clamp_opacity(v: f32) -> f32 {
    if v.is_finite() { v.clamp(super::OPACITY_RANGE.0, super::OPACITY_RANGE.1) } else { Config::default().opacity }
}
pub fn clamp_cols(v: i64) -> u16 {
    v.clamp(super::COLS_RANGE.0, super::COLS_RANGE.1) as u16
}
pub fn clamp_rows(v: i64) -> u16 {
    v.clamp(super::ROWS_RANGE.0, super::ROWS_RANGE.1) as u16
}

/// Persist the settings that changed since the file was last written.
///
/// This is a *merge*: only the managed keys whose value differs from what the
/// existing file already says are edited in place. Comments, ordering, unknown
/// keys, `[llm]` (including `api_key`) and `[theme.custom]` are left exactly as
/// the user wrote them, and nothing is written at all when nothing changed.
pub fn save_config(config: &Config) {
    let path = config_path();
    let existing = std::fs::read_to_string(&path).ok();
    let Some(out) = compute_saved(existing.as_deref(), config) else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // Write-then-rename so a crash never leaves a truncated config behind.
    let tmp = path.with_extension("toml.tmp");
    let res = std::fs::write(&tmp, &out).and_then(|_| std::fs::rename(&tmp, &path));
    match res {
        Ok(_) => log::info!("Config saved: {}", path.display()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            log::error!("Save config failed: {e}");
        }
    }
}

/// One managed setting: `[section] key = literal`.
struct Edit {
    section: &'static str,
    key: &'static str,
    literal: String,
}

fn quote(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

fn fmt_f(v: f32, decimals: usize) -> String {
    format!("{v:.decimals$}")
}

/// Full serialization of every managed setting (no comments, no `[llm]`:
/// secrets and endpoint choices are never generated). `save_config` does not
/// use this; it merges into the user's file instead.
#[cfg_attr(not(test), allow(dead_code))] // used by the config audit and tests, not by the app
pub fn config_to_toml(config: &Config) -> String {
    apply_edits("", &managed_edits(config, None))
}

/// Diff `config` against what `existing` already yields and return the new
/// file text, or `None` when nothing needs to change.
pub fn compute_saved(existing: Option<&str>, config: &Config) -> Option<String> {
    let base = match existing {
        Some(text) => parse_toml_config(text),
        None => Config::default(),
    };
    let edits = managed_edits(config, Some(&base));
    if edits.is_empty() {
        return None;
    }
    Some(apply_edits(existing.unwrap_or(""), &edits))
}

fn apply_edits(text: &str, edits: &[Edit]) -> String {
    let mut text = text.to_string();
    for e in edits {
        text = set_key(&text, e.section, e.key, &e.literal);
    }
    text
}

/// The managed settings of `config`; with a `base`, only those that differ.
fn managed_edits(config: &Config, base: Option<&Config>) -> Vec<Edit> {
    let mut edits: Vec<Edit> = Vec::new();
    let mut push = |section, key, differs: bool, literal: String| {
        if differs || base.is_none() {
            edits.push(Edit { section, key, literal });
        }
    };
    let b = base.unwrap_or(config);

    let fs = clamp_font_size(config.font_size);
    push("general", "font_size", (fs - b.font_size).abs() >= 0.05, fmt_f(fs, 1));
    let (cols, rows) = (clamp_cols(config.cols as i64), clamp_rows(config.rows as i64));
    push("general", "cols", cols != b.cols, cols.to_string());
    push("general", "rows", rows != b.rows, rows.to_string());
    let op = clamp_opacity(config.opacity);
    push("general", "opacity", (op - b.opacity).abs() >= 0.005, fmt_f(op, 2));
    push("general", "theme", config.theme_name != b.theme_name, quote(&config.theme_name));
    if let Some(f) = &config.font_family {
        push("general", "font_family", config.font_family != b.font_family, quote(f));
    }
    if let Some(f) = &config.font_path {
        push("general", "font_path", config.font_path != b.font_path, quote(f));
    }
    push("general", "effect", config.effect != b.effect, quote(config.effect.map_or("none", |k| k.name())));
    let ei = config.effect_intensity.clamp(0.0, 1.0);
    push("general", "effect_intensity", (ei - b.effect_intensity).abs() >= 0.005, fmt_f(ei, 2));
    push("general", "startup_animation", config.startup_animation != b.startup_animation, config.startup_animation.to_string());
    push("ai", "auto_fix", config.ai_auto_fix != b.ai_auto_fix, config.ai_auto_fix.to_string());
    push("ai", "nl_hash", config.ai_nl_hash != b.ai_nl_hash, config.ai_nl_hash.to_string());
    if config.ai_consent != Consent::Unset {
        push("ai", "consent", config.ai_consent != b.ai_consent, quote(config.ai_consent.as_str()));
    }
    for (key, cur, old) in [
        ("fix_provider", config.ai_routing.fix, b.ai_routing.fix),
        ("nl_provider", config.ai_routing.nl, b.ai_routing.nl),
        ("chat_provider", config.ai_routing.chat, b.ai_routing.chat),
    ] {
        if let Some(r) = cur {
            push("ai", key, cur != old, quote(r.as_str()));
        }
    }
    if config.llm_persist {
        // Endpoint choice only: `api_key` is never generated. Provider is
        // always written with the section, since it is what makes `[llm]` an
        // explicit opt-in on the next load.
        let fresh = !b.llm_explicit;
        push("llm", "provider", fresh || config.llm.provider != b.llm.provider, quote(&config.llm.provider));
        push("llm", "model", fresh || config.llm.model != b.llm.model, quote(&config.llm.model));
        push("llm", "api_url", fresh || config.llm.api_url != b.llm.api_url, quote(&config.llm.api_url));
    }
    push("security", "osc52", config.osc52 != b.osc52, quote(config.osc52.as_str()));
    edits
}

/// Keys the loader also accepts at the top level of the file.
fn accepts_top_level(key: &str) -> bool {
    matches!(key, "effect" | "effect_intensity" | "startup_animation")
}

/// Byte index of the `#` that starts a trailing comment (outside any string).
fn comment_start(line: &str) -> Option<usize> {
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (i, c) in line.char_indices() {
        match quote {
            Some(q) => {
                if escaped { escaped = false; }
                else if c == '\\' && q == '"' { escaped = true; }
                else if c == q { quote = None; }
            }
            None => match c {
                '"' | '\'' => quote = Some(c),
                '#' => return Some(i),
                _ => {}
            },
        }
    }
    None
}

fn header_name(line: &str) -> Option<String> {
    let l = line.trim_start_matches('\u{feff}');
    let l = match comment_start(l) { Some(i) => &l[..i], None => l }.trim();
    if l.starts_with('[') && l.ends_with(']') {
        Some(l.trim_matches(|c| c == '[' || c == ']').trim().to_string())
    } else {
        None
    }
}

fn line_key(line: &str) -> Option<&str> {
    let l = line.trim_start_matches('\u{feff}').trim_start();
    if l.starts_with('#') || l.starts_with('[') { return None; }
    let (k, _) = l.split_once('=')?;
    Some(k.trim())
}

/// Line range `[start, end)` of the *body* of `section` (first occurrence;
/// `""` = the top level before any header), and the line of `key` within it.
fn locate(lines: &[String], section: &str, key: &str) -> Option<(usize, usize, Option<usize>)> {
    let (start, mut cur_ok) = if section.is_empty() { (0, true) } else { (usize::MAX, false) };
    let mut start = start;
    let mut end = lines.len();
    let mut hit = None;
    for (i, l) in lines.iter().enumerate() {
        if let Some(h) = header_name(l) {
            if cur_ok {
                end = i;
                break;
            }
            if start == usize::MAX && h == section {
                start = i + 1;
                cur_ok = true;
            }
            continue;
        }
        if cur_ok && hit.is_none() && line_key(l) == Some(key) {
            hit = Some(i);
        }
    }
    if start == usize::MAX { None } else { Some((start, end, hit)) }
}

/// Set `[section] key = literal` in `text`, editing the line in place when the
/// key exists (keeping indentation and the trailing comment) and inserting it
/// at the end of its section otherwise.
fn set_key(text: &str, section: &str, key: &str, literal: &str) -> String {
    let nl = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut lines: Vec<String> = text.lines().map(String::from).collect();

    let mut loc = locate(&lines, section, key);
    if loc.as_ref().is_none_or(|l| l.2.is_none()) && accepts_top_level(key) {
        if let Some(top) = locate(&lines, "", key) {
            if top.2.is_some() { loc = Some(top); }
        }
    }

    match loc {
        Some((_, _, Some(i))) => {
            let old = lines[i].clone();
            let bom = if old.starts_with('\u{feff}') { "\u{feff}" } else { "" };
            let body = old.trim_start_matches('\u{feff}');
            let indent = &body[..body.len() - body.trim_start().len()];
            let trailing = match comment_start(body) {
                Some(ci) => {
                    let ws = body[..ci].len() - body[..ci].trim_end().len();
                    body[ci - ws..].to_string()
                }
                None => String::new(),
            };
            lines[i] = format!("{bom}{indent}{key} = {literal}{trailing}");
        }
        Some((start, end, None)) => {
            // After the last key/value line of the section (or its header).
            let mut at = start;
            for j in start..end {
                if line_key(&lines[j]).is_some() { at = j + 1; }
            }
            lines.insert(at, format!("{key} = {literal}"));
        }
        None => {
            if lines.last().is_some_and(|l| !l.trim().is_empty()) { lines.push(String::new()); }
            lines.push(format!("[{section}]"));
            lines.push(format!("{key} = {literal}"));
        }
    }
    let mut out = lines.join(nl);
    out.push_str(nl);
    out
}

pub fn load_config() -> Config {
    let path = config_path();
    if let Ok(content) = std::fs::read_to_string(&path) {
        log::info!("Loaded config: {}", path.display());
        parse_toml_config(&content)
    } else {
        let mut c = Config::default();
        finish_llm(&mut c);
        c
    }
}

pub fn config_path() -> PathBuf {
    if let Some(p) = CONFIG_PATH_OVERRIDE.get() {
        return p.clone();
    }
    default_config_path()
}

fn default_config_path() -> PathBuf {
    if let Some(home) = dirs::home_dir() {
        // Primary: ~/.config/rift/config.toml
        let rift = home.join(".config").join("rift").join("config.toml");
        if rift.exists() { return rift; }
        // Fallback: ~/.config/rterm/config.toml (backward compat)
        let rterm = home.join(".config").join("rterm").join("config.toml");
        if rterm.exists() { return rterm; }
        return rift;
    }
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("rift")
        .join("config.toml")
}

/// Resolve the LLM section after parsing: an environment API key never
/// enables AI by itself; it is only used once `[llm]` is configured or the
/// user consented to cloud AI.
fn finish_llm(c: &mut Config) {
    if !c.llm_explicit {
        match c.ai_consent {
            Consent::Cloud => {
                if let Some(env) = crate::ai::consent::env_candidate() {
                    c.llm.provider = env.provider.into();
                    c.llm.api_url = env.api_url.into();
                    c.llm.model = env.model.into();
                    c.llm.api_key = Some(env.key);
                    c.llm.enabled = true;
                }
            }
            Consent::Local => {
                c.llm = crate::ai::LlmConfig::default();
                c.llm.enabled = true;
            }
            _ => {}
        }
    }
    c.llm.resolve_api_key();
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
        config.font_size = clamp_font_size(v);
    }
    if let Some(v) = get_int(&map, "general", "cols") {
        config.cols = clamp_cols(v);
    }
    if let Some(v) = get_int(&map, "general", "rows") {
        config.rows = clamp_rows(v);
    }
    if let Some(v) = get_float(&map, "general", "opacity") {
        config.opacity = clamp_opacity(v);
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
    config.input = parse_input_config(&map);
    if let Some(v) = get_int(&map, "general", "startup_animation").or_else(|| get_int(&map, "", "startup_animation")) {
        config.startup_animation = v != 0;
    }
    if let Some(v) = get_int(&map, "general", "scrollback_lines").or_else(|| get_int(&map, "", "scrollback_lines")) {
        config.scrollback_lines = (v.max(0) as usize).min(1_000_000);
    }
    if let Some(v) = get_float(&map, "general", "notify_after_secs").or_else(|| get_float(&map, "", "notify_after_secs")) {
        config.notify_after_secs = (v as f64).clamp(0.0, 86_400.0);
    }
    if let Some(v) = get_int(&map, "general", "bold_is_bright").or_else(|| get_int(&map, "", "bold_is_bright")) {
        config.bold_is_bright = v != 0;
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

    // AI consent + toggles. Auto-fix stays off until the user has consented.
    if let Some(v) = get_str(&map, "ai", "consent") {
        match Consent::parse(&v) {
            Some(c) => config.ai_consent = c,
            None => log::warn!("Unknown [ai] consent '{v}' (valid: cloud, local, declined)"),
        }
    }
    for (key, slot) in [
        ("fix_provider", &mut config.ai_routing.fix),
        ("nl_provider", &mut config.ai_routing.nl),
        ("chat_provider", &mut config.ai_routing.chat),
    ] {
        if let Some(v) = get_str(&map, "ai", key) {
            match crate::ai::local::Route::parse(&v) {
                Some(r) => *slot = Some(r),
                None => log::warn!("Unknown [ai] {key} '{v}' (valid: local, cloud, default)"),
            }
        }
    }
    config.ai_auto_fix = matches!(config.ai_consent, Consent::Cloud | Consent::Local);
    if let Some(v) = get_int(&map, "ai", "auto_fix") {
        config.ai_auto_fix = v != 0;
    }
    if let Some(v) = get_int(&map, "ai", "nl_hash") {
        config.ai_nl_hash = v != 0;
    }

    if let Some(v) = get_str(&map, "security", "osc52") {
        match Osc52Policy::parse(&v) {
            Some(p) => config.osc52 = p,
            None => log::warn!("Unknown [security] osc52 '{v}' (valid: write-only, allow, deny)"),
        }
    }

    // [mcp]: enabled (read tools) and allow_run = "ask" | "never".
    if let Some(v) = get_int(&map, "mcp", "enabled") {
        config.mcp.enabled = v != 0;
    }
    if let Some(v) = get_str(&map, "mcp", "allow_run") {
        match crate::mcp::AllowRun::parse(&v) {
            Some(a) => config.mcp.allow_run = a,
            None => log::warn!("Unknown [mcp] allow_run '{v}' (valid: ask, never)"),
        }
    }

    // LLM config: a `[llm]` section with a provider is an explicit opt-in.
    if let Some(v) = get_str(&map, "llm", "provider") {
        config.llm.provider = v;
        config.llm.enabled = true;
        config.llm_explicit = true;
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

    finish_llm(&mut config);
    config.cloud_opt_in = config.ai_consent == Consent::Cloud
        || (config.llm_explicit && !crate::ai::local::routing::is_local(&config.llm));

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

/// `shift_enter` / `option_as_meta` (in `[input]`, `[general]` or top level) and
/// the `[keybindings]` section (action = "chord" or ["chord", ...]).
fn parse_input_config(map: &TomlMap) -> crate::input::InputConfig {
    let mut ic = crate::input::InputConfig::default();
    let find = |key: &str| ["input", "general", ""].iter().find_map(|sec| get_str(map, sec, key));
    if let Some(v) = find("shift_enter") {
        match crate::input::ShiftEnter::parse(&v) {
            Some(m) => ic.shift_enter = m,
            None => log::warn!("Unknown shift_enter '{v}' (esc-cr | csi-u | lf)"),
        }
    }
    if let Some(v) = find("option_as_meta") {
        match crate::input::OptionAsMeta::parse(&v) {
            Some(m) => ic.option_as_meta = m,
            None => log::warn!("Unknown option_as_meta '{v}' (left | right | both | none)"),
        }
    }
    if let Some(sec) = map.get("keybindings") {
        let mut entries: Vec<_> = sec.iter().collect();
        entries.sort_by(|a, b| a.0.cmp(b.0));
        for (action, val) in entries {
            let chords = match val {
                TomlValue::Str(s) => vec![s.clone()],
                TomlValue::Array(items) => items
                    .iter()
                    .filter_map(|v| if let TomlValue::Str(s) = v { Some(s.clone()) } else { None })
                    .collect(),
                _ => continue,
            };
            ic.keybindings.push((action.trim_matches('"').to_string(), chords));
        }
    }
    ic
}

fn parse_toml(content: &str) -> TomlMap {
    let mut map = TomlMap::new();
    let mut section = String::new();
    for line in content.trim_start_matches('\u{feff}').lines() {
        let line = match comment_start(line) { Some(i) => &line[..i], None => line }.trim();
        if line.is_empty() { continue; }
        if line.starts_with('[') && line.ends_with(']') {
            section = line[1..line.len() - 1].trim().to_string();
            continue;
        }
        if let Some((key, val)) = line.split_once('=') {
            let mut key = key.trim().to_string();
            let mut sec = section.clone();
            // `general.font_size = 22` dotted keys.
            if sec.is_empty() {
                if let Some((s, k)) = key.split_once('.') {
                    sec = s.trim().to_string();
                    key = k.trim().to_string();
                }
            }
            let parsed = parse_value(val.trim());
            map.entry(sec).or_default().insert(key, parsed);
        }
    }
    map
}

fn unescape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c != '\\' { o.push(c); continue; }
        match it.next() {
            Some('n') => o.push('\n'),
            Some('t') => o.push('\t'),
            Some('r') => o.push('\r'),
            Some('"') => o.push('"'),
            Some('\\') => o.push('\\'),
            Some(other) => { o.push('\\'); o.push(other); }
            None => o.push('\\'),
        }
    }
    o
}

fn parse_value(s: &str) -> TomlValue {
    let s = s.trim();
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        return TomlValue::Str(unescape(&s[1..s.len() - 1]));
    }
    if s.len() >= 2 && s.starts_with('\'') && s.ends_with('\'') {
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
            let r = match &arr[0] { TomlValue::Int(v) if (0..=255).contains(v) => *v as u8, _ => return None };
            let g = match &arr[1] { TomlValue::Int(v) if (0..=255).contains(v) => *v as u8, _ => return None };
            let b = match &arr[2] { TomlValue::Int(v) if (0..=255).contains(v) => *v as u8, _ => return None };
            Some((r, g, b))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn notify_after_secs_parses() {
        assert_eq!(parse_toml_config("").notify_after_secs, 10.0);
        assert_eq!(parse_toml_config("notify_after_secs = 30").notify_after_secs, 30.0);
        assert_eq!(parse_toml_config("[general]\nnotify_after_secs = 2.5").notify_after_secs, 2.5);
        assert_eq!(parse_toml_config("notify_after_secs = -4").notify_after_secs, 0.0);
    }

    use super::*;

    #[test]
    fn mcp_section_defaults_and_parses() {
        use crate::mcp::AllowRun;
        let c = parse_toml_config("");
        assert!(c.mcp.enabled);
        assert_eq!(c.mcp.allow_run, AllowRun::Ask);
        let c = parse_toml_config("[mcp]\nenabled = false\nallow_run = \"never\"\n");
        assert!(!c.mcp.enabled);
        assert_eq!(c.mcp.allow_run, AllowRun::Never);
        let c = parse_toml_config("[mcp]\nallow_run = \"bogus\"\n");
        assert_eq!(c.mcp.allow_run, AllowRun::Ask, "unknown values keep the safe default");
    }

    #[test]
    fn ai_toggles_default_on_and_parse() {
        let c = parse_toml_config("[general]\nfont_size = 14.0\n");
        // Auto-fix is off until the user has consented to AI.
        assert!(!c.ai_auto_fix && c.ai_nl_hash);
        let c = parse_toml_config("[ai]\nauto_fix = false\nnl_hash = true\n");
        assert!(!c.ai_auto_fix && c.ai_nl_hash);
        let c = parse_toml_config("[ai]\nauto_fix = true\n");
        assert!(c.ai_auto_fix);
        let c = parse_toml_config("[ai]\nconsent = \"local\"\n");
        assert!(c.ai_auto_fix && c.llm.enabled, "consent=local turns on auto-fix and local AI");
        let c = parse_toml_config("[ai]\nconsent = \"declined\"\n");
        assert!(!c.ai_auto_fix && !c.llm.enabled);
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

    const SAMPLE: &str = "# my rift config\n# keep this comment\n[general]\nfont_size = 17.5 # big\ntheme = \"nord\"\n\n[custom]\nfoo = 1\n\n[llm]\nprovider = \"openai\"\nmodel = \"deepseek-chat\"\napi_url = \"https://api.deepseek.com\"\napi_key = \"sk-secret-in-file\"\n";

    #[test]
    fn save_is_a_noop_when_nothing_changed() {
        let c = parse_toml_config(SAMPLE);
        assert!(compute_saved(Some(SAMPLE), &c).is_none());
    }

    #[test]
    fn save_merges_only_changed_keys_and_keeps_everything_else() {
        let mut c = parse_toml_config(SAMPLE);
        c.font_size = 20.0;
        c.ai_nl_hash = false;
        let out = compute_saved(Some(SAMPLE), &c).unwrap();
        assert!(out.contains("font_size = 20.0 # big"), "{out}");
        assert!(out.contains("# keep this comment") && out.contains("[custom]\nfoo = 1"));
        assert!(out.contains("api_key = \"sk-secret-in-file\""));
        assert!(out.contains("[ai]\nnl_hash = false"), "{out}");
        assert!(!out.contains("cols") && !out.contains("opacity"), "unchanged keys are not added:\n{out}");
        let back = parse_toml_config(&out);
        assert_eq!(back.font_size, 20.0);
        assert!(!back.ai_nl_hash);
    }

    #[test]
    fn save_inserts_into_existing_section_and_keeps_crlf() {
        let src = "[general]\r\ntheme = \"nord\"\r\n\r\n[llm]\r\nprovider = \"openai\"\r\n";
        let mut c = parse_toml_config(src);
        c.opacity = 0.5;
        let out = compute_saved(Some(src), &c).unwrap();
        assert_eq!(out, "[general]\r\ntheme = \"nord\"\r\nopacity = 0.50\r\n\r\n[llm]\r\nprovider = \"openai\"\r\n");
    }

    #[test]
    fn save_never_writes_env_derived_api_keys() {
        let mut c = Config::default();
        c.llm.api_key = Some("sk-from-env".into());
        c.llm.enabled = true;
        c.font_size = 18.0;
        let out = compute_saved(None, &c).unwrap();
        assert!(!out.contains("sk-from-env") && !out.contains("[llm]"), "{out}");
        assert!(out.contains("font_size = 18.0"));
    }

    #[test]
    fn save_clamps_hostile_values_and_edits_top_level_effect_keys() {
        let src = "effect = \"crt\"\n";
        let mut c = parse_toml_config(src);
        c.effect = None;
        c.font_size = 5000.0;
        let out = compute_saved(Some(src), &c).unwrap();
        assert!(out.starts_with("effect = \"none\"\n"), "{out}");
        assert!(out.contains("font_size = 72.0"), "{out}");
    }

    #[test]
    fn consent_and_osc52_round_trip() {
        let mut c = Config::default();
        c.ai_consent = Consent::Declined;
        c.osc52 = Osc52Policy::Deny;
        let out = compute_saved(None, &c).unwrap();
        assert!(out.contains("consent = \"declined\"") && out.contains("osc52 = \"deny\""), "{out}");
        let back = parse_toml_config(&out);
        assert_eq!(back.ai_consent, Consent::Declined);
        assert_eq!(back.osc52, Osc52Policy::Deny);
    }

    #[test]
    fn values_are_sanitised_on_load() {
        let c = parse_toml_config("[general]\nfont_size = 0\ncols = 70000\nrows = -1\nopacity = 99\n");
        assert_eq!((c.font_size, c.cols, c.rows, c.opacity), (6.0, 1000, 10, 1.0));
        let c = parse_toml_config("[general]\nfont_size = 500\nopacity = 0.01\n");
        assert_eq!((c.font_size, c.opacity), (72.0, 0.2));
    }

    #[test]
    fn parser_handles_comments_bom_and_quotes() {
        let c = parse_toml_config("\u{feff}[general]\nfont_size = 22 # bigger\ntheme = 'nord' # dark\nfont_path = \"/tmp/a=b#c.ttf\"\n");
        assert_eq!(c.font_size, 22.0);
        assert_eq!(c.theme_name, "nord");
        assert_eq!(c.font_path.as_deref(), Some("/tmp/a=b#c.ttf"));
    }

    #[test]
    fn local_model_choice_persists_without_api_key() {
        use crate::ai::local::Route;
        let mut c = Config::default();
        c.ai_consent = Consent::Local;
        c.ai_auto_fix = true; // what consent=local implies on load
        c.theme_name = crate::config::LEGACY_THEME.to_string(); // what a file without `theme` loads as
        c.llm = crate::ai::LlmConfig {
            provider: "openai-compatible-local".into(),
            model: "qwen2.5-coder-7b".into(),
            api_url: "http://127.0.0.1:1234".into(),
            api_key: Some("sk-secret".into()),
            enabled: true,
        };
        c.llm_persist = true;
        c.ai_routing.fix = Some(Route::Local);
        c.ai_routing.chat = Some(Route::Default);
        let out = compute_saved(None, &c).unwrap();
        assert!(out.contains("consent = \"local\"") && out.contains("fix_provider = \"local\"") && out.contains("chat_provider = \"default\""), "{out}");
        assert!(out.contains("provider = \"openai-compatible-local\"") && out.contains("model = \"qwen2.5-coder-7b\"") && out.contains("api_url = \"http://127.0.0.1:1234\""), "{out}");
        assert!(!out.contains("sk-secret") && !out.contains("api_key"), "the key is never written: {out}");

        let back = parse_toml_config(&out);
        assert!(back.llm_explicit && back.llm.enabled);
        assert_eq!((back.llm.provider.as_str(), back.llm.model.as_str()), ("openai-compatible-local", "qwen2.5-coder-7b"));
        assert_eq!((back.ai_consent, back.ai_routing.fix, back.ai_routing.nl, back.ai_routing.chat), (Consent::Local, Some(Route::Local), None, Some(Route::Default)));
        assert!(!back.cloud_opt_in, "a local-only setup must not count as a cloud opt-in");
        assert!(compute_saved(Some(&out), &c).is_none(), "second save is a no-op");
    }

    #[test]
    fn switching_models_keeps_the_users_api_key_line() {
        let existing = "[llm]\nprovider = \"openai\"\nmodel = \"gpt-4o\"\napi_url = \"https://api.openai.com\"\napi_key = \"sk-file\"\n";
        let mut c = parse_toml_config(existing);
        assert!(c.cloud_opt_in);
        c.llm.provider = "ollama".into();
        c.llm.model = "llama3.2".into();
        c.llm.api_url = "http://localhost:11434".into();
        c.llm_persist = true;
        let out = compute_saved(Some(existing), &c).unwrap();
        assert!(out.contains("provider = \"ollama\"") && out.contains("model = \"llama3.2\"") && out.contains("api_key = \"sk-file\""), "{out}");
        assert!(!out.contains("gpt-4o"));
    }

    #[test]
    fn unknown_provider_route_is_ignored() {
        let c = parse_toml_config("[ai]\nfix_provider = \"banana\"\nnl_provider = \"CLOUD\"\n");
        assert_eq!((c.ai_routing.fix, c.ai_routing.nl), (None, Some(crate::ai::local::Route::Cloud)));
    }

    #[test]
    fn env_key_alone_leaves_ai_disabled() {
        let mut l = crate::ai::LlmConfig::default();
        // resolve_api_key may fill a key but must never flip `enabled`.
        l.resolve_api_key();
        assert!(!l.enabled);
    }
}
