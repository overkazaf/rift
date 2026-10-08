//! Font management: primary monospace font plus a lazily-loaded fallback chain.
//!
//! Glyph lookup order: primary font, then system fallbacks in priority order
//! (Nerd Font symbols, CJK, mono emoji/symbol fonts, ...). Fallback fonts are
//! only discovered (directory scan, no parsing) at startup and only opened and
//! parsed the first time a glyph misses in everything loaded before them.
//!
//! Limitations (documented, by design):
//! * Color emoji: macOS "Apple Color Emoji" is an sbix bitmap font and
//!   fontdue cannot rasterize it. Such fonts are skipped; emoji fall back to a
//!   monochrome font if one is installed (Noto Emoji, Symbola, Apple Symbols),
//!   otherwise a placeholder box outline is drawn so the cell is not blank.
//! * Ligatures: fontdue does no shaping, so ligatures are not supported.
//!   TODO(ligatures): out of scope; would need a shaping engine (rustybuzz).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use unicode_width::UnicodeWidthChar;

/// Whether `c` occupies two terminal cells.
pub fn is_wide(c: char) -> bool {
    c.width().unwrap_or(1) == 2
}

/// Priority of a fallback font file: lower sorts first. `(tier, sub)`.
type Rank = (u8, u8);

const TIER_SYMBOLS_NERD: u8 = 0;
const TIER_NERD: u8 = 1;
const TIER_CJK_MAC: u8 = 2;
const TIER_CJK_LINUX: u8 = 3;
const TIER_EMOJI_MONO: u8 = 4;
const TIER_APPLE_SYMBOLS: u8 = 5;
const TIER_MENLO: u8 = 6;
const TIER_DEJAVU: u8 = 7;

/// Rank a font file name as a fallback candidate, or `None` if it should not
/// be used. Pure function of the file name (testable without any fonts).
pub fn fallback_rank(file_name: &str) -> Option<Rank> {
    let ext_ok = ["ttf", "otf", "ttc"]
        .iter()
        .any(|e| file_name.to_ascii_lowercase().ends_with(&format!(".{e}")));
    if !ext_ok {
        return None;
    }
    let lower = file_name.to_ascii_lowercase();
    let stem = lower.rsplit_once('.').map(|(s, _)| s).unwrap_or(&lower);
    let squashed: String = stem.chars().filter(|c| !matches!(c, ' ' | '-' | '_')).collect();

    // Color bitmap emoji cannot be rasterized by fontdue.
    if squashed.contains("coloremoji") || squashed.contains("emojicolor") {
        return None;
    }

    const STYLE_WORDS: [&str; 12] = [
        "bold", "italic", "oblique", "light", "thin", "medium", "black", "heavy", "extra",
        "semi", "condensed", "retina",
    ];
    let plain_style = !STYLE_WORDS.iter().any(|w| squashed.contains(w));

    if squashed.contains("symbolsnerdfont") {
        // Prefer the "Mono" variant (single-cell glyphs).
        let sub = if squashed.contains("mono") { 0 } else { 1 };
        return Some((TIER_SYMBOLS_NERD, sub));
    }
    if squashed.contains("nerd") || file_name.contains("NF") {
        if !plain_style || squashed.contains("propo") {
            return None;
        }
        // "Mono" variants have glyphs sized for one cell; "NL" = no ligatures.
        let sub = if squashed.contains("nerdfontmono") || squashed.contains("nfm") {
            0
        } else {
            1
        };
        return Some((TIER_NERD, sub));
    }

    match lower.as_str() {
        "pingfang.ttc" => return Some((TIER_CJK_MAC, 0)),
        "hiragino sans gb.ttc" => return Some((TIER_CJK_MAC, 1)),
        "stheiti medium.ttc" => return Some((TIER_CJK_MAC, 2)),
        "apple symbols.ttf" => return Some((TIER_APPLE_SYMBOLS, 0)),
        "menlo.ttc" => return Some((TIER_MENLO, 0)),
        "dejavusans.ttf" => return Some((TIER_DEJAVU, 0)),
        "droidsansfallback.ttf" | "droidsansfallbackfull.ttf" => {
            return Some((TIER_CJK_LINUX, 2))
        }
        _ => {}
    }
    if lower.starts_with("notosanscjk") {
        // Prefer the Regular weight.
        let sub = if squashed.contains("regular") || lower.ends_with(".ttc") { 0 } else { 1 };
        return Some((TIER_CJK_LINUX, sub));
    }
    if lower.starts_with("wqy") {
        return Some((TIER_CJK_LINUX, 1));
    }
    if (squashed.starts_with("notoemoji") || squashed.starts_with("symbola")) && plain_style {
        return Some((TIER_EMOJI_MONO, 0));
    }
    None
}

/// System font directories searched for fallbacks.
fn system_font_dirs() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut dirs: Vec<PathBuf> = Vec::new();
    if cfg!(target_os = "macos") {
        dirs.push("/System/Library/Fonts".into());
        dirs.push("/Library/Fonts".into());
        if let Some(h) = &home {
            dirs.push(h.join("Library/Fonts"));
        }
    } else {
        dirs.push("/usr/share/fonts".into());
        if let Some(h) = &home {
            dirs.push(h.join(".local/share/fonts"));
            dirs.push(h.join(".fonts"));
        }
    }
    dirs
}

fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            if depth > 0 {
                walk(&p, depth - 1, out);
            }
        } else {
            out.push(p);
        }
    }
}

/// Scan `dirs` and return fallback candidates in priority order. Only file
/// names are inspected; nothing is opened or parsed. `exclude` (normally the
/// primary font) is skipped.
pub fn discover_fallbacks(dirs: &[PathBuf], exclude: Option<&Path>) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for d in dirs {
        walk(d, 4, &mut files);
    }
    let mut ranked: Vec<(Rank, PathBuf)> = files
        .into_iter()
        .filter(|p| exclude.map_or(true, |x| p != x))
        .filter_map(|p| {
            let name = p.file_name()?.to_str()?.to_string();
            fallback_rank(&name).map(|r| (r, p))
        })
        .collect();
    ranked.sort();

    // Bound the chain: Nerd tiers keep only the best two files each.
    let mut per_tier: HashMap<u8, usize> = HashMap::new();
    ranked
        .into_iter()
        .filter(|((tier, _), _)| {
            let n = per_tier.entry(*tier).or_insert(0);
            *n += 1;
            !(matches!(*tier, TIER_SYMBOLS_NERD | TIER_NERD) && *n > 2)
        })
        .map(|(_, p)| p)
        .collect()
}

enum Slot {
    Unloaded,
    Failed,
    /// Parsed font and the pixel size that makes its line height match the primary's.
    Loaded(fontdue::Font, f32),
}

struct Fallback {
    path: PathBuf,
    slot: Slot,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Source {
    Primary,
    Fallback(usize),
    None,
}

pub struct FontManager {
    primary: fontdue::Font,
    fallbacks: Vec<Fallback>,
    font_size: f32,
    primary_line_height: f32,
    pub cell_width: usize,
    pub cell_height: usize,
    pub baseline: usize,
    cache: HashMap<char, Vec<u8>>,
    wide_cache: HashMap<char, Vec<u8>>,
}

fn load_font(path: &Path, size: f32) -> Option<fontdue::Font> {
    let data = std::fs::read(path).ok()?;
    let settings = fontdue::FontSettings {
        collection_index: 0,
        scale: size,
        ..Default::default()
    };
    fontdue::Font::from_bytes(data, settings).ok()
}

impl FontManager {
    pub fn new(font_path: &str, font_size: f32) -> Self {
        let font_data = std::fs::read(font_path)
            .unwrap_or_else(|e| panic!("Failed to read font {font_path}: {e}"));
        log::info!("Loaded font: {font_path}");

        let settings = fontdue::FontSettings {
            collection_index: 0,
            scale: font_size,
            ..Default::default()
        };
        let font = fontdue::Font::from_bytes(font_data, settings)
            .expect("Failed to parse font");

        let metrics = font
            .horizontal_line_metrics(font_size)
            .expect("Font missing horizontal metrics");
        let cell_height = (metrics.ascent - metrics.descent + metrics.line_gap).ceil() as usize;
        let baseline = metrics.ascent.ceil() as usize;

        let (m_metrics, _) = font.rasterize('M', font_size);
        let cell_width = m_metrics.advance_width.ceil() as usize;

        let fallbacks = discover_fallbacks(&system_font_dirs(), Some(Path::new(font_path)))
            .into_iter()
            .map(|path| Fallback { path, slot: Slot::Unloaded })
            .collect::<Vec<_>>();
        log::debug!("Font fallback candidates: {}", fallbacks.len());

        Self {
            primary: font,
            fallbacks,
            font_size,
            primary_line_height: metrics.ascent - metrics.descent,
            cell_width,
            cell_height,
            baseline,
            cache: HashMap::new(),
            wide_cache: HashMap::new(),
        }
    }

    /// Open fallback `i` if not yet loaded. Returns whether it is usable.
    fn ensure_loaded(&mut self, i: usize) -> bool {
        if matches!(self.fallbacks[i].slot, Slot::Unloaded) {
            let path = self.fallbacks[i].path.clone();
            self.fallbacks[i].slot = match load_font(&path, self.font_size) {
                Some(f) => {
                    // Scale so the fallback's ascent+descent fits the primary's line height.
                    let size = match f.horizontal_line_metrics(self.font_size) {
                        Some(m) if m.ascent - m.descent > 0.0 => {
                            let lh = m.ascent - m.descent;
                            (self.font_size * self.primary_line_height / lh)
                                .clamp(self.font_size * 0.25, self.font_size * 1.5)
                        }
                        _ => self.font_size,
                    };
                    log::info!("Loaded fallback font: {} (size {size:.1})", path.display());
                    Slot::Loaded(f, size)
                }
                None => {
                    log::warn!("Failed to load fallback font: {}", path.display());
                    Slot::Failed
                }
            };
        }
        matches!(self.fallbacks[i].slot, Slot::Loaded(..))
    }

    fn pick(&mut self, c: char) -> Source {
        if self.primary.lookup_glyph_index(c) != 0 {
            return Source::Primary;
        }
        for i in 0..self.fallbacks.len() {
            if !self.ensure_loaded(i) {
                continue;
            }
            if let Slot::Loaded(f, _) = &self.fallbacks[i].slot {
                if f.lookup_glyph_index(c) != 0 {
                    return Source::Fallback(i);
                }
            }
        }
        Source::None
    }

    fn render(&mut self, c: char, wide: bool) -> Vec<u8> {
        let (cw, ch, baseline) = (self.cell_width, self.cell_height, self.baseline);
        let target_w = if wide { cw * 2 } else { cw };
        if (c as u32) < 0x20 || c == '\u{7f}' {
            return vec![0u8; target_w * ch];
        }
        match self.pick(c) {
            Source::Primary => {
                place_glyph(&self.primary, c, self.font_size, target_w, ch, baseline, wide, wide)
            }
            Source::Fallback(i) => {
                if let Slot::Loaded(f, size) = &self.fallbacks[i].slot {
                    place_glyph(f, c, *size, target_w, ch, baseline, wide, true)
                } else {
                    vec![0u8; target_w * ch]
                }
            }
            Source::None => {
                if wide || looks_like_emoji(c) {
                    placeholder_box(target_w, ch)
                } else {
                    vec![0u8; target_w * ch]
                }
            }
        }
    }

    /// Coverage bitmap `cell_width * cell_height` for a single-width cell.
    pub fn rasterize(&mut self, c: char) -> &[u8] {
        if !self.cache.contains_key(&c) {
            let bmp = self.render(c, false);
            self.cache.insert(c, bmp);
        }
        &self.cache[&c]
    }

    /// Coverage bitmap `2 * cell_width * cell_height` for a double-width
    /// character, scaled to fit and centered.
    pub fn rasterize_wide(&mut self, c: char) -> &[u8] {
        if !self.wide_cache.contains_key(&c) {
            let bmp = self.render(c, true);
            self.wide_cache.insert(c, bmp);
        }
        &self.wide_cache[&c]
    }
}

fn looks_like_emoji(c: char) -> bool {
    matches!(c as u32, 0x1F000..=0x1FAFF | 0x2600..=0x27BF | 0x2B50 | 0x2B55)
}

/// Hollow rectangle shown when no font can render an emoji / wide glyph.
fn placeholder_box(w: usize, h: usize) -> Vec<u8> {
    let mut bmp = vec![0u8; w * h];
    if w < 4 || h < 4 {
        return bmp;
    }
    let (x0, x1) = (1, w - 2);
    let (y0, y1) = (h / 8, h - 1 - h / 8);
    for x in x0..=x1 {
        bmp[y0 * w + x] = 180;
        bmp[y1 * w + x] = 180;
    }
    for y in y0..=y1 {
        bmp[y * w + x0] = 180;
        bmp[y * w + x1] = 180;
    }
    bmp
}

/// Rasterize `c` from `font` into a `target_w * ch` bitmap, baseline-aligned.
/// With `fit`, the glyph is shrunk if it exceeds the target box. With
/// `center`, the ink is centered horizontally.
#[allow(clippy::too_many_arguments)]
fn place_glyph(
    font: &fontdue::Font,
    c: char,
    size: f32,
    target_w: usize,
    ch: usize,
    baseline: usize,
    center: bool,
    fit: bool,
) -> Vec<u8> {
    let mut out = vec![0u8; target_w * ch];
    let (mut metrics, mut bitmap) = font.rasterize(c, size);
    if fit && metrics.width > 0 && metrics.height > 0 {
        let sx = target_w as f32 / metrics.width as f32;
        let sy = ch as f32 / metrics.height as f32;
        let s = sx.min(sy);
        if s < 1.0 {
            let (m, b) = font.rasterize(c, size * s);
            metrics = m;
            bitmap = b;
        }
    }
    if bitmap.is_empty() || metrics.width == 0 || metrics.height == 0 {
        return out;
    }
    let glyph_top = baseline as i32 - metrics.ymin - metrics.height as i32;
    let x_offset = if center {
        (target_w as i32 - metrics.width as i32) / 2
    } else {
        metrics.xmin
    }
    .max(0) as usize;

    for gy in 0..metrics.height {
        let cy = glyph_top + gy as i32;
        if cy < 0 || cy >= ch as i32 {
            continue;
        }
        for gx in 0..metrics.width {
            let cx = x_offset + gx;
            if cx >= target_w {
                continue;
            }
            if let Some(&v) = bitmap.get(gy * metrics.width + gx) {
                out[cy as usize * target_w + cx] = v;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rank_ordering_of_tiers() {
        let r = |n| fallback_rank(n).unwrap();
        assert!(r("Symbols Nerd Font Mono.ttf") < r("JetBrainsMonoNerdFont-Regular.ttf"));
        assert!(r("JetBrainsMonoNerdFont-Regular.ttf") < r("Hiragino Sans GB.ttc"));
        assert!(r("PingFang.ttc") < r("Hiragino Sans GB.ttc"));
        assert!(r("STHeiti Medium.ttc") < r("NotoSansCJK-Regular.ttc"));
        assert!(r("NotoSansCJK-Regular.ttc") < r("NotoEmoji-Regular.ttf"));
        assert!(r("NotoEmoji-Regular.ttf") < r("Apple Symbols.ttf"));
        assert!(r("Apple Symbols.ttf") < r("Menlo.ttc"));
        assert!(r("Menlo.ttc") < r("DejaVuSans.ttf"));
        assert!(r("MapleMono-NF-Regular.ttf").0 == TIER_NERD);
        assert!(r("FiraCodeNerdFontMono-Regular.ttf") < r("FiraCodeNerdFont-Regular.ttf"));
    }

    #[test]
    fn rank_filters_unwanted() {
        assert!(fallback_rank("JetBrainsMonoNerdFont-Bold.ttf").is_none());
        assert!(fallback_rank("JetBrainsMonoNerdFont-Italic.ttf").is_none());
        assert!(fallback_rank("JetBrainsMonoNerdFontPropo-Regular.ttf").is_none());
        assert!(fallback_rank("Apple Color Emoji.ttc").is_none());
        assert!(fallback_rank("NotoColorEmoji.ttf").is_none());
        assert!(fallback_rank("Arial.ttf").is_none());
        assert!(fallback_rank("readme.txt").is_none());
        assert!(fallback_rank("Menlo.ttc.bak").is_none());
    }

    #[test]
    fn discovery_sorts_and_excludes() {
        let base = std::env::temp_dir().join(format!("rift_font_test_{}", std::process::id()));
        let sub = base.join("truetype/dejavu");
        std::fs::create_dir_all(&sub).unwrap();
        for (d, n) in [
            (&sub, "DejaVuSans.ttf"),
            (&base, "Menlo.ttc"),
            (&base, "Foo-NF-Regular.ttf"),
            (&base, "Foo-NF-Bold.ttf"),
            (&base, "Random.ttf"),
        ] {
            std::fs::write(d.join(n), b"").unwrap();
        }
        let found = discover_fallbacks(&[base.clone()], None);
        let names: Vec<_> = found
            .iter()
            .map(|p| p.file_name().unwrap().to_str().unwrap().to_string())
            .collect();
        assert_eq!(names, ["Foo-NF-Regular.ttf", "Menlo.ttc", "DejaVuSans.ttf"]);

        let excl = base.join("Menlo.ttc");
        let found = discover_fallbacks(&[base.clone()], Some(&excl));
        assert_eq!(found.len(), 2);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn missing_dirs_are_ignored() {
        assert!(discover_fallbacks(&[PathBuf::from("/nonexistent/rift/fonts")], None).is_empty());
    }

    #[test]
    fn placeholder_is_hollow_and_nonblank() {
        let b = placeholder_box(16, 20);
        assert_eq!(b.len(), 16 * 20);
        assert!(b.iter().any(|&v| v > 0));
        assert_eq!(b[10 * 16 + 8], 0); // interior empty
    }

    #[test]
    fn width_classification() {
        assert!(is_wide('中'));
        assert!(is_wide('😀'));
        assert!(!is_wide('A'));
        assert!(looks_like_emoji('😀'));
        assert!(!looks_like_emoji('中'));
    }

    fn test_font() -> Option<&'static str> {
        [
            "/System/Library/Fonts/Menlo.ttc",
            "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
        ]
        .into_iter()
        .find(|p| Path::new(p).exists())
    }

    #[test]
    fn font_dependent_rasterize() {
        let Some(path) = test_font() else {
            eprintln!("skipping: no system font");
            return;
        };
        let mut fm = FontManager::new(path, 16.0);
        let (cw, ch) = (fm.cell_width, fm.cell_height);
        assert!(fm.rasterize('A').iter().any(|&v| v > 0));
        assert_eq!(fm.rasterize('A').len(), cw * ch);
        // Wide bitmap has double width; never panics even with no CJK font
        // (placeholder box is drawn in that case).
        let w = fm.rasterize_wide('中');
        assert_eq!(w.len(), cw * 2 * ch);
        assert!(w.iter().any(|&v| v > 0));
        // Emoji are never blank.
        assert!(fm.rasterize_wide('😀').iter().any(|&v| v > 0));
    }
}
