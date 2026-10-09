//! Keyboard encoding: winit key events -> bytes for the PTY.
//!
//! Two encoders live here:
//!
//! * the legacy xterm encoder (default), and
//! * the kitty keyboard protocol encoder, used when the foreground program has
//!   pushed non-zero flags (`CSI > flags u`). Flags 1 (disambiguate), 2 (event
//!   types), 4 (alternate keys), 8 (all keys as escape codes) and 16
//!   (associated text) are implemented.
//!
//! Both work on [`KeyInput`], a plain-data view of a key event, so the golden
//! byte-sequence tests below do not need a real winit `KeyEvent` (which cannot
//! be constructed outside winit).
//!
//! modifyOtherKeys (xterm `CSI > 4 ; n m`) is intentionally not implemented;
//! programs that want unambiguous keys should use the kitty protocol.

use std::cell::RefCell;
use std::collections::HashSet;
use std::sync::atomic::{AtomicU8, Ordering};

use winit::event::{ElementState, KeyEvent};
use winit::keyboard::{Key, KeyCode, KeyLocation, ModifiersState, NamedKey, PhysicalKey};

// ───────────────────────── configuration ─────────────────────────

/// What Shift+Enter sends when the app has not enabled the kitty protocol.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ShiftEnter {
    /// `ESC CR` (what Claude Code's /terminal-setup configures). Default.
    #[default]
    EscCr,
    /// `CSI 13;2 u`.
    CsiU,
    /// Bare line feed `\n`.
    Lf,
}

impl ShiftEnter {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "esc-cr" | "esc_cr" | "esccr" => Some(Self::EscCr),
            "csi-u" | "csi_u" | "csiu" => Some(Self::CsiU),
            "lf" => Some(Self::Lf),
            _ => None,
        }
    }
}

/// Which macOS Option key acts as Meta (ESC prefix). The other one composes
/// characters (Option+e -> dead acute, Option+a -> a-ring, ...).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum OptionAsMeta {
    #[default]
    Left,
    Right,
    Both,
    None,
}

impl OptionAsMeta {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "left" => Some(Self::Left),
            "right" => Some(Self::Right),
            "both" => Some(Self::Both),
            "none" | "off" | "false" => Some(Self::None),
            _ => None,
        }
    }
}

/// Input-related settings parsed from config.toml (`shift_enter`,
/// `option_as_meta` and the `[keybindings]` section). Read-only here.
#[derive(Clone, Debug, Default)]
pub struct InputConfig {
    pub shift_enter: ShiftEnter,
    pub option_as_meta: OptionAsMeta,
    /// `[keybindings]`: action name -> chords (empty vec = unbound).
    pub keybindings: Vec<(String, Vec<String>)>,
}

// ───────────────────────── key model ─────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Mods {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub sup: bool,
}

impl Mods {
    pub fn from_state(m: ModifiersState) -> Self {
        Self { shift: m.shift_key(), ctrl: m.control_key(), alt: m.alt_key(), sup: m.super_key() }
    }
    /// Kitty/xterm modifier bits (without the +1 offset).
    fn bits(self) -> u32 {
        (self.shift as u32) | ((self.alt as u32) << 1) | ((self.ctrl as u32) << 2) | ((self.sup as u32) << 3)
    }
    fn any(self) -> bool {
        self.shift || self.ctrl || self.alt || self.sup
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EventType {
    Press,
    Repeat,
    Release,
}

/// Which physical Option/Alt key(s) are down; used for `option_as_meta`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AltSide {
    Unknown,
    Left,
    Right,
    Both,
}

/// Plain-data view of a key event.
#[derive(Clone, Debug)]
pub struct KeyInput {
    /// Logical key (shift/option applied).
    pub key: Key,
    /// Key with no modifiers applied (lowercase letter, unshifted symbol).
    pub base: Option<char>,
    /// Key at the same physical position on a US layout (kitty "base layout key").
    pub base_layout: Option<char>,
    /// Text the event produces (`None` for pure functional keys).
    pub text: Option<String>,
    pub location: KeyLocation,
    pub state: EventType,
    pub mods: Mods,
    pub alt_side: AltSide,
}

impl KeyInput {
    #[cfg(test)]
    fn ch(c: char) -> Self {
        Self {
            key: Key::Character(c.to_string().into()),
            base: Some(c.to_ascii_lowercase()),
            base_layout: None,
            text: Some(c.to_string()),
            location: KeyLocation::Standard,
            state: EventType::Press,
            mods: Mods::default(),
            alt_side: AltSide::Unknown,
        }
    }
    #[cfg(test)]
    fn named(n: NamedKey) -> Self {
        let text = match n {
            NamedKey::Space => Some(" ".to_string()),
            _ => None,
        };
        Self {
            key: Key::Named(n),
            base: if n == NamedKey::Space { Some(' ') } else { None },
            base_layout: None,
            text,
            location: KeyLocation::Standard,
            state: EventType::Press,
            mods: Mods::default(),
            alt_side: AltSide::Unknown,
        }
    }
}

/// Everything the encoder needs to know about the receiving terminal.
#[derive(Clone, Copy, Debug)]
pub struct EncodeOpts {
    pub app_cursor: bool,
    pub app_keypad: bool,
    pub kitty_flags: u32,
    pub shift_enter: ShiftEnter,
    pub option_as_meta: OptionAsMeta,
    /// macOS semantics (Option composes characters unless it is Meta).
    pub mac: bool,
}

impl Default for EncodeOpts {
    fn default() -> Self {
        Self {
            app_cursor: false,
            app_keypad: false,
            kitty_flags: 0,
            shift_enter: ShiftEnter::EscCr,
            option_as_meta: OptionAsMeta::Left,
            mac: false,
        }
    }
}

// ───────────────────────── winit glue ─────────────────────────

static ALT_SIDES: AtomicU8 = AtomicU8::new(0);

/// Record which Alt keys are down (call on `ModifiersChanged`).
pub fn note_modifiers(m: &winit::event::Modifiers) {
    use winit::keyboard::ModifiersKeyState::Pressed;
    let l = m.lalt_state() == Pressed;
    let r = m.ralt_state() == Pressed;
    ALT_SIDES.store((l as u8) | ((r as u8) << 1), Ordering::Relaxed);
}

fn current_alt_side() -> AltSide {
    match ALT_SIDES.load(Ordering::Relaxed) {
        1 => AltSide::Left,
        2 => AltSide::Right,
        3 => AltSide::Both,
        _ => AltSide::Unknown,
    }
}

fn single_char(s: &str) -> Option<char> {
    let mut it = s.chars();
    let c = it.next()?;
    if it.next().is_some() { None } else { Some(c) }
}

fn us_layout_char(code: KeyCode) -> Option<char> {
    use KeyCode::*;
    Some(match code {
        KeyA => 'a', KeyB => 'b', KeyC => 'c', KeyD => 'd', KeyE => 'e', KeyF => 'f',
        KeyG => 'g', KeyH => 'h', KeyI => 'i', KeyJ => 'j', KeyK => 'k', KeyL => 'l',
        KeyM => 'm', KeyN => 'n', KeyO => 'o', KeyP => 'p', KeyQ => 'q', KeyR => 'r',
        KeyS => 's', KeyT => 't', KeyU => 'u', KeyV => 'v', KeyW => 'w', KeyX => 'x',
        KeyY => 'y', KeyZ => 'z',
        Digit0 => '0', Digit1 => '1', Digit2 => '2', Digit3 => '3', Digit4 => '4',
        Digit5 => '5', Digit6 => '6', Digit7 => '7', Digit8 => '8', Digit9 => '9',
        Minus => '-', Equal => '=', BracketLeft => '[', BracketRight => ']',
        Backslash => '\\', Semicolon => ';', Quote => '\'', Backquote => '`',
        Comma => ',', Period => '.', Slash => '/',
        _ => return None,
    })
}

/// Build the encoder's view of a winit key event.
pub fn key_input(event: &KeyEvent, modifiers: ModifiersState) -> KeyInput {
    use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;
    let base = match event.key_without_modifiers() {
        Key::Character(s) => single_char(s.as_str()).map(|c| c.to_lowercase().next().unwrap_or(c)),
        Key::Named(NamedKey::Space) => Some(' '),
        _ => None,
    };
    let base_layout = match event.physical_key {
        PhysicalKey::Code(c) => us_layout_char(c),
        _ => None,
    };
    let state = if event.state == ElementState::Released {
        EventType::Release
    } else if event.repeat {
        EventType::Repeat
    } else {
        EventType::Press
    };
    KeyInput {
        key: event.logical_key.clone(),
        base,
        base_layout,
        text: event.text.as_ref().map(|t| t.to_string()),
        location: event.location,
        state,
        mods: Mods::from_state(modifiers),
        alt_side: current_alt_side(),
    }
}

/// Encode a winit key event for the PTY. `None` = the key produces no bytes.
pub fn encode_key(event: &KeyEvent, modifiers: ModifiersState, opts: &EncodeOpts) -> Option<Vec<u8>> {
    encode(&key_input(event, modifiers), opts)
}

thread_local! {
    /// Physical keys whose press was forwarded to the PTY; only their
    /// releases are forwarded (kitty event-type reporting).
    static FORWARDED: RefCell<HashSet<PhysicalKey>> = RefCell::new(HashSet::new());
}

pub fn note_forwarded(key: PhysicalKey, forwarded: bool) {
    FORWARDED.with(|f| {
        if forwarded {
            f.borrow_mut().insert(key);
        }
    });
}

/// True (and forgets the key) if its press was forwarded to the PTY.
pub fn take_forwarded(key: PhysicalKey) -> bool {
    FORWARDED.with(|f| f.borrow_mut().remove(&key))
}

// ───────────────────────── helpers ─────────────────────────

/// US-layout shifted character for ASCII symbols/letters.
pub fn shift_us(c: char) -> char {
    match c {
        'a'..='z' => c.to_ascii_uppercase(),
        '1' => '!', '2' => '@', '3' => '#', '4' => '$', '5' => '%',
        '6' => '^', '7' => '&', '8' => '*', '9' => '(', '0' => ')',
        '-' => '_', '=' => '+', '[' => '{', ']' => '}', '\\' => '|',
        ';' => ':', '\'' => '"', ',' => '<', '.' => '>', '/' => '?', '`' => '~',
        _ => c,
    }
}

fn is_ctrl_char(c: char) -> bool {
    (c as u32) < 0x20 || c as u32 == 0x7f
}

/// Printable text of the event (never control characters).
fn text_of(i: &KeyInput) -> Option<String> {
    if let Some(t) = &i.text {
        if !t.is_empty() && !t.chars().any(is_ctrl_char) {
            return Some(t.clone());
        }
    }
    match &i.key {
        Key::Character(s) if !s.is_empty() && !s.chars().any(is_ctrl_char) => Some(s.to_string()),
        Key::Named(NamedKey::Space) => Some(" ".into()),
        _ => None,
    }
}

/// The key's unmodified character (lowercase).
fn base_char(i: &KeyInput) -> Option<char> {
    i.base.or_else(|| match &i.key {
        Key::Character(s) => single_char(s.as_str()).map(|c| c.to_lowercase().next().unwrap_or(c)),
        Key::Named(NamedKey::Space) => Some(' '),
        _ => None,
    })
}

/// Control byte for Ctrl+<char> per xterm.
fn ctrl_byte(base: char, shift: bool) -> Option<u8> {
    let eff = if shift { shift_us(base) } else { base };
    Some(match eff {
        'a'..='z' => eff as u8 - b'a' + 1,
        'A'..='Z' => eff as u8 - b'A' + 1,
        '@' | ' ' | '2' => 0,
        '[' | '3' => 0x1b,
        '\\' | '4' => 0x1c,
        ']' | '5' => 0x1d,
        '^' | '6' => 0x1e,
        '_' | '7' | '-' | '/' => 0x1f,
        '?' | '8' => 0x7f,
        _ => return None,
    })
}

/// Is the Alt key in this event acting as Meta (ESC prefix)?
fn alt_is_meta(i: &KeyInput, o: &EncodeOpts) -> bool {
    if !i.mods.alt {
        return false;
    }
    if !o.mac {
        return true;
    }
    match (o.option_as_meta, i.alt_side) {
        (OptionAsMeta::None, _) => false,
        (OptionAsMeta::Both, _) => true,
        (OptionAsMeta::Left, AltSide::Left | AltSide::Both | AltSide::Unknown) => true,
        (OptionAsMeta::Left, AltSide::Right) => false,
        (OptionAsMeta::Right, AltSide::Right | AltSide::Both) => true,
        (OptionAsMeta::Right, _) => false,
    }
}

/// Character that follows ESC for Meta+key: derived from the unmodified key
/// so macOS composed characters (a-ring) never leak into Meta chords.
fn meta_text(i: &KeyInput) -> Option<String> {
    if let Some(t) = text_of(i) {
        if single_char(&t).map_or(false, |c| c.is_ascii() && !c.is_ascii_control()) {
            return Some(t);
        }
    }
    let b = base_char(i)?;
    let c = if i.mods.shift { shift_us(b) } else { b };
    Some(c.to_string())
}

fn mod_param(bits: u32, ev: u32) -> String {
    if ev > 1 { format!("{}:{}", bits + 1, ev) } else { format!("{}", bits + 1) }
}

// ───────────────────────── functional keys ─────────────────────────

#[derive(Clone, Copy)]
enum Func {
    /// `CSI 1;m X` / `CSI X` / `SS3 X`. bool = always SS3 when unmodified.
    Letter(u8, bool),
    /// `CSI n;m ~`
    Tilde(u32),
}

fn func_spec(n: NamedKey, kitty: bool) -> Option<Func> {
    use NamedKey::*;
    Some(match n {
        ArrowUp => Func::Letter(b'A', false),
        ArrowDown => Func::Letter(b'B', false),
        ArrowRight => Func::Letter(b'C', false),
        ArrowLeft => Func::Letter(b'D', false),
        Home => Func::Letter(b'H', false),
        End => Func::Letter(b'F', false),
        Insert => Func::Tilde(2),
        Delete => Func::Tilde(3),
        PageUp => Func::Tilde(5),
        PageDown => Func::Tilde(6),
        F1 => Func::Letter(b'P', true),
        F2 => Func::Letter(b'Q', true),
        F3 if kitty => Func::Tilde(13),
        F3 => Func::Letter(b'R', true),
        F4 => Func::Letter(b'S', true),
        F5 => Func::Tilde(15),
        F6 => Func::Tilde(17),
        F7 => Func::Tilde(18),
        F8 => Func::Tilde(19),
        F9 => Func::Tilde(20),
        F10 => Func::Tilde(21),
        F11 => Func::Tilde(23),
        F12 => Func::Tilde(24),
        F13 => Func::Tilde(25),
        F14 => Func::Tilde(26),
        F15 => Func::Tilde(28),
        F16 => Func::Tilde(29),
        F17 => Func::Tilde(31),
        F18 => Func::Tilde(32),
        F19 => Func::Tilde(33),
        F20 => Func::Tilde(34),
        _ => return None,
    })
}

fn func_bytes(f: Func, bits: u32, ev: u32, app_cursor: bool) -> Vec<u8> {
    match f {
        Func::Letter(t, ss3) => {
            if bits == 0 && ev == 1 {
                if ss3 || app_cursor {
                    vec![0x1b, b'O', t]
                } else {
                    vec![0x1b, b'[', t]
                }
            } else {
                format!("\x1b[1;{}{}", mod_param(bits, ev), t as char).into_bytes()
            }
        }
        Func::Tilde(n) => {
            if bits == 0 && ev == 1 {
                format!("\x1b[{n}~").into_bytes()
            } else {
                format!("\x1b[{n};{}~", mod_param(bits, ev)).into_bytes()
            }
        }
    }
}

/// Application keypad (DECKPAM) SS3 final byte for a numpad key.
fn keypad_ss3(i: &KeyInput) -> Option<u8> {
    if i.location != KeyLocation::Numpad {
        return None;
    }
    if matches!(i.key, Key::Named(NamedKey::Enter)) {
        return Some(b'M');
    }
    let c = match &i.key {
        Key::Character(s) => single_char(s.as_str())?,
        _ => return None,
    };
    Some(match c {
        '0'..='9' => b'p' + (c as u8 - b'0'),
        '+' => b'k',
        '-' => b'm',
        '*' => b'j',
        '/' => b'o',
        '.' => b'n',
        ',' => b'l',
        '=' => b'X',
        _ => return None,
    })
}

/// Kitty PUA codes for keypad keys (reported with flag 8).
fn kitty_keypad_code(i: &KeyInput) -> Option<u32> {
    if i.location != KeyLocation::Numpad {
        return None;
    }
    if matches!(i.key, Key::Named(NamedKey::Enter)) {
        return Some(57414);
    }
    let c = match &i.key {
        Key::Character(s) => single_char(s.as_str())?,
        _ => return None,
    };
    Some(match c {
        '0'..='9' => 57399 + (c as u32 - '0' as u32),
        '.' => 57409,
        '/' => 57410,
        '*' => 57411,
        '-' => 57412,
        '+' => 57413,
        '=' => 57415,
        _ => return None,
    })
}

/// Kitty codes for bare modifier keys (reported with flag 8).
fn kitty_modifier_code(n: NamedKey, loc: KeyLocation) -> Option<u32> {
    let right = loc == KeyLocation::Right;
    Some(match n {
        NamedKey::Shift => if right { 57447 } else { 57441 },
        NamedKey::Control => if right { 57448 } else { 57442 },
        NamedKey::Alt => if right { 57449 } else { 57443 },
        NamedKey::Super => if right { 57450 } else { 57444 },
        _ => return None,
    })
}

// ───────────────────────── entry point ─────────────────────────

pub fn encode(i: &KeyInput, o: &EncodeOpts) -> Option<Vec<u8>> {
    let kitty = o.kitty_flags & (1 | 8) != 0;
    if kitty {
        encode_kitty(i, o)
    } else {
        if i.state == EventType::Release {
            return None;
        }
        encode_legacy(i, o)
    }
}

// ───────────────────────── legacy xterm ─────────────────────────

fn encode_legacy(i: &KeyInput, o: &EncodeOpts) -> Option<Vec<u8>> {
    let m = i.mods;
    let bits = m.bits();

    if let Key::Named(n) = &i.key {
        let n = *n;
        // Application keypad (DECKPAM) overrides everything on the numpad.
        if o.app_keypad && !m.sup {
            if let Some(t) = keypad_ss3(i) {
                return Some(vec![0x1b, b'O', t]);
            }
        }
        match n {
            NamedKey::Enter => {
                if m.sup {
                    return None;
                }
                return Some(if m.shift {
                    match o.shift_enter {
                        ShiftEnter::EscCr => b"\x1b\r".to_vec(),
                        ShiftEnter::CsiU => b"\x1b[13;2u".to_vec(),
                        ShiftEnter::Lf => b"\n".to_vec(),
                    }
                } else if m.alt {
                    b"\x1b\r".to_vec()
                } else {
                    b"\r".to_vec()
                });
            }
            NamedKey::Tab => {
                if m.sup {
                    return None;
                }
                return Some(match (m.shift, m.alt) {
                    (true, false) => b"\x1b[Z".to_vec(),
                    (true, true) => b"\x1b\x1b[Z".to_vec(),
                    (false, true) => b"\x1b\t".to_vec(),
                    _ => b"\t".to_vec(),
                });
            }
            NamedKey::Backspace => {
                if m.sup {
                    // macOS convention: Cmd+Backspace kills the line (^U).
                    return if o.mac { Some(vec![0x15]) } else { None };
                }
                let b = if m.ctrl { 0x08 } else { 0x7f };
                return Some(if m.alt { vec![0x1b, b] } else { vec![b] });
            }
            NamedKey::Escape => {
                return Some(if m.alt { vec![0x1b, 0x1b] } else { vec![0x1b] });
            }
            NamedKey::Space => {} // falls through to the character path
            NamedKey::ArrowLeft | NamedKey::ArrowRight if m.sup && o.mac && !m.ctrl && !m.alt => {
                // Cmd+Left/Right: line start/end (^A / ^E).
                return Some(vec![if n == NamedKey::ArrowLeft { 0x01 } else { 0x05 }]);
            }
            _ => {
                let f = func_spec(n, false)?;
                if m.sup {
                    return None;
                }
                return Some(func_bytes(f, bits, 1, o.app_cursor));
            }
        }
    }

    // Character keys.
    if m.sup {
        return None; // Cmd/Super chords are never sent to legacy programs
    }
    if o.app_keypad {
        if let Some(t) = keypad_ss3(i) {
            return Some(vec![0x1b, b'O', t]);
        }
    }
    let meta = alt_is_meta(i, o);
    if m.ctrl {
        let b = base_char(i)
            .filter(|c| c.is_ascii())
            .or(i.base_layout)
            .and_then(|c| ctrl_byte(c, m.shift));
        if let Some(b) = b {
            return Some(if meta { vec![0x1b, b] } else { vec![b] });
        }
    }
    if meta {
        let t = meta_text(i)?;
        let mut out = vec![0x1b];
        out.extend_from_slice(t.as_bytes());
        return Some(out);
    }
    text_of(i).map(|t| t.into_bytes())
}

// ───────────────────────── kitty protocol ─────────────────────────

/// `CSI code[:shifted[:base]] ; mods[:ev] ; text u`
fn csi_u(code: u32, alts: Option<(Option<u32>, Option<u32>)>, bits: u32, ev: u32, text: Option<&str>) -> Vec<u8> {
    let mut s = format!("\x1b[{code}");
    if let Some((shifted, base)) = alts {
        if shifted.is_some() || base.is_some() {
            s.push(':');
            if let Some(sh) = shifted {
                s.push_str(&sh.to_string());
            }
            if let Some(b) = base {
                s.push(':');
                s.push_str(&b.to_string());
            }
        }
    }
    let has_text = text.map_or(false, |t| !t.is_empty());
    if bits != 0 || ev != 1 || has_text {
        s.push(';');
        if bits != 0 || ev != 1 {
            s.push_str(&mod_param(bits, ev));
        }
    }
    if has_text {
        s.push(';');
        let cps: Vec<String> = text.unwrap().chars().map(|c| (c as u32).to_string()).collect();
        s.push_str(&cps.join(":"));
    }
    s.push('u');
    s.into_bytes()
}

fn encode_kitty(i: &KeyInput, o: &EncodeOpts) -> Option<Vec<u8>> {
    let f = o.kitty_flags;
    let report_ev = f & 2 != 0;
    let all = f & 8 != 0;
    let alts_on = f & 4 != 0;
    let text_on = f & 16 != 0;

    let ev: u32 = if report_ev {
        match i.state {
            EventType::Press => 1,
            EventType::Repeat => 2,
            EventType::Release => 3,
        }
    } else {
        1
    };
    if i.state == EventType::Release && !report_ev {
        return None;
    }
    let release = i.state == EventType::Release;

    // Alt as modifier bit: real Alt for functional keys; for text keys only when
    // it is Meta (otherwise macOS Option composes a character).
    let meta = alt_is_meta(i, o);

    if let Key::Named(n) = &i.key {
        let n = *n;
        if n != NamedKey::Space {
            let bits = i.mods.bits();
            match n {
                NamedKey::Enter | NamedKey::Tab | NamedKey::Backspace => {
                    let code = match n {
                        NamedKey::Enter => 13,
                        NamedKey::Tab => 9,
                        _ => 127,
                    };
                    if !all {
                        // Not reported as escape codes while unmodified, and
                        // never released (so `reset` still works at a shell).
                        if release {
                            return None;
                        }
                        if bits == 0 {
                            return Some(match n {
                                NamedKey::Enter => b"\r".to_vec(),
                                NamedKey::Tab => b"\t".to_vec(),
                                _ => vec![0x7f],
                            });
                        }
                    }
                    let code = if i.location == KeyLocation::Numpad && all {
                        kitty_keypad_code(i).unwrap_or(code)
                    } else {
                        code
                    };
                    return Some(csi_u(code, None, bits, ev, None));
                }
                NamedKey::Escape => return Some(csi_u(27, None, bits, ev, None)),
                _ => {}
            }
            if let Some(code) = kitty_modifier_code(n, i.location) {
                return if all { Some(csi_u(code, None, bits, ev, None)) } else { None };
            }
            let spec = func_spec(n, true)?;
            return Some(func_bytes(spec, bits, ev, o.app_cursor));
        }
    }

    // Text-producing keys (including Space).
    let mods = Mods { alt: meta, ..i.mods };
    let bits = mods.bits();
    let code_char = base_char(i)?;

    if !all {
        // Disambiguate only: plain / shifted / composed text stays text.
        if mods.ctrl || mods.sup || mods.alt {
            // fallthrough to CSI u
        } else {
            if release {
                return None;
            }
            return text_of(i).map(|t| t.into_bytes());
        }
    }

    // Keypad keys have their own code points when everything is reported.
    let code = if all { kitty_keypad_code(i).unwrap_or(code_char as u32) } else { code_char as u32 };
    let alts = if alts_on {
        let shifted = if mods.shift {
            let lg = match &i.key {
                Key::Character(s) => single_char(s.as_str()),
                _ => None,
            };
            let sc = lg.filter(|c| *c != code_char).unwrap_or_else(|| shift_us(code_char));
            if sc != code_char { Some(sc as u32) } else { None }
        } else {
            None
        };
        let base = i.base_layout.filter(|b| *b != code_char).map(|b| b as u32);
        Some((shifted, base))
    } else {
        None
    };
    let text = if all && text_on && !release && !mods.ctrl && !mods.sup && !mods.alt {
        text_of(i)
    } else {
        None
    };
    Some(csi_u(code, alts, bits, ev, text.as_deref()))
}

// ───────────────────────── tests ─────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> EncodeOpts {
        EncodeOpts::default()
    }
    fn kopts(flags: u32) -> EncodeOpts {
        EncodeOpts { kitty_flags: flags, ..EncodeOpts::default() }
    }
    fn with(mut i: KeyInput, f: impl Fn(&mut Mods)) -> KeyInput {
        f(&mut i.mods);
        i
    }
    fn shift(i: KeyInput) -> KeyInput { with(i, |m| m.shift = true) }
    fn ctrl(i: KeyInput) -> KeyInput { with(i, |m| m.ctrl = true) }
    fn alt(i: KeyInput) -> KeyInput { with(i, |m| m.alt = true) }
    fn sup(i: KeyInput) -> KeyInput { with(i, |m| m.sup = true) }
    fn ev(mut i: KeyInput, s: EventType) -> KeyInput { i.state = s; i }
    fn enc(i: &KeyInput, o: &EncodeOpts) -> String {
        match encode(i, o) {
            Some(b) => String::from_utf8_lossy(&b).replace('\x1b', "\\e"),
            None => "<none>".into(),
        }
    }
    fn n(k: NamedKey) -> KeyInput { KeyInput::named(k) }
    fn c(ch: char) -> KeyInput { KeyInput::ch(ch) }

    // ---- legacy ----

    #[test]
    fn shift_tab_is_csi_z() {
        assert_eq!(enc(&shift(n(NamedKey::Tab)), &opts()), "\\e[Z");
        assert_eq!(enc(&n(NamedKey::Tab), &opts()), "\t");
        assert_eq!(enc(&alt(n(NamedKey::Tab)), &opts()), "\\e\t");
    }

    #[test]
    fn enter_variants() {
        assert_eq!(enc(&n(NamedKey::Enter), &opts()), "\r");
        assert_eq!(enc(&alt(n(NamedKey::Enter)), &opts()), "\\e\r");
        assert_eq!(enc(&ctrl(n(NamedKey::Enter)), &opts()), "\r");
        assert_eq!(enc(&shift(n(NamedKey::Enter)), &opts()), "\\e\r");
        let mut o = opts();
        o.shift_enter = ShiftEnter::CsiU;
        assert_eq!(enc(&shift(n(NamedKey::Enter)), &o), "\\e[13;2u");
        o.shift_enter = ShiftEnter::Lf;
        assert_eq!(enc(&shift(n(NamedKey::Enter)), &o), "\n");
    }

    #[test]
    fn shift_enter_config_parse() {
        assert_eq!(ShiftEnter::parse("esc-cr"), Some(ShiftEnter::EscCr));
        assert_eq!(ShiftEnter::parse("csi-u"), Some(ShiftEnter::CsiU));
        assert_eq!(ShiftEnter::parse("lf"), Some(ShiftEnter::Lf));
        assert_eq!(ShiftEnter::parse("x"), None);
        assert_eq!(OptionAsMeta::parse("both"), Some(OptionAsMeta::Both));
        assert_eq!(OptionAsMeta::parse("none"), Some(OptionAsMeta::None));
    }

    #[test]
    fn cursor_keys_plain_and_application() {
        assert_eq!(enc(&n(NamedKey::ArrowUp), &opts()), "\\e[A");
        let mut o = opts();
        o.app_cursor = true;
        assert_eq!(enc(&n(NamedKey::ArrowUp), &o), "\\eOA");
        assert_eq!(enc(&n(NamedKey::Home), &o), "\\eOH");
        assert_eq!(enc(&n(NamedKey::End), &o), "\\eOF");
        assert_eq!(enc(&n(NamedKey::Home), &opts()), "\\e[H");
        // modified arrows ignore DECCKM
        assert_eq!(enc(&ctrl(n(NamedKey::ArrowRight)), &o), "\\e[1;5C");
    }

    #[test]
    fn modified_arrows_home_end() {
        assert_eq!(enc(&shift(n(NamedKey::ArrowUp)), &opts()), "\\e[1;2A");
        assert_eq!(enc(&alt(n(NamedKey::ArrowLeft)), &opts()), "\\e[1;3D");
        assert_eq!(enc(&ctrl(n(NamedKey::ArrowLeft)), &opts()), "\\e[1;5D");
        assert_eq!(enc(&ctrl(shift(n(NamedKey::End))), &opts()), "\\e[1;6F");
        assert_eq!(enc(&alt(n(NamedKey::Home)), &opts()), "\\e[1;3H");
    }

    #[test]
    fn tilde_keys() {
        assert_eq!(enc(&n(NamedKey::Insert), &opts()), "\\e[2~");
        assert_eq!(enc(&n(NamedKey::Delete), &opts()), "\\e[3~");
        assert_eq!(enc(&n(NamedKey::PageUp), &opts()), "\\e[5~");
        assert_eq!(enc(&n(NamedKey::PageDown), &opts()), "\\e[6~");
        assert_eq!(enc(&ctrl(n(NamedKey::Delete)), &opts()), "\\e[3;5~");
        assert_eq!(enc(&shift(n(NamedKey::PageUp)), &opts()), "\\e[5;2~");
        assert_eq!(enc(&alt(n(NamedKey::PageDown)), &opts()), "\\e[6;3~");
    }

    #[test]
    fn function_keys() {
        assert_eq!(enc(&n(NamedKey::F1), &opts()), "\\eOP");
        assert_eq!(enc(&n(NamedKey::F2), &opts()), "\\eOQ");
        assert_eq!(enc(&n(NamedKey::F3), &opts()), "\\eOR");
        assert_eq!(enc(&n(NamedKey::F4), &opts()), "\\eOS");
        assert_eq!(enc(&shift(n(NamedKey::F1)), &opts()), "\\e[1;2P");
        assert_eq!(enc(&ctrl(n(NamedKey::F4)), &opts()), "\\e[1;5S");
        assert_eq!(enc(&n(NamedKey::F5), &opts()), "\\e[15~");
        assert_eq!(enc(&n(NamedKey::F6), &opts()), "\\e[17~");
        assert_eq!(enc(&n(NamedKey::F7), &opts()), "\\e[18~");
        assert_eq!(enc(&n(NamedKey::F8), &opts()), "\\e[19~");
        assert_eq!(enc(&n(NamedKey::F9), &opts()), "\\e[20~");
        assert_eq!(enc(&n(NamedKey::F10), &opts()), "\\e[21~");
        assert_eq!(enc(&n(NamedKey::F11), &opts()), "\\e[23~");
        assert_eq!(enc(&n(NamedKey::F12), &opts()), "\\e[24~");
        assert_eq!(enc(&shift(n(NamedKey::F5)), &opts()), "\\e[15;2~");
        assert_eq!(enc(&ctrl(alt(n(NamedKey::F12))), &opts()), "\\e[24;7~");
    }

    #[test]
    fn ctrl_letters_and_symbols() {
        assert_eq!(enc(&ctrl(c('a')), &opts()), "\x01");
        assert_eq!(enc(&ctrl(c('c')), &opts()), "\x03");
        assert_eq!(enc(&ctrl(c('z')), &opts()), "\x1a");
        assert_eq!(enc(&ctrl(shift(c('a'))), &opts()), "\x01");
        // Ctrl+symbol
        let sym = |ch: char, sh: bool| {
            let mut i = ctrl(c(ch));
            i.mods.shift = sh;
            encode(&i, &opts()).unwrap()
        };
        assert_eq!(sym('2', true), vec![0]); // Ctrl+@
        assert_eq!(sym('[', false), vec![0x1b]);
        assert_eq!(sym('\\', false), vec![0x1c]);
        assert_eq!(sym(']', false), vec![0x1d]);
        assert_eq!(sym('6', true), vec![0x1e]); // Ctrl+^
        assert_eq!(sym('-', true), vec![0x1f]); // Ctrl+_
        assert_eq!(sym('-', false), vec![0x1f]);
        assert_eq!(sym('/', true), vec![0x7f]); // Ctrl+?
        assert_eq!(sym('/', false), vec![0x1f]);
        assert_eq!(enc(&ctrl(n(NamedKey::Space)), &opts()), "\0");
        assert_eq!(enc(&ctrl(shift(n(NamedKey::Space))), &opts()), "\0");
        assert_eq!(enc(&n(NamedKey::Space), &opts()), " ");
    }

    #[test]
    fn ctrl_uses_us_base_layout_for_non_latin() {
        let mut i = ctrl(c('с')); // Cyrillic es on the C key
        i.base = Some('с');
        i.base_layout = Some('c');
        assert_eq!(encode(&i, &opts()).unwrap(), vec![0x03]);
    }

    #[test]
    fn alt_is_meta_prefix() {
        assert_eq!(enc(&alt(c('b')), &opts()), "\\eb");
        assert_eq!(enc(&alt(c('x')), &opts()), "\\ex");
        assert_eq!(enc(&alt(shift(c('F'))), &opts()), "\\eF");
        assert_eq!(enc(&alt(n(NamedKey::Space)), &opts()), "\\e ");
        assert_eq!(enc(&alt(ctrl(c('a'))), &opts()), "\\e\x01");
        assert_eq!(enc(&alt(n(NamedKey::Backspace)), &opts()), "\\e\x7f");
        assert_eq!(enc(&alt(n(NamedKey::Escape)), &opts()), "\\e\\e");
    }

    #[test]
    fn backspace_encodings() {
        assert_eq!(encode(&n(NamedKey::Backspace), &opts()).unwrap(), vec![0x7f]);
        assert_eq!(encode(&ctrl(n(NamedKey::Backspace)), &opts()).unwrap(), vec![0x08]);
        assert_eq!(encode(&shift(n(NamedKey::Backspace)), &opts()).unwrap(), vec![0x7f]);
    }

    #[test]
    fn mac_option_as_meta_sides() {
        let mut o = opts();
        o.mac = true;
        // Option+a composes "a-ring" on macOS.
        let mut i = alt(c('a'));
        i.text = Some("å".into());
        i.key = Key::Character("å".into());
        i.base = Some('a');
        i.alt_side = AltSide::Left;
        o.option_as_meta = OptionAsMeta::Left;
        assert_eq!(enc(&i, &o), "\\ea");
        i.alt_side = AltSide::Right;
        assert_eq!(enc(&i, &o), "å");
        o.option_as_meta = OptionAsMeta::Right;
        assert_eq!(enc(&i, &o), "\\ea");
        i.alt_side = AltSide::Left;
        assert_eq!(enc(&i, &o), "å");
        o.option_as_meta = OptionAsMeta::Both;
        assert_eq!(enc(&i, &o), "\\ea");
        o.option_as_meta = OptionAsMeta::None;
        assert_eq!(enc(&i, &o), "å");
        // Option+Shift+a as Meta -> ESC A
        o.option_as_meta = OptionAsMeta::Left;
        i.mods.shift = true;
        i.text = Some("Å".into());
        assert_eq!(enc(&i, &o), "\\eA");
    }

    #[test]
    fn mac_command_chords_not_sent_to_legacy_apps() {
        let mut o = opts();
        o.mac = true;
        assert_eq!(encode(&sup(c('x')), &o), None);
        assert_eq!(encode(&sup(n(NamedKey::Backspace)), &o).unwrap(), vec![0x15]);
        assert_eq!(encode(&sup(n(NamedKey::ArrowLeft)), &o).unwrap(), vec![0x01]);
        assert_eq!(encode(&sup(n(NamedKey::ArrowRight)), &o).unwrap(), vec![0x05]);
    }

    #[test]
    fn plain_text_and_release() {
        assert_eq!(enc(&c('q'), &opts()), "q");
        assert_eq!(enc(&c('Q'), &opts()), "Q");
        assert_eq!(enc(&ev(c('q'), EventType::Release), &opts()), "<none>");
        let mut i = c('é');
        i.base = Some('é');
        assert_eq!(enc(&i, &opts()), "é");
    }

    #[test]
    fn application_keypad() {
        let mut o = opts();
        o.app_keypad = true;
        let mut k5 = c('5');
        k5.location = KeyLocation::Numpad;
        assert_eq!(enc(&k5, &o), "\\eOu");
        let mut ent = n(NamedKey::Enter);
        ent.location = KeyLocation::Numpad;
        assert_eq!(enc(&ent, &o), "\\eOM");
        assert_eq!(enc(&ent, &opts()), "\r");
        assert_eq!(enc(&k5, &opts()), "5");
    }

    // ---- kitty ----

    #[test]
    fn kitty_disambiguate() {
        let o = kopts(1);
        assert_eq!(enc(&n(NamedKey::Escape), &o), "\\e[27u");
        assert_eq!(enc(&ctrl(c('a')), &o), "\\e[97;5u");
        assert_eq!(enc(&alt(c('a')), &o), "\\e[97;3u");
        assert_eq!(enc(&ctrl(shift(c('a'))), &o), "\\e[97;6u");
        assert_eq!(enc(&c('a'), &o), "a");
        assert_eq!(enc(&shift(c('A')), &o), "A");
        assert_eq!(enc(&n(NamedKey::Enter), &o), "\r");
        assert_eq!(enc(&shift(n(NamedKey::Enter)), &o), "\\e[13;2u");
        assert_eq!(enc(&ctrl(n(NamedKey::Enter)), &o), "\\e[13;5u");
        assert_eq!(enc(&alt(n(NamedKey::Enter)), &o), "\\e[13;3u");
        assert_eq!(enc(&n(NamedKey::Tab), &o), "\t");
        assert_eq!(enc(&shift(n(NamedKey::Tab)), &o), "\\e[9;2u");
        assert_eq!(enc(&n(NamedKey::Backspace), &o), "\x7f");
        assert_eq!(enc(&ctrl(n(NamedKey::Backspace)), &o), "\\e[127;5u");
        assert_eq!(enc(&ctrl(n(NamedKey::Space)), &o), "\\e[32;5u");
        assert_eq!(enc(&sup(c('s')), &o), "\\e[115;9u");
    }

    #[test]
    fn kitty_functional_keys_keep_legacy_forms() {
        let o = kopts(1);
        assert_eq!(enc(&n(NamedKey::ArrowUp), &o), "\\e[A");
        assert_eq!(enc(&ctrl(n(NamedKey::ArrowUp)), &o), "\\e[1;5A");
        assert_eq!(enc(&n(NamedKey::F1), &o), "\\eOP");
        assert_eq!(enc(&n(NamedKey::F3), &o), "\\e[13~");
        assert_eq!(enc(&shift(n(NamedKey::F3)), &o), "\\e[13;2~");
        assert_eq!(enc(&n(NamedKey::Delete), &o), "\\e[3~");
        assert_eq!(enc(&n(NamedKey::F12), &o), "\\e[24~");
    }

    #[test]
    fn kitty_report_all_keys() {
        let o = kopts(1 | 8);
        assert_eq!(enc(&c('a'), &o), "\\e[97u");
        assert_eq!(enc(&shift(c('A')), &o), "\\e[97;2u");
        assert_eq!(enc(&n(NamedKey::Enter), &o), "\\e[13u");
        assert_eq!(enc(&n(NamedKey::Tab), &o), "\\e[9u");
        assert_eq!(enc(&n(NamedKey::Backspace), &o), "\\e[127u");
        assert_eq!(enc(&n(NamedKey::Escape), &o), "\\e[27u");
        assert_eq!(enc(&n(NamedKey::Space), &o), "\\e[32u");
        assert_eq!(enc(&n(NamedKey::Shift), &o), "\\e[57441u");
        let mut rctrl = n(NamedKey::Control);
        rctrl.location = KeyLocation::Right;
        assert_eq!(enc(&rctrl, &o), "\\e[57448u");
        let mut kp1 = c('1');
        kp1.location = KeyLocation::Numpad;
        assert_eq!(enc(&kp1, &o), "\\e[57400u");
    }

    #[test]
    fn kitty_event_types() {
        let o = kopts(1 | 2);
        assert_eq!(enc(&ev(ctrl(c('a')), EventType::Release), &o), "\\e[97;5:3u");
        assert_eq!(enc(&ev(ctrl(c('a')), EventType::Repeat), &o), "\\e[97;5:2u");
        assert_eq!(enc(&ctrl(c('a')), &o), "\\e[97;5u");
        assert_eq!(enc(&ev(n(NamedKey::ArrowUp), EventType::Release), &o), "\\e[1;1:3A");
        assert_eq!(enc(&ev(n(NamedKey::ArrowUp), EventType::Repeat), &o), "\\e[1;1:2A");
        assert_eq!(enc(&ev(n(NamedKey::Delete), EventType::Release), &o), "\\e[3;1:3~");
        assert_eq!(enc(&ev(n(NamedKey::Escape), EventType::Release), &o), "\\e[27;1:3u");
        // text keys and Enter/Tab/Backspace do not report releases without flag 8
        assert_eq!(enc(&ev(c('a'), EventType::Release), &o), "<none>");
        assert_eq!(enc(&ev(n(NamedKey::Enter), EventType::Release), &o), "<none>");
        assert_eq!(enc(&ev(n(NamedKey::Backspace), EventType::Release), &o), "<none>");
        // ... but with flag 8 they do
        let o8 = kopts(1 | 2 | 8);
        assert_eq!(enc(&ev(c('a'), EventType::Release), &o8), "\\e[97;1:3u");
        assert_eq!(enc(&ev(n(NamedKey::Enter), EventType::Release), &o8), "\\e[13;1:3u");
        assert_eq!(enc(&ev(c('a'), EventType::Repeat), &o8), "\\e[97;1:2u");
        // flag 2 alone does not enable release for legacy-mode programs
        assert_eq!(enc(&ev(c('a'), EventType::Release), &opts()), "<none>");
    }

    #[test]
    fn kitty_alternate_keys_and_text() {
        // flag 4 + 8: shifted and base-layout keys (spec: CSI 97:65;2u style)
        let o = kopts(1 | 4 | 8);
        assert_eq!(enc(&shift(c('A')), &o), "\\e[97:65;2u");
        // Ctrl+Cyrillic es on a Cyrillic layout: CSI 1089::99;5u
        let mut i = ctrl(c('с'));
        i.base = Some('с');
        i.base_layout = Some('c');
        assert_eq!(enc(&i, &o), "\\e[1089::99;5u");
        // flag 16: associated text
        let o = kopts(1 | 8 | 16);
        assert_eq!(enc(&shift(c('A')), &o), "\\e[97;2;65u");
        assert_eq!(enc(&c('a'), &o), "\\e[97;;97u");
        assert_eq!(enc(&ctrl(c('a')), &o), "\\e[97;5u");
        assert_eq!(enc(&ev(c('a'), EventType::Release), &kopts(1 | 2 | 8 | 16)), "\\e[97;1:3u");
    }

    #[test]
    fn kitty_mac_option_composition_when_not_meta() {
        let mut o = kopts(1);
        o.mac = true;
        o.option_as_meta = OptionAsMeta::Left;
        let mut i = alt(c('a'));
        i.text = Some("å".into());
        i.key = Key::Character("å".into());
        i.base = Some('a');
        i.alt_side = AltSide::Right;
        assert_eq!(enc(&i, &o), "å");
        i.alt_side = AltSide::Left;
        assert_eq!(enc(&i, &o), "\\e[97;3u");
    }

    #[test]
    fn kitty_flags_zero_is_legacy() {
        assert_eq!(enc(&ctrl(c('a')), &kopts(0)), "\x01");
        // only flag 2/4/16 without 1 or 8 behaves like legacy
        assert_eq!(enc(&ctrl(c('a')), &kopts(2)), "\x01");
    }
}
