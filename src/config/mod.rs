pub mod toml;

#[allow(dead_code)]
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
#[allow(dead_code)]
pub const APP_NAME: &str = "rift";
#[allow(dead_code)]
pub const AUTHOR: &str = "overkazaf";
#[allow(dead_code)]
pub const REPO_URL: &str = "https://github.com/overkazaf/rift";

pub fn mod_key() -> &'static str {
    #[cfg(target_os = "macos")]
    { "Cmd" }
    #[cfg(not(target_os = "macos"))]
    { "Ctrl" }
}

/// Theme for brand-new configs. Existing config files without a `theme` key
/// keep [`LEGACY_THEME`] (see `toml::parse_toml_config`).
pub const DEFAULT_THEME: &str = "rift-neon";
pub const LEGACY_THEME: &str = "catppuccin-mocha";

pub type Rgb = (u8, u8, u8);

/// `[security] osc52`: what programs running in the terminal may do to the
/// system clipboard through OSC 52.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Osc52Policy {
    /// Reads and writes are both refused.
    Deny,
    /// Programs may set the clipboard (size-capped, with a toast); reads are refused.
    WriteOnly,
    /// Reads and writes are both honoured.
    Allow,
}

impl Osc52Policy {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "deny" | "off" | "none" => Some(Self::Deny),
            "write-only" | "write_only" | "writeonly" | "write" => Some(Self::WriteOnly),
            "allow" | "on" | "all" => Some(Self::Allow),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Deny => "deny",
            Self::WriteOnly => "write-only",
            Self::Allow => "allow",
        }
    }
    fn to_u8(self) -> u8 {
        match self { Self::Deny => 0, Self::WriteOnly => 1, Self::Allow => 2 }
    }
    fn from_u8(v: u8) -> Self {
        match v { 1 => Self::WriteOnly, 2 => Self::Allow, _ => Self::Deny }
    }
}

/// Process-wide OSC 52 policy consulted by the escape parser. It starts at
/// the most restrictive value so that nothing (tests, headless panes, code
/// that runs before the config is loaded) can touch the clipboard by
/// accident; `main` installs the configured policy at startup.
static OSC52_POLICY: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

pub fn osc52_policy() -> Osc52Policy {
    Osc52Policy::from_u8(OSC52_POLICY.load(std::sync::atomic::Ordering::Relaxed))
}

pub fn set_osc52_policy(p: Osc52Policy) {
    OSC52_POLICY.store(p.to_u8(), std::sync::atomic::Ordering::Relaxed);
}

/// Largest OSC 52 payload (base64 text, ~100 KB decoded) a program may put
/// on the clipboard.
pub const OSC52_MAX_B64: usize = 136_536;

/// Bounds applied to every value read from config.toml (and used when saving).
pub const FONT_SIZE_RANGE: (f32, f32) = (6.0, 72.0);
pub const OPACITY_RANGE: (f32, f32) = (0.2, 1.0);
pub const COLS_RANGE: (i64, i64) = (10, 1000);
pub const ROWS_RANGE: (i64, i64) = (10, 500);

pub struct Config {
    pub font_size: f32,
    pub font_family: Option<String>,
    pub font_path: Option<String>,
    pub cols: u16,
    pub rows: u16,
    pub opacity: f32,
    pub theme: Theme,
    pub theme_name: String,
    pub llm: crate::ai::LlmConfig,
    /// AI → Auto Fix Suggestions: propose a fix when a command fails.
    pub ai_auto_fix: bool,
    /// AI → # Natural Language: `# request` + Enter at the prompt generates a command.
    pub ai_nl_hash: bool,
    /// Active visual effect (`effect = "crt"` in config.toml); `None` = off.
    pub effect: Option<crate::effects::EffectKind>,
    /// Effect strength 0.0..=1.0 (`effect_intensity`).
    pub effect_intensity: f32,
    /// Play the short RIFT logo reveal at startup (`startup_animation`).
    pub startup_animation: bool,
    /// Reopen the previous session's windows, tabs and splits at startup
    /// (`restore_session`). Off: every launch starts with one plain pane.
    pub restore_session: bool,
    /// Quietly check GitHub for a newer release at startup, at most once a
    /// day (`check_updates`). Off by default: no network unless asked.
    pub check_updates: bool,
    /// Draw bold text with the bright palette variant (colors 0-7 -> 8-15).
    pub bold_is_bright: bool,
    /// Which renderer draws the terminal (`renderer = "auto" | "gpu" | "cpu"`).
    pub renderer: RendererMode,
    /// Programming ligatures (`font_ligatures`). `None` = default: on with
    /// the GPU text renderer, off with the CPU one.
    pub font_ligatures: Option<bool>,
    /// Scrollback history per pane in lines (`scrollback_lines`, default 10000).
    pub scrollback_lines: usize,
    /// Notify when a command (OSC 133 block) ran at least this many seconds and
    /// its pane is not in view (`notify_after_secs`, default 10; 0 disables).
    pub notify_after_secs: f64,
    /// Keyboard settings: `shift_enter`, `option_as_meta`, `[keybindings]` (read-only here).
    pub input: crate::input::InputConfig,
    /// `[ai] consent`: the user's answer to the one-time cloud-AI prompt.
    pub ai_consent: crate::ai::consent::Consent,
    /// `[llm]` was written in config.toml (an explicit opt-in to that endpoint).
    pub llm_explicit: bool,
    /// `[security] osc52`.
    pub osc52: Osc52Policy,
    /// `[mcp]`: built-in MCP server for coding agents.
    pub mcp: crate::mcp::McpConfig,
    /// `[agents]`: Agent Mission Control.
    pub agents: crate::agents::AgentsConfig,
    /// `[ai] fix_provider / nl_provider / chat_provider`.
    pub ai_routing: crate::ai::local::Routing,
    /// Write `[llm] provider/model/api_url` on save (set once the user picked a
    /// model; never includes `api_key`).
    pub llm_persist: bool,
    /// The user opted into sending data to a remote endpoint (cloud consent,
    /// or an explicit non-local `[llm]`). A local-only setup never has this,
    /// so a stray API key in the environment cannot be switched to.
    pub cloud_opt_in: bool,
}

/// `renderer` setting. Only has an effect in builds with `--features gpu`;
/// without it the CPU renderer is the only one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum RendererMode {
    /// GPU when a usable adapter exists, otherwise CPU.
    #[default]
    Auto,
    /// Prefer the GPU (falls back to CPU, loudly, if it cannot start).
    Gpu,
    /// Never touch the GPU.
    Cpu,
}

impl RendererMode {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "gpu" => Some(Self::Gpu),
            "cpu" | "software" => Some(Self::Cpu),
            _ => None,
        }
    }

    #[cfg_attr(not(feature = "gpu"), allow(dead_code))]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Gpu => "gpu",
            Self::Cpu => "cpu",
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            font_size: 15.0,
            font_family: None,
            font_path: None,
            cols: 120,
            rows: 36,
            opacity: 0.92,
            theme: Theme::rift_neon(),
            theme_name: DEFAULT_THEME.to_string(),
            llm: crate::ai::LlmConfig::default(),
            // Off until the user has consented to AI (see `ai::consent`).
            ai_auto_fix: false,
            ai_nl_hash: true,
            effect: None,
            effect_intensity: crate::effects::DEFAULT_INTENSITY,
            startup_animation: true,
            restore_session: false,
            check_updates: false,
            bold_is_bright: false,
            renderer: RendererMode::Auto,
            font_ligatures: None,
            scrollback_lines: 10_000,
            notify_after_secs: 10.0,
            input: crate::input::InputConfig::default(),
            ai_consent: crate::ai::consent::Consent::Unset,
            llm_explicit: false,
            osc52: Osc52Policy::WriteOnly,
            mcp: crate::mcp::McpConfig::default(),
            agents: crate::agents::AgentsConfig::default(),
            ai_routing: crate::ai::local::Routing::default(),
            llm_persist: false,
            cloud_opt_in: false,
        }
    }
}

impl Config {
    pub fn available_themes() -> &'static [&'static str] {
        &[
            "rift-neon", "catppuccin-mocha", "hacker-green", "dracula", "nord",
            "solarized-dark", "tokyo-night", "cyberpunk", "gruvbox", "monokai",
        ]
    }

    pub fn theme_by_name(name: &str) -> Option<Theme> {
        match name {
            "rift-neon" => Some(Theme::rift_neon()),
            "catppuccin-mocha" => Some(Theme::catppuccin_mocha()),
            "hacker-green" => Some(Theme::hacker_green()),
            "dracula" => Some(Theme::dracula()),
            "nord" => Some(Theme::nord()),
            "solarized-dark" => Some(Theme::solarized_dark()),
            "tokyo-night" => Some(Theme::tokyo_night()),
            "cyberpunk" => Some(Theme::cyberpunk()),
            "gruvbox" => Some(Theme::gruvbox()),
            "monokai" => Some(Theme::monokai()),
            _ => None,
        }
    }
}

/// Minimum WCAG contrast ratio kept for selected text before it is swapped
/// for the theme fg / bg.
pub const MIN_SELECTION_CONTRAST: f32 = 2.5;

/// `a` moved toward `b` by `t` (0.0 = a, 1.0 = b).
pub fn mix_rgb(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round().clamp(0.0, 255.0) as u8;
    (m(a.0, b.0), m(a.1, b.1), m(a.2, b.2))
}

/// WCAG relative luminance (0.0 black .. 1.0 white).
pub fn relative_luminance(c: Rgb) -> f32 {
    let lin = |v: u8| {
        let v = v as f32 / 255.0;
        if v <= 0.040_45 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * lin(c.0) + 0.7152 * lin(c.1) + 0.0722 * lin(c.2)
}

/// WCAG contrast ratio between two colors (1.0 ..= 21.0).
pub fn contrast_ratio(a: Rgb, b: Rgb) -> f32 {
    let (la, lb) = (relative_luminance(a), relative_luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

#[derive(Clone)]
#[allow(dead_code)]
pub struct Theme {
    pub name: &'static str,
    pub fg: Rgb,
    pub bg: Rgb,
    pub cursor: Rgb,
    pub palette: [Rgb; 16],
    /// Background of selected cells (`None` = derived from accent and bg, see
    /// [`Theme::selection_colors`]).
    pub selection_bg: Option<Rgb>,
    /// Text color of selected cells (`None` = keep each cell's own color,
    /// swapped for a readable one when the contrast is too low).
    pub selection_fg: Option<Rgb>,
}

impl Theme {
    /// Flagship theme: deep blue-black, magenta accent, cyan secondary.
    pub fn rift_neon() -> Self {
        Self {
            name: "rift-neon",
            fg: (214, 224, 255), bg: (8, 10, 24), cursor: (255, 46, 190),
            palette: [
                (22, 26, 56), (255, 64, 112), (0, 240, 160), (255, 214, 64),
                (72, 120, 255), (255, 46, 190), (0, 229, 255), (190, 202, 238),
                (54, 62, 112), (255, 110, 150), (80, 255, 190), (255, 232, 120),
                (120, 160, 255), (255, 110, 220), (110, 242, 255), (236, 242, 255),
            ],
            selection_bg: None,
            selection_fg: None,
        }
    }

    /// The accent colour used for chrome, logo and effects. By convention it
    /// is the cursor colour; every built-in theme keeps it clearly chromatic.
    pub fn accent(&self) -> Rgb {
        self.cursor
    }

    /// Background of selected cells: the configured `selection_bg`, else the
    /// accent blended into the background. Tinted like the theme and clearly
    /// separated from the background on dark and light themes alike.
    pub fn selection_bg(&self) -> Rgb {
        if let Some(c) = self.selection_bg {
            return c;
        }
        let light = relative_luminance(self.bg) > 0.4;
        // Back off toward the bg until the theme's own text reads well on it
        // (low-contrast themes such as solarized), but stay visible.
        let mut t = if light { 0.28 } else { 0.32 };
        loop {
            let c = mix_rgb(self.bg, self.accent(), t);
            if t <= 0.2 || contrast_ratio(self.fg, c) >= 4.0 {
                return c;
            }
            t -= 0.02;
        }
    }

    /// Text color of a selected cell whose own color is `fg`, drawn on
    /// `sel_bg`: the configured `selection_fg`, else `fg` itself as long as
    /// it stays readable, else whichever of theme fg / bg contrasts more
    /// (white / black if neither is readable).
    pub fn selection_text(&self, fg: Rgb, sel_bg: Rgb) -> Rgb {
        if let Some(c) = self.selection_fg {
            return c;
        }
        if contrast_ratio(fg, sel_bg) >= MIN_SELECTION_CONTRAST {
            return fg;
        }
        let theme = if contrast_ratio(self.fg, sel_bg) >= contrast_ratio(self.bg, sel_bg) { self.fg } else { self.bg };
        if contrast_ratio(theme, sel_bg) >= MIN_SELECTION_CONTRAST {
            return theme;
        }
        let (white, black) = ((255, 255, 255), (0, 0, 0));
        if contrast_ratio(white, sel_bg) >= contrast_ratio(black, sel_bg) { white } else { black }
    }

    pub fn catppuccin_mocha() -> Self {
        Self {
            name: "catppuccin-mocha",
            fg: (205, 214, 244),
            bg: (30, 30, 46),
            cursor: (203, 166, 247),
            palette: [
                (69, 71, 90), (243, 139, 168), (166, 227, 161), (249, 226, 175),
                (137, 180, 250), (245, 194, 231), (148, 226, 213), (186, 194, 222),
                (88, 91, 112), (243, 139, 168), (166, 227, 161), (249, 226, 175),
                (137, 180, 250), (245, 194, 231), (148, 226, 213), (205, 214, 244),
            ],
            selection_bg: None,
            selection_fg: None,
        }
    }

    pub fn hacker_green() -> Self {
        Self {
            name: "hacker-green",
            fg: (0, 255, 65), bg: (0, 10, 2), cursor: (0, 255, 65),
            palette: [
                (0, 40, 10), (255, 0, 0), (0, 255, 65), (255, 255, 0),
                (0, 120, 255), (255, 0, 255), (0, 255, 255), (0, 200, 50),
                (0, 80, 20), (255, 80, 80), (80, 255, 120), (255, 255, 80),
                (80, 180, 255), (255, 80, 255), (80, 255, 255), (0, 255, 65),
            ],
            selection_bg: None,
            selection_fg: None,
        }
    }

    pub fn dracula() -> Self {
        Self {
            name: "dracula",
            fg: (248, 248, 242), bg: (40, 42, 54), cursor: (189, 147, 249),
            palette: [
                (68, 71, 90), (255, 85, 85), (80, 250, 123), (241, 250, 140),
                (98, 114, 164), (255, 121, 198), (139, 233, 253), (248, 248, 242),
                (98, 114, 164), (255, 110, 110), (105, 255, 148), (255, 255, 165),
                (125, 140, 190), (255, 146, 218), (164, 255, 255), (255, 255, 255),
            ],
            selection_bg: None,
            selection_fg: None,
        }
    }

    pub fn nord() -> Self {
        Self {
            name: "nord",
            fg: (216, 222, 233), bg: (46, 52, 64), cursor: (136, 192, 208),
            palette: [
                (59, 66, 82), (191, 97, 106), (163, 190, 140), (235, 203, 139),
                (129, 161, 193), (180, 142, 173), (136, 192, 208), (229, 233, 240),
                (76, 86, 106), (191, 97, 106), (163, 190, 140), (235, 203, 139),
                (129, 161, 193), (180, 142, 173), (143, 188, 187), (236, 239, 244),
            ],
            selection_bg: None,
            selection_fg: None,
        }
    }

    pub fn solarized_dark() -> Self {
        Self {
            name: "solarized-dark",
            fg: (131, 148, 150), bg: (0, 43, 54), cursor: (38, 139, 210),
            palette: [
                (7, 54, 66), (220, 50, 47), (133, 153, 0), (181, 137, 0),
                (38, 139, 210), (211, 54, 130), (42, 161, 152), (238, 232, 213),
                (0, 43, 54), (203, 75, 22), (88, 110, 117), (101, 123, 131),
                (131, 148, 150), (108, 113, 196), (147, 161, 161), (253, 246, 227),
            ],
            selection_bg: None,
            selection_fg: None,
        }
    }

    pub fn tokyo_night() -> Self {
        Self {
            name: "tokyo-night",
            fg: (169, 177, 214), bg: (26, 27, 38), cursor: (122, 162, 247),
            palette: [
                (65, 72, 104), (247, 118, 142), (158, 206, 106), (224, 175, 104),
                (122, 162, 247), (187, 154, 247), (125, 207, 255), (169, 177, 214),
                (65, 72, 104), (247, 118, 142), (158, 206, 106), (224, 175, 104),
                (122, 162, 247), (187, 154, 247), (125, 207, 255), (192, 202, 245),
            ],
            selection_bg: None,
            selection_fg: None,
        }
    }

    pub fn cyberpunk() -> Self {
        Self {
            name: "cyberpunk",
            fg: (240, 240, 255), bg: (10, 0, 20), cursor: (255, 0, 255),
            palette: [
                (20, 10, 40), (255, 0, 80), (0, 255, 136), (255, 230, 0),
                (0, 150, 255), (255, 0, 255), (0, 255, 255), (200, 200, 220),
                (40, 20, 80), (255, 50, 120), (50, 255, 170), (255, 255, 50),
                (50, 180, 255), (255, 50, 255), (50, 255, 255), (240, 240, 255),
            ],
            selection_bg: None,
            selection_fg: None,
        }
    }

    pub fn gruvbox() -> Self {
        Self {
            name: "gruvbox",
            fg: (235, 219, 178), bg: (40, 40, 40), cursor: (254, 128, 25),
            palette: [
                (60, 56, 54), (204, 36, 29), (152, 151, 26), (215, 153, 33),
                (69, 133, 136), (177, 98, 134), (104, 157, 106), (168, 153, 132),
                (146, 131, 116), (251, 73, 52), (184, 187, 38), (250, 189, 47),
                (131, 165, 152), (211, 134, 155), (142, 192, 124), (235, 219, 178),
            ],
            selection_bg: None,
            selection_fg: None,
        }
    }

    pub fn monokai() -> Self {
        Self {
            name: "monokai",
            fg: (252, 252, 250), bg: (45, 42, 46), cursor: (255, 216, 102),
            palette: [
                (65, 62, 66), (255, 97, 136), (169, 220, 118), (255, 216, 102),
                (120, 220, 232), (171, 157, 242), (120, 220, 232), (252, 252, 250),
                (114, 112, 114), (255, 97, 136), (169, 220, 118), (255, 216, 102),
                (120, 220, 232), (171, 157, 242), (120, 220, 232), (255, 255, 255),
            ],
            selection_bg: None,
            selection_fg: None,
        }
    }

    pub fn resolve_indexed(&self, idx: u8) -> Rgb {
        if (idx as usize) < 16 {
            self.palette[idx as usize]
        } else if idx < 232 {
            let i = idx - 16;
            ((i / 36) * 51, ((i / 6) % 6) * 51, (i % 6) * 51)
        } else {
            let v = (idx - 232) * 10 + 8;
            (v, v, v)
        }
    }
}

pub fn resolve_font_path(config: &Config) -> String {
    if let Some(ref path) = config.font_path {
        if font_file_ok(path) {
            log::info!("Using configured font path: {path}");
            return path.clone();
        }
        log::warn!("Font path is missing or not a usable font file: {path}, falling back");
    }

    if let Some(ref family) = config.font_family {
        if let Some(path) = find_font_by_name(family) {
            log::info!("Found font '{family}' at {path}");
            return path;
        }
        log::warn!("Font family not found: {family}, falling back to default");
    }

    find_font_path()
}

/// Is `path` a regular file that parses as a font (TTF/OTF/TTC)?
fn font_file_ok(path: &str) -> bool {
    let p = std::path::Path::new(path);
    if !p.is_file() {
        return false;
    }
    match std::fs::read(p) {
        Ok(data) => ttf_parser::Face::parse(&data, 0).is_ok(),
        Err(_) => false,
    }
}

fn find_font_by_name(name: &str) -> Option<String> {
    let search_dirs = [
        "/Library/Fonts",
        "/System/Library/Fonts",
        "/System/Library/Fonts/Supplemental",
        "/usr/share/fonts",
        "/usr/share/fonts/truetype",
        "/usr/local/share/fonts",
    ];
    if let Ok(home) = std::env::var("HOME") {
        let user_fonts = format!("{home}/Library/Fonts");
        if std::path::Path::new(&user_fonts).exists() {
            if let Some(found) = search_dir_for_font(&user_fonts, name) {
                return Some(found);
            }
        }
    }
    for dir in &search_dirs {
        if let Some(found) = search_dir_for_font(dir, name) {
            return Some(found);
        }
    }
    None
}

fn search_dir_for_font(dir: &str, name: &str) -> Option<String> {
    let dir_path = std::path::Path::new(dir);
    if !dir_path.exists() { return None; }
    let name_lower = name.to_lowercase().replace(' ', "");
    let variants = [
        format!("{name_lower}-regular"),
        name_lower.clone(),
        format!("{name_lower}-medium"),
        format!("{name_lower}nf-regular"),
        format!("{name_lower}nf"),
    ];
    let entries = std::fs::read_dir(dir_path).ok()?;
    for entry in entries.flatten() {
        let file_name = entry.file_name().to_string_lossy().to_lowercase();
        if !file_name.ends_with(".ttf") && !file_name.ends_with(".otf") && !file_name.ends_with(".ttc") {
            continue;
        }
        for variant in &variants {
            if file_name.contains(variant.as_str()) {
                return Some(entry.path().display().to_string());
            }
        }
    }
    None
}

pub fn find_font_path() -> String {
    let candidates = [
        "/System/Library/Fonts/Menlo.ttc",
        "/System/Library/Fonts/SFMono.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
        "/usr/share/fonts/liberation-mono/LiberationMono-Regular.ttf",
        "/usr/share/fonts/TTF/DejaVuSansMono.ttf",
    ];
    for path in &candidates {
        if std::path::Path::new(path).exists() {
            return path.to_string();
        }
    }
    log::error!("No monospace font found!");
    candidates[0].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_theme_resolves_and_has_a_chromatic_accent() {
        for name in Config::available_themes() {
            let t = Config::theme_by_name(name).unwrap_or_else(|| panic!("{name} unresolved"));
            assert_eq!(t.name, *name);
            let a = t.accent();
            let chroma = a.0.max(a.1).max(a.2) - a.0.min(a.1).min(a.2);
            assert!(chroma >= 60, "{name}: accent {a:?} too grey");
            assert_ne!(a, t.bg, "{name}");
        }
        assert_eq!(Config::available_themes().len(), 10);
    }

    #[test]
    fn selection_colors_are_visible_and_text_stays_readable() {
        let light = Theme { name: "light", fg: (40, 40, 40), bg: (250, 250, 248), cursor: (38, 110, 220), ..Theme::nord() };
        let themes = Config::available_themes().iter().map(|n| Config::theme_by_name(n).unwrap()).chain([light]);
        for t in themes {
            let sel = t.selection_bg();
            // Clearly separated from the background, text readable on it.
            assert!(contrast_ratio(sel, t.bg) >= 1.3, "{}: selection {sel:?} too close to bg", t.name);
            assert!(contrast_ratio(t.selection_text(t.fg, sel), sel) >= MIN_SELECTION_CONTRAST, "{}", t.name);
            for c in t.palette {
                let txt = t.selection_text(c, sel);
                assert!(txt == c || contrast_ratio(txt, sel) >= contrast_ratio(c, sel), "{}", t.name);
            }
            // Low-contrast text is swapped for the theme fg / bg.
            assert_ne!(t.selection_text(sel, sel), sel, "{}", t.name);
        }
        // Explicit colors win.
        let t = Theme { selection_bg: Some((1, 2, 3)), selection_fg: Some((4, 5, 6)), ..Theme::nord() };
        assert_eq!(t.selection_bg(), (1, 2, 3));
        assert_eq!(t.selection_text((200, 200, 200), (1, 2, 3)), (4, 5, 6));
    }

    #[test]
    fn new_configs_default_to_rift_neon() {
        let c = Config::default();
        assert_eq!(c.theme_name, "rift-neon");
        assert_eq!(c.theme.name, "rift-neon");
        assert_eq!(c.effect, None);
        assert!(c.startup_animation);
    }
}
