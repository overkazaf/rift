pub mod effects;
mod pipeline;

pub use effects::{CrtParams, GlitchParams, MatrixParams, MatrixState, NeonParams, ShaderEffect,
                   AmberParams, HologramParams, PixelateParams, ThermalParams,
                   RaindropParams, VhsParams, GridParams, FilmGrainParams, InvertParams, DesaturateParams,
                   ChromaticParams, PulseParams, SnowParams, UnderwaterParams, NeonOutlineParams, ScanlineRgbParams};
// WGSL shaders available for future wgpu migration
#[allow(unused_imports)]
pub use pipeline::{CRT_SHADER_WGSL, FULLSCREEN_QUAD_WGSL};

pub struct ShaderPipeline {
    active_effect: Option<ShaderEffect>,
    matrix_state: MatrixState,
    work_buffer: Vec<u32>,
}

impl ShaderPipeline {
    pub fn new() -> Self {
        Self {
            active_effect: None,
            matrix_state: MatrixState::new(),
            work_buffer: Vec::new(),
        }
    }

    pub fn set_effect(&mut self, effect: Option<ShaderEffect>) {
        if effect.is_none() {
            log::info!("Shader effect: off");
        } else {
            log::info!("Shader effect: {:?}", effect.as_ref().unwrap());
        }
        self.active_effect = effect;
    }

    pub fn has_effect(&self) -> bool {
        self.active_effect.is_some()
    }

    pub fn active_effect(&self) -> Option<&ShaderEffect> {
        self.active_effect.as_ref()
    }

    pub fn apply(&mut self, buffer: &mut [u32], width: u32, height: u32, time: f32) {
        if self.active_effect.is_none() { return; }

        // Reuse pre-allocated work buffer instead of allocating each frame
        let len = buffer.len();
        if self.work_buffer.len() != len {
            self.work_buffer.resize(len, 0);
        }
        self.work_buffer.copy_from_slice(buffer);

        match self.active_effect {
            Some(ShaderEffect::Crt(ref p)) => {
                effects::apply_crt(buffer, &self.work_buffer, width, height, p, time);
            }
            Some(ShaderEffect::Glitch(ref p)) => {
                effects::apply_glitch(buffer, &self.work_buffer, width, height, p, time);
            }
            Some(ShaderEffect::NeonGlow(ref p)) => {
                effects::apply_neon(buffer, &self.work_buffer, width, height, p, time);
            }
            Some(ShaderEffect::MatrixRain(ref p)) => {
                let p = p.clone();
                self.matrix_state.apply(buffer, width, height, &p, time);
            }
            Some(ShaderEffect::Amber(ref p)) => {
                effects::apply_amber(buffer, &self.work_buffer, width, height, p, time);
            }
            Some(ShaderEffect::Hologram(ref p)) => {
                effects::apply_hologram(buffer, &self.work_buffer, width, height, p, time);
            }
            Some(ShaderEffect::Pixelate(ref p)) => {
                effects::apply_pixelate(buffer, &self.work_buffer, width, height, p, time);
            }
            Some(ShaderEffect::Thermal(ref p)) => {
                effects::apply_thermal(buffer, &self.work_buffer, width, height, p, time);
            }
            Some(ShaderEffect::Raindrop(ref p)) => {
                effects::apply_raindrop(buffer, &self.work_buffer, width, height, p, time);
            }
            Some(ShaderEffect::Vhs(ref p)) => {
                effects::apply_vhs(buffer, &self.work_buffer, width, height, p, time);
            }
            Some(ShaderEffect::CyberpunkGrid(ref p)) => {
                effects::apply_grid(buffer, &self.work_buffer, width, height, p, time);
            }
            Some(ShaderEffect::FilmGrain(ref p)) => {
                effects::apply_film_grain(buffer, &self.work_buffer, width, height, p, time);
            }
            Some(ShaderEffect::Invert(ref p)) => {
                effects::apply_invert(buffer, &self.work_buffer, width, height, p, time);
            }
            Some(ShaderEffect::Desaturate(ref p)) => {
                effects::apply_desaturate(buffer, &self.work_buffer, width, height, p, time);
            }
            Some(ShaderEffect::Chromatic(ref p)) => {
                effects::apply_chromatic(buffer, &self.work_buffer, width, height, p, time);
            }
            Some(ShaderEffect::Pulse(ref p)) => {
                effects::apply_pulse(buffer, &self.work_buffer, width, height, p, time);
            }
            Some(ShaderEffect::Snow(ref p)) => {
                effects::apply_snow(buffer, &self.work_buffer, width, height, p, time);
            }
            Some(ShaderEffect::Underwater(ref p)) => {
                effects::apply_underwater(buffer, &self.work_buffer, width, height, p, time);
            }
            Some(ShaderEffect::NeonOutline(ref p)) => {
                effects::apply_neon_outline(buffer, &self.work_buffer, width, height, p, time);
            }
            Some(ShaderEffect::ScanlineRgb(ref p)) => {
                effects::apply_scanline_rgb(buffer, &self.work_buffer, width, height, p, time);
            }
            _ => {}
        }
    }
}
