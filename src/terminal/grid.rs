#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Color {
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

#[derive(Clone, Copy, Default)]
pub struct Attrs {
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub reverse: bool,
    pub hidden: bool,
}

#[derive(Clone, Copy)]
pub struct Cell {
    pub c: char,
    pub fg: Color,
    pub bg: Color,
    pub attrs: Attrs,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            c: ' ',
            fg: Color::Default,
            bg: Color::Default,
            attrs: Attrs::default(),
        }
    }
}

impl Cell {
    pub fn blank_with(fg: Color, bg: Color) -> Self {
        Self { c: ' ', fg, bg, attrs: Attrs::default() }
    }
}
