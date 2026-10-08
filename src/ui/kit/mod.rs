//! Rift UI kit: one design system for every software-drawn overlay.
//!
//! * [`Tokens`] — colours, spacing, radii and shadow derived from the active
//!   [`Theme`](crate::config::Theme) (never hard-coded palettes).
//! * [`Ctx`] — a drawing context bundling pixel buffer, size, font and tokens.
//! * Widgets (`panel`, `list`, `text_input`, `button`, `badge`, `kbd_hint`,
//!   `table`, `toast`, `progress`, ...) are methods on [`Ctx`].
//! * [`layout`] — pure placement helpers (`centered`, `bottom_sheet`, `side_panel`).
//! * [`gallery`] — a visual-QA page showing every widget.
//!
//! Typical overlay `render()`:
//! ```ignore
//! let tk = Tokens::new(theme, font.cell_width, font.cell_height);
//! let mut cx = Ctx::new(buffer, width, height, font, &tk);
//! cx.backdrop(tk.backdrop);
//! let rect = cx.centered(60, 720, 70);
//! let body = cx.panel(rect, &PanelSpec::new("Title").sub("subtitle").hints(&[("Esc", "close")]));
//! ```

#![allow(dead_code)]

pub mod draw;
pub mod gallery;
pub mod layout;
#[cfg(test)]
mod qa_overlays;
pub mod tokens;
pub mod widgets;

pub use draw::ellipsize;
#[allow(unused_imports)]
pub use layout::{Rect, Side};
#[allow(unused_imports)]
pub use tokens::{contrast, ensure_contrast, luminance, mix, Tokens, Tone};
pub use widgets::*;

use crate::renderer::font::FontManager;

/// Drawing context: pixel buffer + size + font + design tokens.
pub struct Ctx<'a> {
    pub buf: &'a mut [u32],
    pub w: usize,
    pub h: usize,
    pub font: &'a mut FontManager,
    pub tk: &'a Tokens,
}

impl<'a> Ctx<'a> {
    pub fn new(
        buf: &'a mut [u32],
        w: usize,
        h: usize,
        font: &'a mut FontManager,
        tk: &'a Tokens,
    ) -> Self {
        Self { buf, w, h, font, tk }
    }

    /// Whole-screen rectangle.
    pub fn screen(&self) -> Rect {
        Rect::new(0, 0, self.w, self.h)
    }
}
