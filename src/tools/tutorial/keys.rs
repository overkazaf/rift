//! Key references used by tutorial captions, key overlays and panel hints.
//!
//! Demos never spell out a shortcut. They name it, and playback resolves the
//! name against the keymap the user actually has (defaults plus
//! `[keybindings]` overrides), so a caption always shows the chord that
//! works on this machine:
//!
//! * `split_right` — an action of [`crate::app::keymap::ACTIONS`]
//! * `@ask_inline` — a chord owned by another module (listed in
//!   [`FIXED`], each verified against the code that handles it)
//! * `=Tab` — a plain key inside an overlay (allow-listed in [`LITERALS`])

use crate::app::keymap::{ActionDef, Keymap, ACTIONS};

/// Chords handled outside the keymap (see `keymap::FIXED` for the docs).
pub struct Fixed {
    pub id: &'static str,
    pub mac: &'static str,
    pub other: &'static str,
    /// Chords (keymap syntax) that must parse and must not be bound to a
    /// keymap action - verified by tests.
    pub chords: &'static [&'static str],
}

/// Each entry mirrors a handler:
/// * `ask_inline`: `ai::inline::on_key` (Super+K; `ASK_KEY` in `ai/inline/ui.rs`)
/// * `block_jump`, `block_copy`: `blocks_ui::on_key` (Super+Shift+Up/Down/C)
/// * `pane_focus`, `pane_zoom`, `pane_close`, `pane_next`: `app::panes::shortcut_cmd`
pub const FIXED: &[Fixed] = &[
    Fixed { id: "ask_inline", mac: "Cmd+K", other: "Super+K", chords: &["cmd+k"] },
    Fixed { id: "block_jump", mac: "Cmd+Shift+Up/Down", other: "Super+Shift+Up/Down", chords: &["cmd+shift+up", "cmd+shift+down"] },
    Fixed { id: "block_copy", mac: "Cmd+Shift+C", other: "Super+Shift+C", chords: &["cmd+shift+c"] },
    Fixed { id: "pane_focus", mac: "Cmd+Opt+Arrows", other: "Alt+Arrows", chords: &["cmd+alt+left", "cmd+alt+right", "cmd+alt+up", "cmd+alt+down"] },
    Fixed { id: "pane_zoom", mac: "Cmd+Shift+Enter", other: "Super+Shift+Enter", chords: &["cmd+shift+enter"] },
    Fixed { id: "pane_close", mac: "Cmd+W", other: "Super+W", chords: &["cmd+w"] },
    Fixed { id: "pane_next", mac: "Cmd+]", other: "Super+]", chords: &["cmd+]"] },
];

/// Plain keys a demo may show (keys typed inside an overlay, not global
/// shortcuts). Modifier chords are not allowed here on purpose.
pub const LITERALS: &[&str] = &[
    "Enter", "Tab", "Esc", "Space", "Up", "Down", "Left", "Right", "Shift+Left", "Shift+Right",
    "Y", "N", "1", "2", "3", "1-3", "r", "v", "p", "w", "#", "Click", "Hover", "Type",
];

pub enum KeyRef {
    Action(&'static ActionDef),
    Fixed(&'static Fixed),
    Literal(&'static str),
}

pub fn lookup(r: &str) -> Result<KeyRef, String> {
    if let Some(id) = r.strip_prefix('@') {
        return FIXED.iter().find(|f| f.id == id).map(KeyRef::Fixed).ok_or_else(|| format!("unknown fixed chord '@{id}'"));
    }
    if let Some(k) = r.strip_prefix('=') {
        return LITERALS.iter().find(|l| **l == k).map(|l| KeyRef::Literal(l)).ok_or_else(|| format!("key '{k}' is not an allowed literal"));
    }
    ACTIONS.iter().find(|d| d.name == r).map(KeyRef::Action).ok_or_else(|| format!("unknown keymap action '{r}'"))
}

/// Text for a key reference on this platform with this keymap. Unknown
/// references render as `?name` (tests keep bundled demos free of them).
pub fn display(r: &str, km: &Keymap) -> String {
    match lookup(r) {
        Ok(KeyRef::Action(def)) => km.primary(def.action).unwrap_or_else(|| format!("({} unbound)", def.desc)),
        Ok(KeyRef::Fixed(f)) => (if cfg!(target_os = "macos") { f.mac } else { f.other }).to_string(),
        Ok(KeyRef::Literal(l)) => l.to_string(),
        Err(_) => format!("?{r}"),
    }
}

/// A caption split into text runs and key chips.
#[derive(Clone, Debug, PartialEq)]
pub enum Seg {
    Text(String),
    Key(String),
}

pub fn segments(text: &str, km: &Keymap) -> Vec<Seg> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(i) = rest.find("{key:") {
        if i > 0 {
            out.push(Seg::Text(rest[..i].to_string()));
        }
        let after = &rest[i + 5..];
        match after.find('}') {
            Some(j) => {
                out.push(Seg::Key(display(&after[..j], km)));
                rest = &after[j + 1..];
            }
            None => {
                out.push(Seg::Text(rest[i..].to_string()));
                rest = "";
            }
        }
    }
    if !rest.is_empty() {
        out.push(Seg::Text(rest.to_string()));
    }
    out
}

/// Placeholders replaced by plain chord text (panel lines).
pub fn expand(text: &str, km: &Keymap) -> String {
    segments(text, km)
        .into_iter()
        .map(|s| match s {
            Seg::Text(t) | Seg::Key(t) => t,
        })
        .collect()
}
