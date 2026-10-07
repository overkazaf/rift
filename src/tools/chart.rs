#![allow(dead_code)]

use crate::config::Rgb;
use crate::renderer::font::FontManager;

pub struct DataDetector;

#[derive(Debug)]
pub struct DataSeries {
    pub labels: Vec<String>,
    pub values: Vec<f64>,
    pub name: String,
}

pub enum ChartType {
    Bar,
    Line,
    Sparkline,
}

impl DataDetector {
    pub fn detect(text: &str) -> Option<Vec<DataSeries>> {
        Self::try_json_array(text)
            .or_else(|| Self::try_csv(text))
            .or_else(|| Self::try_key_value(text))
    }

    fn try_json_array(text: &str) -> Option<Vec<DataSeries>> {
        let text = text.trim();
        if !text.starts_with('[') || !text.ends_with(']') {
            return None;
        }
        let inner = &text[1..text.len() - 1];
        let nums: Vec<f64> = inner
            .split(',')
            .filter_map(|s| s.trim().parse().ok())
            .collect();
        if nums.len() >= 3 {
            Some(vec![DataSeries {
                labels: (1..=nums.len()).map(|i| i.to_string()).collect(),
                values: nums,
                name: "data".into(),
            }])
        } else {
            None
        }
    }

    fn try_csv(text: &str) -> Option<Vec<DataSeries>> {
        let lines: Vec<&str> = text.lines().collect();
        if lines.len() < 3 {
            return None;
        }
        let header: Vec<&str> = lines[0].split(',').map(|s| s.trim()).collect();
        if header.len() < 2 {
            return None;
        }
        let mut labels = Vec::new();
        let mut values = Vec::new();
        for line in &lines[1..] {
            let cols: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
            if cols.len() < 2 {
                continue;
            }
            labels.push(cols[0].to_string());
            if let Ok(v) = cols[1].parse::<f64>() {
                values.push(v);
            }
        }
        if values.len() >= 2 {
            Some(vec![DataSeries {
                labels,
                values,
                name: header.get(1).unwrap_or(&"value").to_string(),
            }])
        } else {
            None
        }
    }

    fn try_key_value(text: &str) -> Option<Vec<DataSeries>> {
        let mut labels = Vec::new();
        let mut values = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            let (key, val) = if let Some(pos) = line.find(':') {
                (&line[..pos], &line[pos + 1..])
            } else if let Some(pos) = line.find('=') {
                (&line[..pos], &line[pos + 1..])
            } else {
                continue;
            };
            let key = key.trim();
            if let Ok(v) = val.trim().parse::<f64>() {
                labels.push(key.to_string());
                values.push(v);
            }
        }
        if values.len() >= 2 {
            Some(vec![DataSeries {
                labels,
                values,
                name: "data".into(),
            }])
        } else {
            None
        }
    }
}

pub struct ChartRenderer;

impl ChartRenderer {
    pub fn render_bar(
        buffer: &mut [u32],
        buf_width: usize,
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        data: &DataSeries,
        font: &mut FontManager,
        theme_fg: Rgb,
        _theme_bg: Rgb,
        accent: Rgb,
    ) {
        if data.values.is_empty() || w == 0 || h == 0 {
            return;
        }
        let max_val = data
            .values
            .iter()
            .cloned()
            .fold(f64::NEG_INFINITY, f64::max);
        if max_val <= 0.0 {
            return;
        }

        let bar_count = data.values.len();
        let gap = 2usize;
        let total_gaps = gap * (bar_count + 1);
        let bar_w = if w > total_gaps {
            ((w - total_gaps) / bar_count).max(4)
        } else {
            4
        };
        let chart_h = h.saturating_sub(font.cell_height + 8);

        let colors: [Rgb; 6] = [
            accent,
            (80, 200, 120),
            (100, 150, 255),
            (255, 180, 60),
            (200, 100, 255),
            (255, 100, 100),
        ];

        for (i, val) in data.values.iter().enumerate() {
            let bar_h = ((val / max_val) * chart_h as f64) as usize;
            let bx = x + gap + i * (bar_w + gap);
            let by = y + chart_h.saturating_sub(bar_h);

            let color = colors[i % colors.len()];
            let px = pack(color.0, color.1, color.2);

            for row in by..(y + chart_h) {
                let end_col = (bx + bar_w).min(x + w);
                for col in bx..end_col {
                    let idx = row * buf_width + col;
                    if idx < buffer.len() {
                        buffer[idx] = px;
                    }
                }
            }

            let val_str = if *val >= 1000.0 {
                format!("{:.0}k", val / 1000.0)
            } else if *val == val.floor() {
                format!("{:.0}", val)
            } else {
                format!("{:.1}", val)
            };
            let cw = font.cell_width;
            let label_x = bx + bar_w.saturating_sub(val_str.len() * cw) / 2;
            let label_y = by.saturating_sub(font.cell_height + 2);
            crate::ui::render_text(
                buffer, buf_width, font, &val_str, label_x, label_y, theme_fg,
            );
        }
    }

    pub fn render_sparkline(
        buffer: &mut [u32],
        buf_width: usize,
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        values: &[f64],
        color: Rgb,
    ) {
        if values.is_empty() || w == 0 || h == 0 {
            return;
        }

        let min = values.iter().cloned().fold(f64::INFINITY, f64::min);
        let max = values.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        let range = (max - min).max(0.001);

        let px = pack(color.0, color.1, color.2);
        let count = values.len().min(w);

        for i in 0..count {
            let vx = x + (i * w / values.len());
            if vx >= x + w {
                break;
            }
            let normalized = ((values[i] - min) / range) as f32;
            let bar_pixels = ((normalized * h as f32) as usize).min(h);
            let vy = y + h - bar_pixels;

            for row in vy..(y + h) {
                let idx = row * buf_width + vx;
                if idx >= buffer.len() {
                    continue;
                }
                if row == vy {
                    buffer[idx] = px;
                } else {
                    let base = buffer[idx];
                    let br = (base >> 16) & 0xff;
                    let bg = (base >> 8) & 0xff;
                    let bb = base & 0xff;
                    let a: u32 = 40;
                    let inv = 255 - a;
                    let r = (color.0 as u32 * a + br * inv) / 255;
                    let g = (color.1 as u32 * a + bg * inv) / 255;
                    let b = (color.2 as u32 * a + bb * inv) / 255;
                    buffer[idx] = (r << 16) | (g << 8) | b;
                }
            }
        }
    }
}

use crate::ui::pack;
