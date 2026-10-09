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
//! * Ligatures: fontdue does no shaping. Runs of same-style primary-font cells
//!   are shaped with rustybuzz in `renderer::shape`; glyphs that differ from
//!   the plain cmap mapping (contextual alternates, ligatures) are rasterized
//!   by glyph id through [`FontManager::rasterize_gid`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use unicode_width::UnicodeWidthChar;

use crate::terminal::grid::UnderlineStyle;

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

/// A fontdue font plus a corrected glyph map for U+0080..=U+00FF.
///
/// fontdue 0.9 merges every cmap subtable, so fonts that also ship a Mac
/// Roman subtable (e.g. Menlo) map Latin-1 code points to the wrong glyphs
/// (`·` renders as `∑`, `×` as `◊`, `é` as something else). We resolve that
/// range with ttf-parser, which honours the Unicode subtable, and rasterize
/// by glyph index.
struct GFont {
    font: fontdue::Font,
    latin1: [u16; 128],
    /// Raw font bytes (primary / styled faces only) for the shaper.
    data: Option<&'static [u8]>,
    index: u32,
}

/// Read a font file once and keep it for the lifetime of the process, so
/// parsed faces (rustybuzz) can borrow it. Cached per path: re-initialising
/// the font (zoom, display scale change) does not leak another copy.
fn static_font_data(path: &Path) -> Option<&'static [u8]> {
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, &'static [u8]>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut map = cache.lock().ok()?;
    if let Some(d) = map.get(path) {
        return Some(d);
    }
    let bytes = std::fs::read(path).ok()?;
    let leaked: &'static [u8] = Box::leak(bytes.into_boxed_slice());
    map.insert(path.to_path_buf(), leaked);
    Some(leaked)
}

impl GFont {
    fn from_static(data: &'static [u8], size: f32, index: u32) -> Option<Self> {
        let mut f = Self::from_bytes(data, size, index)?;
        f.data = Some(data);
        Some(f)
    }

    fn from_bytes(data: &[u8], size: f32, index: u32) -> Option<Self> {
        let settings = fontdue::FontSettings {
            collection_index: index,
            scale: size,
            ..Default::default()
        };
        let font = fontdue::Font::from_bytes(data, settings).ok()?;
        let mut latin1 = [0u16; 128];
        match ttf_parser::Face::parse(data, index) {
            Ok(face) => {
                for (i, slot) in latin1.iter_mut().enumerate() {
                    if let Some(c) = char::from_u32(0x80 + i as u32) {
                        *slot = face.glyph_index(c).map_or(0, |g| g.0);
                    }
                }
            }
            Err(_) => {
                for (i, slot) in latin1.iter_mut().enumerate() {
                    if let Some(c) = char::from_u32(0x80 + i as u32) {
                        *slot = font.lookup_glyph_index(c);
                    }
                }
            }
        }
        Some(Self { font, latin1, data: None, index })
    }

    fn glyph(&self, c: char) -> u16 {
        match c as u32 {
            cp @ 0x80..=0xFF => self.latin1[(cp - 0x80) as usize],
            _ => self.font.lookup_glyph_index(c),
        }
    }

    fn rasterize(&self, c: char, size: f32) -> (fontdue::Metrics, Vec<u8>) {
        self.font.rasterize_indexed(self.glyph(c), size)
    }
}

enum Slot {
    Unloaded,
    Failed,
    /// Parsed font and the pixel size that makes its line height match the primary's.
    Loaded(GFont, f32),
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
    primary: GFont,
    fallbacks: Vec<Fallback>,
    font_size: f32,
    primary_line_height: f32,
    pub cell_width: usize,
    pub cell_height: usize,
    pub baseline: usize,
    cache: HashMap<(char, u8), Vec<u8>>,
    wide_cache: HashMap<(char, u8), Vec<u8>>,
    mark_cache: HashMap<(char, bool), Vec<u8>>,
    primary_path: PathBuf,
    /// Lazily loaded bold / italic / bold-italic faces (index = style bits;
    /// slot 0 unused). Outer `None` = not tried yet, inner `None` = no such
    /// face (synthesized instead).
    styled: [Option<Option<GFont>>; 4],
    /// Underline / strikethrough / overline geometry in cell pixels.
    pub deco: DecoMetrics,
}

fn load_font(path: &Path, size: f32) -> Option<GFont> {
    let data = std::fs::read(path).ok()?;
    GFont::from_bytes(&data, size, 0)
}

impl FontManager {
    pub fn new(font_path: &str, font_size: f32) -> Self {
        let font_data = static_font_data(Path::new(font_path))
            .unwrap_or_else(|| panic!("Failed to read font {font_path}"));
        log::info!("Loaded font: {font_path}");

        let font = GFont::from_static(font_data, font_size, 0).expect("Failed to parse font");

        let metrics = font
            .font
            .horizontal_line_metrics(font_size)
            .expect("Font missing horizontal metrics");
        let cell_height = (metrics.ascent - metrics.descent + metrics.line_gap).ceil() as usize;
        let baseline = metrics.ascent.ceil() as usize;

        let (m_metrics, _) = font.font.rasterize('M', font_size);
        let cell_width = m_metrics.advance_width.ceil() as usize;
        let deco = deco_metrics(font_data, font_size, baseline, cell_height, cell_width);

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
            mark_cache: HashMap::new(),
            primary_path: PathBuf::from(font_path),
            styled: [None, None, None, None],
            deco,
        }
    }

    /// Load (once) the real bold / italic / bold-italic face for `slot`.
    /// Returns whether one exists.
    fn ensure_styled(&mut self, slot: usize) -> bool {
        if slot == 0 {
            return true;
        }
        if self.styled[slot].is_none() {
            let (b, i) = (slot & BOLD as usize != 0, slot & ITALIC as usize != 0);
            let face = find_face(&self.primary_path, b, i).and_then(|(path, idx)| {
                let data = static_font_data(&path)?;
                let f = GFont::from_static(data, self.font_size, idx)?;
                log::info!("Loaded styled face (bold={b}, italic={i}): {} #{idx}", path.display());
                Some(f)
            });
            self.styled[slot] = Some(face);
        }
        matches!(self.styled[slot], Some(Some(_)))
    }

    /// Choose the face for `style`: `(slot, synth_bold, synth_italic)`.
    fn face_for(&mut self, style: u8) -> (usize, bool, bool) {
        let (wb, wi) = (style & BOLD != 0, style & ITALIC != 0);
        let exact = style as usize;
        if self.ensure_styled(exact) {
            return (exact, false, false);
        }
        if wb && wi {
            if self.ensure_styled(BOLD as usize) {
                return (BOLD as usize, false, true);
            }
            if self.ensure_styled(ITALIC as usize) {
                return (ITALIC as usize, true, false);
            }
        }
        (0, wb, wi)
    }

    /// Whether the primary font ships a real face for these style bits.
    #[allow(dead_code)]
    pub fn has_real_face(&mut self, style: u8) -> bool {
        self.ensure_styled(style as usize)
    }

    /// Open fallback `i` if not yet loaded. Returns whether it is usable.
    fn ensure_loaded(&mut self, i: usize) -> bool {
        if matches!(self.fallbacks[i].slot, Slot::Unloaded) {
            let path = self.fallbacks[i].path.clone();
            self.fallbacks[i].slot = match load_font(&path, self.font_size) {
                Some(f) => {
                    // Scale so the fallback's ascent+descent fits the primary's line height.
                    let size = match f.font.horizontal_line_metrics(self.font_size) {
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
        if self.primary.glyph(c) != 0 {
            return Source::Primary;
        }
        for i in 0..self.fallbacks.len() {
            if !self.ensure_loaded(i) {
                continue;
            }
            if let Slot::Loaded(f, _) = &self.fallbacks[i].slot {
                if f.glyph(c) != 0 {
                    return Source::Fallback(i);
                }
            }
        }
        Source::None
    }

    fn render(&mut self, c: char, wide: bool, style: u8) -> Vec<u8> {
        let (cw, ch, baseline) = (self.cell_width, self.cell_height, self.baseline);
        let target_w = if wide { cw * 2 } else { cw };
        if (c as u32) < 0x20 || c == '\u{7f}' {
            return vec![0u8; target_w * ch];
        }
        let size = self.font_size;
        let (mut sb, mut si) = (style & BOLD != 0, style & ITALIC != 0);
        let mut out: Vec<u8>;
        match self.pick(c) {
            Source::Primary => {
                let (slot, fb, fi) = self.face_for(style);
                let styled = if slot == 0 { None } else { self.styled[slot].as_ref().and_then(|f| f.as_ref()) };
                match styled {
                    Some(f) if f.glyph(c) != 0 => {
                        out = place_glyph(f, c, size, target_w, ch, baseline, wide, wide);
                        sb = fb;
                        si = fi;
                    }
                    _ => {
                        out = place_glyph(&self.primary, c, size, target_w, ch, baseline, wide, wide);
                    }
                }
            }
            Source::Fallback(i) => {
                if let Slot::Loaded(f, fsize) = &self.fallbacks[i].slot {
                    out = place_glyph(f, c, *fsize, target_w, ch, baseline, wide, true);
                } else {
                    out = vec![0u8; target_w * ch];
                }
            }
            Source::None => {
                out = if wide || looks_like_emoji(c) {
                    placeholder_box(target_w, ch)
                } else {
                    vec![0u8; target_w * ch]
                };
                sb = false;
                si = false;
            }
        }
        if synth_ok(c) {
            if sb {
                embolden(&mut out, target_w, ch);
            }
            if si && !wide {
                shear(&mut out, target_w, ch, baseline);
            }
        }
        out
    }

    /// Coverage bitmap `cell_width * cell_height` for a single-width cell.
    pub fn rasterize(&mut self, c: char) -> &[u8] {
        self.rasterize_styled(c, 0)
    }

    /// Like [`rasterize`] for a style (`BOLD | ITALIC` bits).
    pub fn rasterize_styled(&mut self, c: char, style: u8) -> &[u8] {
        let key = (c, style);
        if !self.cache.contains_key(&key) {
            let bmp = self.render(c, false, style);
            self.cache.insert(key, bmp);
        }
        &self.cache[&key]
    }

    /// Coverage bitmap `2 * cell_width * cell_height` for a double-width
    /// character, scaled to fit and centered.
    pub fn rasterize_wide(&mut self, c: char) -> &[u8] {
        self.rasterize_wide_styled(c, 0)
    }

    pub fn rasterize_wide_styled(&mut self, c: char, style: u8) -> &[u8] {
        let key = (c, style);
        if !self.wide_cache.contains_key(&key) {
            let bmp = self.render(c, true, style);
            self.wide_cache.insert(key, bmp);
        }
        &self.wide_cache[&key]
    }
}

impl FontManager {
    /// Coverage bitmap of a combining mark, sized for the base glyph's box
    /// (`cell_width` or twice that when `wide`), positioned using the mark's
    /// own metrics so it can be overlaid on the base glyph.
    pub fn rasterize_mark(&mut self, c: char, wide: bool) -> &[u8] {
        let key = (c, wide);
        if !self.mark_cache.contains_key(&key) {
            let bmp = self.render_mark(c, wide);
            self.mark_cache.insert(key, bmp);
        }
        &self.mark_cache[&key]
    }

    fn render_mark(&mut self, c: char, wide: bool) -> Vec<u8> {
        let (cw, ch, baseline) = (self.cell_width, self.cell_height, self.baseline);
        let target_w = if wide { cw * 2 } else { cw };
        let size = self.font_size;
        let (metrics, bitmap) = match self.pick(c) {
            Source::Primary => self.primary.rasterize(c, size),
            Source::Fallback(i) => match &self.fallbacks[i].slot {
                Slot::Loaded(f, fsize) => f.rasterize(c, *fsize),
                _ => return vec![0u8; target_w * ch],
            },
            Source::None => return vec![0u8; target_w * ch],
        };
        place_mark(&metrics, &bitmap, target_w, ch, baseline)
    }
}

/// The face a run of `style` text is shaped with (see [`FontManager::shaping_face`]).
#[derive(Clone, Copy)]
pub struct ShapeFace {
    /// Raw font file bytes and collection index (for rustybuzz).
    pub data: &'static [u8],
    pub index: u32,
    /// Face slot: 0 = primary, otherwise the style bits of a loaded styled face.
    pub slot: usize,
    /// Bold / italic still to be synthesized on top of this face.
    pub synth_bold: bool,
    pub synth_italic: bool,
}

/// A glyph rasterized by glyph id: tight coverage bitmap plus the offset of
/// its top-left corner from the glyph origin (left edge of the glyph's first
/// cell, top of the cell). The ink may extend outside the cell (ligature
/// glyphs reach back into the previous cell).
pub struct GidBitmap {
    pub data: Vec<u8>,
    pub w: usize,
    pub h: usize,
    pub x: i32,
    pub y: i32,
}

impl FontManager {
    pub fn font_size(&self) -> f32 {
        self.font_size
    }

    /// Whether the primary face maps `c` to a real glyph.
    #[inline]
    pub fn primary_has_glyph(&self, c: char) -> bool {
        self.primary.glyph(c) != 0
    }

    /// Whether the face in `slot` maps `c` to a real glyph.
    pub fn slot_has_glyph(&self, slot: usize, c: char) -> bool {
        if slot == 0 {
            return self.primary.glyph(c) != 0;
        }
        self.styled[slot].as_ref().and_then(|f| f.as_ref()).map_or(false, |f| f.glyph(c) != 0)
    }

    /// The face used for primary-font text of `style`, with the synthesis
    /// that [`render`](Self::render) would apply on top of it. `None` when
    /// the font bytes are unavailable.
    pub fn shaping_face(&mut self, style: u8) -> Option<ShapeFace> {
        let (slot, synth_bold, synth_italic) = self.face_for(style);
        let f = if slot == 0 { Some(&self.primary) } else { self.styled[slot].as_ref().and_then(|f| f.as_ref()) }?;
        Some(ShapeFace { data: f.data?, index: f.index, slot, synth_bold, synth_italic })
    }

    /// Rasterize glyph `gid` of the face in `slot`, baseline-aligned to the
    /// cell grid with its origin at the cell's left edge. Unlike
    /// [`place_glyph`] the ink is not clipped to one cell horizontally.
    pub fn rasterize_gid(&mut self, slot: usize, gid: u16, synth_bold: bool, synth_italic: bool) -> GidBitmap {
        let (ch, baseline, size) = (self.cell_height, self.baseline, self.font_size);
        let empty = GidBitmap { data: Vec::new(), w: 0, h: 0, x: 0, y: 0 };
        let font = if slot == 0 { Some(&self.primary) } else { self.styled[slot].as_ref().and_then(|f| f.as_ref()) };
        let Some(font) = font else { return empty };
        let (m, bitmap) = font.font.rasterize_indexed(gid, size);
        if bitmap.is_empty() || m.width == 0 || m.height == 0 {
            return empty;
        }
        // Margins leave room for the horizontal spill of synthetic bold / oblique.
        let margin = 2 + (ch as f32 * OBLIQUE_SLANT).ceil() as usize;
        let w = m.width + 2 * margin;
        let mut out = vec![0u8; w * ch];
        let top = baseline as i32 - m.ymin - m.height as i32;
        for gy in 0..m.height {
            let cy = top + gy as i32;
            if cy < 0 || cy >= ch as i32 {
                continue;
            }
            let dst = &mut out[cy as usize * w + margin..cy as usize * w + margin + m.width];
            dst.copy_from_slice(&bitmap[gy * m.width..(gy + 1) * m.width]);
        }
        if synth_bold {
            embolden(&mut out, w, ch);
        }
        if synth_italic {
            shear(&mut out, w, ch, baseline);
        }
        GidBitmap { data: out, w, h: ch, x: m.xmin - margin as i32, y: 0 }
    }
}

/// True for code points that should be overlaid on the preceding glyph:
/// zero-width combining marks, excluding joiners / variation selectors.
pub fn is_overlay_mark(c: char) -> bool {
    use unicode_width::UnicodeWidthChar;
    let u = c as u32;
    if matches!(u, 0x200B..=0x200F | 0x2060..=0x2064 | 0xFE00..=0xFE0F | 0xE0000..=0xE01EF) {
        return false;
    }
    !c.is_control() && c.width() == Some(0)
}

/// Place a mark bitmap into a `target_w * ch` box. Zero-advance marks are
/// designed to hang left of the pen (which sits after the base glyph), so
/// they are offset from the right edge of the box; marks with an advance are
/// drawn from the box origin using their own `xmin`. Out-of-box ink is
/// shifted back inside.
fn place_mark(m: &fontdue::Metrics, bitmap: &[u8], target_w: usize, ch: usize, baseline: usize) -> Vec<u8> {
    let mut out = vec![0u8; target_w * ch];
    if bitmap.is_empty() || m.width == 0 || m.height == 0 {
        return out;
    }
    let pen = if m.advance_width.abs() < 0.5 { target_w as i32 } else { 0 };
    let mut x0 = pen + m.xmin;
    x0 = x0.min(target_w as i32 - m.width as i32).max(0);
    let top = baseline as i32 - m.ymin - m.height as i32;
    for gy in 0..m.height {
        let cy = top + gy as i32;
        if cy < 0 || cy >= ch as i32 {
            continue;
        }
        for gx in 0..m.width {
            let cx = x0 as usize + gx;
            if cx >= target_w {
                continue;
            }
            if let Some(&v) = bitmap.get(gy * m.width + gx) {
                out[cy as usize * target_w + cx] = v;
            }
        }
    }
    out
}

/// Style bits for [`FontManager::rasterize_styled`].
pub const BOLD: u8 = 1;
pub const ITALIC: u8 = 2;

#[inline]
pub fn style_bits(bold: bool, italic: bool) -> u8 {
    bold as u8 | (italic as u8) << 1
}

/// Block elements, box drawing and powerline glyphs must keep their exact
/// cell geometry, so they are never synthetically emboldened or slanted.
fn synth_ok(c: char) -> bool {
    !matches!(c as u32, 0x2500..=0x259F | 0xE0A0..=0xE0BF | 0x2800..=0x28FF)
}

/// Synthetic bold: union of the glyph and itself shifted right by one pixel.
fn embolden(bmp: &mut [u8], w: usize, h: usize) {
    for y in 0..h {
        let row = &mut bmp[y * w..(y + 1) * w];
        for x in (1..w).rev() {
            let (a, b) = (row[x] as u32, row[x - 1] as u32);
            // alpha union: a + b - a*b/255
            row[x] = (a + b - (a * b + 127) / 255) as u8;
        }
    }
}

/// tan(12 degrees).
const OBLIQUE_SLANT: f32 = 0.2126;

/// Synthetic oblique: shear rows horizontally (sub-pixel, linearly
/// interpolated) around a pivot a bit above the baseline so the slant is
/// balanced between ascenders and descenders.
fn shear(bmp: &mut [u8], w: usize, h: usize, baseline: usize) {
    let pivot = baseline as f32 - h as f32 * 0.3;
    let src = bmp.to_vec();
    for y in 0..h {
        let shift = (pivot - y as f32) * OBLIQUE_SLANT;
        let whole = shift.floor();
        let frac = shift - whole;
        let whole = whole as i32;
        let (fa, fb) = (((1.0 - frac) * 256.0) as u32, (frac * 256.0) as u32);
        for x in 0..w {
            // dest x takes source x - shift
            let s0 = x as i32 - whole;
            let v0 = if s0 >= 0 && (s0 as usize) < w { src[y * w + s0 as usize] as u32 } else { 0 };
            let s1 = s0 - 1;
            let v1 = if s1 >= 0 && (s1 as usize) < w { src[y * w + s1 as usize] as u32 } else { 0 };
            bmp[y * w + x] = ((v0 * fa + v1 * fb) >> 8) as u8;
        }
    }
}

#[cfg(test)]
pub fn embolden_for_test(b: &mut [u8], w: usize, h: usize) { embolden(b, w, h) }
#[cfg(test)]
pub fn shear_for_test(b: &mut [u8], w: usize, h: usize, base: usize) { shear(b, w, h, base) }

/// Find the real `bold` / `italic` sibling of `primary`: another face inside
/// the same collection (Menlo.ttc), or a sibling file (`-Bold`, `-Italic`,
/// `-BoldItalic`, `-Oblique`, ...) in the same directory.
/// Returns `(file, collection index)`.
pub fn find_face(primary: &Path, bold: bool, italic: bool) -> Option<(PathBuf, u32)> {
    if !bold && !italic {
        return Some((primary.to_path_buf(), 0));
    }
    let ext = primary.extension()?.to_str()?.to_string();
    if matches!(ext.to_ascii_lowercase().as_str(), "ttc" | "otc") {
        let data = std::fs::read(primary).ok()?;
        let n = ttf_parser::fonts_in_collection(&data)?;
        for i in 0..n {
            let Ok(face) = ttf_parser::Face::parse(&data, i) else { continue };
            let is_it = face.is_italic() || face.is_oblique();
            if face.is_bold() == bold && is_it == italic {
                return Some((primary.to_path_buf(), i));
            }
        }
        return None;
    }
    let dir = primary.parent()?;
    let stem = primary.file_stem()?.to_str()?;
    let mut base = stem;
    for suffix in ["-Regular", "_Regular", " Regular", "Regular", "-Book", "-Medium"] {
        if let Some(b) = base.strip_suffix(suffix) {
            base = b;
            break;
        }
    }
    let names: &[&str] = match (bold, italic) {
        (true, false) => &["Bold"],
        (false, true) => &["Italic", "Oblique"],
        _ => &["BoldItalic", "BoldOblique", "Bold Italic", "Bold Oblique"],
    };
    let exts = [ext.as_str(), "ttf", "otf"];
    for name in names {
        for sep in ["-", "", " ", "_"] {
            for e in exts {
                let cand = dir.join(format!("{base}{sep}{name}.{e}"));
                if cand.is_file() && cand != primary {
                    return Some((cand, 0));
                }
            }
        }
    }
    None
}

/// Geometry of text decorations in cell pixels (distance from the cell top).
#[derive(Clone, Copy, Debug)]
pub struct DecoMetrics {
    pub cell_w: usize,
    pub ul_top: usize,
    pub ul_thick: usize,
    pub strike_top: usize,
    pub strike_thick: usize,
}

/// Derive decoration geometry from the font's `post` / `OS/2` metrics
/// (scaled to `size`), with sane fallbacks, clamped to fit in the cell.
fn deco_metrics(data: &[u8], size: f32, baseline: usize, ch: usize, cw: usize) -> DecoMetrics {
    let mut ul = None;
    let mut st = None;
    let mut scale = 0.0;
    if let Ok(face) = ttf_parser::Face::parse(data, 0) {
        scale = size / face.units_per_em() as f32;
        ul = face.underline_metrics();
        st = face.strikeout_metrics();
    }
    let default_thick = (size / 14.0).round().max(1.0);
    let thick_of = |m: Option<ttf_parser::LineMetrics>| {
        m.map(|m| (m.thickness as f32 * scale).round()).filter(|t| *t >= 1.0).unwrap_or(default_thick)
    };
    let ul_thick = thick_of(ul);
    let st_thick = thick_of(st);
    let ul_pos = ul.map(|m| m.position as f32 * scale).unwrap_or(-size * 0.12);
    let st_pos = st.map(|m| m.position as f32 * scale).unwrap_or(size * 0.3);
    let b = baseline as f32;
    // Keep underline just below the baseline, strike clear of the baseline.
    let mut ul_top = (b - ul_pos).round().max(b + 1.0);
    let max_top = (ch as f32 - ul_thick).max(0.0);
    if ul_top > max_top {
        ul_top = max_top;
    }
    let strike_top = (b - st_pos).round().clamp(0.0, (ch as f32 - st_thick).max(0.0));
    DecoMetrics {
        cell_w: cw.max(1),
        ul_top: ul_top as usize,
        ul_thick: ul_thick as usize,
        strike_top: strike_top as usize,
        strike_thick: st_thick as usize,
    }
}

/// Decorations of one cell.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Deco {
    pub ul: UnderlineStyle,
    pub strike: bool,
    pub over: bool,
}

impl Deco {
    #[inline]
    pub fn any(&self) -> bool {
        self.ul != UnderlineStyle::None || self.strike || self.over
    }

    /// Compact key for caches.
    #[inline]
    pub fn bits(&self) -> u64 {
        self.ul as u64 | (self.strike as u64) << 3 | (self.over as u64) << 4
    }
}

/// Paint underline / strikethrough / overline for a cell spanning `w` pixels
/// starting at (`x0`, `y0`) in `buf` (row stride `stride`, cell height `ch`).
/// Solid pixels; underline uses `ul_px`, strike / overline use `fg_px`.
#[allow(clippy::too_many_arguments)]
pub fn paint_decor(
    buf: &mut [u32],
    stride: usize,
    x0: usize,
    y0: usize,
    w: usize,
    ch: usize,
    d: &Deco,
    m: &DecoMetrics,
    fg_px: u32,
    ul_px: u32,
) {
    let mut put = |x: usize, y: usize, px: u32| {
        if y < ch && x < w {
            let i = (y0 + y) * stride + x0 + x;
            if let Some(p) = buf.get_mut(i) {
                *p = px;
            }
        }
    };
    let t = m.ul_thick.max(1);
    match d.ul {
        UnderlineStyle::None => {}
        UnderlineStyle::Single => {
            for dy in 0..t {
                for x in 0..w {
                    put(x, m.ul_top + dy, ul_px);
                }
            }
        }
        UnderlineStyle::Double => {
            let gap = t.max(1);
            let total = 2 * t + gap;
            let top = m.ul_top.min(ch.saturating_sub(total));
            for line in 0..2 {
                for dy in 0..t {
                    for x in 0..w {
                        put(x, top + line * (t + gap) + dy, ul_px);
                    }
                }
            }
        }
        UnderlineStyle::Dotted | UnderlineStyle::Dashed => {
            let (on, period) = if d.ul == UnderlineStyle::Dotted {
                (t, 2 * t)
            } else {
                let p = (m.cell_w / 2).max(4);
                ((p * 2 / 3).max(2), p)
            };
            for x in 0..w {
                if x % period < on {
                    for dy in 0..t {
                        put(x, m.ul_top + dy, ul_px);
                    }
                }
            }
        }
        UnderlineStyle::Curly => {
            let amp = (t as f32).max(1.0);
            let max_base = (ch as f32 - t as f32 - amp).max(0.0);
            let base = (m.ul_top as f32 + amp).min(max_base);
            for x in 0..w {
                let phase = (x as f32 + 0.5) / m.cell_w as f32 * std::f32::consts::TAU;
                let y = (base + amp * phase.sin()).round().max(0.0) as usize;
                for dy in 0..t {
                    put(x, y + dy, ul_px);
                }
            }
        }
    }
    if d.strike {
        for dy in 0..m.strike_thick.max(1) {
            for x in 0..w {
                put(x, m.strike_top + dy, fg_px);
            }
        }
    }
    if d.over {
        for dy in 0..m.ul_thick.max(1) {
            for x in 0..w {
                put(x, dy, fg_px);
            }
        }
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
    font: &GFont,
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

    /// fontdue merges Mac Roman cmap entries over Unicode for U+0080..=U+00FF;
    /// make sure Latin-1 glyphs come from the Unicode table (Menlo regression).
    #[test]
    fn latin1_glyphs_not_aliased_to_mac_roman() {
        let path = "/System/Library/Fonts/Menlo.ttc";
        if !Path::new(path).exists() {
            return;
        }
        let mut fm = FontManager::new(path, 16.0);
        let dot = fm.rasterize('\u{00B7}').to_vec();
        let sum = fm.rasterize('\u{2211}').to_vec();
        assert_ne!(dot, sum, "middle dot must not render as summation");
        let times = fm.rasterize('\u{00D7}').to_vec();
        let lozenge = fm.rasterize('\u{25CA}').to_vec();
        assert_ne!(times, lozenge, "multiply sign must not render as lozenge");
        // The middle dot is small: far less ink than the summation sign.
        let ink = |b: &[u8]| b.iter().filter(|&&v| v > 64).count();
        assert!(ink(&dot) * 3 < ink(&sum));
    }

}
