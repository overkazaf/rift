//! wgpu GPU rendering pipeline (`--features gpu`).
//!
//! A frame is built from two layers and one optional post effect:
//!
//!   1. **Terminal layer** - the terminal grid, drawn by the GPU from
//!      instance buffers (`gpu_text`): cell backgrounds, glyph quads sampled
//!      from an atlas, decorations, cursor, selection / search highlights.
//!      One instanced draw call into an offscreen texture. Only rows whose
//!      content changed are re-uploaded, only new glyphs reach the atlas, and
//!      the pass is skipped entirely when nothing changed.
//!   2. **UI layer** - everything the CPU still draws (tab bar, pane borders,
//!      overlays, UI-kit panels, images) in the CPU frame buffer. Pixels with
//!      `0xFF` in the top byte are transparent (the terminal layer shows
//!      through). The layer is uploaded as a texture by *diffing* against what
//!      the GPU already has: unchanged rows cost a memcmp, changed rows are
//!      converted and uploaded as dirty rectangles.
//!   3. **Composite / effects** - one fullscreen pass that composites the
//!      layers and, when active, runs the WGSL effect shader (CRT, Glitch,
//!      Neon Glow, Matrix Rain, Amber, Hologram) over the result. Effects
//!      never touch the CPU buffers, so damage tracking stays effective.
//!
//! The terminal layer is rendered with plain gamma-space alpha blending into
//! the sRGB texture's `Rgba8Unorm` view, which is exactly what the CPU
//! renderer's integer blend does, so both paths agree to within rounding.
//!
//! Frames without terminal instances (startup splash, time warp) use the
//! legacy path: the whole CPU buffer is the (opaque) UI layer.

use std::sync::Arc;
use std::time::{Duration, Instant};

use winit::window::Window;

use super::gpu_text::{GpuText, Inst};
use crate::effects::ActiveEffect;

// ── Uniform block shared with `FX_WGSL` / `BLIT_WGSL` (`struct Params`, 64 bytes) ──

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct FxUniforms {
    time: f32,
    intensity: f32,
    effect: u32,
    _pad0: u32,
    resolution: [f32; 2],
    /// Window opacity applied as the last step (1.0 where the OS handles it).
    opacity: f32,
    _pad1: f32,
    /// Theme background, sRGB 0..1 (w unused).
    bg: [f32; 4],
    /// Theme accent, sRGB 0..1 (w unused).
    accent: [f32; 4],
}

impl FxUniforms {
    fn new(fx: Option<&ActiveEffect>, time: f32, width: u32, height: u32, opacity: f32) -> Self {
        let c = |(r, g, b): (u8, u8, u8)| [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0];
        match fx {
            Some(fx) => Self {
                time,
                intensity: fx.intensity.clamp(0.0, 1.0),
                effect: fx.kind.id(),
                _pad0: 0,
                resolution: [width as f32, height as f32],
                opacity,
                _pad1: 0.0,
                bg: c(fx.bg),
                accent: c(fx.accent),
            },
            None => Self {
                time,
                intensity: 0.0,
                effect: 0,
                _pad0: 0,
                resolution: [width as f32, height as f32],
                opacity,
                _pad1: 0.0,
                bg: [0.0; 4],
                accent: [0.0; 4],
            },
        }
    }
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct Globals {
    screen: [f32; 2],
    _pad: [f32; 2],
}

/// What one frame cost on the CPU side and what it moved to the GPU.
#[derive(Default, Clone, Copy, Debug)]
pub struct FrameStats {
    pub atlas_uploads: usize,
    pub atlas_bytes: usize,
    pub rows_uploaded: usize,
    pub inst_bytes: usize,
    pub ui_rects: usize,
    pub ui_bytes: usize,
    /// The terminal pass ran this frame (false = reused last frame's layer).
    pub term_pass: bool,
    /// Atlas + instance upload time.
    pub t_inst: Duration,
    /// UI layer diff + convert + upload time.
    pub t_ui: Duration,
    /// Encode + submit time.
    pub t_submit: Duration,
    /// Time spent waiting for the GPU to finish (offscreen runs only).
    pub t_wait: Duration,
}

const TEX_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
/// View format used to render the terminal layer (raw gamma-space values).
const TERM_RENDER_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const INST_SIZE: usize = std::mem::size_of::<Inst>();

// ── Shared device resources ──

/// Everything that does not depend on a surface: device, pipelines, atlas,
/// instance buffer and the two layer textures.
struct GpuCore {
    device: wgpu::Device,
    queue: wgpu::Queue,

    // Terminal pass
    text_pipeline: wgpu::RenderPipeline,
    text_bgl: wgpu::BindGroupLayout,
    text_bg: Option<wgpu::BindGroup>,
    globals: wgpu::Buffer,
    atlas_tex: Option<wgpu::Texture>,
    color_tex: Option<wgpu::Texture>,
    inst_buf: Option<wgpu::Buffer>,
    inst_cap: usize,
    layout_seen: u64,
    tail_prev: Vec<Inst>,

    // Composite pass
    blit_pipeline: wgpu::RenderPipeline,
    fx_pipeline: wgpu::RenderPipeline,
    comp_bgl: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    uniforms: wgpu::Buffer,

    // Size dependent
    size: (u32, u32),
    term_tex: Option<wgpu::Texture>,
    term_render_view: Option<wgpu::TextureView>,
    ui_tex: Option<wgpu::Texture>,
    comp_bg: Option<wgpu::BindGroup>,
    term_stale: bool,

    // UI layer mirror: what the GPU texture currently holds
    ui_prev: Vec<u32>,
    ui_valid: bool,
    ui_keyed: bool,
    rgba: Vec<u8>,
    staging: Vec<u8>,
}

impl GpuCore {
    fn new(device: wgpu::Device, queue: wgpu::Queue, target_format: wgpu::TextureFormat) -> Self {
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("fx_uniforms"),
            size: std::mem::size_of::<FxUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let globals = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("text_globals"),
            size: std::mem::size_of::<Globals>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // ── Composite pipelines ──
        let comp_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("comp_bgl"),
            entries: &[tex_entry(0), sampler_entry(1), uniform_entry(2), tex_entry(3)],
        });
        let comp_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("comp_layout"),
            bind_group_layouts: &[Some(&comp_bgl)],
            immediate_size: 0,
        });
        let blit_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("blit_shader"),
            source: wgpu::ShaderSource::Wgsl(BLIT_WGSL.into()),
        });
        let fx_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("fx_shader"),
            source: wgpu::ShaderSource::Wgsl(FX_WGSL.into()),
        });
        let blit_pipeline = create_pipeline(&device, &comp_layout, &blit_shader, target_format, "blit");
        let fx_pipeline = create_pipeline(&device, &comp_layout, &fx_shader, target_format, "fx");

        // ── Terminal (instanced) pipeline ──
        let text_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("text_bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });
        let text_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("text_layout"),
            bind_group_layouts: &[Some(&text_bgl)],
            immediate_size: 0,
        });
        let text_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("text_shader"),
            source: wgpu::ShaderSource::Wgsl(TEXT_WGSL.into()),
        });
        let attrs = [
            wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x2, offset: 0, shader_location: 0 },
            wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x2, offset: 8, shader_location: 1 },
            wgpu::VertexAttribute { format: wgpu::VertexFormat::Uint32, offset: 16, shader_location: 2 },
            wgpu::VertexAttribute { format: wgpu::VertexFormat::Uint32, offset: 20, shader_location: 3 },
            wgpu::VertexAttribute { format: wgpu::VertexFormat::Uint32, offset: 24, shader_location: 4 },
        ];
        let text_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("text"),
            layout: Some(&text_layout),
            vertex: wgpu::VertexState {
                module: &text_shader,
                entry_point: Some("vs_main"),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: INST_SIZE as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &attrs,
                })],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &text_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: TERM_RENDER_FORMAT,
                    blend: Some(wgpu::BlendState {
                        color: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::SrcAlpha,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation: wgpu::BlendOperation::Add,
                        },
                        alpha: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation: wgpu::BlendOperation::Add,
                        },
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        Self {
            device,
            queue,
            text_pipeline,
            text_bgl,
            text_bg: None,
            globals,
            atlas_tex: None,
            color_tex: None,
            inst_buf: None,
            inst_cap: 0,
            layout_seen: 0,
            tail_prev: Vec::new(),
            blit_pipeline,
            fx_pipeline,
            comp_bgl,
            sampler,
            uniforms,
            size: (0, 0),
            term_tex: None,
            term_render_view: None,
            ui_tex: None,
            comp_bg: None,
            term_stale: true,
            ui_prev: Vec::new(),
            ui_valid: false,
            ui_keyed: false,
            rgba: Vec::new(),
            staging: Vec::new(),
        }
    }

    /// (Re)create the window-sized layer textures and the composite bind group.
    fn resize(&mut self, w: u32, h: u32) {
        let term = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("terminal_layer"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: TEX_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[TERM_RENDER_FORMAT],
        });
        let term_sample = term.create_view(&Default::default());
        self.term_render_view = Some(term.create_view(&wgpu::TextureViewDescriptor {
            format: Some(TERM_RENDER_FORMAT),
            ..Default::default()
        }));
        let ui = create_texture(&self.device, w, h);
        let ui_view = ui.create_view(&Default::default());
        self.comp_bg = Some(self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("comp_bg"),
            layout: &self.comp_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&term_sample) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                wgpu::BindGroupEntry { binding: 2, resource: self.uniforms.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&ui_view) },
            ],
        }));
        self.term_tex = Some(term);
        self.ui_tex = Some(ui);
        self.size = (w, h);
        self.ui_valid = false;
        self.term_stale = true;
        self.queue.write_buffer(
            &self.globals,
            0,
            bytemuck::bytes_of(&Globals { screen: [w as f32, h as f32], _pad: [0.0; 2] }),
        );
    }

    /// Atlas textures sized for `text`'s atlases (created once).
    fn ensure_atlas(&mut self, text: &GpuText) {
        if self.atlas_tex.is_some() {
            return;
        }
        let a = &text.atlas;
        let atlas = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("glyph_atlas"),
            size: wgpu::Extent3d { width: a.page_w, height: a.page_h, depth_or_array_layers: a.max_pages as u32 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let c = &text.color_atlas;
        let color = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("color_atlas"),
            size: wgpu::Extent3d { width: c.page_w, height: c.page_h, depth_or_array_layers: c.max_pages as u32 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let atlas_view = atlas.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let color_view = color.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2),
            base_array_layer: 0,
            array_layer_count: Some(1),
            ..Default::default()
        });
        self.text_bg = Some(self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("text_bg"),
            layout: &self.text_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: self.globals.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&atlas_view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&color_view) },
            ],
        }));
        self.atlas_tex = Some(atlas);
        self.color_tex = Some(color);
    }

    /// Write new glyphs into the atlas textures.
    fn upload_atlas(&mut self, text: &mut GpuText, stats: &mut FrameStats) {
        for (atlas, tex, bpp) in [
            (&mut text.atlas, self.atlas_tex.as_ref(), 1u32),
            (&mut text.color_atlas, self.color_tex.as_ref(), 4u32),
        ] {
            let Some(tex) = tex else { continue };
            for u in atlas.take_uploads() {
                self.queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: tex,
                        mip_level: 0,
                        origin: wgpu::Origin3d { x: u.x as u32, y: u.y as u32, z: u.page as u32 },
                        aspect: wgpu::TextureAspect::All,
                    },
                    &u.data,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(u.w as u32 * bpp),
                        rows_per_image: Some(u.h as u32),
                    },
                    wgpu::Extent3d { width: u.w as u32, height: u.h as u32, depth_or_array_layers: 1 },
                );
                stats.atlas_uploads += 1;
                stats.atlas_bytes += u.data.len();
            }
        }
    }

    /// Bring the GPU instance buffer up to date with `text`. Returns the
    /// number of instances to draw and whether anything changed.
    fn sync_instances(&mut self, text: &mut GpuText, stats: &mut FrameStats) -> (u32, bool) {
        let need = text.buffer_instances();
        let mut changed = false;
        if text.layout_gen != self.layout_seen || self.inst_buf.is_none() || need > self.inst_cap {
            if self.inst_buf.is_none() || need > self.inst_cap {
                self.inst_buf = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("instances"),
                    size: (need * INST_SIZE) as u64,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }));
                self.inst_cap = need;
            }
            text.mark_all_dirty();
            self.layout_seen = text.layout_gen;
            self.tail_prev.clear();
            changed = true;
        }
        let Some(buf) = self.inst_buf.as_ref() else { return (0, false) };
        let cap = text.cap;
        let stride = (cap * INST_SIZE) as u64;
        for (i, s) in text.slots.iter_mut().enumerate() {
            if !s.dirty {
                continue;
            }
            s.dirty = false;
            let n = s.inst.len().min(cap);
            let wr = n.max(s.gpu_len as usize).min(cap);
            s.gpu_len = n as u32;
            if wr == 0 {
                continue;
            }
            self.staging.clear();
            self.staging.extend_from_slice(bytemuck::cast_slice(&s.inst[..n]));
            self.staging.resize(wr * INST_SIZE, 0);
            self.queue.write_buffer(buf, i as u64 * stride, &self.staging);
            stats.rows_uploaded += 1;
            stats.inst_bytes += self.staging.len();
            changed = true;
        }
        // Per-frame tail quads (dimming, margin strips) sit after the slots.
        let tail_off = text.slots.len() as u64 * stride;
        let tail = &text.tail[..text.tail.len().min(crate::renderer::gpu_text::TAIL_CAP)];
        if tail != self.tail_prev.as_slice() {
            if !tail.is_empty() {
                self.queue.write_buffer(buf, tail_off, bytemuck::cast_slice(tail));
                stats.inst_bytes += tail.len() * INST_SIZE;
            }
            self.tail_prev.clear();
            self.tail_prev.extend_from_slice(tail);
            changed = true;
        }
        ((text.slots.len() * cap + tail.len()) as u32, changed)
    }

    /// Make the UI texture match `ui` by uploading only the rows that differ
    /// from what the GPU already holds.
    fn sync_ui(&mut self, ui: &[u32], keyed: bool, stats: &mut FrameStats) {
        let (w, h) = (self.size.0 as usize, self.size.1 as usize);
        let Some(tex) = self.ui_tex.as_ref() else { return };
        if ui.len() < w * h {
            return;
        }
        let ui = &ui[..w * h];
        let full = !self.ui_valid || self.ui_prev.len() != w * h || self.ui_keyed != keyed;
        let mut rects: Vec<(usize, usize, usize, usize)> = Vec::new(); // x0, x1, y0, y1 (exclusive)
        if full {
            rects.push((0, w, 0, h));
        } else {
            let mut cur: Option<(usize, usize, usize, usize)> = None;
            let mut gap = 0usize;
            for y in 0..h {
                let (a, b) = (&ui[y * w..(y + 1) * w], &self.ui_prev[y * w..(y + 1) * w]);
                match row_diff(a, b) {
                    Some((x0, x1)) => {
                        cur = Some(match cur {
                            Some((cx0, cx1, y0, _)) => (cx0.min(x0), cx1.max(x1), y0, y + 1),
                            None => (x0, x1, y, y + 1),
                        });
                        gap = 0;
                    }
                    None => {
                        if cur.is_some() {
                            gap += 1;
                            if gap > 3 {
                                rects.extend(cur.take());
                                gap = 0;
                            }
                        }
                    }
                }
            }
            rects.extend(cur);
            if rects.len() > 12 {
                // Too fragmented: one bounding rectangle is cheaper than many copies.
                let (x0, x1) = rects.iter().fold((w, 0), |(a, b), r| (a.min(r.0), b.max(r.1)));
                let (y0, y1) = (rects[0].2, rects[rects.len() - 1].3);
                rects = vec![(x0, x1, y0, y1)];
            }
        }
        if rects.is_empty() {
            return;
        }
        if full {
            self.ui_prev.clear();
            self.ui_prev.extend_from_slice(ui);
        }
        for &(x0, x1, y0, y1) in &rects {
            let rw = x1 - x0;
            self.rgba.resize(rw * (y1 - y0) * 4, 0);
            for (i, y) in (y0..y1).enumerate() {
                let row = &ui[y * w + x0..y * w + x1];
                let dst: &mut [u32] = bytemuck::cast_slice_mut(&mut self.rgba[i * rw * 4..(i + 1) * rw * 4]);
                convert_row(row, dst, keyed);
                if !full {
                    self.ui_prev[y * w + x0..y * w + x1].copy_from_slice(row);
                }
            }
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: tex,
                    mip_level: 0,
                    origin: wgpu::Origin3d { x: x0 as u32, y: y0 as u32, z: 0 },
                    aspect: wgpu::TextureAspect::All,
                },
                &self.rgba,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(4 * rw as u32),
                    rows_per_image: Some((y1 - y0) as u32),
                },
                wgpu::Extent3d { width: rw as u32, height: (y1 - y0) as u32, depth_or_array_layers: 1 },
            );
            stats.ui_bytes += self.rgba.len();
        }
        stats.ui_rects += rects.len();
        self.ui_valid = true;
        self.ui_keyed = keyed;
    }

    /// Encode and submit one frame into `target`.
    #[allow(clippy::too_many_arguments)]
    fn render(
        &mut self,
        mut text: Option<&mut GpuText>,
        ui: &[u32],
        keyed: bool,
        width: u32,
        height: u32,
        effect: Option<&ActiveEffect>,
        time: f32,
        opacity: f32,
        target: &wgpu::TextureView,
    ) -> FrameStats {
        let mut stats = FrameStats::default();
        if (width, height) != self.size {
            self.resize(width, height);
        }

        let t0 = Instant::now();
        let mut draw_count = 0;
        let mut term_dirty = self.term_stale;
        if let Some(text) = text.as_deref_mut() {
            self.ensure_atlas(text);
            self.upload_atlas(text, &mut stats);
            let (n, changed) = self.sync_instances(text, &mut stats);
            draw_count = n;
            term_dirty |= changed || stats.atlas_uploads > 0;
        }
        stats.t_inst = t0.elapsed();

        let t1 = Instant::now();
        self.sync_ui(ui, keyed, &mut stats);
        stats.t_ui = t1.elapsed();

        let t2 = Instant::now();
        let u = FxUniforms::new(effect, time, width, height, opacity);
        self.queue.write_buffer(&self.uniforms, 0, bytemuck::bytes_of(&u));

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("render_enc") });
        if let (Some(_), true, Some(view), Some(bg), Some(buf)) =
            (text.as_ref(), term_dirty, self.term_render_view.as_ref(), self.text_bg.as_ref(), self.inst_buf.as_ref())
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("terminal_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.text_pipeline);
            pass.set_bind_group(0, bg, &[]);
            pass.set_vertex_buffer(0, buf.slice(..));
            pass.draw(0..6, 0..draw_count);
            stats.term_pass = true;
            self.term_stale = false;
        }
        {
            let pipeline = if effect.is_some() { &self.fx_pipeline } else { &self.blit_pipeline };
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("composite_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(pipeline);
            if let Some(bg) = self.comp_bg.as_ref() {
                pass.set_bind_group(0, bg, &[]);
            }
            pass.draw(0..3, 0..1);
        }
        self.queue.submit(std::iter::once(encoder.finish()));
        stats.t_submit = t2.elapsed();
        stats
    }
}

/// `0x00RRGGBB` pixels to RGBA8 bytes (as little-endian `u32`s). With `keyed`,
/// pixels carrying `0xFF` in the top byte become transparent.
#[inline]
fn convert_row(src: &[u32], dst: &mut [u32], keyed: bool) {
    if keyed {
        for (d, &px) in dst.iter_mut().zip(src) {
            let a = if px >> 24 == 0xFF { 0 } else { 0xFF00_0000 };
            *d = (px >> 16 & 0xFF) | (px & 0xFF00) | (px & 0xFF) << 16 | a;
        }
    } else {
        for (d, &px) in dst.iter_mut().zip(src) {
            *d = (px >> 16 & 0xFF) | (px & 0xFF00) | (px & 0xFF) << 16 | 0xFF00_0000;
        }
    }
}

/// First and last differing index of two equal-length rows, or `None`.
fn row_diff(a: &[u32], b: &[u32]) -> Option<(usize, usize)> {
    if a == b {
        return None;
    }
    const CHUNK: usize = 32;
    let n = a.len();
    let mut first = 0;
    while first < n {
        let e = (first + CHUNK).min(n);
        if a[first..e] != b[first..e] {
            break;
        }
        first = e;
    }
    first += a[first..].iter().zip(&b[first..]).position(|(x, y)| x != y).unwrap_or(0);
    let mut last = n;
    while last > first {
        let s = last.saturating_sub(CHUNK).max(first);
        if a[s..last] != b[s..last] {
            break;
        }
        last = s;
    }
    while last > first && a[last - 1] == b[last - 1] {
        last -= 1;
    }
    Some((first, last.max(first + 1)))
}

// ── Window pipeline ──

pub struct GpuPipeline {
    core: GpuCore,
    surface: wgpu::Surface<'static>,
    surface_config: wgpu::SurfaceConfiguration,
    /// Stats of the last presented frame.
    pub last: FrameStats,
}

impl GpuPipeline {
    pub fn new(window: Arc<Window>) -> Result<Self, String> {
        let size = window.inner_size();
        let (width, height) = (size.width.max(1), size.height.max(1));

        let instance = new_instance();
        let surface = instance.create_surface(window).map_err(|e| format!("create_surface: {e}"))?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            compatible_surface: Some(&surface),
            ..Default::default()
        }))
        .map_err(|e| format!("No compatible GPU adapter: {e}"))?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("rift"),
            ..Default::default()
        }))
        .map_err(|e| format!("request_device: {e}"))?;

        let mut surface_config = surface
            .get_default_config(&adapter, width, height)
            .ok_or_else(|| "Surface not supported by adapter".to_string())?;
        surface_config.present_mode = wgpu::PresentMode::Fifo;
        surface.configure(&device, &surface_config);
        let format = surface_config.format;

        Ok(Self { core: GpuCore::new(device, queue, format), surface, surface_config, last: FrameStats::default() })
    }

    /// Legacy frame: the whole CPU buffer is the (opaque) UI layer. Used for
    /// the startup splash and time warp.
    pub fn render_frame(&mut self, pixels: &[u32], width: u32, height: u32, effect: Option<ActiveEffect>, time: f32) {
        self.render_scene(None, pixels, false, width, height, effect, time, 1.0);
    }

    /// Render a frame: terminal layer from `text` (if any) under the UI layer
    /// in `ui` (`keyed`: top byte 0xFF = transparent), with the optional effect.
    #[allow(clippy::too_many_arguments)]
    pub fn render_scene(
        &mut self,
        text: Option<&mut GpuText>,
        ui: &[u32],
        keyed: bool,
        width: u32,
        height: u32,
        effect: Option<ActiveEffect>,
        time: f32,
        opacity: f32,
    ) {
        if width == 0 || height == 0 {
            return;
        }
        if width != self.surface_config.width || height != self.surface_config.height {
            self.surface_config.width = width;
            self.surface_config.height = height;
            self.surface.configure(&self.core.device, &self.surface_config);
        }
        let output = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t) | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.core.device, &self.surface_config);
                return;
            }
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => return,
            _ => {
                log::error!("wgpu surface: unexpected status");
                return;
            }
        };
        let view = output.texture.create_view(&Default::default());
        let frame_start = Instant::now();
        self.last = self.core.render(text, ui, keyed, width, height, effect.as_ref(), time, opacity, &view);
        self.core.queue.present(output);
        let frame_ms = frame_start.elapsed().as_secs_f64() * 1000.0;
        if frame_ms > 16.0 {
            log::debug!("GPU frame slow: {:.1}ms", frame_ms);
        }
    }
}

fn new_instance() -> wgpu::Instance {
    wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::all(),
        flags: wgpu::InstanceFlags::empty(),
        memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
        backend_options: wgpu::BackendOptions::default(),
        display: None,
    })
}

// ── Offscreen rendering (headless screenshots, tests, benchmarks) ──

/// A GPU context without a window: renders the same frame the app shows into
/// a texture and reads it back.
pub struct OffscreenGpu {
    core: GpuCore,
    target: Option<(wgpu::Texture, wgpu::TextureView, wgpu::Buffer, u32, u32)>,
    pub adapter_name: String,
}

impl OffscreenGpu {
    /// `Err` when there is no usable adapter (CI without a GPU).
    pub fn new() -> Result<Self, String> {
        let instance = new_instance();
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .map_err(|e| format!("No GPU adapter: {e}"))?;
        let adapter_name = adapter.get_info().name;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("rift-offscreen"),
            ..Default::default()
        }))
        .map_err(|e| format!("request_device: {e}"))?;
        Ok(Self { core: GpuCore::new(device, queue, TEX_FORMAT), target: None, adapter_name })
    }

    fn ensure_target(&mut self, w: u32, h: u32) {
        if matches!(&self.target, Some((_, _, _, tw, th)) if (*tw, *th) == (w, h)) {
            return;
        }
        let tex = self.core.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("offscreen_target"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: TEX_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = tex.create_view(&Default::default());
        let bytes_per_row = (4 * w).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let readback = self.core.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("offscreen_readback"),
            size: (bytes_per_row * h) as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.target = Some((tex, view, readback, w, h));
    }

    /// Render one frame and wait for the GPU to finish (no readback). The
    /// returned stats are what the windowed app would also see; use this to
    /// time a frame.
    #[allow(clippy::too_many_arguments)]
    pub fn frame(
        &mut self,
        text: Option<&mut GpuText>,
        ui: &[u32],
        keyed: bool,
        width: u32,
        height: u32,
        effect: Option<ActiveEffect>,
        time: f32,
    ) -> Result<FrameStats, String> {
        if width == 0 || height == 0 || ui.len() < (width * height) as usize {
            return Err("bad frame size".into());
        }
        self.ensure_target(width, height);
        let view = self.target.as_ref().map(|t| &t.1).ok_or("no target")?;
        let mut stats = self.core.render(text, ui, keyed, width, height, effect.as_ref(), time, 1.0, view);
        let t = Instant::now();
        self.core
            .device
            .poll(wgpu::PollType::Wait { submission_index: None, timeout: None })
            .map_err(|e| format!("poll: {e}"))?;
        stats.t_wait = t.elapsed();
        Ok(stats)
    }

    /// Forget what the UI layer texture holds, so the next frame uploads it
    /// completely (what the pre-diff pipeline did on every frame).
    pub fn invalidate_ui(&mut self) {
        self.core.ui_valid = false;
    }

    /// Copy the last rendered frame back as `0x00RRGGBB` pixels.
    pub fn read_pixels(&mut self) -> Result<Vec<u32>, String> {
        let (tex, _, readback, w, h) = self.target.as_ref().ok_or("nothing rendered yet")?;
        let (w, h) = (*w, *h);
        let bytes_per_row = (4 * w).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let mut encoder = self
            .core
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("offscreen_copy") });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(bytes_per_row),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        self.core.queue.submit(std::iter::once(encoder.finish()));
        let (tx, rx) = std::sync::mpsc::channel();
        readback.slice(..).map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.core
            .device
            .poll(wgpu::PollType::Wait { submission_index: None, timeout: None })
            .map_err(|e| format!("poll: {e}"))?;
        rx.recv().map_err(|e| e.to_string())?.map_err(|e| format!("map: {e}"))?;
        let data = readback.slice(..).get_mapped_range().map_err(|e| format!("mapped range: {e}"))?;
        let mut out = Vec::with_capacity((w * h) as usize);
        for row in 0..h as usize {
            let line = &data[row * bytes_per_row as usize..row * bytes_per_row as usize + 4 * w as usize];
            out.extend(line.chunks_exact(4).map(|p| (p[0] as u32) << 16 | (p[1] as u32) << 8 | p[2] as u32));
        }
        drop(data);
        readback.unmap();
        Ok(out)
    }

    /// Render a frame and read it back.
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        text: Option<&mut GpuText>,
        ui: &[u32],
        keyed: bool,
        width: u32,
        height: u32,
        effect: Option<ActiveEffect>,
        time: f32,
    ) -> Result<Vec<u32>, String> {
        self.frame(text, ui, keyed, width, height, effect, time)?;
        self.read_pixels()
    }
}

/// Run the effect shader (or a plain blit when `effect` is `None`) over
/// `pixels` (0x00RRGGBB, `width * height`) on the GPU without any window or
/// surface, and read the result back. Same shaders, texture formats and
/// uniforms as the windowed pipeline, so the output matches what the app
/// shows with the GPU renderer.
pub fn render_offscreen(
    pixels: &[u32],
    width: u32,
    height: u32,
    effect: Option<ActiveEffect>,
    time: f32,
) -> Result<Vec<u32>, String> {
    OffscreenGpu::new()?.render(None, pixels, false, width, height, effect, time)
}

// ── Helpers ──

fn create_texture(device: &wgpu::Device, width: u32, height: u32) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("ui_layer"),
        size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: TEX_FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

fn tex_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn sampler_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    }
}

fn uniform_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn create_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    module: &wgpu::ShaderModule,
    format: wgpu::TextureFormat,
    label: &str,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module,
            entry_point: Some("vs_main"),
            buffers: &[],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

// ── Shaders ──

/// Instanced terminal pass: solid quads and atlas-sampled coverage glyphs.
/// Atlas texels are fetched 1:1 (`textureLoad`), so glyph pixels are exactly
/// the rasterizer's output.
pub const TEXT_WGSL: &str = r#"
struct Globals {
    screen: vec2<f32>,
    _pad: vec2<f32>,
}

@group(0) @binding(0) var<uniform> G: Globals;
@group(0) @binding(1) var atlas: texture_2d_array<f32>;
@group(0) @binding(2) var color_atlas: texture_2d<f32>;

struct VIn {
    @builtin(vertex_index) vi: u32,
    @location(0) pos: vec2<f32>,
    @location(1) size: vec2<f32>,
    @location(2) uv: u32,
    @location(3) color: u32,
    @location(4) kind: u32,
}

struct VOut {
    @builtin(position) position: vec4<f32>,
    @location(0) @interpolate(flat) color: vec4<f32>,
    @location(1) texel: vec2<f32>,
    @location(2) @interpolate(flat) kind: u32,
}

@vertex
fn vs_main(in: VIn) -> VOut {
    var cx = array<f32, 6>(0.0, 1.0, 0.0, 1.0, 1.0, 0.0);
    var cy = array<f32, 6>(0.0, 0.0, 1.0, 0.0, 1.0, 1.0);
    let c = vec2<f32>(cx[in.vi], cy[in.vi]);
    let px = in.pos + c * in.size;
    var out: VOut;
    out.position = vec4<f32>(px.x / G.screen.x * 2.0 - 1.0, 1.0 - px.y / G.screen.y * 2.0, 0.0, 1.0);
    out.color = unpack4x8unorm(in.color);
    out.texel = vec2<f32>(f32(in.uv & 0xffffu), f32(in.uv >> 16u)) + c * in.size;
    out.kind = in.kind;
    return out;
}

@fragment
fn fs_main(in: VOut) -> @location(0) vec4<f32> {
    let k = in.kind & 0xffu;
    if k == 0u {
        return in.color;
    }
    let t = vec2<i32>(floor(in.texel));
    if k == 1u {
        var a = textureLoad(atlas, t, i32(in.kind >> 8u), 0).r;
        // Same snap as the CPU glyph blit: near-full coverage is the pure fg color.
        if a >= 250.0 / 255.0 {
            a = 1.0;
        }
        return vec4<f32>(in.color.rgb, in.color.a * a);
    }
    let c = textureLoad(color_atlas, t, 0);
    return vec4<f32>(c.rgb, c.a * in.color.a);
}
"#;

/// Passthrough shader (no effect).
const BLIT_WGSL: &str = r#"
struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) idx: u32) -> VertexOutput {
    var out: VertexOutput;
    let x = f32(i32(idx) / 2) * 4.0 - 1.0;
    let y = f32(i32(idx) % 2) * 4.0 - 1.0;
    out.position = vec4(x, y, 0.0, 1.0);
    out.uv = vec2((x + 1.0) * 0.5, (1.0 - y) * 0.5);
    return out;
}

struct Params {
    time: f32,
    intensity: f32,
    effect: u32,
    _pad0: u32,
    resolution: vec2<f32>,
    opacity: f32,
    _pad1: f32,
    bg: vec4<f32>,
    accent: vec4<f32>,
}

@group(0) @binding(0) var frame_tex: texture_2d<f32>;
@group(0) @binding(1) var frame_sampler: sampler;
@group(0) @binding(2) var<uniform> P: Params;
@group(0) @binding(3) var ui_tex: texture_2d<f32>;

// Terminal layer (frame_tex) shows through where the UI layer is transparent.
fn composite(uv: vec2<f32>) -> vec3<f32> {
    let u = textureSampleLevel(ui_tex, frame_sampler, uv, 0.0);
    let t = textureSampleLevel(frame_tex, frame_sampler, uv, 0.0);
    return mix(t.rgb, u.rgb, u.a);
}

@fragment
fn fs_main(@location(0) uv: vec2<f32>) -> @location(0) vec4<f32> {
    return vec4<f32>(composite(uv) * P.opacity, 1.0);
}
"#;

/// The six Rift effects in one fragment shader; `Params.effect` selects:
/// 1 CRT, 2 Glitch, 3 Neon Glow, 4 Matrix Rain, 5 Amber, 6 Hologram
/// (same numbering as `EffectKind::id`). Sampling uses `textureSampleLevel`
/// so effects may branch on per-pixel data without uniformity issues.
pub const FX_WGSL: &str = r#"
struct Params {
    time: f32,
    intensity: f32,
    effect: u32,
    _pad0: u32,
    resolution: vec2<f32>,
    opacity: f32,
    _pad1: f32,
    bg: vec4<f32>,
    accent: vec4<f32>,
}

@group(0) @binding(0) var frame_tex: texture_2d<f32>;
@group(0) @binding(1) var frame_sampler: sampler;
@group(0) @binding(2) var<uniform> P: Params;
@group(0) @binding(3) var ui_tex: texture_2d<f32>;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) idx: u32) -> VertexOutput {
    var out: VertexOutput;
    let x = f32(i32(idx) / 2) * 4.0 - 1.0;
    let y = f32(i32(idx) % 2) * 4.0 - 1.0;
    out.position = vec4(x, y, 0.0, 1.0);
    out.uv = vec2((x + 1.0) * 0.5, (1.0 - y) * 0.5);
    return out;
}

const PI: f32 = 3.14159265;
const GLITCH_SLOT: f32 = 0.2;

// The scene the effects distort: terminal layer, with the UI layer on top
// wherever it is opaque.
fn tex(uv: vec2<f32>) -> vec3<f32> {
    let u = textureSampleLevel(ui_tex, frame_sampler, uv, 0.0);
    let t = textureSampleLevel(frame_tex, frame_sampler, uv, 0.0);
    return mix(t.rgb, u.rgb, u.a);
}

fn lin3(c: vec3<f32>) -> vec3<f32> {
    return pow(c, vec3<f32>(2.2));
}

fn luma(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
}

// Same integer hash as `effects::hash_u32` on the CPU.
fn hash_u(x: u32) -> u32 {
    var h = x;
    h = h ^ (h >> 16u);
    h = h * 0x7feb352du;
    h = h ^ (h >> 15u);
    h = h * 0x846ca68bu;
    h = h ^ (h >> 16u);
    return h;
}

fn hash1(x: u32) -> f32 {
    return f32(hash_u(x) & 0xffffffu) / 16777216.0;
}

// ── 1. CRT: curvature + scanlines + vignette + subtle chromatic aberration ──
fn fx_crt(uv: vec2<f32>, k: f32) -> vec3<f32> {
    let c = uv * 2.0 - 1.0;
    let bend = 0.08 * k;
    let q = c * vec2<f32>(1.0 + bend * c.y * c.y, 1.0 + bend * c.x * c.x);
    let suv = q * 0.5 + 0.5;
    let inside = step(0.0, suv.x) * step(suv.x, 1.0) * step(0.0, suv.y) * step(suv.y, 1.0);
    let px = 1.0 / P.resolution;
    let ca = 1.5 * k * px.x * (0.5 + length(c));
    var col = vec3<f32>(
        tex(suv + vec2<f32>(ca, 0.0)).r,
        tex(suv).g,
        tex(suv - vec2<f32>(ca, 0.0)).b,
    );
    let row = suv.y * P.resolution.y;
    col = col * (1.0 - 0.30 * k * (0.5 - 0.5 * sin(row * PI)));
    col = col * (1.0 - 0.55 * k * smoothstep(0.35, 1.25, length(c)));
    col = col * (1.0 + 0.08 * k);
    return col * inside;
}

// ── 2. Glitch: short, occasional bursts (~12% of 0.2s slots) ──
fn glitch_on(t: f32) -> bool {
    if t < 0.0 {
        return false;
    }
    let slot = u32(floor(t / GLITCH_SLOT));
    return hash_u(slot) % 100u < 12u;
}

fn fx_glitch(uv: vec2<f32>, k: f32) -> vec3<f32> {
    if !glitch_on(P.time) {
        return tex(uv);
    }
    let tick = u32(P.time * 30.0);
    let sid = u32(floor(uv.y * 28.0));
    var off = 0.0;
    var hit = 0.0;
    if hash1(sid * 7919u + tick * 104729u) < 0.30 {
        off = (hash1(sid * 31u + tick * 17u) - 0.5) * 0.14 * (0.3 + k);
        hit = 1.0;
    }
    if abs(uv.y - hash1(tick * 3u + 1u)) < 0.004 {
        off = off + 0.05 * (0.3 + k);
        hit = 1.0;
    }
    let split = (0.004 + 0.012 * k) * (0.4 + hit);
    var col = vec3<f32>(
        tex(vec2<f32>(uv.x + off + split, uv.y)).r,
        tex(vec2<f32>(uv.x + off, uv.y)).g,
        tex(vec2<f32>(uv.x + off - split, uv.y)).b,
    );
    col = col + lin3(P.accent.rgb) * hit * 0.06 * k;
    return col;
}

// ── 3. Neon Glow: bloom of bright text ──
fn fx_neon(uv: vec2<f32>, k: f32) -> vec3<f32> {
    let base = tex(uv);
    let px = 1.0 / P.resolution;
    var glow = vec3<f32>(0.0);
    var wsum = 0.0;
    for (var r = 0; r < 3; r++) {
        let radius = 2.5 + 4.5 * f32(r) + 1.5 * f32(r * r);
        let w = 1.0 / (1.0 + 0.7 * f32(r) * f32(r));
        for (var a = 0; a < 8; a++) {
            let ang = f32(a) * (PI / 4.0);
            let o = vec2<f32>(cos(ang), sin(ang)) * radius * px;
            glow = glow + max(tex(uv + o) - vec3<f32>(0.35), vec3<f32>(0.0)) * w;
            wsum = wsum + w;
        }
    }
    glow = glow / wsum;
    return base + glow * 3.5 * k;
}

// ── 4. Matrix Rain: low-alpha rain layer behind the text ──
fn fx_matrix(uv: vec2<f32>, k: f32) -> vec3<f32> {
    let base = tex(uv);
    let frag = uv * P.resolution;
    let cell = vec2<f32>(11.0, 20.0);
    let cid = floor(frag / cell);
    let local = frag / cell - cid;
    let cx = u32(max(cid.x, 0.0));
    let cy = u32(max(cid.y, 0.0));
    let speed = 4.0 + 8.0 * hash1(cx * 7919u + 13u);
    let rows = P.resolution.y / cell.y;
    let trail = 8.0 + 14.0 * hash1(cx * 104729u + 7u);
    let period = rows + trail + 6.0;
    let head = fract(P.time * speed / period + hash1(cx * 31337u + 3u)) * period;
    let d = head - cid.y;
    var rain = 0.0;
    if d >= 0.0 && d < trail {
        rain = pow(1.0 - d / trail, 2.0);
        if d < 1.0 {
            rain = 1.5;
        }
    }
    let tick = u32(P.time * (6.0 + 6.0 * hash1(cx * 977u + 5u)));
    let gx = u32(floor(local.x * 4.0));
    let gy = u32(floor(local.y * 6.0));
    let edge = step(0.15, local.x) * step(local.x, 0.85) * step(0.1, local.y) * step(local.y, 0.9);
    let bit = step(0.45, hash1(cx * 977u + cy * 131u + gx * 17u + gy * 29u + tick * 7919u));
    let tint = mix(vec3<f32>(0.04, 1.0, 0.35), lin3(P.accent.rgb), 0.25);
    // Only paint where the frame is (nearly) background so glyphs stay on top.
    let bgness = 1.0 - smoothstep(0.015, 0.10, distance(base, lin3(P.bg.rgb)));
    return base + tint * rain * bit * edge * (0.55 * k) * bgness;
}

// ── 5. Amber: monochrome phosphor ──
fn fx_amber(uv: vec2<f32>, k: f32) -> vec3<f32> {
    let c = tex(uv);
    let l = min(luma(c) * 1.15, 1.0);
    let amber = vec3<f32>(1.0, 0.45, 0.0) * l;
    return mix(c, amber, 0.35 + 0.65 * k);
}

// ── 6. Hologram: cyan tint + scan sweep + flicker ──
fn fx_holo(uv: vec2<f32>, k: f32) -> vec3<f32> {
    let sweep = fract(P.time * 0.22);
    let dist = uv.y - sweep;
    let band = exp(-dist * dist / 0.0015);
    let suv = vec2<f32>(uv.x + band * 0.004 * k, uv.y);
    let c = tex(suv);
    let l = luma(c);
    let tint = vec3<f32>(0.05, 0.85, 1.0);
    var col = mix(c, tint * l * 1.3, 0.45 + 0.45 * k);
    col = col + tint * 0.015 * k;
    col = col + tint * band * 0.35 * k * (0.3 + l);
    let row = uv.y * P.resolution.y;
    col = col * (0.93 + 0.07 * sin(row * 1.5708));
    col = col * (1.0 - 0.06 * k * hash1(u32(P.time * 24.0)));
    return col;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let k = clamp(P.intensity, 0.0, 1.0);
    var col: vec3<f32>;
    switch P.effect {
        case 1u: { col = fx_crt(in.uv, k); }
        case 2u: { col = fx_glitch(in.uv, k); }
        case 3u: { col = fx_neon(in.uv, k); }
        case 4u: { col = fx_matrix(in.uv, k); }
        case 5u: { col = fx_amber(in.uv, k); }
        case 6u: { col = fx_holo(in.uv, k); }
        default: { col = tex(in.uv); }
    }
    return vec4<f32>(col * P.opacity, 1.0);
}
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use wgpu::naga;

    fn parse(src: &str) -> naga::Module {
        naga::front::wgsl::parse_str(src).unwrap_or_else(|e| panic!("WGSL parse error:\n{}", e.emit_to_string(src)))
    }

    fn validate(src: &str) -> naga::Module {
        let module = parse(src);
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
            .validate(&module)
            .unwrap_or_else(|e| panic!("WGSL validation error: {e:?}"));
        module
    }

    #[test]
    fn fx_shader_parses_and_validates() {
        let m = validate(FX_WGSL);
        let names: Vec<_> = m.entry_points.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"vs_main") && names.contains(&"fs_main"), "{names:?}");
    }

    #[test]
    fn blit_shader_parses_and_validates() {
        validate(BLIT_WGSL);
    }

    #[test]
    fn text_shader_parses_and_validates() {
        let m = validate(TEXT_WGSL);
        let names: Vec<_> = m.entry_points.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"vs_main") && names.contains(&"fs_main"), "{names:?}");
    }

    #[test]
    fn instance_layout_matches_vertex_attributes() {
        // 5 attributes: f32x2, f32x2, u32, u32, u32.
        assert_eq!(std::mem::size_of::<Inst>(), 28);
        assert_eq!(INST_SIZE, 28);
    }

    #[test]
    fn row_diff_finds_span() {
        let a = vec![1u32; 100];
        let mut b = a.clone();
        assert_eq!(row_diff(&a, &b), None);
        b[37] = 9;
        assert_eq!(row_diff(&a, &b), Some((37, 38)));
        b[90] = 9;
        assert_eq!(row_diff(&a, &b), Some((37, 91)));
        let mut c = a.clone();
        c[0] = 5;
        c[99] = 5;
        assert_eq!(row_diff(&a, &c), Some((0, 100)));
    }

    #[test]
    fn uniform_layout_matches_wgsl() {
        assert_eq!(std::mem::size_of::<FxUniforms>(), 64);
        let m = validate(FX_WGSL);
        let (_, ty) = m
            .types
            .iter()
            .find(|(_, t)| t.name.as_deref() == Some("Params"))
            .expect("Params struct");
        match &ty.inner {
            naga::TypeInner::Struct { span, .. } => assert_eq!(*span, 64),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn every_effect_id_has_a_shader_case() {
        for k in crate::effects::EffectKind::ALL {
            assert!(FX_WGSL.contains(&format!("case {}u:", k.id())), "no case for {:?}", k);
        }
    }

    #[test]
    fn glitch_schedule_matches_cpu_constants() {
        assert!(FX_WGSL.contains("const GLITCH_SLOT: f32 = 0.2;"));
        assert!(FX_WGSL.contains("% 100u < 12u"));
        assert!(FX_WGSL.contains("0x7feb352du") && FX_WGSL.contains("0x846ca68bu"));
    }
}
