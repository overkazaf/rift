use crate::config::Rgb;
use crate::renderer::font::FontManager;

pub fn render_text(
    buffer: &mut [u32],
    buf_width: usize,
    font: &mut FontManager,
    text: &str,
    x: usize,
    y: usize,
    color: Rgb,
) {
    let cw = font.cell_width;
    let ch = font.cell_height;
    for (i, c) in text.chars().enumerate() {
        let gx = x + i * cw;
        if gx + cw > buf_width { break; }
        if c == ' ' { continue; }
        let bitmap = font.rasterize(c);
        for cy in 0..ch {
            for cx in 0..cw {
                let coverage = bitmap[cy * cw + cx] as u32;
                if coverage == 0 { continue; }
                let idx = (y + cy) * buf_width + gx + cx;
                if idx < buffer.len() {
                    let inv = 255 - coverage;
                    let base = buffer[idx];
                    let br = (base >> 16) & 0xff;
                    let bg = (base >> 8) & 0xff;
                    let bb = base & 0xff;
                    let r = (color.0 as u32 * coverage + br * inv) / 255;
                    let g = (color.1 as u32 * coverage + bg * inv) / 255;
                    let b = (color.2 as u32 * coverage + bb * inv) / 255;
                    buffer[idx] = (r << 16) | (g << 8) | b;
                }
            }
        }
    }
}

pub fn fill_rect(buffer: &mut [u32], buf_w: usize, x: usize, y: usize, w: usize, h: usize, px: u32) {
    for dy in 0..h {
        let off = (y + dy) * buf_w + x;
        let end = (off + w).min(buffer.len());
        if off < buffer.len() { buffer[off..end].fill(px); }
    }
}

pub fn draw_border(buffer: &mut [u32], buf_w: usize, x: usize, y: usize, w: usize, h: usize, px: u32) {
    for dx in 0..w {
        set_px(buffer, buf_w, y, x + dx, px);
        if h > 0 { set_px(buffer, buf_w, y + h - 1, x + dx, px); }
    }
    for dy in 0..h {
        set_px(buffer, buf_w, y + dy, x, px);
        if w > 0 { set_px(buffer, buf_w, y + dy, x + w - 1, px); }
    }
}

pub fn set_px(buffer: &mut [u32], width: usize, y: usize, x: usize, px: u32) {
    if x < width {
        let idx = y * width + x;
        if idx < buffer.len() {
            buffer[idx] = px;
        }
    }
}

static BACKDROP_USED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Record that a full-window dim backdrop was drawn this frame. The GPU text
/// renderer cannot dim terminal text it did not draw into the CPU buffer, so
/// it hands such frames to the CPU renderer (see `Renderer::set_gpu_suspended`).
pub fn mark_backdrop() {
    BACKDROP_USED.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Whether a backdrop was drawn since the last call.
#[allow(dead_code)]
pub fn take_backdrop() -> bool {
    BACKDROP_USED.swap(false, std::sync::atomic::Ordering::Relaxed)
}

#[inline]
pub fn pack(r: u8, g: u8, b: u8) -> u32 {
    (r as u32) << 16 | (g as u32) << 8 | b as u32
}

#[inline]
pub fn pack_rgb(c: Rgb) -> u32 {
    pack(c.0, c.1, c.2)
}

#[inline]
pub fn darken(c: Rgb, n: u8) -> Rgb {
    (c.0.saturating_sub(n), c.1.saturating_sub(n), c.2.saturating_sub(n))
}

#[inline]
pub fn lighten(c: Rgb, n: u8) -> Rgb {
    (c.0.saturating_add(n), c.1.saturating_add(n), c.2.saturating_add(n))
}

#[inline]
pub fn dim(c: Rgb, f: f32) -> Rgb {
    ((c.0 as f32 * f) as u8, (c.1 as f32 * f) as u8, (c.2 as f32 * f) as u8)
}

#[allow(dead_code)]
pub fn dim_backdrop(buffer: &mut [u32], factor: u32) {
    mark_backdrop();
    for px in buffer.iter_mut() {
        let r = ((*px >> 16) & 0xff) / factor;
        let g = ((*px >> 8) & 0xff) / factor;
        let b = (*px & 0xff) / factor;
        *px = (r << 16) | (g << 8) | b;
    }
}

/// Animated spinner — returns a frame character based on elapsed time.
/// Uses ASCII chars for maximum font compatibility.
pub fn spinner_char(elapsed_secs: f32) -> char {
    const FRAMES: &[char] = &['|', '/', '-', '\\'];
    let idx = (elapsed_secs * 8.0) as usize % FRAMES.len();
    FRAMES[idx]
}

/// Render an animated spinner with message
pub fn render_spinner(
    buffer: &mut [u32], buf_width: usize,
    font: &mut FontManager,
    x: usize, y: usize,
    message: &str,
    accent: Rgb, text_color: Rgb,
    elapsed_secs: f32,
) {
    let spinner = spinner_char(elapsed_secs);
    let cw = font.cell_width;

    // Render spinner character in accent color
    let s = spinner.to_string();
    render_text(buffer, buf_width, font, &s, x, y, accent);

    // Render message
    render_text(buffer, buf_width, font, message, x + cw * 2, y, text_color);
}

/// Render a pulsing dot indicator (3 dots animating)
pub fn render_dots(
    buffer: &mut [u32], buf_width: usize,
    font: &mut FontManager,
    x: usize, y: usize,
    color: Rgb,
    elapsed_secs: f32,
) {
    let phase = (elapsed_secs * 3.0) as usize % 4;
    let dots = match phase {
        0 => "   ",
        1 => ".  ",
        2 => ".. ",
        _ => "...",
    };
    render_text(buffer, buf_width, font, dots, x, y, color);
}

pub fn trunc(s: &str, max: usize) -> &str {
    if s.chars().count() <= max { return s; }
    let mut end = 0;
    for (i, (byte_pos, _)) in s.char_indices().enumerate() {
        if i >= max { break; }
        end = byte_pos;
    }
    if end == 0 && !s.is_empty() { &s[..1.min(s.len())] } else { &s[..end] }
}
