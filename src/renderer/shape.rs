//! Text shaping for programming ligatures (rustybuzz, a pure-Rust HarfBuzz port).
//!
//! Terminal text is laid out on a fixed grid, so shaping is deliberately
//! narrow: a *run* is a sequence of adjacent single-width cells that share
//! style and colors and whose characters all come from the primary face. The
//! run is shaped once (results are cached by run text); afterwards only the
//! glyphs that differ from the plain `char -> glyph` mapping matter:
//!
//! * a **ligature** glyph that covers several cells (`->`, `!==` in fonts that
//!   ship one glyph per sequence), and
//! * **contextual alternates** (JetBrains Mono, Fira Code, Cascadia): the
//!   cell keeps one glyph but it is a different glyph, often with ink that
//!   reaches back into the previous cell, paired with a blank spacer glyph.
//!
//! Everything else stays on the regular per-character path, so text without
//! ligatures is rendered exactly as before.
//!
//! Callers decide the run boundaries; the cursor cell and selection edges are
//! excluded from runs, which is how terminals show the raw characters while
//! editing or selecting.

use std::collections::HashMap;
use std::sync::Arc;

use super::font::ShapeFace;

/// A shaped glyph that is not the plain mapping of its cell's character.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShGlyph {
    /// First cell of the cluster, relative to the start of the run.
    pub cell: u16,
    /// Number of cells in the cluster.
    pub span: u16,
    pub gid: u16,
    /// Pixel offset from the cell's origin (HarfBuzz offset plus the pen
    /// position inside multi-glyph clusters).
    pub x_off: i16,
    pub y_off: i16,
}

/// Result of shaping one run.
#[derive(Debug, PartialEq, Eq)]
pub struct ShapedRun {
    /// Non-plain glyphs in cluster order.
    pub glyphs: Vec<ShGlyph>,
    /// `covered[i]`: cell `i` is part of a non-plain cluster; its character
    /// must not be drawn with the plain glyph.
    pub covered: Vec<bool>,
}

struct Entry {
    chars: Box<[char]>,
    result: Option<Arc<ShapedRun>>,
}

const CACHE_CAP: usize = 16_384;

/// Shaping engine with a run cache. One per font; call [`Shaper::clear`] when
/// the font changes.
#[derive(Default)]
pub struct Shaper {
    /// Parsed faces by slot (`None` = unparsable).
    faces: HashMap<usize, Option<rustybuzz::Face<'static>>>,
    /// Whether runs of ASCII letters / digits can never change shape in a face
    /// (probed once per face), which skips the shaper for most words.
    alnum_inert: HashMap<usize, bool>,
    cache: HashMap<u64, Vec<Entry>>,
    entries: usize,
    /// Shaper invocations (cache misses), for tests and profiling.
    pub shapes: u64,
}

fn run_hash(slot: usize, run: &[char]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64 ^ slot as u64;
    for &c in run {
        h = (h ^ c as u64).wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn is_alnum_run(run: &[char]) -> bool {
    run.iter().all(|c| c.is_ascii_alphanumeric())
}

impl Shaper {
    pub fn new() -> Self {
        Self::default()
    }

    /// Drop everything (font or size changed).
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    fn face(&mut self, f: &ShapeFace) -> Option<&rustybuzz::Face<'static>> {
        self.faces
            .entry(f.slot)
            .or_insert_with(|| rustybuzz::Face::from_slice(f.data, f.index))
            .as_ref()
    }

    /// Shape `run` (>= 2 single-width characters) with `face` at `size` px
    /// for a grid of `cell_w`-pixel cells. Returns `None` when nothing in the
    /// run differs from plain per-character rendering.
    pub fn shape(&mut self, face: &ShapeFace, size: f32, cell_w: usize, run: &[char]) -> Option<Arc<ShapedRun>> {
        if run.len() < 2 || run.len() > u16::MAX as usize {
            return None;
        }
        let key = run_hash(face.slot, run);
        if let Some(bucket) = self.cache.get(&key) {
            if let Some(e) = bucket.iter().find(|e| *e.chars == *run) {
                return e.result.clone();
            }
        }
        if is_alnum_run(run) && self.alnum_is_inert(face) {
            return None;
        }
        let result = self.shape_uncached(face, size, cell_w, run).map(Arc::new);
        if self.entries >= CACHE_CAP {
            self.cache.clear();
            self.entries = 0;
        }
        self.cache.entry(key).or_default().push(Entry { chars: run.into(), result: result.clone() });
        self.entries += 1;
        result
    }

    fn shape_uncached(&mut self, face: &ShapeFace, size: f32, cell_w: usize, run: &[char]) -> Option<ShapedRun> {
        self.shapes += 1;
        let bf = self.face(face)?;
        let upem = bf.units_per_em().max(1) as f32;
        let unit_px = size / upem;
        // Grid-snapped pen scale: advance of 'M' maps to exactly one cell.
        let m_adv = bf
            .glyph_index('M')
            .and_then(|g| bf.glyph_hor_advance(g))
            .map(|a| a as f32)
            .filter(|a| *a > 0.0)
            .unwrap_or(upem * 0.6);
        let pen_px = cell_w as f32 / m_adv;

        let mut buf = rustybuzz::UnicodeBuffer::new();
        for (i, &c) in run.iter().enumerate() {
            buf.add(c, i as u32);
        }
        buf.set_direction(rustybuzz::Direction::LeftToRight);
        buf.guess_segment_properties();
        let out = rustybuzz::shape(bf, &[], buf);
        let infos = out.glyph_infos();
        let pos = out.glyph_positions();
        if infos.is_empty() {
            return None;
        }

        let cmap: Vec<u16> = run.iter().map(|&c| bf.glyph_index(c).map_or(0, |g| g.0)).collect();
        let n = run.len();
        let mut glyphs = Vec::new();
        let mut covered = vec![false; n];

        // Group glyphs by cluster (clusters are monotone for LTR text).
        let mut i = 0;
        while i < infos.len() {
            let cluster = infos[i].cluster as usize;
            let mut j = i;
            while j < infos.len() && infos[j].cluster as usize == cluster {
                j += 1;
            }
            let end = if j < infos.len() { (infos[j].cluster as usize).min(n) } else { n };
            let span = end.saturating_sub(cluster).max(1);
            let plain = j - i == 1
                && span == 1
                && cluster < n
                && infos[i].glyph_id as u16 == cmap[cluster]
                && pos[i].x_offset == 0
                && pos[i].y_offset == 0;
            if !plain && cluster < n {
                let mut pen = 0f32;
                for k in i..j {
                    glyphs.push(ShGlyph {
                        cell: cluster as u16,
                        span: span as u16,
                        gid: infos[k].glyph_id as u16,
                        x_off: (pen * pen_px + pos[k].x_offset as f32 * unit_px).round() as i16,
                        y_off: (-(pos[k].y_offset as f32) * unit_px).round() as i16,
                    });
                    pen += pos[k].x_advance as f32;
                }
                for c in covered.iter_mut().skip(cluster).take(span) {
                    *c = true;
                }
            }
            i = j;
        }
        if glyphs.is_empty() {
            None
        } else {
            Some(ShapedRun { glyphs, covered })
        }
    }

    /// True if no pair of ASCII letters / digits changes shape in this face
    /// (underscore is excluded on purpose: `__` is a ligature in several fonts).
    /// Fonts like Fira Code (`www`, `0x`) fail the probe and shape every run.
    fn alnum_is_inert(&mut self, face: &ShapeFace) -> bool {
        if let Some(&v) = self.alnum_inert.get(&face.slot) {
            return v;
        }
        let v = self.probe_alnum(face);
        self.alnum_inert.insert(face.slot, v);
        v
    }

    fn probe_alnum(&mut self, face: &ShapeFace) -> bool {
        let Some(bf) = self.face(face) else { return true };
        let chars: Vec<char> = ('a'..='z').chain('A'..='Z').chain('0'..='9').collect();
        let cmap: HashMap<char, u16> = chars.iter().map(|&c| (c, bf.glyph_index(c).map_or(0, |g| g.0))).collect();
        let plain = |run: &[char], bf: &rustybuzz::Face<'static>| {
            let mut buf = rustybuzz::UnicodeBuffer::new();
            for (i, &c) in run.iter().enumerate() {
                buf.add(c, i as u32);
            }
            buf.set_direction(rustybuzz::Direction::LeftToRight);
            buf.guess_segment_properties();
            let out = rustybuzz::shape(bf, &[], buf);
            let (infos, pos) = (out.glyph_infos(), out.glyph_positions());
            infos.len() == run.len()
                && infos.iter().zip(run).enumerate().all(|(k, (g, c))| {
                    g.cluster as usize == k && g.glyph_id as u16 == cmap[c] && pos[k].x_offset == 0 && pos[k].y_offset == 0
                })
        };
        for &a in &chars {
            for &b in &chars {
                if !plain(&[a, b], bf) {
                    log::debug!("alnum pair {a}{b} is not inert");
                    return false;
                }
            }
            // Repeated letters catch `www`-style ligatures.
            if !plain(&[a, a, a], bf) {
                log::debug!("alnum triple {a}{a}{a} is not inert");
                return false;
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer::font::FontManager;

    pub(crate) fn ligature_font() -> Option<String> {
        let home = std::env::var("HOME").ok()?;
        [
            format!("{home}/Library/Fonts/JetBrainsMonoNerdFontMono-Regular.ttf"),
            format!("{home}/Library/Fonts/JetBrainsMonoNerdFont-Regular.ttf"),
            "/usr/share/fonts/truetype/jetbrains-mono/JetBrainsMono-Regular.ttf".to_string(),
            "/usr/share/fonts/truetype/firacode/FiraCode-Regular.ttf".to_string(),
        ]
        .into_iter()
        .find(|p| std::path::Path::new(p).exists())
    }

    fn chars(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    #[test]
    fn plain_text_is_not_special() {
        let Some(path) = ligature_font() else { return };
        let mut fm = FontManager::new(&path, 15.0);
        let face = fm.shaping_face(0).unwrap();
        let mut sh = Shaper::new();
        let (size, cw) = (fm.font_size(), fm.cell_width);
        assert!(sh.shape(&face, size, cw, &chars("hello")).is_none());
        assert!(sh.shape(&face, size, cw, &chars("fn(a,b)")).is_none());
        assert!(sh.shape(&face, size, cw, &chars("x")).is_none());
    }

    #[test]
    fn arrow_ligature_is_detected_and_cached() {
        let Some(path) = ligature_font() else { return };
        let mut fm = FontManager::new(&path, 15.0);
        let face = fm.shaping_face(0).unwrap();
        let mut sh = Shaper::new();
        let (size, cw) = (fm.font_size(), fm.cell_width);
        let run = chars("a->b");
        let r = sh.shape(&face, size, cw, &run).expect("-> must change shape in a ligature font");
        // The two cells of "->" are covered, 'a' and 'b' stay plain.
        assert_eq!(r.covered, vec![false, true, true, false]);
        assert!(r.glyphs.iter().all(|g| (1..=2).contains(&(g.cell as usize))));
        let before = sh.shapes;
        let r2 = sh.shape(&face, size, cw, &run).unwrap();
        assert_eq!(sh.shapes, before, "second lookup must hit the cache");
        assert_eq!(*r, *r2);
        // Non-ligature punctuation is left alone.
        assert!(sh.shape(&face, size, cw, &chars("(a)")).is_none());
    }

    #[test]
    fn alnum_probe_skips_words_in_jetbrains_mono() {
        let Some(path) = ligature_font() else { return };
        if !path.contains("JetBrains") {
            return;
        }
        let mut fm = FontManager::new(&path, 15.0);
        let face = fm.shaping_face(0).unwrap();
        let mut sh = Shaper::new();
        let (size, cw) = (fm.font_size(), fm.cell_width);
        assert!(sh.shape(&face, size, cw, &chars("identifier1")).is_none());
        assert_eq!(sh.shapes, 0, "pure alphanumeric runs bypass the shaper");
    }
}
