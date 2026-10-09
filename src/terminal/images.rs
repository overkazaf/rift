//! Inline image support (Kitty graphics protocol — the "direct" transmission
//! path, which is also what `kitty +kitten icat`, `chafa --format=kitty`,
//! `viu`, timg, wezterm's `imgcat`, etc. all use).
//!
//! Scope, deliberately kept small:
//!   * Transmission medium: direct (base64 escaped in-band) only. File-backed
//!     (`t=f`/`t=t`) and shared-memory (`t=s`) transmission are not supported.
//!   * Formats: raw RGB/RGBA (`f=24`/`f=32`) and PNG (`f=100`, the default —
//!     8-bit depth, non-interlaced; palette/gray/gray+alpha/RGB/RGBA color
//!     types are all handled, since those cover the overwhelming majority of
//!     real-world PNGs produced by screenshot tools, `icat`, etc.).
//!   * Placement: one placement per image id, positioned at the cursor
//!     unless the command gives an explicit row/column. No virtual
//!     placements, no z-index, no animation, no unicode placeholders.
//!   * Chunked transmission (`m=1` / `m=0`) is supported since real clients
//!     split anything but tiny images across multiple escape sequences.
//!
//! Placements are tracked independently of the terminal grid (as opposed to
//! e.g. storing an `Option<ImageCell>` directly on every `Cell`), so a
//! placement's (row, col) is an absolute grid coordinate that does *not*
//! scroll with the screen — an image dropped into scrollback will stay put
//! rather than riding the text up, which is a known simplification.

use std::cell::Cell;
use std::collections::HashMap;
use std::io::Read;

/// Largest accepted image edge, in pixels.
pub const MAX_DIM: u32 = 8192;
/// Largest decoded (RGBA8) size of one image.
pub const MAX_IMAGE_BYTES: usize = 64 << 20;
/// Total decoded bytes kept per terminal; least-recently-used images are evicted beyond it.
pub const STORE_BUDGET: usize = 256 << 20;
/// Cap on base64 payload accumulated across `m=1` chunks (base64 of one max-size image, plus slack).
const MAX_PENDING_B64: usize = MAX_IMAGE_BYTES / 3 * 4 + (1 << 16);

/// Is `w x h` RGBA within the per-image limits?
fn dims_ok(w: u32, h: u32) -> bool {
    w > 0 && h > 0 && w <= MAX_DIM && h <= MAX_DIM && (w as usize) * (h as usize) * 4 <= MAX_IMAGE_BYTES
}

/// Where and how big a transmitted image is displayed.
#[derive(Clone, Copy, Debug)]
pub struct ImagePlacement {
    pub row: usize,
    pub col: usize,
    pub cell_rows: usize,
    pub cell_cols: usize,
}

/// A decoded image living in the terminal's image store.
pub struct TermImage {
    pub id: u32,
    pub width: u32,
    pub height: u32,
    /// Tightly packed RGBA8 pixels, row-major, `width * height * 4` bytes.
    pub data: Vec<u8>,
    pub placement: Option<ImagePlacement>,
}

/// Returned by [`ImageStore::get_cell`] to tell the renderer that a given
/// grid cell is covered by part of a placed image, and which part.
#[derive(Clone, Copy, Debug)]
pub struct ImageCell {
    pub image_id: u32,
    pub offset_x: u32,
    pub offset_y: u32,
}

/// Accumulates a chunked transmission (`m=1` ... `m=0`) across multiple APC
/// commands. Only the *first* chunk's control keys are meaningful per the
/// Kitty spec (continuations typically carry only `m=`), so we keep those
/// and just keep appending base64 payload bytes.
struct PendingTransmission {
    meta: Vec<(u8, String)>,
    data: Vec<u8>,
}

#[derive(Default)]
pub struct ImageStore {
    images: HashMap<u32, TermImage>,
    next_id: u32,
    pending: Option<PendingTransmission>,
    /// A chunked transmission exceeded its cap: swallow the remaining chunks.
    discarding: bool,
    /// Sum of `data.len()` over `images`.
    total_bytes: usize,
    /// LRU clock + per-image last-use stamp (interior mutability: `get_image` is `&self`).
    tick: Cell<u64>,
    used: HashMap<u32, Cell<u64>>,
}

impl ImageStore {
    pub fn new() -> Self {
        Self {
            images: HashMap::new(),
            next_id: 1,
            pending: None,
            discarding: false,
            total_bytes: 0,
            tick: Cell::new(0),
            used: HashMap::new(),
        }
    }

    /// Store a decoded image, assigning an id automatically if `id == 0`
    /// (matches Kitty's "client lets the terminal pick an id" convention).
    /// Returns the id the image was actually stored under.
    pub fn add_image(&mut self, id: u32, width: u32, height: u32, data: Vec<u8>) -> u32 {
        let id = if id == 0 {
            let assigned = self.next_id;
            self.next_id = self.next_id.wrapping_add(1).max(1);
            assigned
        } else {
            id
        };
        if let Some(old) = self.images.remove(&id) {
            self.total_bytes = self.total_bytes.saturating_sub(old.data.len());
        }
        self.total_bytes += data.len();
        self.images.insert(id, TermImage { id, width, height, data, placement: None });
        self.touch(id);
        self.evict_to_budget(id);
        id
    }

    fn touch(&mut self, id: u32) {
        let t = self.tick.get() + 1;
        self.tick.set(t);
        self.used.entry(id).or_insert_with(|| Cell::new(0)).set(t);
    }

    /// Drop least-recently-used images (never `keep`) until under [`STORE_BUDGET`].
    fn evict_to_budget(&mut self, keep: u32) {
        while self.total_bytes > STORE_BUDGET {
            let victim = self
                .images
                .keys()
                .copied()
                .filter(|k| *k != keep)
                .min_by_key(|k| self.used.get(k).map_or(0, |c| c.get()));
            match victim {
                Some(v) => self.delete_image(v),
                None => break,
            }
        }
    }

    /// Decoded bytes currently held.
    #[allow(dead_code)]
    pub fn total_bytes(&self) -> usize {
        self.total_bytes
    }

    /// Place a previously-transmitted image at `(row, col)`, spanning
    /// `cell_rows` x `cell_cols` terminal cells (each clamped to at least 1).
    pub fn place_image(&mut self, id: u32, row: usize, col: usize, cell_rows: usize, cell_cols: usize) {
        if let Some(img) = self.images.get_mut(&id) {
            img.placement = Some(ImagePlacement {
                row,
                col,
                cell_rows: cell_rows.max(1),
                cell_cols: cell_cols.max(1),
            });
            self.touch(id);
        }
    }

    /// Look up whether `(row, col)` falls inside any placed image, and if
    /// so, which image and which cell-sized slice of it.
    pub fn get_cell(&self, row: usize, col: usize) -> Option<ImageCell> {
        for img in self.images.values() {
            if let Some(p) = &img.placement {
                if row >= p.row && row < p.row + p.cell_rows && col >= p.col && col < p.col + p.cell_cols {
                    return Some(ImageCell {
                        image_id: img.id,
                        offset_x: (col - p.col) as u32,
                        offset_y: (row - p.row) as u32,
                    });
                }
            }
        }
        None
    }

    /// True when at least one image currently has a placement. Lets the
    /// renderer skip per-cell `get_cell` probing for the (usual) no-image case.
    pub fn has_placements(&self) -> bool {
        self.images.values().any(|i| i.placement.is_some())
    }

    pub fn get_image(&self, id: u32) -> Option<&TermImage> {
        let img = self.images.get(&id)?;
        if let Some(c) = self.used.get(&id) {
            let t = self.tick.get() + 1;
            self.tick.set(t);
            c.set(t);
        }
        Some(img)
    }

    pub fn delete_image(&mut self, id: u32) {
        if let Some(old) = self.images.remove(&id) {
            self.total_bytes = self.total_bytes.saturating_sub(old.data.len());
        }
        self.used.remove(&id);
    }

    /// Drop every stored image and any in-flight chunked transmission.
    pub fn clear(&mut self) {
        self.images.clear();
        self.used.clear();
        self.total_bytes = 0;
        self.pending = None;
        self.discarding = false;
    }

    /// Entry point for a decoded Kitty graphics APC command — everything
    /// after the leading `G` of `ESC _ G ... ESC \`. `cursor_row`/`cursor_col`
    /// are used as the placement position when the command doesn't give an
    /// explicit one.
    pub fn handle_kitty_command(&mut self, cursor_row: usize, cursor_col: usize, buf: &[u8]) {
        let (keys, payload) = parse_command(buf);
        let action = get(&keys, b'a').unwrap_or("t");

        match action {
            "t" | "T" => self.accumulate_and_maybe_decode(&keys, payload, cursor_row, cursor_col),
            "p" => {
                if let Some(id) = get_u32(&keys, b'i') {
                    let row = get_usize(&keys, b'r').unwrap_or(cursor_row);
                    let col = get_usize(&keys, b'c').unwrap_or(cursor_col);
                    self.place_existing(id, row, col);
                }
            }
            "d" => match get_u32(&keys, b'i') {
                Some(id) => self.delete_image(id),
                None => self.clear(),
            },
            _ => {}
        }
    }

    fn place_existing(&mut self, id: u32, row: usize, col: usize) {
        if let Some(img) = self.images.get(&id) {
            let (cell_rows, cell_cols) = default_span(img.width, img.height);
            self.place_image(id, row, col, cell_rows, cell_cols);
        }
    }

    fn accumulate_and_maybe_decode(
        &mut self,
        keys: &[(u8, String)],
        payload_b64: &[u8],
        cursor_row: usize,
        cursor_col: usize,
    ) {
        let more = get_u32(keys, b'm').unwrap_or(0) == 1;

        if self.discarding {
            if !more {
                self.discarding = false;
            }
            return;
        }
        let have = self.pending.as_ref().map_or(0, |p| p.data.len());
        if have.saturating_add(payload_b64.len()) > MAX_PENDING_B64 {
            // Oversized transmission: drop what we have and ignore the rest of it.
            self.pending = None;
            self.discarding = more;
            return;
        }

        let pending = self.pending.get_or_insert_with(|| PendingTransmission {
            meta: keys.to_vec(),
            data: Vec::new(),
        });
        pending.data.extend_from_slice(payload_b64);

        if more {
            return; // Wait for the remaining chunks.
        }

        let PendingTransmission { meta, data } = match self.pending.take() {
            Some(p) => p,
            None => return,
        };

        let action = get(&meta, b'a').unwrap_or("t");
        let display = action == "T";
        let format = get_u32(&meta, b'f').unwrap_or(32);
        let id = get_u32(&meta, b'i').unwrap_or(0);

        let Some(raw) = base64_decode(&data) else { return };

        let decoded = match format {
            32 => raw_dims(&meta).filter(|&(w, h)| dims_ok(w, h)).map(|(w, h)| (w, h, rgba_from_raw(&raw, w, h, 4))),
            24 => raw_dims(&meta).filter(|&(w, h)| dims_ok(w, h)).map(|(w, h)| (w, h, rgba_from_raw(&raw, w, h, 3))),
            100 => decode_png(&raw),
            _ => None,
        };

        let Some((w, h, rgba)) = decoded else { return };
        if !dims_ok(w, h) {
            return;
        }

        let actual_id = self.add_image(id, w, h, rgba);
        if display {
            let row = get_usize(&meta, b'r').unwrap_or(cursor_row);
            let col = get_usize(&meta, b'c').unwrap_or(cursor_col);
            let (cell_rows, cell_cols) = default_span(w, h);
            self.place_image(actual_id, row, col, cell_rows, cell_cols);
        }
    }
}

fn raw_dims(keys: &[(u8, String)]) -> Option<(u32, u32)> {
    let w = get_u32(keys, b's')?;
    // The real Kitty key for height is `v`; `h` is accepted too since that's
    // the spelling this terminal's own doc examples use.
    let h = get_u32(keys, b'v').or_else(|| get_u32(keys, b'h'))?;
    Some((w, h))
}

/// Guess how many terminal cells an image should span when no explicit size
/// was requested, from a rough assumed cell pixel size. The renderer stretches
/// the image to exactly fill however many cells are ultimately chosen, so
/// this only needs to be "reasonable", not pixel-accurate.
fn default_span(width: u32, height: u32) -> (usize, usize) {
    const CELL_W_PX: u32 = 9;
    const CELL_H_PX: u32 = 18;
    let cols = width.div_ceil(CELL_W_PX).max(1) as usize;
    let rows = height.div_ceil(CELL_H_PX).max(1) as usize;
    (rows, cols)
}

fn rgba_from_raw(raw: &[u8], width: u32, height: u32, channels: usize) -> Vec<u8> {
    let pixels = (width as usize).saturating_mul(height as usize);
    let mut out = vec![0u8; pixels * 4];
    for i in 0..pixels {
        let si = i * channels;
        if si + channels > raw.len() {
            break;
        }
        let di = i * 4;
        match channels {
            4 => out[di..di + 4].copy_from_slice(&raw[si..si + 4]),
            3 => {
                out[di] = raw[si];
                out[di + 1] = raw[si + 1];
                out[di + 2] = raw[si + 2];
                out[di + 3] = 255;
            }
            _ => {}
        }
    }
    out
}

// ── Kitty control-data parsing ──

/// Splits `Gf=32,s=2,v=2,a=T;<payload>` style APC content into its
/// comma-separated `key=value` pairs (keys in the Kitty protocol are always
/// a single ASCII letter) and the trailing payload bytes.
fn parse_command(buf: &[u8]) -> (Vec<(u8, String)>, &[u8]) {
    let semi = buf.iter().position(|&b| b == b';');
    let (header, payload) = match semi {
        Some(i) => (&buf[..i], &buf[i + 1..]),
        None => (buf, &buf[0..0]),
    };

    let header_str = String::from_utf8_lossy(header);
    let mut keys = Vec::new();
    for pair in header_str.split(',') {
        if pair.is_empty() {
            continue;
        }
        if let Some(eq) = pair.find('=') {
            if let Some(key) = pair.as_bytes().first().copied() {
                keys.push((key, pair[eq + 1..].to_string()));
            }
        }
    }
    (keys, payload)
}

fn get<'a>(keys: &'a [(u8, String)], key: u8) -> Option<&'a str> {
    keys.iter().find(|(k, _)| *k == key).map(|(_, v)| v.as_str())
}

fn get_u32(keys: &[(u8, String)], key: u8) -> Option<u32> {
    get(keys, key).and_then(|v| v.parse().ok())
}

fn get_usize(keys: &[(u8, String)], key: u8) -> Option<usize> {
    get(keys, key).and_then(|v| v.parse().ok())
}

// ── base64 ──

fn base64_decode(input: &[u8]) -> Option<Vec<u8>> {
    fn val(b: u8) -> Option<u8> {
        match b {
            b'A'..=b'Z' => Some(b - b'A'),
            b'a'..=b'z' => Some(b - b'a' + 26),
            b'0'..=b'9' => Some(b - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }

    let mut out = Vec::with_capacity(input.len() / 4 * 3 + 3);
    let mut buf = 0u32;
    let mut bits = 0u32;
    for &b in input {
        if b == b'=' || b == b'\n' || b == b'\r' {
            continue;
        }
        let v = val(b)?;
        buf = (buf << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    Some(out)
}

// ── Minimal PNG decoder (8-bit depth, non-interlaced) ──

struct PngHeader {
    width: u32,
    height: u32,
    bit_depth: u8,
    color_type: u8,
    interlace: u8,
}

/// Parses IHDR/PLTE/tRNS/IDAT chunks, inflates the concatenated IDAT stream,
/// reverses PNG's per-scanline filtering, and expands pixels to RGBA8.
/// Returns `None` for anything outside the supported subset (16-bit depth,
/// Adam7 interlacing, malformed data, etc.) rather than guessing.
fn decode_png(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    const SIG: [u8; 8] = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
    if !bytes.starts_with(&SIG) {
        return None;
    }

    let mut pos = 8;
    let mut header: Option<PngHeader> = None;
    let mut idat: Vec<u8> = Vec::new();
    let mut palette: Vec<[u8; 3]> = Vec::new();
    let mut trns: Vec<u8> = Vec::new();

    while pos + 8 <= bytes.len() {
        let len = u32::from_be_bytes(bytes[pos..pos + 4].try_into().unwrap()) as usize;
        let typ = &bytes[pos + 4..pos + 8];
        let data_start = pos + 8;
        let data_end = match data_start.checked_add(len) {
            Some(e) => e,
            None => break,
        };
        if data_end + 4 > bytes.len() {
            break;
        }
        let data = &bytes[data_start..data_end];

        match typ {
            b"IHDR" => {
                if data.len() < 13 {
                    return None;
                }
                header = Some(PngHeader {
                    width: u32::from_be_bytes(data[0..4].try_into().unwrap()),
                    height: u32::from_be_bytes(data[4..8].try_into().unwrap()),
                    bit_depth: data[8],
                    color_type: data[9],
                    interlace: data[12],
                });
            }
            b"PLTE" => {
                palette = data.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect();
            }
            b"tRNS" => trns = data.to_vec(),
            b"IDAT" => idat.extend_from_slice(data),
            b"IEND" => break,
            _ => {}
        }
        pos = data_end + 4; // skip CRC
    }

    let hdr = header?;
    if hdr.interlace != 0 || hdr.bit_depth != 8 {
        return None; // Adam7 / sub-byte depths unsupported — keep it simple.
    }
    if hdr.width == 0 || hdr.height == 0 {
        return None;
    }

    let width = hdr.width as usize;
    let height = hdr.height as usize;
    if !dims_ok(hdr.width, hdr.height) {
        return None; // Per-image limits, checked before any pixel buffer is allocated.
    }

    let channels: usize = match hdr.color_type {
        0 => 1, // grayscale
        2 => 3, // RGB
        3 => 1, // palette index
        4 => 2, // grayscale + alpha
        6 => 4, // RGBA
        _ => return None,
    };

    let stride = width * channels;
    // A valid stream inflates to exactly `height * (1 + stride)` bytes; never let
    // a hostile one produce more than that (zip-bomb guard).
    let inflated = inflate_zlib(&idat, height.checked_mul(stride + 1)?)?;
    let mut raw = vec![0u8; stride * height];
    unfilter(&inflated, &mut raw, width, height, channels)?;

    let mut rgba = vec![0u8; width * height * 4];
    for y in 0..height {
        for x in 0..width {
            let si = y * stride + x * channels;
            let di = (y * width + x) * 4;
            match hdr.color_type {
                0 => {
                    let g = raw[si];
                    rgba[di..di + 4].copy_from_slice(&[g, g, g, 255]);
                }
                2 => {
                    rgba[di..di + 4].copy_from_slice(&[raw[si], raw[si + 1], raw[si + 2], 255]);
                }
                3 => {
                    let idx = raw[si] as usize;
                    let c = palette.get(idx).copied().unwrap_or([0, 0, 0]);
                    let a = trns.get(idx).copied().unwrap_or(255);
                    rgba[di..di + 4].copy_from_slice(&[c[0], c[1], c[2], a]);
                }
                4 => {
                    let g = raw[si];
                    rgba[di..di + 4].copy_from_slice(&[g, g, g, raw[si + 1]]);
                }
                6 => rgba[di..di + 4].copy_from_slice(&raw[si..si + 4]),
                _ => {}
            }
        }
    }

    Some((hdr.width, hdr.height, rgba))
}

/// Inflate `data`, refusing streams that expand past `limit` bytes.
fn inflate_zlib(data: &[u8], limit: usize) -> Option<Vec<u8>> {
    let decoder = flate2::read::ZlibDecoder::new(data);
    let mut out = Vec::new();
    decoder.take(limit as u64 + 1).read_to_end(&mut out).ok()?;
    if out.len() > limit {
        return None;
    }
    Some(out)
}

/// Reverses PNG's per-scanline filtering (spec section on filter types
/// 0=None, 1=Sub, 2=Up, 3=Average, 4=Paeth) in place into `out`.
fn unfilter(inflated: &[u8], out: &mut [u8], width: usize, height: usize, bpp: usize) -> Option<()> {
    let stride = width * bpp;
    if inflated.len() < (stride + 1).saturating_mul(height) {
        return None;
    }

    let mut prev = vec![0u8; stride];
    let mut pos = 0;
    for y in 0..height {
        let filter = inflated[pos];
        pos += 1;
        let line = &inflated[pos..pos + stride];
        pos += stride;

        let out_row = &mut out[y * stride..(y + 1) * stride];
        for x in 0..stride {
            let a = if x >= bpp { out_row[x - bpp] } else { 0 };
            let b = prev[x];
            let c = if x >= bpp { prev[x - bpp] } else { 0 };
            let raw = line[x];
            out_row[x] = match filter {
                0 => raw,
                1 => raw.wrapping_add(a),
                2 => raw.wrapping_add(b),
                3 => raw.wrapping_add(((a as u16 + b as u16) / 2) as u8),
                4 => raw.wrapping_add(paeth(a, b, c)),
                _ => return None,
            };
        }
        prev.copy_from_slice(out_row);
    }
    Some(())
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = a as i32 + b as i32 - c as i32;
    let pa = (p - a as i32).abs();
    let pb = (p - b as i32).abs();
    let pc = (p - c as i32).abs();
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::Terminal;

    // Hand-crafted (not from any image editor) 3x3 RGBA PNG. Rows use PNG
    // filter types 0 (None), 1 (Sub) and 2 (Up) respectively, so decoding it
    // correctly exercises chunk parsing, zlib inflate, and the unfilter math
    // for the two most common real-world filter types.
    const RGBA_PNG: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x03, 0x08, 0x06, 0x00, 0x00, 0x00, 0x56,
        0x28, 0xb5, 0xbf, 0x00, 0x00, 0x00, 0x2d, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0xf8,
        0xcf, 0xc0, 0xf0, 0x1f, 0x08, 0x1b, 0x80, 0x94, 0x03, 0x23, 0x97, 0x88, 0xdc, 0xff, 0x7d,
        0x4d, 0x6e, 0x27, 0x2d, 0x73, 0xe6, 0xdb, 0x30, 0x7d, 0x7d, 0xfd, 0x90, 0xc1, 0x22, 0x6b,
        0x8e, 0x45, 0xbd, 0x9d, 0x2c, 0x0f, 0x00, 0x26, 0x8f, 0x0e, 0xe7, 0xae, 0xe6, 0xdf, 0x89,
        0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];
    const RGBA_EXPECTED: [[u8; 4]; 9] = [
        [255, 0, 0, 255],
        [0, 255, 0, 128],
        [0, 0, 255, 64],
        [10, 20, 30, 255],
        [200, 150, 100, 200],
        [1, 2, 3, 4],
        [255, 255, 255, 255],
        [0, 0, 0, 0],
        [128, 64, 32, 16],
    ];
    const RGBA_PNG_B64: &str = "iVBORw0KGgoAAAANSUhEUgAAAAMAAAADCAYAAABWKLW/AAAALUlEQVR42mP4z8DwHwgbgJQDI5eI3P99TW4nLXPm2zB9ff2QwSJrjkW9nSwPACaPDueu5t+JAAAAAElFTkSuQmCC";

    // 4x5 grayscale PNG whose 5 rows use filter types 0,1,2,3,4 respectively
    // (None/Sub/Up/Average/Paeth — one row each), so all five PNG filter
    // reconstruction functions get exercised at least once (the RGBA
    // fixture above only covers None/Sub/Up).
    const GRAY_PNG: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x05, 0x08, 0x00, 0x00, 0x00, 0x00, 0x47,
        0xc6, 0x12, 0x07, 0x00, 0x00, 0x00, 0x1e, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0xe0,
        0x36, 0x08, 0xad, 0x62, 0x4c, 0x53, 0x55, 0x55, 0x65, 0x8a, 0x06, 0x02, 0xe6, 0x3d, 0x07,
        0x1c, 0x1c, 0x58, 0xa2, 0x81, 0x5c, 0x00, 0x4a, 0x02, 0x06, 0x1c, 0x8e, 0xaa, 0xa3, 0x70,
        0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];
    const GRAY_EXPECTED: [u8; 20] = [
        11, 48, 85, 122, 102, 139, 176, 213, 193, 230, 11, 48, 28, 65, 102, 139, 119, 156, 193,
        230,
    ];

    fn base64_encode_for_test(data: &[u8]) -> String {
        const ALPHA: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in data.chunks(3) {
            let b0 = chunk[0];
            let b1 = chunk.get(1).copied();
            let b2 = chunk.get(2).copied();
            out.push(ALPHA[(b0 >> 2) as usize] as char);
            out.push(ALPHA[(((b0 & 0x03) << 4) | (b1.unwrap_or(0) >> 4)) as usize] as char);
            match b1 {
                Some(b1) => {
                    out.push(ALPHA[(((b1 & 0x0f) << 2) | (b2.unwrap_or(0) >> 6)) as usize] as char);
                }
                None => out.push('='),
            }
            match b2 {
                Some(b2) => out.push(ALPHA[(b2 & 0x3f) as usize] as char),
                None => out.push('='),
            }
        }
        out
    }

    /// Simulates exactly what `Pane::process_output` does: feed the raw PTY
    /// bytes of one `ESC _ G<payload> ESC \` APC command through
    /// `Terminal::feed_apc_byte`, one byte at a time.
    fn feed_apc_command(term: &mut Terminal, payload: &str) {
        term.feed_apc_byte(0x1b);
        term.feed_apc_byte(b'_');
        for b in payload.bytes() {
            term.feed_apc_byte(b);
        }
        term.feed_apc_byte(0x1b);
        term.feed_apc_byte(b'\\');
    }

    #[test]
    fn decodes_rgba_png_with_sub_and_up_filters() {
        let (w, h, rgba) = decode_png(RGBA_PNG).expect("valid RGBA PNG should decode");
        assert_eq!((w, h), (3, 3));
        for (i, px) in RGBA_EXPECTED.iter().enumerate() {
            assert_eq!(&rgba[i * 4..i * 4 + 4], px, "pixel {i} mismatch");
        }
    }

    #[test]
    fn decodes_grayscale_png_with_all_filter_types() {
        let (w, h, rgba) = decode_png(GRAY_PNG).expect("valid grayscale PNG should decode");
        assert_eq!((w, h), (4, 5));
        for (i, &g) in GRAY_EXPECTED.iter().enumerate() {
            assert_eq!(&rgba[i * 4..i * 4 + 4], &[g, g, g, 255], "pixel {i} mismatch");
        }
    }

    #[test]
    fn base64_round_trips_through_production_decoder() {
        let data: Vec<u8> = (0..=255u8).collect();
        let encoded = base64_encode_for_test(&data);
        let decoded = base64_decode(encoded.as_bytes()).expect("valid base64");
        assert_eq!(decoded, data);
    }

    #[test]
    fn raw_rgba_direct_transmission_end_to_end() {
        let mut term = Terminal::new(80, 24);
        term.cursor_row = 2;
        term.cursor_col = 3;

        // 2x1 RGBA: opaque red, half-alpha blue.
        let raw: Vec<u8> = vec![255, 0, 0, 255, 0, 0, 255, 128];
        let b64 = base64_encode_for_test(&raw);
        let payload = format!("Gf=32,s=2,v=1,a=T,i=5;{b64}");
        feed_apc_command(&mut term, &payload);

        let img = term.image_store.get_image(5).expect("image 5 should be stored");
        assert_eq!((img.width, img.height), (2, 1));
        assert_eq!(img.data, raw);

        // Placed at the cursor; a 2px-wide image spans 1 cell at the assumed
        // 9px/cell default, so only column 3 (the cursor column) is covered.
        let cell = term.image_store.get_cell(2, 3).expect("cursor cell should be covered");
        assert_eq!(cell.image_id, 5);
        assert_eq!((cell.offset_x, cell.offset_y), (0, 0));
        assert!(term.image_store.get_cell(2, 4).is_none());
        assert!(term.image_store.get_cell(3, 3).is_none());
    }

    #[test]
    fn chunked_png_transmission_reassembles_before_decoding() {
        let mut term = Terminal::new(80, 24);
        term.cursor_row = 0;
        term.cursor_col = 0;

        let mid = RGBA_PNG_B64.len() / 2;
        let (part_a, part_b) = RGBA_PNG_B64.split_at(mid);

        // First chunk carries the real control data plus `m=1`; the
        // continuation only needs `m=0` per the Kitty spec.
        feed_apc_command(&mut term, &format!("Gf=100,a=T,i=9,m=1;{part_a}"));
        assert!(
            term.image_store.get_image(9).is_none(),
            "image must not appear until the final chunk arrives"
        );
        feed_apc_command(&mut term, &format!("Gm=0;{part_b}"));

        let img = term.image_store.get_image(9).expect("image 9 should exist after final chunk");
        assert_eq!((img.width, img.height), (3, 3));
        for (i, px) in RGBA_EXPECTED.iter().enumerate() {
            assert_eq!(&img.data[i * 4..i * 4 + 4], px, "pixel {i} mismatch");
        }
        assert!(term.image_store.get_cell(0, 0).is_some());
    }

    #[test]
    fn transmit_only_does_not_place_until_explicit_put() {
        let mut term = Terminal::new(80, 24);
        let raw: Vec<u8> = vec![1, 2, 3, 4];
        let b64 = base64_encode_for_test(&raw);
        feed_apc_command(&mut term, &format!("Gf=32,s=1,v=1,a=t,i=3;{b64}"));

        assert!(term.image_store.get_image(3).is_some());
        assert!(term.image_store.get_cell(0, 0).is_none(), "a=t must not place");

        term.cursor_row = 1;
        term.cursor_col = 1;
        feed_apc_command(&mut term, "Ga=p,i=3;");
        let cell = term.image_store.get_cell(1, 1).expect("a=p should place at the cursor");
        assert_eq!(cell.image_id, 3);
    }

    #[test]
    fn delete_action_removes_image() {
        let mut term = Terminal::new(80, 24);
        let raw: Vec<u8> = vec![9, 9, 9, 9];
        let b64 = base64_encode_for_test(&raw);
        feed_apc_command(&mut term, &format!("Gf=32,s=1,v=1,a=T,i=4;{b64}"));
        assert!(term.image_store.get_image(4).is_some());

        feed_apc_command(&mut term, "Ga=d,i=4;");
        assert!(term.image_store.get_image(4).is_none());
    }

    #[test]
    fn non_apc_bytes_are_not_mistaken_for_graphics_commands() {
        // A bare `ESC \` with nothing before it, and a CSI sequence, must
        // not trip the scanner into (mis)dispatching anything.
        let mut term = Terminal::new(80, 24);
        term.feed_apc_byte(0x1b);
        term.feed_apc_byte(b'\\');
        term.feed_apc_byte(0x1b);
        term.feed_apc_byte(b'[');
        term.feed_apc_byte(b'1');
        term.feed_apc_byte(b'm');
        assert!(term.image_store.get_cell(0, 0).is_none());
    }

    #[test]
    fn store_evicts_least_recently_used_over_budget() {
        let mut store = ImageStore::new();
        let img = vec![0u8; MAX_IMAGE_BYTES]; // 64 MB each: 4 fit in 256 MB exactly
        for id in 1..=4 {
            store.add_image(id, 4096, 4096, img.clone());
        }
        assert_eq!(store.total_bytes(), 4 * MAX_IMAGE_BYTES);
        assert!(store.get_image(1).is_some()); // touch 1: now 2 is the oldest
        store.add_image(5, 4096, 4096, img.clone());
        assert!(store.get_image(2).is_none(), "LRU image evicted");
        assert!(store.get_image(1).is_some() && store.get_image(5).is_some());
        assert!(store.total_bytes() <= STORE_BUDGET);
    }

    #[test]
    fn oversized_dimensions_are_rejected_before_allocation() {
        let mut term = Terminal::new(80, 24);
        // 65535 x 65535 raw RGBA would be 17 GB.
        feed_apc_command(&mut term, "Ga=T,f=32,s=65535,v=65535;AAAA");
        assert!(term.image_store.get_cell(0, 0).is_none());
        assert_eq!(term.image_store.total_bytes(), 0);
        // 8193 wide is over the edge limit even though the byte size is small.
        feed_apc_command(&mut term, "Ga=T,f=32,s=8193,v=1,i=9;AAAA");
        assert!(term.image_store.get_image(9).is_none());
    }

    #[test]
    fn png_that_inflates_past_its_header_is_refused() {
        // Header says 2x2 RGBA (needs 2*(1+8)=18 inflated bytes); stream inflates to 1 MB.
        use std::io::Write;
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
        z.write_all(&vec![0u8; 1 << 20]).unwrap();
        let idat = z.finish().unwrap();
        assert!(inflate_zlib(&idat, 18).is_none());
        assert_eq!(inflate_zlib(&idat, 1 << 20).map(|v| v.len()), Some(1 << 20));
    }

    #[test]
    fn runaway_chunked_transmission_is_dropped() {
        let mut store = ImageStore::new();
        let big = vec![b'A'; MAX_PENDING_B64 / 2 + 1];
        store.handle_kitty_command(0, 0, &[b"a=t,f=32,s=1,v=1,m=1;".as_slice(), &big].concat());
        store.handle_kitty_command(0, 0, &[b"m=1;".as_slice(), &big].concat());
        assert!(store.pending.is_none() && store.discarding);
        store.handle_kitty_command(0, 0, b"m=0;AAAA"); // tail of the oversized transmission is swallowed
        assert!(!store.discarding && store.get_image(1).is_none());
    }
}
