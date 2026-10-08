//! Design tokens derived from the active theme.

use crate::config::{Rgb, Theme};

/// Semantic colour role used by badges, toasts, progress bars and list rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Neutral,
    Accent,
    Success,
    Warning,
    Danger,
}

#[derive(Clone, Copy, Debug)]
pub struct Spacing {
    pub xs: usize,
    pub sm: usize,
    pub md: usize,
    pub lg: usize,
    pub xl: usize,
}

#[derive(Clone, Copy, Debug)]
pub struct Tokens {
    // surfaces
    pub bg: Rgb,
    pub field: Rgb,
    pub surface: Rgb,
    pub surface_alt: Rgb,
    pub elevated: Rgb,
    pub border: Rgb,
    pub border_strong: Rgb,
    // text
    pub text: Rgb,
    pub text_muted: Rgb,
    pub text_faint: Rgb,
    // accents
    pub accent: Rgb,
    pub accent_soft: Rgb,
    pub selection: Rgb,
    pub danger: Rgb,
    pub warning: Rgb,
    pub success: Rgb,
    // metrics
    pub scale: usize,
    pub sp: Spacing,
    pub radius: usize,
    pub radius_sm: usize,
    pub shadow_blur: usize,
    pub shadow_offset: usize,
    pub shadow_alpha: u8,
    pub backdrop: f32,
    pub cw: usize,
    pub ch: usize,
    pub row_h: usize,
    pub input_h: usize,
    pub button_h: usize,
}

// ---- colour math ------------------------------------------------------

pub fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let t = t.clamp(0.0, 1.0);
    let f = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round().clamp(0.0, 255.0) as u8;
    (f(a.0, b.0), f(a.1, b.1), f(a.2, b.2))
}

fn lin(c: u8) -> f32 {
    let s = c as f32 / 255.0;
    if s <= 0.03928 { s / 12.92 } else { ((s + 0.055) / 1.055).powf(2.4) }
}

/// WCAG relative luminance.
pub fn luminance(c: Rgb) -> f32 {
    0.2126 * lin(c.0) + 0.7152 * lin(c.1) + 0.0722 * lin(c.2)
}

/// WCAG contrast ratio (1.0 ..= 21.0).
pub fn contrast(a: Rgb, b: Rgb) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// Push `fg` toward white (dark `bg`) or black (light `bg`) until it reaches
/// the requested contrast against `bg`.
pub fn ensure_contrast(fg: Rgb, bg: Rgb, min: f32) -> Rgb {
    let target = if luminance(bg) < 0.4 { (255, 255, 255) } else { (0, 0, 0) };
    for i in 0..=20 {
        let c = mix(fg, target, i as f32 * 0.05);
        if contrast(c, bg) >= min {
            return c;
        }
    }
    target
}

fn chroma(c: Rgb) -> u8 {
    c.0.max(c.1).max(c.2) - c.0.min(c.1).min(c.2)
}

fn round_up(v: usize, to: usize) -> usize {
    v.div_ceil(to.max(1)) * to.max(1)
}

impl Tokens {
    /// Derive the full token set from a theme and the font cell metrics.
    pub fn new(theme: &Theme, cw: usize, ch: usize) -> Self {
        let dark = luminance(theme.bg) < 0.4;
        let lift: Rgb = if dark { (255, 255, 255) } else { (0, 0, 0) };

        let bg = theme.bg;
        let surface = mix(bg, lift, 0.05);
        let surface_alt = mix(bg, lift, 0.09);
        let elevated = mix(bg, lift, 0.13);
        let field = mix(bg, if dark { (0, 0, 0) } else { (255, 255, 255) }, 0.18);

        // Text must stay legible on every surface the kit paints.
        let mut text = theme.fg;
        for s in [surface, surface_alt, elevated] {
            text = ensure_contrast(text, s, 7.0);
        }
        text = ensure_contrast(text, field, 7.0);
        let text_muted = ensure_contrast(mix(surface, text, 0.55), elevated, 3.6);
        let text_faint = mix(surface, text, 0.35);
        let border = mix(surface, text, 0.14);
        let border_strong = mix(surface, text, 0.30);

        // Accent is the theme cursor colour; a near-grey cursor (Dracula,
        // Monokai...) would make the accent indistinguishable from text, so
        // fall back to the palette blue in that case.
        let mut accent = theme.cursor;
        if chroma(accent) < 24 {
            accent = if chroma(theme.palette[4]) >= 40 { theme.palette[4] } else { theme.palette[6] };
        }
        let accent = ensure_contrast(accent, elevated, 3.2);
        // Tinted backgrounds fade the accent in only as far as text stays AA.
        let tinted = |max: f32| {
            let mut t = max;
            while t > 0.04 && contrast(text, mix(surface, accent, t)) < 4.8 {
                t -= 0.02;
            }
            mix(surface, accent, t)
        };
        let accent_soft = tinted(0.20);
        let selection = tinted(0.32);

        // Semantic colours: fixed hues loosely tinted by the theme palette.
        let sem = |base: Rgb, pal: Rgb| ensure_contrast(mix(base, pal, 0.25), elevated, 4.5);
        let danger = sem((243, 120, 140), theme.palette[1]);
        let warning = sem((238, 196, 112), theme.palette[3]);
        let success = sem((126, 214, 140), theme.palette[2]);

        let scale = ((ch as f32 / 18.0).round() as usize).clamp(1, 4);
        let sp = Spacing { xs: 4 * scale, sm: 8 * scale, md: 12 * scale, lg: 16 * scale, xl: 24 * scale };
        let row_h = round_up(ch + sp.sm, 4 * scale);
        Self {
            bg,
            field,
            surface,
            surface_alt,
            elevated,
            border,
            border_strong,
            text,
            text_muted,
            text_faint,
            accent,
            accent_soft,
            selection,
            danger,
            warning,
            success,
            scale,
            sp,
            radius: 8 * scale,
            radius_sm: 4 * scale,
            shadow_blur: 16 * scale,
            shadow_offset: 4 * scale,
            shadow_alpha: if dark { 150 } else { 70 },
            backdrop: 0.6,
            cw,
            ch,
            row_h,
            input_h: row_h + sp.sm,
            button_h: row_h + sp.xs,
        }
    }

    /// Foreground colour for a tone.
    pub fn tone(&self, t: Tone) -> Rgb {
        match t {
            Tone::Neutral => self.text_muted,
            Tone::Accent => self.accent,
            Tone::Success => self.success,
            Tone::Warning => self.warning,
            Tone::Danger => self.danger,
        }
    }

    /// Soft (tinted) background for a tone, on top of `base`.
    pub fn tint(&self, t: Tone, base: Rgb) -> Rgb {
        match t {
            Tone::Neutral => mix(base, self.text, 0.10),
            _ => mix(base, self.tone(t), 0.18),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn themes() -> Vec<Theme> {
        vec![
            Theme::catppuccin_mocha(),
            Theme::hacker_green(),
            Theme::dracula(),
            Theme::nord(),
            Theme::solarized_dark(),
            Theme::tokyo_night(),
            Theme::cyberpunk(),
            Theme::gruvbox(),
            Theme::monokai(),
        ]
    }

    #[test]
    fn text_contrast_meets_wcag_aa_on_all_surfaces() {
        for th in themes() {
            let tk = Tokens::new(&th, 9, 18);
            for (name, s) in [
                ("surface", tk.surface),
                ("surface_alt", tk.surface_alt),
                ("elevated", tk.elevated),
                ("field", tk.field),
                ("accent_soft", tk.accent_soft),
                ("selection", tk.selection),
            ] {
                let c = contrast(tk.text, s);
                assert!(c >= 4.5, "{}: text on {} contrast {:.2}", th.name, name, c);
            }
        }
    }

    #[test]
    fn muted_accent_and_semantic_colors_are_legible() {
        for th in themes() {
            let tk = Tokens::new(&th, 9, 18);
            assert!(contrast(tk.text_muted, tk.surface) >= 3.0, "{} muted", th.name);
            assert!(contrast(tk.accent, tk.surface) >= 3.0, "{} accent", th.name);
            for (n, c) in [("danger", tk.danger), ("warning", tk.warning), ("success", tk.success)] {
                assert!(contrast(c, tk.elevated) >= 4.5, "{} {}", th.name, n);
            }
            // a (near-)grey cursor must not collapse the accent into plain text
            if chroma(th.cursor) < 24 {
                assert_ne!(tk.accent, tk.text, "{} accent == text", th.name);
            }
        }
    }

    #[test]
    fn surfaces_are_ordered_and_distinct() {
        for th in themes() {
            let tk = Tokens::new(&th, 9, 18);
            let l = |c| luminance(c);
            assert!(l(tk.surface) > l(tk.bg) && l(tk.surface_alt) > l(tk.surface) && l(tk.elevated) > l(tk.surface_alt), "{}", th.name);
            assert!(l(tk.field) <= l(tk.bg), "{}", th.name);
        }
    }

    #[test]
    fn spacing_stays_on_grid_and_scales() {
        let th = Theme::catppuccin_mocha();
        let a = Tokens::new(&th, 9, 18);
        assert_eq!((a.sp.xs, a.sp.sm, a.sp.md, a.sp.lg, a.sp.xl), (4, 8, 12, 16, 24));
        assert_eq!(a.radius, 8);
        let b = Tokens::new(&th, 18, 36);
        assert_eq!(b.sp.sm, 16);
        assert_eq!(b.radius, 16);
        assert_eq!(a.row_h % 4, 0);
        assert!(a.row_h >= 18 + 8);
        assert_eq!(b.row_h % 8, 0);
    }

    #[test]
    fn mix_and_contrast_basics() {
        assert_eq!(mix((0, 0, 0), (255, 255, 255), 0.5), (128, 128, 128));
        assert!((contrast((0, 0, 0), (255, 255, 255)) - 21.0).abs() < 0.01);
        let c = ensure_contrast((60, 60, 60), (30, 30, 30), 4.5);
        assert!(contrast(c, (30, 30, 30)) >= 4.5);
    }
}
