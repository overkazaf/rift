use std::collections::HashMap;

pub struct FontManager {
    font: fontdue::Font,
    font_size: f32,
    pub cell_width: usize,
    pub cell_height: usize,
    pub baseline: usize,
    cache: HashMap<char, Vec<u8>>,
}

impl FontManager {
    pub fn new(font_path: &str, font_size: f32) -> Self {
        let font_data = std::fs::read(font_path)
            .unwrap_or_else(|e| panic!("Failed to read font {font_path}: {e}"));
        log::info!("Loaded font: {font_path}");

        let settings = fontdue::FontSettings {
            collection_index: 0,
            scale: font_size,
            ..Default::default()
        };
        let font = fontdue::Font::from_bytes(font_data, settings)
            .expect("Failed to parse font");

        let metrics = font
            .horizontal_line_metrics(font_size)
            .expect("Font missing horizontal metrics");
        let cell_height = (metrics.ascent - metrics.descent + metrics.line_gap).ceil() as usize;
        let baseline = metrics.ascent.ceil() as usize;

        let (m_metrics, _) = font.rasterize('M', font_size);
        let cell_width = m_metrics.advance_width.ceil() as usize;

        Self { font, font_size, cell_width, cell_height, baseline, cache: HashMap::new() }
    }

    pub fn rasterize(&mut self, c: char) -> &[u8] {
        let cw = self.cell_width;
        let ch = self.cell_height;
        let baseline = self.baseline;
        let font_size = self.font_size;

        self.cache.entry(c).or_insert_with_key(|&c| {
            // Skip characters the font can't render (Powerline/Nerd Font glyphs etc.)
            let glyph_index = self.font.lookup_glyph_index(c);
            if glyph_index == 0 && c != '\0' {
                // Missing glyph — render as empty cell (not garbage)
                return vec![0u8; cw * ch];
            }

            let (metrics, bitmap) = self.font.rasterize(c, font_size);
            if bitmap.is_empty() || metrics.width == 0 || metrics.height == 0 {
                return vec![0u8; cw * ch];
            }

            let mut cell_bitmap = vec![0u8; cw * ch];
            let glyph_top = baseline as i32 - metrics.ymin as i32 - metrics.height as i32;

            for gy in 0..metrics.height {
                let cy = glyph_top + gy as i32;
                if cy < 0 || cy >= ch as i32 { continue; }
                let x_offset = metrics.xmin.max(0) as usize;
                for gx in 0..metrics.width {
                    let cx = x_offset + gx;
                    if cx >= cw { continue; }
                    let src = gy * metrics.width + gx;
                    if src < bitmap.len() {
                        cell_bitmap[cy as usize * cw + cx] = bitmap[src];
                    }
                }
            }
            cell_bitmap
        })
    }
}
