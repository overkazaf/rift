#[derive(Debug, Clone)]
pub enum ShaderEffect {
    Crt(CrtParams),
    MatrixRain(MatrixParams),
    NeonGlow(NeonParams),
    Glitch(GlitchParams),
    Amber(AmberParams),
    Hologram(HologramParams),
    Pixelate(PixelateParams),
    Thermal(ThermalParams),
    Raindrop(RaindropParams),
    Vhs(VhsParams),
    CyberpunkGrid(GridParams),
    FilmGrain(FilmGrainParams),
    Invert(InvertParams),
    Desaturate(DesaturateParams),
    Chromatic(ChromaticParams),
    Pulse(PulseParams),
    Snow(SnowParams),
    Underwater(UnderwaterParams),
    NeonOutline(NeonOutlineParams),
    ScanlineRgb(ScanlineRgbParams),
    Custom { path: std::path::PathBuf },
}

#[derive(Debug, Clone)]
pub struct CrtParams {
    pub scanline_intensity: f32,
    pub curvature: f32,
    pub chromatic_aberration: f32,
    pub flicker: f32,
    pub vignette: f32,
}

impl Default for CrtParams {
    fn default() -> Self {
        Self {
            scanline_intensity: 0.45,
            curvature: 0.35,
            chromatic_aberration: 0.006,
            flicker: 0.06,
            vignette: 0.35,
        }
    }
}

#[derive(Debug, Clone)]
pub struct MatrixParams {
    pub speed: f32,
    pub density: f32,
    pub color: (f32, f32, f32),
    pub opacity: f32,
}

impl Default for MatrixParams {
    fn default() -> Self {
        Self {
            speed: 1.5,
            density: 0.6,
            color: (0.0, 1.0, 0.3),
            opacity: 0.4,
        }
    }
}

#[derive(Debug, Clone)]
pub struct NeonParams {
    pub glow_radius: f32,
    pub intensity: f32,
    pub color: (f32, f32, f32),
}

impl Default for NeonParams {
    fn default() -> Self {
        Self {
            glow_radius: 6.0,
            intensity: 0.6,
            color: (0.4, 0.8, 1.0),
        }
    }
}

#[derive(Debug, Clone)]
pub struct GlitchParams {
    pub frequency: f32,
    pub intensity: f32,
    pub block_size: f32,
}

impl Default for GlitchParams {
    fn default() -> Self {
        Self {
            frequency: 1.2,
            intensity: 0.6,
            block_size: 12.0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AmberParams {
    pub intensity: f32,
    pub phosphor_decay: f32,
}

impl Default for AmberParams {
    fn default() -> Self {
        Self { intensity: 0.85, phosphor_decay: 0.1 }
    }
}

#[derive(Debug, Clone)]
pub struct HologramParams {
    pub scan_speed: f32,
    pub jitter: f32,
    pub opacity: f32,
}

impl Default for HologramParams {
    fn default() -> Self {
        Self { scan_speed: 1.0, jitter: 0.3, opacity: 0.8 }
    }
}

#[derive(Debug, Clone)]
pub struct PixelateParams {
    pub block_size: usize,
}

impl Default for PixelateParams {
    fn default() -> Self {
        Self { block_size: 4 }
    }
}

#[derive(Debug, Clone)]
pub struct ThermalParams {
    pub intensity: f32,
}

impl Default for ThermalParams {
    fn default() -> Self {
        Self { intensity: 0.8 }
    }
}

#[derive(Debug, Clone)]
pub struct RaindropParams {
    pub intensity: f32,
    pub speed: f32,
}

impl Default for RaindropParams {
    fn default() -> Self { Self { intensity: 0.4, speed: 1.5 } }
}

#[derive(Debug, Clone)]
pub struct VhsParams {
    pub noise: f32,
    pub tracking: f32,
    pub color_bleed: f32,
}

impl Default for VhsParams {
    fn default() -> Self { Self { noise: 0.3, tracking: 0.4, color_bleed: 0.3 } }
}

#[derive(Debug, Clone)]
pub struct GridParams {
    pub spacing: usize,
    pub opacity: f32,
    pub color: (u8, u8, u8),
}

impl Default for GridParams {
    fn default() -> Self { Self { spacing: 40, opacity: 0.08, color: (0, 255, 200) } }
}

#[derive(Debug, Clone)]
pub struct FilmGrainParams {
    pub intensity: f32,
    pub size: f32,
}

impl Default for FilmGrainParams {
    fn default() -> Self { Self { intensity: 0.15, size: 1.0 } }
}

#[derive(Debug, Clone)]
pub struct InvertParams {
    pub intensity: f32,
}

impl Default for InvertParams {
    fn default() -> Self { Self { intensity: 1.0 } }
}

#[derive(Debug, Clone)]
pub struct DesaturateParams {
    pub intensity: f32,
}

impl Default for DesaturateParams {
    fn default() -> Self { Self { intensity: 0.8 } }
}

// ── CPU post-processing implementations ──

pub fn apply_crt(buffer: &mut [u32], src: &[u32], width: u32, height: u32, params: &CrtParams, time: f32) {
    let w = width as usize;
    let h = height as usize;
    if w == 0 || h == 0 { return; }

    for y in 0..h {
        for x in 0..w {
            let u = x as f32 / w as f32;
            let v = y as f32 / h as f32;

            // Barrel distortion (CRT curvature)
            let cu = u - 0.5;
            let cv = v - 0.5;
            let dist = cu * cu + cv * cv;
            let su = cu * (1.0 + dist * params.curvature) + 0.5;
            let sv = cv * (1.0 + dist * params.curvature) + 0.5;

            if su < 0.0 || su >= 1.0 || sv < 0.0 || sv >= 1.0 {
                buffer[y * w + x] = 0;
                continue;
            }

            // Chromatic aberration — offset R and B channels horizontally
            let sy = ((sv * h as f32) as usize).min(h - 1);
            let sx = ((su * w as f32) as usize).min(w - 1);
            let r_x = (((su + params.chromatic_aberration) * w as f32) as usize).min(w - 1);
            let b_x = ((su - params.chromatic_aberration) * w as f32).max(0.0) as usize;

            let r = (src[sy * w + r_x] >> 16) & 0xff;
            let g = (src[sy * w + sx] >> 8) & 0xff;
            let b = src[sy * w + b_x] & 0xff;

            let mut pixel = (r << 16) | (g << 8) | b;

            // Scanlines
            let scanline = (v * h as f32 * std::f32::consts::PI).sin() * 0.5 + 0.5;
            let scan_dim = 1.0 - params.scanline_intensity * (1.0 - scanline);
            pixel = dim_pixel(pixel, scan_dim);

            // Flicker
            let flicker = 1.0 - params.flicker * (time * 8.0).sin().abs();
            pixel = dim_pixel(pixel, flicker);

            // Vignette
            let vig_dist = ((u - 0.5).powi(2) + (v - 0.5).powi(2)).sqrt();
            let vig = smoothstep(0.8, 0.3, vig_dist);
            let vig_factor = 1.0 - params.vignette * (1.0 - vig);
            pixel = dim_pixel(pixel, vig_factor);

            buffer[y * w + x] = pixel;
        }
    }
}

pub fn apply_glitch(buffer: &mut [u32], src: &[u32], width: u32, height: u32, params: &GlitchParams, time: f32) {
    let w = width as usize;
    let h = height as usize;
    if w == 0 || h == 0 { return; }

    // Erratic triggering: hash-based, not smooth sine
    let frame = (time * 60.0) as u32;
    let trigger_hash = xorshift(frame.wrapping_mul(2654435761));
    let trigger_chance = (params.frequency * 0.4 * 255.0) as u32;
    if (trigger_hash & 0xFF) > trigger_chance { return; }
    let block_h = (params.block_size as usize).max(4);
    let seed = xorshift(frame.wrapping_mul(1664525).wrapping_add(1013904223));

    let num_bands = 4 + (params.intensity * 8.0) as usize;
    for i in 0..num_bands {
        let band_seed = xorshift(seed.wrapping_add(i as u32 * 7919));
        let band_y = (band_seed as usize) % h;
        let band_height = (block_h + (band_seed >> 8) as usize % (block_h * 2)).min(h - band_y);
        let max_shift = (w as f32 * 0.15 * params.intensity) as i32;
        let offset = ((band_seed >> 16) as i32 % (max_shift * 2 + 1)) - max_shift;

        if offset.abs() < 2 { continue; }

        for by in band_y..(band_y + band_height).min(h) {
            for bx in 0..w {
                let src_x = (bx as i32 - offset).clamp(0, w as i32 - 1) as usize;
                buffer[by * w + bx] = src[by * w + src_x];
            }
        }

        // Color channel split — more aggressive
        if i % 2 == 0 {
            let channel_offset = (offset.abs()).max(3);
            for by in band_y..(band_y + band_height).min(h) {
                for bx in 0..w {
                    let r_src = (bx as i32 + channel_offset).clamp(0, w as i32 - 1) as usize;
                    let b_src = (bx as i32 - channel_offset).clamp(0, w as i32 - 1) as usize;
                    let r = (src[by * w + r_src] >> 16) & 0xff;
                    let g = (buffer[by * w + bx] >> 8) & 0xff;
                    let b = src[by * w + b_src] & 0xff;
                    buffer[by * w + bx] = (r << 16) | (g << 8) | b;
                }
            }
        }
    }

    // Occasional full-screen horizontal shift for dramatic effect
    let big_glitch = xorshift(frame.wrapping_mul(48271));
    if (big_glitch & 0x1F) == 0 {
        let shift = ((big_glitch >> 8) as i32 % 40) - 20;
        let gy = ((big_glitch >> 16) as usize) % h;
        let gh = (h / 8).min(h - gy);
        for by in gy..(gy + gh) {
            for bx in 0..w {
                let sx = (bx as i32 - shift).clamp(0, w as i32 - 1) as usize;
                buffer[by * w + bx] = src[by * w + sx];
            }
        }
    }
}

pub fn apply_neon(buffer: &mut [u32], src: &[u32], width: u32, height: u32, params: &NeonParams, _time: f32) {
    let w = width as usize;
    let h = height as usize;
    if w == 0 || h == 0 { return; }

    let radius = params.glow_radius as usize;
    if radius == 0 { return; }
    let mut glow = vec![0.0f32; w * h * 3];

    let threshold = 100u32;

    for y in 0..h {
        for x in 0..w {
            let px = src[y * w + x];
            let r = (px >> 16) & 0xff;
            let g = (px >> 8) & 0xff;
            let b = px & 0xff;
            let brightness = r.max(g).max(b);

            if brightness > threshold {
                let strength = (brightness - threshold) as f32 / (255 - threshold) as f32;
                let y_start = y.saturating_sub(radius);
                let y_end = (y + radius + 1).min(h);
                let x_start = x.saturating_sub(radius);
                let x_end = (x + radius + 1).min(w);

                for gy in y_start..y_end {
                    for gx in x_start..x_end {
                        let dx = gx as f32 - x as f32;
                        let dy = gy as f32 - y as f32;
                        let dist = (dx * dx + dy * dy).sqrt();
                        if dist > radius as f32 { continue; }
                        let falloff = 1.0 - dist / radius as f32;
                        let amount = falloff * falloff * strength * params.intensity;
                        let idx = (gy * w + gx) * 3;
                        glow[idx] += amount * params.color.0;
                        glow[idx + 1] += amount * params.color.1;
                        glow[idx + 2] += amount * params.color.2;
                    }
                }
            }
        }
    }

    // Composite glow onto buffer
    for y in 0..h {
        for x in 0..w {
            let idx = (y * w + x) * 3;
            let gr = glow[idx];
            let gg = glow[idx + 1];
            let gb = glow[idx + 2];
            if gr < 0.01 && gg < 0.01 && gb < 0.01 { continue; }

            let px = buffer[y * w + x];
            let r = ((px >> 16) & 0xff) as f32;
            let g = ((px >> 8) & 0xff) as f32;
            let b = (px & 0xff) as f32;

            let nr = (r + gr * 255.0).min(255.0) as u32;
            let ng = (g + gg * 255.0).min(255.0) as u32;
            let nb = (b + gb * 255.0).min(255.0) as u32;
            buffer[y * w + x] = (nr << 16) | (ng << 8) | nb;
        }
    }
}

/// Matrix rain state for persistent column positions
pub struct MatrixState {
    columns: Vec<f32>,
    width: usize,
}

impl MatrixState {
    pub fn new() -> Self {
        Self { columns: Vec::new(), width: 0 }
    }

    pub fn apply(&mut self, buffer: &mut [u32], width: u32, height: u32, params: &MatrixParams, time: f32) {
        let w = width as usize;
        let h = height as usize;
        if w == 0 || h == 0 { return; }

        let col_spacing = 12usize;
        let num_cols = w / col_spacing;

        // Reinitialize if width changed
        if self.width != w {
            self.width = w;
            self.columns.resize(num_cols, 0.0);
            for (i, col) in self.columns.iter_mut().enumerate() {
                *col = pseudo_random(i as u32 * 31337) * h as f32;
            }
        }

        let char_h = 14usize;
        let trail_len = 15;
        let cr = (params.color.0 * 255.0) as u32;
        let cg = (params.color.1 * 255.0) as u32;
        let cb = (params.color.2 * 255.0) as u32;

        for (i, head_y) in self.columns.iter_mut().enumerate() {
            *head_y += params.speed * 60.0 * 0.016;
            if *head_y > (h + trail_len * char_h) as f32 {
                *head_y = -(pseudo_random(xorshift((time * 1000.0) as u32 + i as u32)) * h as f32 * 0.3);
            }

            let cx = i * col_spacing + col_spacing / 2;
            if cx >= w { continue; }

            for t in 0..trail_len {
                let cy = *head_y as i32 - (t * char_h) as i32;
                if cy < 0 || cy >= h as i32 { continue; }

                let fade = 1.0 - t as f32 / trail_len as f32;
                // Head character is extra bright
                let head_boost = if t == 0 { 2.0 } else { 1.0 };
                let alpha = (fade * fade * params.opacity * head_boost * 255.0).min(255.0) as u32;
                if alpha == 0 { continue; }

                // Draw a small "character" block
                let block_w = 6usize.min(w - cx);
                let block_h = (char_h - 2).min(h - cy as usize);
                let char_seed = xorshift((i * trail_len + t) as u32 + (time * 3.0) as u32);

                for by in 0..block_h {
                    let py = cy as usize + by;
                    for bx in 0..block_w {
                        let px_x = cx.saturating_sub(block_w / 2) + bx;
                        if px_x >= w { continue; }
                        // Simple pseudo-glyph pattern
                        let bit = (char_seed >> ((by * block_w + bx) % 32)) & 1;
                        if bit == 0 && t > 0 { continue; }

                        let idx = py * w + px_x;
                        let existing = buffer[idx];
                        let er = (existing >> 16) & 0xff;
                        let eg = (existing >> 8) & 0xff;
                        let eb = existing & 0xff;
                        let inv = 255 - alpha;
                        // Head glyph glows white-green
                        let (fr, fg_c, fb) = if t == 0 { (200, 255, 220) } else { (cr, cg, cb) };
                        let r = (fr * alpha + er * inv) / 255;
                        let g = (fg_c * alpha + eg * inv) / 255;
                        let b = (fb * alpha + eb * inv) / 255;
                        buffer[idx] = (r << 16) | (g << 8) | b;
                    }
                }
            }
        }
    }
}

// ── New effects ──

pub fn apply_amber(buffer: &mut [u32], src: &[u32], width: u32, height: u32, params: &AmberParams, _time: f32) {
    let len = (width as usize * height as usize).min(buffer.len()).min(src.len());
    for i in 0..len {
        let px = src[i];
        let r = ((px >> 16) & 0xff) as f32;
        let g = ((px >> 8) & 0xff) as f32;
        let b = (px & 0xff) as f32;
        let lum = (0.299 * r + 0.587 * g + 0.114 * b) / 255.0;
        let lum = lum * params.intensity + (1.0 - params.intensity) * (r + g + b) / 765.0;
        let ar = (lum * 255.0).min(255.0) as u32;
        let ag = (lum * 176.0).min(255.0) as u32;
        let ab = (lum * 20.0).min(255.0) as u32;
        buffer[i] = (ar << 16) | (ag << 8) | ab;
    }
}

pub fn apply_hologram(buffer: &mut [u32], src: &[u32], width: u32, height: u32, params: &HologramParams, time: f32) {
    let w = width as usize;
    let h = height as usize;
    if w == 0 || h == 0 { return; }
    let scan_y = ((time * params.scan_speed * 100.0) as usize) % h;
    for y in 0..h {
        let scan_dist = ((y as i32 - scan_y as i32).abs() as f32) / h as f32;
        let scan_bright = if scan_dist < 0.02 { 1.5 } else { 1.0 };
        let jitter = if params.jitter > 0.0 {
            let hash = (y as u32).wrapping_mul(2654435761).wrapping_add((time * 60.0) as u32);
            let hf = xorshift(hash) as f32 / u32::MAX as f32;
            ((hf - 0.5) * params.jitter * 10.0) as i32
        } else { 0 };
        for x in 0..w {
            let sx = (x as i32 + jitter).clamp(0, w as i32 - 1) as usize;
            let si = y * w + sx;
            let di = y * w + x;
            if si >= src.len() || di >= buffer.len() { continue; }
            let px = src[si];
            let r = ((px >> 16) & 0xff) as f32;
            let g = ((px >> 8) & 0xff) as f32;
            let b = (px & 0xff) as f32;
            let lum = (0.299 * r + 0.587 * g + 0.114 * b) / 255.0;
            let mut hr = (lum * 80.0 * scan_bright).min(255.0) as u32;
            let mut hg = (lum * 200.0 * scan_bright).min(255.0) as u32;
            let mut hb = (lum * 255.0 * scan_bright).min(255.0) as u32;
            if y % 3 == 0 { hr = hr * 7 / 10; hg = hg * 7 / 10; hb = hb * 7 / 10; }
            buffer[di] = (hr << 16) | (hg << 8) | hb;
        }
    }
}

pub fn apply_pixelate(buffer: &mut [u32], src: &[u32], width: u32, height: u32, params: &PixelateParams, _time: f32) {
    let w = width as usize;
    let h = height as usize;
    if w == 0 || h == 0 { return; }
    let bs = params.block_size.max(2).min(16);
    for by in (0..h).step_by(bs) {
        for bx in (0..w).step_by(bs) {
            let (mut tr, mut tg, mut tb, mut count) = (0u32, 0u32, 0u32, 0u32);
            for dy in 0..bs {
                for dx in 0..bs {
                    let (y, x) = (by + dy, bx + dx);
                    if y < h && x < w {
                        let i = y * w + x;
                        if i < src.len() {
                            let px = src[i];
                            tr += (px >> 16) & 0xff;
                            tg += (px >> 8) & 0xff;
                            tb += px & 0xff;
                            count += 1;
                        }
                    }
                }
            }
            if count == 0 { continue; }
            let avg = ((tr / count) << 16) | ((tg / count) << 8) | (tb / count);
            for dy in 0..bs {
                for dx in 0..bs {
                    let (y, x) = (by + dy, bx + dx);
                    if y < h && x < w {
                        let i = y * w + x;
                        if i < buffer.len() { buffer[i] = avg; }
                    }
                }
            }
        }
    }
}

pub fn apply_thermal(buffer: &mut [u32], src: &[u32], width: u32, height: u32, params: &ThermalParams, _time: f32) {
    let len = (width as usize * height as usize).min(buffer.len()).min(src.len());
    for i in 0..len {
        let px = src[i];
        let r = ((px >> 16) & 0xff) as f32;
        let g = ((px >> 8) & 0xff) as f32;
        let b = (px & 0xff) as f32;
        let lum = (0.299 * r + 0.587 * g + 0.114 * b) / 255.0;
        let (tr, tg, tb) = thermal_color(lum);
        let inv = 1.0 - params.intensity;
        let fr = (tr as f32 * params.intensity + r * inv).min(255.0) as u32;
        let fg = (tg as f32 * params.intensity + g * inv).min(255.0) as u32;
        let fb = (tb as f32 * params.intensity + b * inv).min(255.0) as u32;
        buffer[i] = (fr << 16) | (fg << 8) | fb;
    }
}

fn thermal_color(t: f32) -> (u8, u8, u8) {
    let t = t.clamp(0.0, 1.0);
    if t < 0.2 {
        let f = t / 0.2;
        (0, 0, (f * 128.0) as u8)
    } else if t < 0.4 {
        let f = (t - 0.2) / 0.2;
        ((f * 128.0) as u8, 0, (128.0 + f * 127.0) as u8)
    } else if t < 0.6 {
        let f = (t - 0.4) / 0.2;
        ((128.0 + f * 127.0) as u8, 0, (255.0 - f * 255.0) as u8)
    } else if t < 0.8 {
        let f = (t - 0.6) / 0.2;
        (255, (f * 255.0) as u8, 0)
    } else {
        let f = (t - 0.8) / 0.2;
        (255, 255, (f * 255.0) as u8)
    }
}

// ── Helpers ──

// ── New effects ──

pub fn apply_raindrop(buffer: &mut [u32], src: &[u32], width: u32, height: u32, params: &RaindropParams, time: f32) {
    let w = width as usize;
    let h = height as usize;
    // Generate a few ripple centers based on time
    let num_drops = 5;
    for i in 0..(w * h).min(buffer.len()).min(src.len()) {
        let x = i % w;
        let y = i / w;
        let mut dx_total: f32 = 0.0;
        let mut dy_total: f32 = 0.0;
        for d in 0..num_drops {
            let seed = (d as f32 * 7.31 + (time * params.speed).floor() * 13.7) as u32;
            let cx = (pseudo_random(seed) * w as f32) as f32;
            let cy = (pseudo_random(seed.wrapping_add(1000)) * h as f32) as f32;
            let phase = (time * params.speed).fract();
            let dist = ((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)).sqrt();
            let ripple = (dist * 0.3 - phase * 20.0).sin() * params.intensity * (1.0 - phase) * (1.0 / (dist * 0.05 + 1.0));
            dx_total += ripple;
            dy_total += ripple;
        }
        let sx = (x as f32 + dx_total).clamp(0.0, (w - 1) as f32) as usize;
        let sy = (y as f32 + dy_total).clamp(0.0, (h - 1) as f32) as usize;
        let si = sy * w + sx;
        if si < src.len() { buffer[i] = src[si]; }
    }
}

pub fn apply_vhs(buffer: &mut [u32], src: &[u32], width: u32, height: u32, params: &VhsParams, time: f32) {
    let w = width as usize;
    let h = height as usize;
    let frame_seed = (time * 60.0) as u32;
    for y in 0..h {
        // Tracking error: random horizontal line offset
        let line_hash = xorshift(y as u32 * 31 + frame_seed);
        let tracking_offset = if pseudo_random(line_hash) < params.tracking * 0.1 {
            ((pseudo_random(line_hash.wrapping_add(99)) - 0.5) * 30.0 * params.tracking) as i32
        } else { 0 };
        for x in 0..w {
            let sx = (x as i32 + tracking_offset).clamp(0, w as i32 - 1) as usize;
            let i = y * w + x;
            let si = y * w + sx;
            if i >= buffer.len() || si >= src.len() { continue; }
            let px = src[si];
            let mut r = ((px >> 16) & 0xff) as f32;
            let mut g = ((px >> 8) & 0xff) as f32;
            let mut b = (px & 0xff) as f32;
            // Color bleed: shift R channel slightly
            let bleed_x = (sx as i32 + (params.color_bleed * 3.0) as i32).clamp(0, w as i32 - 1) as usize;
            let bleed_i = y * w + bleed_x;
            if bleed_i < src.len() { r = ((src[bleed_i] >> 16) & 0xff) as f32; }
            // Noise
            let noise_val = (pseudo_random(i as u32 + frame_seed) - 0.5) * params.noise * 80.0;
            r = (r + noise_val).clamp(0.0, 255.0);
            g = (g + noise_val).clamp(0.0, 255.0);
            b = (b + noise_val).clamp(0.0, 255.0);
            // Scanline darkening (every other line slightly dimmer)
            if y % 2 == 0 { r *= 0.92; g *= 0.92; b *= 0.92; }
            buffer[i] = ((r as u32) << 16) | ((g as u32) << 8) | b as u32;
        }
    }
}

pub fn apply_grid(buffer: &mut [u32], _src: &[u32], width: u32, height: u32, params: &GridParams, _time: f32) {
    let w = width as usize;
    let h = height as usize;
    let spacing = params.spacing.max(4);
    let alpha = (params.opacity * 255.0) as u32;
    let inv = 255 - alpha;
    let (gr, gg, gb) = params.color;
    for y in 0..h {
        for x in 0..w {
            if x % spacing == 0 || y % spacing == 0 {
                let i = y * w + x;
                if i < buffer.len() {
                    let px = buffer[i];
                    let r = (((px >> 16) & 0xff) * inv + gr as u32 * alpha) / 255;
                    let g = (((px >> 8) & 0xff) * inv + gg as u32 * alpha) / 255;
                    let b = ((px & 0xff) * inv + gb as u32 * alpha) / 255;
                    buffer[i] = (r << 16) | (g << 8) | b;
                }
            }
        }
    }
}

pub fn apply_film_grain(buffer: &mut [u32], _src: &[u32], width: u32, height: u32, params: &FilmGrainParams, time: f32) {
    let w = width as usize;
    let h = height as usize;
    let frame_seed = (time * 60.0) as u32;
    let range = (params.intensity * 60.0) as i32;
    for i in 0..(w * h).min(buffer.len()) {
        let noise = ((pseudo_random(i as u32 + frame_seed) - 0.5) * range as f32 * 2.0) as i32;
        let px = buffer[i];
        let r = (((px >> 16) & 0xff) as i32 + noise).clamp(0, 255) as u32;
        let g = (((px >> 8) & 0xff) as i32 + noise).clamp(0, 255) as u32;
        let b = ((px & 0xff) as i32 + noise).clamp(0, 255) as u32;
        buffer[i] = (r << 16) | (g << 8) | b;
    }
}

pub fn apply_invert(buffer: &mut [u32], _src: &[u32], width: u32, height: u32, params: &InvertParams, _time: f32) {
    let n = (width as usize * height as usize).min(buffer.len());
    let intensity = params.intensity.clamp(0.0, 1.0);
    let inv_i = (intensity * 255.0) as u32;
    let keep = 255 - inv_i;
    for i in 0..n {
        let px = buffer[i];
        let r = ((255 - ((px >> 16) & 0xff)) * inv_i + ((px >> 16) & 0xff) * keep) / 255;
        let g = ((255 - ((px >> 8) & 0xff)) * inv_i + ((px >> 8) & 0xff) * keep) / 255;
        let b = ((255 - (px & 0xff)) * inv_i + (px & 0xff) * keep) / 255;
        buffer[i] = (r << 16) | (g << 8) | b;
    }
}

pub fn apply_desaturate(buffer: &mut [u32], _src: &[u32], width: u32, height: u32, params: &DesaturateParams, _time: f32) {
    let n = (width as usize * height as usize).min(buffer.len());
    let intensity = params.intensity.clamp(0.0, 1.0);
    let inv = 1.0 - intensity;
    for i in 0..n {
        let px = buffer[i];
        let r = ((px >> 16) & 0xff) as f32;
        let g = ((px >> 8) & 0xff) as f32;
        let b = (px & 0xff) as f32;
        let lum = 0.299 * r + 0.587 * g + 0.114 * b;
        let fr = (r * inv + lum * intensity).min(255.0) as u32;
        let fg = (g * inv + lum * intensity).min(255.0) as u32;
        let fb = (b * inv + lum * intensity).min(255.0) as u32;
        buffer[i] = (fr << 16) | (fg << 8) | fb;
    }
}

fn dim_pixel(pixel: u32, factor: f32) -> u32 {
    let r = (((pixel >> 16) & 0xff) as f32 * factor).min(255.0).max(0.0) as u32;
    let g = (((pixel >> 8) & 0xff) as f32 * factor).min(255.0).max(0.0) as u32;
    let b = ((pixel & 0xff) as f32 * factor).min(255.0).max(0.0) as u32;
    (r << 16) | (g << 8) | b
}

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

// ── Effect 15: Chromatic (hue shift) ──

#[derive(Debug, Clone)]
pub struct ChromaticParams {
    pub shift: f32,
}

impl Default for ChromaticParams {
    fn default() -> Self { Self { shift: 30.0 } }
}

pub fn apply_chromatic(buffer: &mut [u32], src: &[u32], width: u32, height: u32, params: &ChromaticParams, time: f32) {
    let shift = (params.shift + time * 20.0) % 360.0;
    let len = (width * height) as usize;
    for i in 0..len {
        if i >= src.len() || i >= buffer.len() { break; }
        let px = src[i];
        let r = ((px >> 16) & 0xff) as f32;
        let g = ((px >> 8) & 0xff) as f32;
        let b = (px & 0xff) as f32;
        let (h, s, v) = rgb_to_hsv(r, g, b);
        let (nr, ng, nb) = hsv_to_rgb((h + shift) % 360.0, s, v);
        buffer[i] = (nr as u32) << 16 | (ng as u32) << 8 | nb as u32;
    }
}

// ── Effect 16: Pulse (breathing brightness) ──

#[derive(Debug, Clone)]
pub struct PulseParams {
    pub speed: f32,
    pub depth: f32,
}

impl Default for PulseParams {
    fn default() -> Self { Self { speed: 1.0, depth: 0.15 } }
}

pub fn apply_pulse(buffer: &mut [u32], src: &[u32], width: u32, height: u32, params: &PulseParams, time: f32) {
    let factor = 1.0 + (time * params.speed * std::f32::consts::TAU).sin() * params.depth;
    let len = (width * height) as usize;
    for i in 0..len {
        if i >= src.len() || i >= buffer.len() { break; }
        let px = src[i];
        let r = (((px >> 16) & 0xff) as f32 * factor).min(255.0) as u32;
        let g = (((px >> 8) & 0xff) as f32 * factor).min(255.0) as u32;
        let b = ((px & 0xff) as f32 * factor).min(255.0) as u32;
        buffer[i] = (r << 16) | (g << 8) | b;
    }
}

// ── Effect 17: Snow (TV static noise) ──

#[derive(Debug, Clone)]
pub struct SnowParams {
    pub density: f32,
    pub blend: f32,
}

impl Default for SnowParams {
    fn default() -> Self { Self { density: 0.3, blend: 0.5 } }
}

pub fn apply_snow(buffer: &mut [u32], src: &[u32], width: u32, height: u32, params: &SnowParams, time: f32) {
    let seed_base = (time * 1000.0) as u32;
    let len = (width * height) as usize;
    let alpha = (params.blend * 255.0) as u32;
    let inv = 255 - alpha;
    for i in 0..len {
        if i >= src.len() || i >= buffer.len() { break; }
        let rng = pseudo_random(i as u32 ^ seed_base);
        if rng < params.density {
            let noise_val = if pseudo_random(i as u32 ^ seed_base.wrapping_add(7919)) > 0.5 { 255u32 } else { 0u32 };
            let px = src[i];
            let r = (((px >> 16) & 0xff) * inv + noise_val * alpha) / 255;
            let g = (((px >> 8) & 0xff) * inv + noise_val * alpha) / 255;
            let b = ((px & 0xff) * inv + noise_val * alpha) / 255;
            buffer[i] = (r << 16) | (g << 8) | b;
        } else {
            buffer[i] = src[i];
        }
    }
}

// ── Effect 18: Underwater (blue-green tint + wave distortion) ──

#[derive(Debug, Clone)]
pub struct UnderwaterParams {
    pub wave_speed: f32,
    pub tint: f32,
}

impl Default for UnderwaterParams {
    fn default() -> Self { Self { wave_speed: 1.0, tint: 0.3 } }
}

pub fn apply_underwater(buffer: &mut [u32], src: &[u32], width: u32, height: u32, params: &UnderwaterParams, time: f32) {
    let w = width as usize;
    let h = height as usize;
    for y in 0..h {
        let wave_offset = ((y as f32 * 0.05 + time * params.wave_speed).sin() * 3.0) as i32;
        for x in 0..w {
            let sx = (x as i32 + wave_offset).clamp(0, w as i32 - 1) as usize;
            let si = y * w + sx;
            let di = y * w + x;
            if si >= src.len() || di >= buffer.len() { continue; }
            let px = src[si];
            let r = ((px >> 16) & 0xff) as f32;
            let g = ((px >> 8) & 0xff) as f32;
            let b = (px & 0xff) as f32;
            let t = params.tint;
            let nr = (r * (1.0 - t * 0.5)).min(255.0) as u32;
            let ng = (g * (1.0 + t * 0.2)).min(255.0) as u32;
            let nb = (b * (1.0 + t * 0.4)).min(255.0) as u32;
            buffer[di] = (nr << 16) | (ng << 8) | nb;
        }
    }
}

// ── Effect 19: Neon Outline (edge detection with neon color) ──

#[derive(Debug, Clone)]
pub struct NeonOutlineParams {
    pub threshold: f32,
    pub color: (u8, u8, u8),
}

impl Default for NeonOutlineParams {
    fn default() -> Self { Self { threshold: 0.3, color: (0, 255, 200) } }
}

pub fn apply_neon_outline(buffer: &mut [u32], src: &[u32], width: u32, height: u32, params: &NeonOutlineParams, _time: f32) {
    let w = width as usize;
    let h = height as usize;
    let thresh = params.threshold * 255.0;
    for y in 1..h.saturating_sub(1) {
        for x in 1..w.saturating_sub(1) {
            let idx = y * w + x;
            if idx >= src.len() || idx >= buffer.len() { continue; }
            let c = lum(src[idx]);
            let r = lum(src[idx + 1]);
            let d = lum(src[idx + w]);
            let edge = ((c - r).abs() + (c - d).abs()) * 0.5;
            if edge > thresh {
                let a = (edge / 255.0).min(1.0);
                let inv = 1.0 - a;
                let px = src[idx];
                let pr = ((px >> 16) & 0xff) as f32;
                let pg = ((px >> 8) & 0xff) as f32;
                let pb = (px & 0xff) as f32;
                let nr = (params.color.0 as f32 * a + pr * inv).min(255.0) as u32;
                let ng = (params.color.1 as f32 * a + pg * inv).min(255.0) as u32;
                let nb = (params.color.2 as f32 * a + pb * inv).min(255.0) as u32;
                buffer[idx] = (nr << 16) | (ng << 8) | nb;
            } else {
                buffer[idx] = src[idx];
            }
        }
    }
}

fn lum(px: u32) -> f32 {
    let r = ((px >> 16) & 0xff) as f32;
    let g = ((px >> 8) & 0xff) as f32;
    let b = (px & 0xff) as f32;
    0.299 * r + 0.587 * g + 0.114 * b
}

// ── Effect 20: Scanline RGB (LCD sub-pixel simulation) ──

#[derive(Debug, Clone)]
pub struct ScanlineRgbParams {
    pub intensity: f32,
}

impl Default for ScanlineRgbParams {
    fn default() -> Self { Self { intensity: 0.4 } }
}

pub fn apply_scanline_rgb(buffer: &mut [u32], src: &[u32], width: u32, height: u32, params: &ScanlineRgbParams, _time: f32) {
    let w = width as usize;
    let h = height as usize;
    let boost = 1.0 + params.intensity;
    let suppress = 1.0 - params.intensity * 0.5;
    for y in 0..h {
        let phase = y % 3;
        for x in 0..w {
            let i = y * w + x;
            if i >= src.len() || i >= buffer.len() { continue; }
            let px = src[i];
            let mut r = ((px >> 16) & 0xff) as f32;
            let mut g = ((px >> 8) & 0xff) as f32;
            let mut b = (px & 0xff) as f32;
            match phase {
                0 => { r *= boost; g *= suppress; b *= suppress; }
                1 => { r *= suppress; g *= boost; b *= suppress; }
                _ => { r *= suppress; g *= suppress; b *= boost; }
            }
            buffer[i] = ((r.min(255.0) as u32) << 16) | ((g.min(255.0) as u32) << 8) | (b.min(255.0) as u32);
        }
    }
}

// ── RGB ↔ HSV conversion helpers ──

fn rgb_to_hsv(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let r = r / 255.0;
    let g = g / 255.0;
    let b = b / 255.0;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let delta = max - min;
    let v = max;
    let s = if max == 0.0 { 0.0 } else { delta / max };
    let h = if delta == 0.0 {
        0.0
    } else if max == r {
        60.0 * (((g - b) / delta) % 6.0)
    } else if max == g {
        60.0 * ((b - r) / delta + 2.0)
    } else {
        60.0 * ((r - g) / delta + 4.0)
    };
    let h = if h < 0.0 { h + 360.0 } else { h };
    (h, s, v)
}

fn hsv_to_rgb(h: f32, s: f32, v: f32) -> (u8, u8, u8) {
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = v - c;
    let (r, g, b) = if h < 60.0 { (c, x, 0.0) }
        else if h < 120.0 { (x, c, 0.0) }
        else if h < 180.0 { (0.0, c, x) }
        else if h < 240.0 { (0.0, x, c) }
        else if h < 300.0 { (x, 0.0, c) }
        else { (c, 0.0, x) };
    (((r + m) * 255.0).min(255.0) as u8,
     ((g + m) * 255.0).min(255.0) as u8,
     ((b + m) * 255.0).min(255.0) as u8)
}

fn xorshift(mut x: u32) -> u32 {
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    x
}

fn pseudo_random(seed: u32) -> f32 {
    (xorshift(seed) & 0xFFFF) as f32 / 65535.0
}
