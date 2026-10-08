//! Visual effects: six curated looks that share one visual language.
//!
//! * With the `gpu` feature (and a working wgpu device) every effect runs as a
//!   WGSL fragment shader (see `renderer/gpu.rs`); the CPU pipeline below stays
//!   idle so the damage-tracked back buffer keeps working.
//! * Without a GPU, [`ShaderPipeline`] offers cheap CPU versions of CRT, Amber
//!   and Hologram. The remaining effects are GPU-only.

/// Default `effect_intensity` for new configs.
pub const DEFAULT_INTENSITY: f32 = 0.6;

/// The six effects. The numeric id doubles as the `Ctrl+Shift+<n>` shortcut
/// and as the effect id passed to the GPU shader.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum EffectKind {
    Crt = 1,
    Glitch = 2,
    Neon = 3,
    Matrix = 4,
    Amber = 5,
    Hologram = 6,
}

impl EffectKind {
    pub const ALL: [EffectKind; 6] = [
        EffectKind::Crt,
        EffectKind::Glitch,
        EffectKind::Neon,
        EffectKind::Matrix,
        EffectKind::Amber,
        EffectKind::Hologram,
    ];

    pub fn id(self) -> u32 {
        self as u32
    }

    pub fn from_id(id: u32) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.id() == id)
    }

    /// Stable config / serialization name.
    pub fn name(self) -> &'static str {
        match self {
            EffectKind::Crt => "crt",
            EffectKind::Glitch => "glitch",
            EffectKind::Neon => "neon",
            EffectKind::Matrix => "matrix",
            EffectKind::Amber => "amber",
            EffectKind::Hologram => "hologram",
        }
    }

    /// Human-readable label for menus and the palette.
    pub fn label(self) -> &'static str {
        match self {
            EffectKind::Crt => "CRT",
            EffectKind::Glitch => "Glitch",
            EffectKind::Neon => "Neon Glow",
            EffectKind::Matrix => "Matrix Rain",
            EffectKind::Amber => "Amber",
            EffectKind::Hologram => "Hologram",
        }
    }

    pub fn from_name(s: &str) -> Option<Self> {
        let s: String = s.trim().to_ascii_lowercase().chars().filter(|c| c.is_ascii_alphanumeric()).collect();
        match s.as_str() {
            "crt" => Some(EffectKind::Crt),
            "glitch" => Some(EffectKind::Glitch),
            "neon" | "neonglow" => Some(EffectKind::Neon),
            "matrix" | "matrixrain" => Some(EffectKind::Matrix),
            "amber" => Some(EffectKind::Amber),
            "hologram" | "holo" => Some(EffectKind::Hologram),
            _ => None,
        }
    }

    /// Parse a config value: `Some(None)` means an explicit "off".
    pub fn parse_setting(s: &str) -> Option<Option<EffectKind>> {
        match s.trim().to_ascii_lowercase().as_str() {
            "" | "off" | "none" | "false" => Some(None),
            other => Self::from_name(other).map(Some),
        }
    }

    /// Has a (cheap) CPU implementation for builds / machines without a GPU.
    pub fn cpu_capable(self) -> bool {
        matches!(self, EffectKind::Crt | EffectKind::Amber | EffectKind::Hologram)
    }

    /// Changes over time, so the frame loop must keep redrawing at 60fps.
    pub fn animated(self) -> bool {
        matches!(self, EffectKind::Glitch | EffectKind::Matrix | EffectKind::Hologram)
    }
}

/// An effect ready to hand to the GPU: kind, intensity and the theme colours
/// the shaders key off (`bg` for Matrix Rain's "behind the text" mask,
/// `accent` for tints).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ActiveEffect {
    pub kind: EffectKind,
    pub intensity: f32,
    pub bg: (u8, u8, u8),
    pub accent: (u8, u8, u8),
}

// ── Glitch burst schedule (mirrored by `glitch_on` in the WGSL) ──

const GLITCH_SLOT_SECS: f32 = 0.2;
const GLITCH_DUTY_PERCENT: u32 = 12;

/// Integer hash shared bit-for-bit with the shader.
pub fn hash_u32(x: u32) -> u32 {
    let mut h = x;
    h ^= h >> 16;
    h = h.wrapping_mul(0x7feb_352d);
    h ^= h >> 15;
    h = h.wrapping_mul(0x846c_a68b);
    h ^= h >> 16;
    h
}

/// True while a glitch burst is running at `time` seconds (~12% of 0.2s slots).
pub fn glitch_burst(time: f32) -> bool {
    if time < 0.0 {
        return false;
    }
    let slot = (time / GLITCH_SLOT_SECS).floor() as u32;
    hash_u32(slot) % 100 < GLITCH_DUTY_PERCENT
}

/// CPU-side effect state. Owns the selected effect + intensity for both the
/// GPU and CPU paths; only runs pixels itself when no GPU is active.
pub struct ShaderPipeline {
    kind: Option<EffectKind>,
    intensity: f32,
    gpu_active: bool,
    warned: bool,
    /// Per-column vignette factors (0..=256), reused between frames.
    vx: Vec<u16>,
}

impl ShaderPipeline {
    pub fn new() -> Self {
        Self { kind: None, intensity: DEFAULT_INTENSITY, gpu_active: false, warned: false, vx: Vec::new() }
    }

    pub fn set_effect(&mut self, kind: Option<EffectKind>) {
        match kind {
            Some(k) => log::info!("Effect: {} (intensity {:.2})", k.name(), self.intensity),
            None => log::info!("Effect: off"),
        }
        self.kind = kind;
        self.warned = false;
    }

    pub fn set_intensity(&mut self, v: f32) {
        self.intensity = v.clamp(0.0, 1.0);
    }

    pub fn kind(&self) -> Option<EffectKind> {
        self.kind
    }

    pub fn intensity(&self) -> f32 {
        self.intensity
    }

    /// Mark the wgpu pipeline as initialised: effects then run on the GPU only.
    pub fn set_gpu_active(&mut self, on: bool) {
        self.gpu_active = on;
    }

    pub fn gpu_active(&self) -> bool {
        self.gpu_active
    }

    /// Whether `kind` can render in the current setup (GPU or CPU fallback).
    pub fn supports(&self, kind: EffectKind) -> bool {
        self.gpu_active || kind.cpu_capable()
    }

    /// Does the frame loop need to redraw continuously at `time` seconds?
    /// Static effects (CRT, Neon, Amber) only repaint when content changes;
    /// Glitch only animates during its short bursts.
    pub fn animating(&self, time: f32) -> bool {
        let Some(k) = self.kind else { return false };
        if !self.supports(k) {
            return false;
        }
        match k {
            EffectKind::Glitch => glitch_burst(time) || glitch_burst(time - 0.1),
            k => k.animated(),
        }
    }

    /// CPU fallback. No-op while the GPU path is active.
    pub fn apply(&mut self, buf: &mut [u32], width: u32, height: u32, time: f32) {
        let Some(kind) = self.kind else { return };
        if self.gpu_active {
            return;
        }
        let (w, h) = (width as usize, height as usize);
        if w == 0 || h == 0 || buf.len() < w * h {
            return;
        }
        let i = self.intensity;
        match kind {
            EffectKind::Crt => self.cpu_crt(buf, w, h, i),
            EffectKind::Amber => cpu_amber(buf, i),
            EffectKind::Hologram => cpu_hologram(buf, w, h, i, time),
            other => {
                if !self.warned {
                    self.warned = true;
                    log::info!("Effect '{}' needs the GPU renderer (build with --features gpu); skipping", other.name());
                }
            }
        }
    }

    /// Scanlines + vignette, single in-place pass with integer math.
    fn cpu_crt(&mut self, buf: &mut [u32], w: usize, h: usize, i: f32) {
        let vig = 0.55 * i;
        self.vx.clear();
        self.vx.extend((0..w).map(|x| {
            let dx = (x as f32 + 0.5) / w as f32 * 2.0 - 1.0;
            (256.0 * (1.0 - vig * 0.5 * dx * dx)).clamp(0.0, 256.0) as u16
        }));
        let scan = (256.0 * (1.0 - 0.30 * i)) as u32;
        for y in 0..h {
            let dy = (y as f32 + 0.5) / h as f32 * 2.0 - 1.0;
            let vy = 1.0 - vig * 0.5 * dy * dy;
            let row_f = if y % 2 == 1 { scan } else { 256 };
            let row_f = (row_f as f32 * vy) as u32;
            for (px, &vx) in buf[y * w..(y + 1) * w].iter_mut().zip(&self.vx) {
                let f = (row_f * vx as u32) >> 8;
                let p = *px;
                let r = (((p >> 16) & 0xff) * f) >> 8;
                let g = (((p >> 8) & 0xff) * f) >> 8;
                let b = ((p & 0xff) * f) >> 8;
                *px = (r << 16) | (g << 8) | b;
            }
        }
    }
}

#[inline]
fn luma8(p: u32) -> u32 {
    (77 * ((p >> 16) & 0xff) + 150 * ((p >> 8) & 0xff) + 29 * (p & 0xff)) >> 8
}

#[inline]
fn lerp8(a: u32, b: u32, t256: u32) -> u32 {
    (a * (256 - t256) + b * t256) >> 8
}

/// Monochrome phosphor: luminance mapped onto amber.
fn cpu_amber(buf: &mut [u32], i: f32) {
    let t = ((0.35 + 0.65 * i) * 256.0) as u32;
    for px in buf.iter_mut() {
        let p = *px;
        let l = (luma8(p) * 295 >> 8).min(255);
        let (ar, ag, ab) = (l, l * 176 >> 8, 0);
        let r = lerp8((p >> 16) & 0xff, ar, t);
        let g = lerp8((p >> 8) & 0xff, ag, t);
        let b = lerp8(p & 0xff, ab, t);
        *px = (r << 16) | (g << 8) | b;
    }
}

/// Cyan tint + travelling scan sweep + faint flicker.
fn cpu_hologram(buf: &mut [u32], w: usize, h: usize, i: f32, time: f32) {
    let t = ((0.45 + 0.45 * i) * 256.0) as u32;
    let sweep = (time * 0.22).fract() * h as f32;
    let band = (h as f32 * 0.05).max(1.0);
    let flick = 1.0 - 0.06 * i * (hash_u32((time * 24.0) as u32) & 0xff) as f32 / 255.0;
    let flick = (flick * 256.0) as u32;
    for y in 0..h {
        let d = ((y as f32 - sweep).abs() / band).min(1.0);
        let glow = ((1.0 - d) * (1.0 - d) * 90.0 * i) as u32;
        let line = if y % 4 == 3 { 215u32 } else { 256 };
        let f = (flick * line) >> 8;
        for px in &mut buf[y * w..(y + 1) * w] {
            let p = *px;
            let l = luma8(p);
            let (cr, cg, cb) = (l * 60 >> 8, (l * 245 >> 8) + 6, (l * 255 >> 8) + 10);
            let r = lerp8((p >> 16) & 0xff, cr, t);
            let g = lerp8((p >> 8) & 0xff, cg, t);
            let b = lerp8(p & 0xff, cb, t);
            let r = ((r * f >> 8) + glow / 4).min(255);
            let g = ((g * f >> 8) + glow).min(255);
            let b = ((b * f >> 8) + glow).min(255);
            *px = (r << 16) | (g << 8) | b;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_stable_and_roundtrip() {
        let ids: Vec<u32> = EffectKind::ALL.iter().map(|k| k.id()).collect();
        assert_eq!(ids, vec![1, 2, 3, 4, 5, 6]);
        for k in EffectKind::ALL {
            assert_eq!(EffectKind::from_id(k.id()), Some(k));
            assert_eq!(EffectKind::from_name(k.name()), Some(k));
            assert_eq!(EffectKind::parse_setting(k.name()), Some(Some(k)));
        }
        assert_eq!(EffectKind::from_id(0), None);
        assert_eq!(EffectKind::from_id(7), None);
    }

    #[test]
    fn parse_setting_handles_off_aliases_and_junk() {
        assert_eq!(EffectKind::parse_setting("off"), Some(None));
        assert_eq!(EffectKind::parse_setting("none"), Some(None));
        assert_eq!(EffectKind::parse_setting("Neon Glow"), Some(Some(EffectKind::Neon)));
        assert_eq!(EffectKind::parse_setting("matrix-rain"), Some(Some(EffectKind::Matrix)));
        assert_eq!(EffectKind::parse_setting("pixelate"), None);
    }

    #[test]
    fn cpu_capability_split() {
        let cpu: Vec<_> = EffectKind::ALL.iter().filter(|k| k.cpu_capable()).map(|k| k.name()).collect();
        assert_eq!(cpu, vec!["crt", "amber", "hologram"]);
    }

    #[test]
    fn gpu_active_disables_cpu_pass() {
        let mut p = ShaderPipeline::new();
        p.set_effect(Some(EffectKind::Amber));
        let mut buf = vec![0x00ff_ffffu32; 16 * 16];
        p.set_gpu_active(true);
        p.apply(&mut buf, 16, 16, 0.0);
        assert!(buf.iter().all(|&px| px == 0x00ff_ffff), "CPU must not touch pixels when GPU is active");
        p.set_gpu_active(false);
        p.apply(&mut buf, 16, 16, 0.0);
        assert!(buf.iter().any(|&px| px != 0x00ff_ffff));
    }

    #[test]
    fn cpu_effects_stay_in_range_and_gpu_only_is_noop() {
        for k in EffectKind::ALL {
            let mut p = ShaderPipeline::new();
            p.set_effect(Some(k));
            p.set_intensity(1.0);
            let mut buf: Vec<u32> = (0..32 * 24).map(|i| (i as u32).wrapping_mul(0x0101_0101) & 0x00ff_ffff).collect();
            let before = buf.clone();
            p.apply(&mut buf, 32, 24, 1.3);
            assert!(buf.iter().all(|&px| px >> 24 == 0), "{k:?} leaked into alpha byte");
            if !k.cpu_capable() {
                assert_eq!(buf, before, "{k:?} is GPU-only and must not alter pixels on CPU");
            }
        }
    }

    #[test]
    fn animation_scheduling() {
        let mut p = ShaderPipeline::new();
        assert!(!p.animating(0.0));
        p.set_effect(Some(EffectKind::Crt));
        assert!(!p.animating(1.0), "CRT is static");
        p.set_effect(Some(EffectKind::Hologram));
        assert!(p.animating(1.0));
        p.set_effect(Some(EffectKind::Matrix));
        assert!(!p.animating(1.0), "GPU-only effect does not animate without a GPU");
        p.set_gpu_active(true);
        assert!(p.animating(1.0));
        p.set_effect(Some(EffectKind::Glitch));
        let on = (0..2000).filter(|i| p.animating(*i as f32 * 0.016)).count();
        assert!(on > 0 && on < 1000, "glitch should animate only in bursts, got {on}/2000 frames");
    }

    #[test]
    fn glitch_bursts_are_occasional() {
        let slots = 10_000u32;
        let on = (0..slots).filter(|s| glitch_burst(*s as f32 * GLITCH_SLOT_SECS + 0.01)).count();
        let pct = on as f32 / slots as f32 * 100.0;
        assert!((8.0..16.0).contains(&pct), "burst duty {pct}%");
    }

    #[test]
    fn intensity_is_clamped() {
        let mut p = ShaderPipeline::new();
        p.set_intensity(3.0);
        assert_eq!(p.intensity(), 1.0);
        p.set_intensity(-1.0);
        assert_eq!(p.intensity(), 0.0);
    }
}
