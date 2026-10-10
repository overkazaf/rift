//! Help > Tutorials: bundled demo recordings played back inside Rift.
//!
//! * [`cast`] — the file format (asciicast v2, the session recorder's format,
//!   plus Rift markers for captions, key overlays and UI state).
//! * [`keys`] — shortcut references in captions, resolved against the
//!   user's effective keymap.
//! * [`player`] — the private demo world and the playback state machine
//!   (Time-Warp-style frame replay with exact seeking).
//! * [`view`] — the list and the player, drawn with the UI kit.
//! * `gen` (tests only) — the generator that writes `assets/demos/*.cast`.
//!   Regenerate with `RIFT_REGEN_DEMOS=1 cargo test --bin rift tutorial::gen`.
//!
//! Entry points: Help > Tutorials…, the command palette ("Help: Tutorials",
//! "Help: Play demo: …"), the welcome guide, and `rift --demo <name|list|FILE.cast>`.
//! Playback never writes to a PTY: see [`player`].

pub mod cast;
#[cfg(test)]
mod gen;
pub mod keys;
pub mod player;
#[cfg(test)]
mod tests;
pub mod view;

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use cast::Cast;
use player::{Outcome, Player, TKey};

/// A bundled demo.
pub struct DemoInfo {
    pub id: &'static str,
    pub title: &'static str,
    /// One line for the list.
    pub summary: &'static str,
    pub source: &'static str,
}

macro_rules! demo {
    ($id:literal, $title:literal, $summary:literal) => {
        DemoInfo { id: $id, title: $title, summary: $summary, source: include_str!(concat!("../../../assets/demos/", $id, ".cast")) }
    };
}

pub const DEMOS: &[DemoInfo] = &[
    demo!("blocks", "Command blocks", "Every command is a block: status chips, hover toolbar, folding, jumping, copying output."),
    demo!("splits", "Splits & panes", "Split side by side or stacked, move focus, zoom a pane."),
    demo!("ai", "AI: fix, ask, # commands", "Inline fix suggestions, Ask about this, # natural language (sample answers)."),
    demo!("palette", "Command palette", "Every action in one fuzzy search, plus theme / font / ssh / cd / > / ? commands."),
    demo!("preview", "Preview-Then-Accept", "How Rift stops dangerous commands before they run, and lets routine ones through."),
    demo!("agents", "Agent Mission Control", "Supervise Claude Code, Codex and friends from one dock; answer prompts in place."),
    demo!("tabs", "Tabs & windows", "Open, switch and close tabs; windows that start in the same directory."),
    demo!("time-warp", "Time Warp & recording", "Rewind the screen of a pane, and record sessions as asciinema casts."),
];

pub fn find(id: &str) -> Option<&'static DemoInfo> {
    DEMOS.iter().find(|d| d.id == id)
}

pub fn load(d: &DemoInfo) -> Result<Cast, String> {
    Cast::parse(d.source).map_err(|e| format!("demo '{}': {e}", d.id))
}

/// `rift --demo list`
pub fn list_text() -> String {
    let mut s = String::from("Rift tutorials (rift --demo <name>):\n");
    for d in DEMOS {
        let dur = load(d).map(|c| view::fmt_time(c.duration())).unwrap_or_else(|_| "?".into());
        s.push_str(&format!("  {:<10} {:>5}  {} - {}\n", d.id, dur, d.title, d.summary));
    }
    s.push_str("Also: rift --demo path/to/recording.cast (an asciinema v2 file, e.g. one made with the session recorder)\n");
    s
}

// ───────────────────────────── startup (`--demo`) ─────────────────────────────

#[derive(Clone, Debug, PartialEq)]
pub enum StartupDemo {
    Bundled(&'static str),
    File(PathBuf),
}

static STARTUP: Mutex<Option<StartupDemo>> = Mutex::new(None);

/// Validate `rift --demo <arg>` and remember it for the first window.
pub fn set_startup(arg: &str) -> Result<(), String> {
    let sd = if let Some(d) = find(arg) {
        StartupDemo::Bundled(d.id)
    } else if arg.ends_with(".cast") && std::path::Path::new(arg).is_file() {
        let src = std::fs::read_to_string(arg).map_err(|e| format!("{arg}: {e}"))?;
        Cast::parse(&src).map_err(|e| format!("{arg}: {e}"))?;
        StartupDemo::File(PathBuf::from(arg))
    } else {
        let names: Vec<&str> = DEMOS.iter().map(|d| d.id).collect();
        return Err(format!("unknown demo '{arg}' (available: {}, or a .cast file)", names.join(", ")));
    };
    *STARTUP.lock().unwrap_or_else(|e| e.into_inner()) = Some(sd);
    Ok(())
}

pub fn startup_pending() -> bool {
    STARTUP.lock().map(|g| g.is_some()).unwrap_or(false)
}

fn take_startup() -> Option<StartupDemo> {
    STARTUP.lock().ok().and_then(|mut g| g.take())
}

// ───────────────────────────── per-window state ─────────────────────────────

/// Tutorial list + player of one window.
pub struct TutorialUi {
    /// The list is open (selected row).
    pub picker: Option<usize>,
    pub player: Option<Player>,
    /// A `--demo` to start once the renderer's cell size is known.
    pending: Option<StartupDemo>,
    last_tick: Option<Instant>,
}

impl Default for TutorialUi {
    fn default() -> Self {
        Self::new()
    }
}

impl TutorialUi {
    pub fn new() -> Self {
        Self { picker: None, player: None, pending: None, last_tick: None }
    }

    /// State for the app's first window: picks up `rift --demo`.
    pub fn for_first_window() -> Self {
        Self { pending: take_startup(), ..Self::new() }
    }

    /// Anything to draw (list or player).
    pub fn visible(&self) -> bool {
        self.picker.is_some() || self.playing()
    }

    /// The player replaces the terminal view.
    pub fn playing(&self) -> bool {
        self.player.is_some() || self.pending.is_some()
    }

    /// Needs continuous redraws (playing, not paused).
    pub fn animating(&self) -> bool {
        self.pending.is_some() || self.player.as_ref().is_some_and(|p| p.playing)
    }

    pub fn open_picker(&mut self) {
        let sel = self.player.as_ref().and_then(|p| p.id).and_then(|id| DEMOS.iter().position(|d| d.id == id)).unwrap_or(0);
        self.player = None;
        self.picker = Some(sel);
    }

    pub fn close(&mut self) {
        self.picker = None;
        self.player = None;
        self.pending = None;
        self.last_tick = None;
    }

    pub fn play(&mut self, id: &str, cell: (usize, usize)) -> Result<(), String> {
        let d = find(id).ok_or_else(|| format!("unknown demo '{id}'"))?;
        let cast = load(d)?;
        self.player = Some(Player::new(cast, d.title.to_string(), Some(d.id), cell));
        self.picker = None;
        self.last_tick = None;
        Ok(())
    }

    pub fn play_file(&mut self, path: &std::path::Path, cell: (usize, usize)) -> Result<(), String> {
        let src = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let cast = Cast::parse(&src)?;
        let title = if cast.title.is_empty() {
            format!("Recording: {}", path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default())
        } else {
            cast.title.clone()
        };
        self.player = Some(Player::new(cast, title, None, cell));
        self.picker = None;
        self.last_tick = None;
        Ok(())
    }

    /// Start a pending `--demo` now that the cell size is known.
    pub fn start_pending(&mut self, cell: (usize, usize)) {
        if let Some(sd) = self.pending.take() {
            let r = match &sd {
                StartupDemo::Bundled(id) => self.play(id, cell),
                StartupDemo::File(p) => self.play_file(p, cell),
            };
            if let Err(e) = r {
                log::warn!("--demo: {e}");
            }
        }
    }

    /// Advance playback by the wall-clock time since the last frame.
    pub fn tick(&mut self, now: Instant) {
        let Some(p) = &mut self.player else {
            self.last_tick = None;
            return;
        };
        if !p.playing {
            self.last_tick = None;
            return;
        }
        if let Some(last) = self.last_tick {
            // A stalled frame must not skip half the demo.
            let dt = now.saturating_duration_since(last).min(Duration::from_millis(250));
            p.advance(dt.as_secs_f64());
        }
        self.last_tick = Some(now);
    }

    /// Handle a key while the list or player is open. Always consumes the key.
    pub fn handle_key(&mut self, key: TKey, cell: (usize, usize)) {
        if let Some(sel) = self.picker {
            let n = DEMOS.len();
            match key {
                TKey::Esc | TKey::Char('q') => self.close(),
                TKey::Up | TKey::Left => self.picker = Some((sel + n - 1) % n),
                TKey::Down | TKey::Right => self.picker = Some((sel + 1) % n),
                TKey::Enter | TKey::Space => {
                    let _ = self.play(DEMOS[sel].id, cell);
                }
                TKey::Char(c) if c.is_ascii_digit() => {
                    let i = (c as usize).wrapping_sub('1' as usize);
                    if i < n {
                        let _ = self.play(DEMOS[i].id, cell);
                    }
                }
                _ => {}
            }
            return;
        }
        let Some(p) = &mut self.player else { return };
        let was_playing = p.playing;
        match p.handle(key) {
            Outcome::None => {}
            Outcome::Exit => self.close(),
            Outcome::List => self.open_picker(),
            Outcome::Next => {
                let next = p.id.and_then(|id| DEMOS.iter().position(|d| d.id == id)).map_or(0, |i| (i + 1) % DEMOS.len());
                let _ = self.play(DEMOS[next].id, cell);
            }
        }
        // Resume / seek: restart the frame clock so no time is skipped.
        if self.player.as_ref().is_some_and(|p| p.playing != was_playing) {
            self.last_tick = None;
        }
    }

    /// Demos with their durations, for the list.
    pub fn catalog() -> Vec<(&'static DemoInfo, f64)> {
        static DURATIONS: std::sync::OnceLock<Vec<f64>> = std::sync::OnceLock::new();
        let d = DURATIONS.get_or_init(|| DEMOS.iter().map(|d| load(d).map(|c| c.duration()).unwrap_or(0.0)).collect());
        DEMOS.iter().zip(d.iter().copied()).collect()
    }
}
