//! Glyph atlas for the GPU text renderer: CPU-side bookkeeping.
//!
//! Glyph bitmaps are cropped to their ink box, packed into fixed-size pages
//! with a shelf allocator, and handed to the GPU as small uploads. Pages are
//! reclaimed wholesale, least recently used first, when the atlas is full.
//! This module knows nothing about wgpu: it produces [`Upload`]s that the
//! pipeline copies into a texture array, which keeps it unit-testable.
//!
//! Two instances are used: an 8-bit coverage atlas for monochrome glyphs
//! (everything fontdue produces) and a 32-bit RGBA atlas reserved for color
//! glyphs (bitmap emoji), which no current font backend supplies.

use std::collections::HashMap;

use super::FxBuild;

/// Identity of an atlas entry. The pixel size is fixed per atlas (it is
/// cleared when the font changes), so it is not part of the key.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum GlyphKey {
    /// A character rendered through the regular per-cell path
    /// (`wide`: two-cell glyph).
    Char { c: char, style: u8, wide: bool },
    /// Combining mark overlay.
    Mark { c: char, wide: bool },
    /// Shaped glyph (ligature / contextual alternate) by glyph id.
    Gid { slot: u8, gid: u16, synth: u8 },
    /// Underline / strike mask for a decoration style, `w` pixels wide.
    Deco { bits: u64, w: u16 },
    /// Dotted hyperlink underline, `w` pixels wide.
    LinkDots { w: u16 },
    /// Color glyph (RGBA atlas). Reserved: no font backend produces color
    /// bitmaps yet, but the atlas, shader and pipeline support them.
    #[allow(dead_code)]
    Color { c: char, wide: bool },
}

/// Where a glyph lives in the atlas and how to place it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AtlasEntry {
    pub page: u8,
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
    /// Offset of the ink box from the glyph origin (cell top-left).
    pub off_x: i16,
    pub off_y: i16,
}

impl AtlasEntry {
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.w == 0 || self.h == 0
    }
}

/// A tightly packed bitmap to insert (`w * h * bytes_per_pixel` bytes).
pub struct Bitmap {
    pub data: Vec<u8>,
    pub w: usize,
    pub h: usize,
    pub off_x: i32,
    pub off_y: i32,
}

impl Bitmap {
    pub fn empty() -> Self {
        Self { data: Vec::new(), w: 0, h: 0, off_x: 0, off_y: 0 }
    }

    /// Crop an 8-bit coverage bitmap of `w * h` (whose top-left sits at
    /// `(off_x, off_y)` from the glyph origin) to the box of non-zero pixels.
    pub fn crop_mask(data: &[u8], w: usize, h: usize, off_x: i32, off_y: i32) -> Self {
        let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0usize, 0usize);
        for y in 0..h {
            let row = &data[y * w..(y + 1) * w];
            if let Some(first) = row.iter().position(|&v| v != 0) {
                let last = row.iter().rposition(|&v| v != 0).unwrap_or(first);
                x0 = x0.min(first);
                x1 = x1.max(last + 1);
                y0 = y0.min(y);
                y1 = y1.max(y + 1);
            }
        }
        if x1 <= x0 || y1 <= y0 {
            return Self::empty();
        }
        let (cw, chh) = (x1 - x0, y1 - y0);
        let mut out = Vec::with_capacity(cw * chh);
        for y in y0..y1 {
            out.extend_from_slice(&data[y * w + x0..y * w + x1]);
        }
        Self { data: out, w: cw, h: chh, off_x: off_x + x0 as i32, off_y: off_y + y0 as i32 }
    }
}

/// A pending texture write for the GPU.
pub struct Upload {
    pub page: u8,
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
    pub data: Vec<u8>,
}

struct Shelf {
    y: u32,
    h: u32,
    x: u32,
}

struct Page {
    shelves: Vec<Shelf>,
    /// Next free y for a new shelf.
    y: u32,
    /// Frame this page last served a lookup or insert.
    last_used: u64,
}

impl Page {
    fn new() -> Self {
        Self { shelves: Vec::new(), y: 0, last_used: 0 }
    }

    fn alloc(&mut self, w: u32, h: u32, pw: u32, ph: u32) -> Option<(u32, u32)> {
        // Best fit: the shortest existing shelf that is tall enough and has room.
        let mut best: Option<usize> = None;
        for (i, s) in self.shelves.iter().enumerate() {
            if s.h >= h && s.x + w <= pw && best.map_or(true, |b| s.h < self.shelves[b].h) {
                best = Some(i);
            }
        }
        // A much taller shelf would waste space: open a new one if possible.
        if let Some(i) = best {
            let s = &self.shelves[i];
            if s.h <= h + h / 2 + 1 || self.y + h > ph {
                let s = &mut self.shelves[i];
                let pos = (s.x, s.y);
                s.x += w;
                return Some(pos);
            }
        }
        if self.y + h <= ph && w <= pw {
            let pos = (0, self.y);
            self.shelves.push(Shelf { y: self.y, h, x: w });
            self.y += h;
            return Some(pos);
        }
        best.map(|i| {
            let s = &mut self.shelves[i];
            let pos = (s.x, s.y);
            s.x += w;
            pos
        })
    }
}

struct Slot {
    entry: AtlasEntry,
}

pub struct Atlas {
    pub page_w: u32,
    pub page_h: u32,
    pub max_pages: usize,
    bpp: usize,
    pages: Vec<Page>,
    map: HashMap<GlyphKey, Slot, FxBuild>,
    pending: Vec<Upload>,
    frame: u64,
    /// Pages reclaimed so far.
    pub evictions: u64,
    /// Set when a page was evicted: cached instances that point into the
    /// atlas may be stale. Cleared by [`Atlas::take_evicted`].
    evicted: bool,
}

impl Atlas {
    pub fn new(page_w: u32, page_h: u32, max_pages: usize, bytes_per_pixel: usize) -> Self {
        Self {
            page_w,
            page_h,
            max_pages: max_pages.max(1),
            bpp: bytes_per_pixel,
            pages: Vec::new(),
            map: HashMap::default(),
            pending: Vec::new(),
            frame: 1,
            evictions: 0,
            evicted: false,
        }
    }

    /// Forget every glyph (font or size changed).
    pub fn clear(&mut self) {
        self.pages.clear();
        self.map.clear();
        self.pending.clear();
    }

    /// Start a new frame (drives LRU page selection).
    pub fn begin_frame(&mut self) {
        self.frame += 1;
    }

    /// Whether a page was evicted since the last call.
    pub fn take_evicted(&mut self) -> bool {
        std::mem::take(&mut self.evicted)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn glyph_count(&self) -> usize {
        self.map.len()
    }

    /// Writes to apply to the GPU texture before drawing.
    pub fn take_uploads(&mut self) -> Vec<Upload> {
        std::mem::take(&mut self.pending)
    }

    /// Look up `key`, marking its page as used this frame.
    #[inline]
    pub fn get(&mut self, key: &GlyphKey) -> Option<AtlasEntry> {
        let e = self.map.get(key)?.entry;
        if !e.is_empty() {
            if let Some(p) = self.pages.get_mut(e.page as usize) {
                p.last_used = self.frame;
            }
        }
        Some(e)
    }

    /// Entry for `key`, rasterizing it with `make` on a miss. `None` only if
    /// the bitmap cannot be placed (larger than a page, or every page was in
    /// use this frame).
    pub fn get_or_insert_with(&mut self, key: GlyphKey, make: impl FnOnce() -> Bitmap) -> Option<AtlasEntry> {
        if let Some(e) = self.get(&key) {
            return Some(e);
        }
        self.insert(key, make())
    }

    pub fn insert(&mut self, key: GlyphKey, bmp: Bitmap) -> Option<AtlasEntry> {
        let (w, h) = (bmp.w as u32, bmp.h as u32);
        if w == 0 || h == 0 {
            let e = AtlasEntry { page: 0, x: 0, y: 0, w: 0, h: 0, off_x: 0, off_y: 0 };
            self.map.insert(key, Slot { entry: e });
            return Some(e);
        }
        if w > self.page_w || h > self.page_h || bmp.data.len() != bmp.w * bmp.h * self.bpp {
            return None;
        }
        let (page, x, y) = self.place(w, h)?;
        let e = AtlasEntry {
            page: page as u8,
            x: x as u16,
            y: y as u16,
            w: w as u16,
            h: h as u16,
            off_x: bmp.off_x as i16,
            off_y: bmp.off_y as i16,
        };
        self.pending.push(Upload { page: e.page, x: e.x, y: e.y, w: e.w, h: e.h, data: bmp.data });
        self.map.insert(key, Slot { entry: e });
        Some(e)
    }

    fn place(&mut self, w: u32, h: u32) -> Option<(usize, u32, u32)> {
        let (pw, ph) = (self.page_w, self.page_h);
        // Newest page first: it is the one with free space.
        for i in (0..self.pages.len()).rev() {
            if let Some((x, y)) = self.pages[i].alloc(w, h, pw, ph) {
                self.pages[i].last_used = self.frame;
                return Some((i, x, y));
            }
        }
        let i = if self.pages.len() < self.max_pages {
            self.pages.push(Page::new());
            self.pages.len() - 1
        } else {
            self.evict_lru_page()?
        };
        let (x, y) = self.pages[i].alloc(w, h, pw, ph)?;
        self.pages[i].last_used = self.frame;
        Some((i, x, y))
    }

    /// Reclaim the least recently used page that was not used this frame.
    fn evict_lru_page(&mut self) -> Option<usize> {
        let victim = self
            .pages
            .iter()
            .enumerate()
            .filter(|(_, p)| p.last_used < self.frame)
            .min_by_key(|(_, p)| p.last_used)
            .map(|(i, _)| i)?;
        self.pages[victim] = Page::new();
        self.map.retain(|_, s| s.entry.is_empty() || s.entry.page as usize != victim);
        self.pending.retain(|u| u.page as usize != victim);
        self.evictions += 1;
        self.evicted = true;
        Some(victim)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bmp(w: usize, h: usize, v: u8) -> Bitmap {
        Bitmap { data: vec![v; w * h], w, h, off_x: 0, off_y: 0 }
    }

    fn key(i: u32) -> GlyphKey {
        GlyphKey::Char { c: char::from_u32(0x4e00 + i).unwrap(), style: 0, wide: false }
    }

    #[test]
    fn crop_trims_to_ink_box() {
        let mut data = vec![0u8; 8 * 6];
        data[2 * 8 + 3] = 9;
        data[4 * 8 + 5] = 7;
        let b = Bitmap::crop_mask(&data, 8, 6, -1, 0);
        assert_eq!((b.w, b.h), (3, 3));
        assert_eq!((b.off_x, b.off_y), (-1 + 3, 2));
        assert_eq!(b.data[0], 9);
        assert_eq!(*b.data.last().unwrap(), 7);
        assert_eq!(Bitmap::crop_mask(&[0; 12], 4, 3, 0, 0).w, 0);
    }

    #[test]
    fn shelf_packing_is_dense() {
        // 64x64 page, 8x8 glyphs: a perfect packing holds 64.
        let mut a = Atlas::new(64, 64, 1, 1);
        for i in 0..64 {
            assert!(a.insert(key(i), bmp(8, 8, 1)).is_some(), "glyph {i}");
        }
        assert_eq!(a.page_count(), 1);
        // Entries never overlap and stay inside the page.
        let mut seen = std::collections::HashSet::new();
        for i in 0..64 {
            let e = a.get(&key(i)).unwrap();
            assert!(e.x as u32 + e.w as u32 <= 64 && e.y as u32 + e.h as u32 <= 64);
            assert!(seen.insert((e.x, e.y)));
        }
    }

    #[test]
    fn mixed_heights_share_shelves() {
        let mut a = Atlas::new(128, 128, 1, 1);
        // Many 6x10 and 6x12 tiles: they should land on a handful of shelves.
        for i in 0..40 {
            let h = if i % 2 == 0 { 10 } else { 12 };
            a.insert(key(i), bmp(6, h, 1)).unwrap();
        }
        let shelves: usize = a.pages.iter().map(|p| p.shelves.len()).sum();
        assert!(shelves <= 4, "{shelves} shelves");
    }

    #[test]
    fn uploads_cover_only_new_glyphs() {
        let mut a = Atlas::new(64, 64, 1, 1);
        a.get_or_insert_with(key(0), || bmp(4, 4, 5)).unwrap();
        a.get_or_insert_with(key(1), || bmp(4, 4, 6)).unwrap();
        assert_eq!(a.take_uploads().len(), 2);
        a.get_or_insert_with(key(0), || panic!("must be cached")).unwrap();
        assert!(a.take_uploads().is_empty());
    }

    #[test]
    fn blank_glyphs_take_no_space() {
        let mut a = Atlas::new(16, 16, 1, 1);
        let e = a.insert(key(0), Bitmap::empty()).unwrap();
        assert!(e.is_empty());
        assert_eq!(a.page_count(), 0);
        assert!(a.take_uploads().is_empty());
    }

    #[test]
    fn full_atlas_evicts_least_recently_used_page() {
        // Two 16x16 pages of four 8x8 tiles each.
        let mut a = Atlas::new(16, 16, 2, 1);
        a.begin_frame();
        for i in 0..4 {
            a.insert(key(i), bmp(8, 8, 1)).unwrap(); // page 0
        }
        a.begin_frame();
        for i in 4..8 {
            a.insert(key(i), bmp(8, 8, 2)).unwrap(); // page 1
        }
        assert_eq!(a.page_count(), 2);
        assert!(!a.take_evicted());
        // Touch page 1 in a later frame; page 0 becomes the LRU victim.
        a.begin_frame();
        assert!(a.get(&key(4)).is_some());
        a.begin_frame();
        a.take_uploads();
        let e = a.insert(key(8), bmp(8, 8, 3)).unwrap();
        assert_eq!(e.page, 0);
        assert_eq!(a.evictions, 1);
        assert!(a.take_evicted());
        // Page 0's old glyphs are gone, page 1's survive.
        assert!(a.get(&key(0)).is_none());
        assert!(a.get(&key(5)).is_some());
        // The reclaimed page is reusable.
        for i in 9..12 {
            assert_eq!(a.insert(key(i), bmp(8, 8, 4)).unwrap().page, 0);
        }
    }

    #[test]
    fn pages_used_this_frame_are_not_evicted() {
        let mut a = Atlas::new(8, 8, 1, 1);
        a.begin_frame();
        a.insert(key(0), bmp(8, 8, 1)).unwrap();
        // Same frame, nothing evictable: the new glyph is dropped, the old one stays valid.
        assert!(a.insert(key(1), bmp(8, 8, 1)).is_none());
        assert!(a.get(&key(0)).is_some());
        a.begin_frame();
        assert!(a.insert(key(1), bmp(8, 8, 1)).is_some());
    }

    #[test]
    fn oversized_bitmap_is_rejected() {
        let mut a = Atlas::new(16, 16, 1, 1);
        assert!(a.insert(key(0), bmp(17, 4, 1)).is_none());
    }

    #[test]
    fn rgba_atlas_checks_byte_length() {
        let mut a = Atlas::new(16, 16, 1, 4);
        let good = Bitmap { data: vec![0; 4 * 4 * 4], w: 4, h: 4, off_x: 0, off_y: 0 };
        assert!(a.insert(GlyphKey::Color { c: '😀', wide: true }, good).is_some());
        assert!(a.insert(key(1), bmp(4, 4, 1)).is_none(), "8-bit data in an RGBA atlas");
    }
}
