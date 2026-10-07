pub mod toml;

#[allow(dead_code)]
pub const VERSION: &str = "0.3.0";
#[allow(dead_code)]
pub const APP_NAME: &str = "rift";

pub fn mod_key() -> &'static str {
    #[cfg(target_os = "macos")]
    { "Cmd" }
    #[cfg(not(target_os = "macos"))]
    { "Ctrl" }
}

pub type Rgb = (u8, u8, u8);

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
            theme: Theme::catppuccin_mocha(),
            theme_name: "catppuccin-mocha".to_string(),
            llm: crate::ai::LlmConfig::default(),
        }
    }
}

impl Config {
    pub fn available_themes() -> &'static [&'static str] {
        &[
            "catppuccin-mocha", "hacker-green", "dracula", "nord",
            "solarized-dark", "tokyo-night", "cyberpunk", "gruvbox", "monokai",
        ]
    }

    pub fn theme_by_name(name: &str) -> Option<Theme> {
        match name {
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

#[derive(Clone)]
#[allow(dead_code)]
pub struct Theme {
    pub name: &'static str,
    pub fg: Rgb,
    pub bg: Rgb,
    pub cursor: Rgb,
    pub palette: [Rgb; 16],
}

impl Theme {
    pub fn catppuccin_mocha() -> Self {
        Self {
            name: "catppuccin-mocha",
            fg: (205, 214, 244),
            bg: (30, 30, 46),
            cursor: (245, 224, 220),
            palette: [
                (69, 71, 90), (243, 139, 168), (166, 227, 161), (249, 226, 175),
                (137, 180, 250), (245, 194, 231), (148, 226, 213), (186, 194, 222),
                (88, 91, 112), (243, 139, 168), (166, 227, 161), (249, 226, 175),
                (137, 180, 250), (245, 194, 231), (148, 226, 213), (205, 214, 244),
            ],
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
        }
    }

    pub fn dracula() -> Self {
        Self {
            name: "dracula",
            fg: (248, 248, 242), bg: (40, 42, 54), cursor: (248, 248, 242),
            palette: [
                (68, 71, 90), (255, 85, 85), (80, 250, 123), (241, 250, 140),
                (98, 114, 164), (255, 121, 198), (139, 233, 253), (248, 248, 242),
                (98, 114, 164), (255, 110, 110), (105, 255, 148), (255, 255, 165),
                (125, 140, 190), (255, 146, 218), (164, 255, 255), (255, 255, 255),
            ],
        }
    }

    pub fn nord() -> Self {
        Self {
            name: "nord",
            fg: (216, 222, 233), bg: (46, 52, 64), cursor: (216, 222, 233),
            palette: [
                (59, 66, 82), (191, 97, 106), (163, 190, 140), (235, 203, 139),
                (129, 161, 193), (180, 142, 173), (136, 192, 208), (229, 233, 240),
                (76, 86, 106), (191, 97, 106), (163, 190, 140), (235, 203, 139),
                (129, 161, 193), (180, 142, 173), (143, 188, 187), (236, 239, 244),
            ],
        }
    }

    pub fn solarized_dark() -> Self {
        Self {
            name: "solarized-dark",
            fg: (131, 148, 150), bg: (0, 43, 54), cursor: (131, 148, 150),
            palette: [
                (7, 54, 66), (220, 50, 47), (133, 153, 0), (181, 137, 0),
                (38, 139, 210), (211, 54, 130), (42, 161, 152), (238, 232, 213),
                (0, 43, 54), (203, 75, 22), (88, 110, 117), (101, 123, 131),
                (131, 148, 150), (108, 113, 196), (147, 161, 161), (253, 246, 227),
            ],
        }
    }

    pub fn tokyo_night() -> Self {
        Self {
            name: "tokyo-night",
            fg: (169, 177, 214), bg: (26, 27, 38), cursor: (192, 202, 245),
            palette: [
                (65, 72, 104), (247, 118, 142), (158, 206, 106), (224, 175, 104),
                (122, 162, 247), (187, 154, 247), (125, 207, 255), (169, 177, 214),
                (65, 72, 104), (247, 118, 142), (158, 206, 106), (224, 175, 104),
                (122, 162, 247), (187, 154, 247), (125, 207, 255), (192, 202, 245),
            ],
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
        }
    }

    pub fn gruvbox() -> Self {
        Self {
            name: "gruvbox",
            fg: (235, 219, 178), bg: (40, 40, 40), cursor: (235, 219, 178),
            palette: [
                (60, 56, 54), (204, 36, 29), (152, 151, 26), (215, 153, 33),
                (69, 133, 136), (177, 98, 134), (104, 157, 106), (168, 153, 132),
                (146, 131, 116), (251, 73, 52), (184, 187, 38), (250, 189, 47),
                (131, 165, 152), (211, 134, 155), (142, 192, 124), (235, 219, 178),
            ],
        }
    }

    pub fn monokai() -> Self {
        Self {
            name: "monokai",
            fg: (252, 252, 250), bg: (45, 42, 46), cursor: (252, 252, 250),
            palette: [
                (65, 62, 66), (255, 97, 136), (169, 220, 118), (255, 216, 102),
                (120, 220, 232), (171, 157, 242), (120, 220, 232), (252, 252, 250),
                (114, 112, 114), (255, 97, 136), (169, 220, 118), (255, 216, 102),
                (120, 220, 232), (171, 157, 242), (120, 220, 232), (255, 255, 255),
            ],
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
        if std::path::Path::new(path).exists() {
            log::info!("Using configured font path: {path}");
            return path.clone();
        }
        log::warn!("Font path not found: {path}, falling back");
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
