//! Rift's own keybindings: the single built-in default table, the chord
//! parser/matcher, and user overrides from the `[keybindings]` config section.
//!
//! Design rules (see docs in `--list-keybindings`):
//!  * macOS: Rift chords use Cmd (`cmd` / `mod`); Cmd chords never reach a program.
//!  * Linux/Windows: `mod` is Ctrl and Rift chords are Ctrl+Shift+<key>, so plain
//!    Ctrl/Alt chords belong to the shell and editors.
//!  * Every chord containing Ctrl or Alt (without Cmd/Super) is passed through to the
//!    foreground program while it owns the keyboard (alternate screen, kitty keyboard
//!    protocol enabled, or mouse reporting on) - except the `Essential` actions.

use std::collections::HashMap;

use winit::event::KeyEvent;
use winit::keyboard::{Key, ModifiersState, NamedKey};

/// Whether an action keeps working while a program owns the keyboard.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Guard {
    /// Clipboard / tab management: always handled by Rift.
    Essential,
    /// Suppressed (key passes through) while a full-screen program owns the keys.
    Passthrough,
}

macro_rules! actions {
    ($( $variant:ident, $name:literal, $desc:literal, $guard:ident, [$($mac:literal),*], [$($other:literal),*]; )*) => {
        #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
        pub enum Action { $($variant),* }

        pub const ACTIONS: &[ActionDef] = &[
            $( ActionDef {
                action: Action::$variant,
                name: $name,
                desc: $desc,
                guard: Guard::$guard,
                mac: &[$($mac),*],
                other: &[$($other),*],
            } ),*
        ];
    };
}

pub struct ActionDef {
    pub action: Action,
    pub name: &'static str,
    pub desc: &'static str,
    pub guard: Guard,
    pub mac: &'static [&'static str],
    pub other: &'static [&'static str],
}

impl ActionDef {
    pub fn defaults(&self) -> &'static [&'static str] {
        if cfg!(target_os = "macos") { self.mac } else { self.other }
    }
}

// name, description, guard, [macOS chords], [other chords]
// `mod` = Cmd on macOS, Ctrl elsewhere; `cmd` = Cmd / Super.
actions! {
    Copy,           "copy",            "Copy selection",                     Essential,   ["cmd+c"], ["ctrl+shift+c", "ctrl+insert"];
    Paste,          "paste",           "Paste clipboard",                    Essential,   ["cmd+v"], ["ctrl+shift+v", "shift+insert"];
    SelectAll,      "select_all",      "Select all",                         Passthrough, ["cmd+a"], ["super+a"];
    SplitRight,     "split_right",     "Split pane left/right",              Passthrough, ["cmd+d"], ["super+d"];
    SplitDown,      "split_down",      "Split pane top/bottom",              Passthrough, ["mod+shift+d", "mod+shift+-"], ["ctrl+shift+d"];
    Search,         "search",          "Search scrollback",                  Passthrough, ["cmd+f", "mod+shift+f"], ["ctrl+shift+f", "super+f"];
    CommandPalette, "command_palette", "Command palette",                    Passthrough, ["cmd+p"], ["super+p"];
    HistorySearch,  "history_search",  "Smart history search",               Passthrough, ["cmd+y"], ["ctrl+alt+r"];
    Autocomplete,   "autocomplete",    "Autocomplete suggestions",           Passthrough, ["cmd+."], ["ctrl+shift+space"];
    NewTab,         "new_tab",         "New tab",                            Essential,   ["mod+shift+t"], ["ctrl+shift+t"];
    CloseTab,       "close_tab",       "Close tab",                          Essential,   ["mod+shift+w"], ["ctrl+shift+w"];
    PrevTab,        "prev_tab",        "Previous tab",                       Essential,   ["mod+shift+[", "ctrl+shift+tab"], ["ctrl+shift+[", "ctrl+shift+tab"];
    NextTab,        "next_tab",        "Next tab",                           Essential,   ["mod+shift+]", "ctrl+tab"], ["ctrl+shift+]", "ctrl+tab"];
    Preferences,    "preferences",     "Preferences",                        Passthrough, ["mod+shift+,"], ["ctrl+shift+,"];
    Welcome,        "welcome",         "Welcome guide / shortcut help",      Passthrough, ["mod+shift+/"], ["ctrl+shift+/"];
    Recording,      "recording",       "Toggle session recording",           Passthrough, ["mod+shift+r"], ["ctrl+shift+r"];
    TimeWarp,       "time_warp",       "Time warp (history replay)",         Passthrough, ["mod+shift+z"], ["ctrl+shift+z"];
    Hud,            "hud",             "System HUD",                         Passthrough, ["mod+shift+h"], ["ctrl+shift+h"];
    Ssh,            "ssh",             "SSH connect",                        Passthrough, ["mod+shift+s"], ["ctrl+shift+s"];
    AiAssistant,    "ai_assistant",    "AI assistant panel",                 Passthrough, ["mod+shift+a"], ["ctrl+shift+a"];
    Browser,        "browser",         "Embedded browser",                   Passthrough, ["mod+shift+b"], ["ctrl+shift+b"];
    GitPanel,       "git_panel",       "Git panel",                          Passthrough, ["mod+shift+g"], ["ctrl+shift+g"];
    CompareOutput,  "compare_output",  "Compare pane output",                Passthrough, ["mod+shift+k"], ["ctrl+shift+k"];
    FileManager,    "file_manager",    "File manager",                       Passthrough, ["mod+shift+e"], ["ctrl+shift+e"];
    Cicd,           "cicd",            "CI/CD panel",                        Passthrough, ["mod+shift+i"], ["ctrl+shift+i"];
    Teaching,       "teaching",        "Teaching mode",                      Passthrough, ["mod+shift+l"], ["ctrl+shift+l"];
    Heatmap,        "heatmap",         "Command heatmap",                    Passthrough, ["mod+shift+y"], ["ctrl+shift+y"];
    Docker,         "docker",          "Docker panel",                       Passthrough, ["mod+shift+o"], ["ctrl+shift+o"];
    Regex,          "regex",           "Regex playground",                   Passthrough, ["mod+shift+x"], ["ctrl+shift+x"];
    SecretMask,     "secret_mask",     "Secret masking",                     Passthrough, ["mod+shift+m"], ["ctrl+shift+m"];
    AuditLog,       "audit_log",       "Audit log",                          Passthrough, ["mod+shift+u"], ["ctrl+shift+u"];
    Broadcast,      "broadcast",       "Broadcast input to all panes",       Passthrough, ["mod+shift+p"], ["ctrl+shift+p"];
    Observer,       "observer",        "Observer summary",                   Passthrough, ["mod+shift+n"], ["ctrl+shift+n"];
    ZoomIn,         "zoom_in",         "Font zoom in",                       Passthrough, ["cmd+=", "cmd+shift+="], ["ctrl+="];
    ZoomOut,        "zoom_out",        "Font zoom out",                      Passthrough, ["cmd+-"], ["ctrl+-"];
    ZoomReset,      "zoom_reset",      "Reset font zoom",                    Passthrough, ["cmd+0"], ["ctrl+0"];
    EffectCrt,      "effect_crt",      "Effect: CRT",                        Passthrough, ["ctrl+shift+1"], ["ctrl+shift+1"];
    EffectGlitch,   "effect_glitch",   "Effect: Glitch",                     Passthrough, ["ctrl+shift+2"], ["ctrl+shift+2"];
    EffectNeon,     "effect_neon",     "Effect: Neon",                       Passthrough, ["ctrl+shift+3"], ["ctrl+shift+3"];
    EffectMatrix,   "effect_matrix",   "Effect: Matrix",                     Passthrough, ["ctrl+shift+4"], ["ctrl+shift+4"];
    EffectAmber,    "effect_amber",    "Effect: Amber",                      Passthrough, ["ctrl+shift+5"], ["ctrl+shift+5"];
    EffectHologram, "effect_hologram", "Effect: Hologram",                   Passthrough, ["ctrl+shift+6"], ["ctrl+shift+6"];
    EffectOff,      "effect_off",      "Effect: off",                        Passthrough, ["ctrl+shift+0"], ["ctrl+shift+0"];
    EffectIntensityUp,   "effect_intensity_up",   "Effect intensity +",      Passthrough, ["ctrl+shift+="], ["ctrl+shift+="];
    EffectIntensityDown, "effect_intensity_down", "Effect intensity -",      Passthrough, ["ctrl+shift+-"], ["ctrl+shift+-"];
}

/// Chords owned by other modules; listed for documentation only (not rebindable).
pub const FIXED: &[(&str, &str, &str)] = &[
    ("Quit", "Cmd+Q", "Ctrl+Shift+Q (window close)"),
    ("Ask AI about this (inline)", "Cmd+K", "-"),
    ("Command blocks: jump / copy", "Cmd+Shift+Up/Down, Cmd+C on block", "-"),
    ("Pane focus", "Cmd+Alt+Arrows, Cmd+[ / Cmd+]", "Alt+Arrows (only toward an existing neighbour)"),
    ("Pane resize", "Cmd+Ctrl+Arrows", "Cmd+Ctrl+Arrows"),
    ("Pane swap", "Cmd+Ctrl+Shift+Arrows", "Cmd+Ctrl+Shift+Arrows"),
    ("Pane zoom", "Cmd+Shift+Enter", "Cmd+Shift+Enter"),
    ("Pane equalize", "Cmd+Ctrl+=", "Cmd+Ctrl+="),
    ("Close pane", "Cmd+W", "Cmd+W"),
    ("Scrollback", "Shift+PageUp/PageDown/Home/End (shell screen only)", "same"),
    ("Autocomplete (legacy)", "Ctrl+Space at a shell prompt (OSC 133), not in alt screen", "same"),
];

// ───────────────────────── chords ─────────────────────────

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum KeyId {
    Char(char),
    Named(&'static str),
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Chord {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub sup: bool,
    pub key: KeyId,
}

impl Chord {
    /// True for chords a terminal program could also receive (Ctrl/Alt, no Cmd/Super).
    pub fn is_program_chord(&self) -> bool {
        (self.ctrl || self.alt) && !self.sup
    }

    pub fn parse(s: &str) -> Result<Chord, String> {
        let s = s.trim().to_ascii_lowercase();
        if s.is_empty() {
            return Err("empty chord".into());
        }
        // Split on '+', but a trailing "+" (or "++") is the '+' key itself.
        let (mods_part, key_part) = if s == "+" {
            ("", "+")
        } else if let Some(stripped) = s.strip_suffix("++") {
            (stripped, "+")
        } else if let Some(idx) = s.rfind('+') {
            (&s[..idx], &s[idx + 1..])
        } else {
            ("", s.as_str())
        };
        let mut c = Chord { ctrl: false, alt: false, shift: false, sup: false, key: KeyId::Char(' ') };
        for m in mods_part.split('+').filter(|m| !m.is_empty()) {
            match m.trim() {
                "ctrl" | "control" => c.ctrl = true,
                "alt" | "opt" | "option" => c.alt = true,
                "shift" => c.shift = true,
                "cmd" | "command" | "super" | "win" | "meta" => c.sup = true,
                "mod" => {
                    if cfg!(target_os = "macos") { c.sup = true } else { c.ctrl = true }
                }
                other => return Err(format!("unknown modifier '{other}'")),
            }
        }
        c.key = parse_key(key_part.trim())?;
        Ok(c)
    }

    pub fn display(&self) -> String {
        let mut parts: Vec<String> = vec![];
        if self.ctrl { parts.push("Ctrl".into()); }
        if self.alt { parts.push(if cfg!(target_os = "macos") { "Opt".into() } else { "Alt".into() }); }
        if self.shift { parts.push("Shift".into()); }
        if self.sup { parts.push(if cfg!(target_os = "macos") { "Cmd".into() } else { "Super".into() }); }
        parts.push(match &self.key {
            KeyId::Char(c) => c.to_uppercase().to_string(),
            KeyId::Named(n) => {
                let mut cs = n.chars();
                cs.next().map(|f| f.to_uppercase().collect::<String>() + cs.as_str()).unwrap_or_default()
            }
        });
        parts.join("+")
    }
}

const NAMED: &[&str] = &[
    "enter", "tab", "space", "escape", "backspace", "delete", "insert", "home", "end",
    "pageup", "pagedown", "up", "down", "left", "right",
    "f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8", "f9", "f10", "f11", "f12",
];

fn parse_key(k: &str) -> Result<KeyId, String> {
    let canon = match k {
        "return" => "enter",
        "esc" => "escape",
        "del" => "delete",
        "ins" => "insert",
        "pgup" => "pageup",
        "pgdn" | "pgdown" => "pagedown",
        "plus" => return Ok(KeyId::Char('+')),
        "minus" => return Ok(KeyId::Char('-')),
        "comma" => return Ok(KeyId::Char(',')),
        "period" => return Ok(KeyId::Char('.')),
        other => other,
    };
    if let Some(n) = NAMED.iter().find(|n| **n == canon) {
        return Ok(KeyId::Named(n));
    }
    let mut it = canon.chars();
    match (it.next(), it.next()) {
        (Some(c), None) => Ok(KeyId::Char(c)),
        _ => Err(format!("unknown key '{k}'")),
    }
}

fn named_id(n: &NamedKey) -> Option<&'static str> {
    use NamedKey::*;
    Some(match n {
        Enter => "enter",
        Tab => "tab",
        Space => "space",
        Escape => "escape",
        Backspace => "backspace",
        Delete => "delete",
        Insert => "insert",
        Home => "home",
        End => "end",
        PageUp => "pageup",
        PageDown => "pagedown",
        ArrowUp => "up",
        ArrowDown => "down",
        ArrowLeft => "left",
        ArrowRight => "right",
        F1 => "f1", F2 => "f2", F3 => "f3", F4 => "f4", F5 => "f5", F6 => "f6",
        F7 => "f7", F8 => "f8", F9 => "f9", F10 => "f10", F11 => "f11", F12 => "f12",
        _ => return None,
    })
}

/// Chord for a key event: the unmodified key plus the held modifiers.
pub fn chord_of(event: &KeyEvent, m: ModifiersState) -> Option<Chord> {
    use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;
    let mut key = event.key_without_modifiers();
    // Some platforms report control characters / empty for the unmodified key.
    if let Key::Character(s) = &key {
        if s.chars().next().map_or(true, |c| (c as u32) < 0x20) {
            key = event.logical_key.clone();
        }
    }
    let id = match &key {
        Key::Named(n) => KeyId::Named(named_id(n)?),
        Key::Character(s) => {
            let c = s.chars().next()?;
            KeyId::Char(c.to_lowercase().next().unwrap_or(c))
        }
        _ => return None,
    };
    Some(Chord { ctrl: m.control_key(), alt: m.alt_key(), shift: m.shift_key(), sup: m.super_key(), key: id })
}

// ───────────────────────── keymap ─────────────────────────

pub struct Keymap {
    map: HashMap<Chord, Action>,
    /// Effective chord list per action (defaults or user override).
    chords: Vec<(Action, Vec<Chord>, bool)>,
}

impl Default for Keymap {
    fn default() -> Self {
        Self::build(&[])
    }
}

impl Keymap {
    /// Build from defaults plus `[keybindings]` overrides (action -> chords).
    /// Invalid entries are logged and ignored; an empty list / "none" unbinds.
    pub fn build(overrides: &[(String, Vec<String>)]) -> Keymap {
        let mut chords: Vec<(Action, Vec<Chord>, bool)> = Vec::new();
        for def in ACTIONS {
            let mut list = Vec::new();
            for s in def.defaults() {
                match Chord::parse(s) {
                    Ok(c) => list.push(c),
                    Err(e) => log::error!("built-in keybinding {}={s}: {e}", def.name),
                }
            }
            chords.push((def.action, list, false));
        }
        for (name, specs) in overrides {
            let Some(def) = ACTIONS.iter().find(|d| d.name == name.as_str()) else {
                log::warn!("[keybindings] unknown action '{name}'");
                continue;
            };
            let mut list = Vec::new();
            for s in specs {
                if s.is_empty() || s.eq_ignore_ascii_case("none") || s.eq_ignore_ascii_case("unbound") {
                    continue;
                }
                match Chord::parse(s) {
                    Ok(c) => list.push(c),
                    Err(e) => log::warn!("[keybindings] {name} = \"{s}\": {e}"),
                }
            }
            if let Some(slot) = chords.iter_mut().find(|(a, _, _)| *a == def.action) {
                slot.1 = list;
                slot.2 = true;
            }
        }
        // User overrides win over built-in chords bound to other actions.
        let mut map = HashMap::new();
        for pass_user in [false, true] {
            for (a, list, user) in &chords {
                if *user != pass_user {
                    continue;
                }
                for c in list {
                    map.insert(c.clone(), *a);
                }
            }
        }
        Keymap { map, chords }
    }

    pub fn lookup(&self, chord: &Chord) -> Option<Action> {
        self.map.get(chord).copied()
    }

    pub fn lookup_event(&self, event: &KeyEvent, m: ModifiersState) -> Option<(Action, Chord)> {
        let chord = chord_of(event, m)?;
        let a = self.lookup(&chord)?;
        Some((a, chord))
    }

    /// Chords currently bound to `action`, formatted for display.
    pub fn display(&self, action: Action) -> String {
        self.chords
            .iter()
            .find(|(a, _, _)| *a == action)
            .map(|(_, l, _)| l.iter().map(Chord::display).collect::<Vec<_>>().join(", "))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "(unbound)".into())
    }

    /// Table printed by `rift --list-keybindings`.
    pub fn table(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("{:<22} {:<34} {}\n", "ACTION", "KEYS", "DESCRIPTION"));
        for def in ACTIONS {
            let user = self.chords.iter().any(|(a, _, u)| *a == def.action && *u);
            out.push_str(&format!(
                "{:<22} {:<34} {}{}\n",
                def.name,
                self.display(def.action),
                def.desc,
                if user { " [custom]" } else { "" }
            ));
        }
        out.push_str("\nFixed (not rebindable):\n");
        let mac = cfg!(target_os = "macos");
        for (what, m, o) in FIXED {
            out.push_str(&format!("  {:<30} {}\n", what, if mac { m } else { o }));
        }
        out.push_str(
            "\nChords with Ctrl or Alt pass through to the program while it uses the alternate\n\
             screen, the kitty keyboard protocol or mouse reporting (except copy/paste/tab actions).\n\
             Override in config.toml:\n  [keybindings]\n  history_search = \"cmd+shift+h\"\n  zoom_in = [\"cmd+=\", \"cmd+shift+=\"]\n  autocomplete = \"none\"\n",
        );
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_chords() {
        let c = Chord::parse("Ctrl+Shift+C").unwrap();
        assert!(c.ctrl && c.shift && !c.alt && !c.sup);
        assert_eq!(c.key, KeyId::Char('c'));
        assert_eq!(Chord::parse("cmd++").unwrap().key, KeyId::Char('+'));
        assert_eq!(Chord::parse("ctrl+shift+tab").unwrap().key, KeyId::Named("tab"));
        assert_eq!(Chord::parse("alt+Return").unwrap().key, KeyId::Named("enter"));
        assert_eq!(Chord::parse("mod+shift+,").unwrap().key, KeyId::Char(','));
        assert!(Chord::parse("hyper+x").is_err());
        assert!(Chord::parse("ctrl+").is_err());
        assert!(Chord::parse("ctrl+notakey").is_err());
    }

    #[test]
    fn all_defaults_parse_on_every_platform() {
        for d in ACTIONS {
            for s in d.mac.iter().chain(d.other.iter()) {
                Chord::parse(s).unwrap_or_else(|e| panic!("{}: {s}: {e}", d.name));
            }
        }
    }

    #[test]
    fn no_duplicate_default_chords_per_platform() {
        for mac in [true, false] {
            let mut seen: HashMap<Chord, &str> = HashMap::new();
            for d in ACTIONS {
                for s in if mac { d.mac } else { d.other } {
                    // "mod" resolves per host platform, so compare textual chords instead.
                    let c = Chord::parse(&s.replace("mod", if mac { "cmd" } else { "ctrl" })).unwrap();
                    if let Some(prev) = seen.insert(c, d.name) {
                        panic!("chord {s} bound to both {prev} and {} (mac={mac})", d.name);
                    }
                }
            }
        }
    }

    #[test]
    fn linux_defaults_leave_readline_chords_alone() {
        for d in ACTIONS {
            for s in d.other {
                let c = Chord::parse(s).unwrap();
                if c.is_program_chord() && !c.sup {
                    // Bare Ctrl+<letter> / Alt+<letter> are the shell's.
                    let plain_ctrl_letter = c.ctrl && !c.shift && !c.alt && matches!(c.key, KeyId::Char(ch) if ch.is_ascii_alphabetic() || ch == '_');
                    let plain_alt_letter = c.alt && !c.ctrl && !c.shift && matches!(c.key, KeyId::Char(ch) if ch.is_ascii_alphabetic());
                    assert!(!plain_ctrl_letter && !plain_alt_letter, "{} steals {s}", d.name);
                }
                assert!(!(c.ctrl && !c.shift && !c.alt && c.key == KeyId::Named("space")), "{} steals Ctrl+Space", d.name);
            }
        }
    }

    #[test]
    fn user_override_replaces_and_unbinds() {
        let km = Keymap::build(&[
            ("history_search".into(), vec!["ctrl+shift+9".into()]),
            ("autocomplete".into(), vec!["none".into()]),
            ("bogus".into(), vec!["ctrl+x".into()]),
        ]);
        let hs = Chord::parse("ctrl+shift+9").unwrap();
        assert_eq!(km.lookup(&hs), Some(Action::HistorySearch));
        assert_eq!(km.display(Action::Autocomplete), "(unbound)");
        assert!(km.table().contains("[custom]"));
    }

    #[test]
    fn user_binding_wins_over_default_conflict() {
        let km = Keymap::build(&[("hud".into(), vec!["cmd+c".into()])]);
        assert_eq!(km.lookup(&Chord::parse("cmd+c").unwrap()), Some(Action::Hud));
    }

    #[test]
    fn default_table_prints() {
        let t = Keymap::default().table();
        assert!(t.contains("history_search"));
        assert!(t.contains("Fixed"));
    }
}
