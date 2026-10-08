//! wgpu GPU rendering pipeline (`--features gpu`).
//!
//! CPU text rendering (fontdue) produces a pixel buffer, which is uploaded
//! as a GPU texture. Two render pipelines are available:
//!   - **blit**: passthrough (no effect)
//!   - **fx**: one WGSL fragment shader implementing the six Rift effects
//!     (CRT, Glitch, Neon Glow, Matrix Rain, Amber, Hologram), selected by the
//!     `effect` uniform. Uniforms: time, resolution, intensity, effect id
//!     (plus the theme background / accent colours).
//!
//! While this pipeline is active the CPU `ShaderPipeline` never touches the
//! frame, so the renderer's damage tracking stays effective.

use std::sync::Arc;
use winit::window::Window;

use crate::effects::ActiveEffect;

// ── Uniform block shared with `FX_WGSL` (`struct Params`, 64 bytes) ──

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct FxUniforms {
    time: f32,
    intensity: f32,
    effect: u32,
    _pad0: u32,
    resolution: [f32; 2],
    _pad1: [f32; 2],
    /// Theme background, sRGB 0..1 (w unused).
    bg: [f32; 4],
    /// Theme accent, sRGB 0..1 (w unused).
    accent: [f32; 4],
}

impl FxUniforms {
    fn new(fx: &ActiveEffect, time: f32, width: u32, height: u32) -> Self {
        let c = |(r, g, b): (u8, u8, u8)| [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0];
        Self {
            time,
            intensity: fx.intensity.clamp(0.0, 1.0),
            effect: fx.kind.id(),
            _pad0: 0,
            resolution: [width as f32, height as f32],
            _pad1: [0.0; 2],
            bg: c(fx.bg),
            accent: c(fx.accent),
        }
    }
}

// ── GPU Pipeline ──

pub struct GpuPipeline {
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface: wgpu::Surface<'static>,
    surface_config: wgpu::SurfaceConfiguration,

    blit_pipeline: wgpu::RenderPipeline,
    fx_pipeline: wgpu::RenderPipeline,

    blit_bgl: wgpu::BindGroupLayout,
    fx_bgl: wgpu::BindGroupLayout,

    sampler: wgpu::Sampler,
    pixel_texture: wgpu::Texture,
    pixel_view: wgpu::TextureView,
    uniform_buffer: wgpu::Buffer,
    blit_bind_group: wgpu::BindGroup,
    fx_bind_group: wgpu::BindGroup,

    width: u32,
    height: u32,
    rgba_buffer: Vec<u8>,
}

impl GpuPipeline {
    pub fn new(window: Arc<Window>) -> Result<Self, String> {
        let size = window.inner_size();
        let (width, height) = (size.width.max(1), size.height.max(1));

        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            flags: wgpu::InstanceFlags::empty(),
            memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
            backend_options: wgpu::BackendOptions::default(),
            display: None,
        });
        let surface = instance
            .create_surface(window)
            .map_err(|e| format!("create_surface: {e}"))?;
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

        let pixel_texture = create_texture(&device, width, height);
        let pixel_view = pixel_texture.create_view(&Default::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("fx_uniforms"),
            size: std::mem::size_of::<FxUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // ── Blit pipeline ──
        let blit_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("blit_bgl"),
            entries: &[tex_entry(0), sampler_entry(1)],
        });
        let blit_bind_group = make_blit_bg(&device, &blit_bgl, &pixel_view, &sampler);
        let blit_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("blit_shader"),
            source: wgpu::ShaderSource::Wgsl(BLIT_WGSL.into()),
        });
        let blit_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("blit_layout"),
            bind_group_layouts: &[Some(&blit_bgl)],
            immediate_size: 0,
        });
        let blit_pipeline = create_pipeline(&device, &blit_layout, &blit_shader, format, "blit");

        // ── FX pipeline ──
        let fx_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("fx_bgl"),
            entries: &[tex_entry(0), sampler_entry(1), uniform_entry(2)],
        });
        let fx_bind_group = make_fx_bg(&device, &fx_bgl, &pixel_view, &sampler, &uniform_buffer);
        let fx_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("fx_shader"),
            source: wgpu::ShaderSource::Wgsl(FX_WGSL.into()),
        });
        let fx_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("fx_layout"),
            bind_group_layouts: &[Some(&fx_bgl)],
            immediate_size: 0,
        });
        let fx_pipeline = create_pipeline(&device, &fx_layout, &fx_shader, format, "fx");

        Ok(Self {
            device,
            queue,
            surface,
            surface_config,
            blit_pipeline,
            fx_pipeline,
            blit_bgl,
            fx_bgl,
            sampler,
            pixel_texture,
            pixel_view,
            uniform_buffer,
            blit_bind_group,
            fx_bind_group,
            width,
            height,
            rgba_buffer: Vec::new(),
        })
    }

    /// Render a frame. With `effect == None` the blit pipeline presents the
    /// buffer untouched; otherwise the FX shader runs on the GPU.
    pub fn render_frame(
        &mut self,
        pixels: &[u32],
        width: u32,
        height: u32,
        effect: Option<ActiveEffect>,
        time: f32,
    ) {
        if width == 0 || height == 0 {
            return;
        }
        let frame_start = std::time::Instant::now();

        if width != self.width || height != self.height {
            self.resize(width, height);
        }

        // XRGB → RGBA (pre-allocated, no per-frame alloc)
        let pixel_count = (width * height) as usize;
        let rgba_len = pixel_count * 4;
        if self.rgba_buffer.len() != rgba_len {
            self.rgba_buffer.resize(rgba_len, 0);
        }
        for (i, &px) in pixels.iter().enumerate().take(pixel_count) {
            let off = i * 4;
            self.rgba_buffer[off] = ((px >> 16) & 0xff) as u8;
            self.rgba_buffer[off + 1] = ((px >> 8) & 0xff) as u8;
            self.rgba_buffer[off + 2] = (px & 0xff) as u8;
            self.rgba_buffer[off + 3] = 255;
        }

        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.pixel_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &self.rgba_buffer,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4 * width),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        );

        if let Some(fx) = effect.as_ref() {
            let u = FxUniforms::new(fx, time, width, height);
            self.queue.write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(&u));
        }
        let (pipeline, bind_group) = if effect.is_some() {
            (&self.fx_pipeline, &self.fx_bind_group)
        } else {
            (&self.blit_pipeline, &self.blit_bind_group)
        };

        let output = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t) | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.surface_config);
                return;
            }
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => return,
            _ => {
                log::error!("wgpu surface: unexpected status");
                return;
            }
        };
        let view = output.texture.create_view(&Default::default());

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("render_enc") });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("render_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, bind_group, &[]);
            pass.draw(0..3, 0..1);
        }

        self.queue.submit(std::iter::once(encoder.finish()));
        self.queue.present(output);

        let frame_ms = frame_start.elapsed().as_secs_f64() * 1000.0;
        if frame_ms > 16.0 {
            log::debug!("GPU frame slow: {:.1}ms", frame_ms);
        }
    }

    fn resize(&mut self, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        self.surface_config.width = width;
        self.surface_config.height = height;
        self.surface.configure(&self.device, &self.surface_config);
        self.pixel_texture = create_texture(&self.device, width, height);
        self.pixel_view = self.pixel_texture.create_view(&Default::default());
        self.blit_bind_group = make_blit_bg(&self.device, &self.blit_bgl, &self.pixel_view, &self.sampler);
        self.fx_bind_group =
            make_fx_bg(&self.device, &self.fx_bgl, &self.pixel_view, &self.sampler, &self.uniform_buffer);
    }
}

// ── Helpers ──

fn make_blit_bg(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    view: &wgpu::TextureView,
    sampler: &wgpu::Sampler,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("blit_bg"),
        layout,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(view) },
            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(sampler) },
        ],
    })
}

fn make_fx_bg(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    view: &wgpu::TextureView,
    sampler: &wgpu::Sampler,
    uniforms: &wgpu::Buffer,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("fx_bg"),
        layout,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(view) },
            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(sampler) },
            wgpu::BindGroupEntry { binding: 2, resource: uniforms.as_entire_binding() },
        ],
    })
}

fn create_texture(device: &wgpu::Device, width: u32, height: u32) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("pixels"),
        size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
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

@group(0) @binding(0) var frame_tex: texture_2d<f32>;
@group(0) @binding(1) var frame_sampler: sampler;

@fragment
fn fs_main(@location(0) uv: vec2<f32>) -> @location(0) vec4<f32> {
    return textureSampleLevel(frame_tex, frame_sampler, uv, 0.0);
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
    _pad1: vec2<f32>,
    bg: vec4<f32>,
    accent: vec4<f32>,
}

@group(0) @binding(0) var frame_tex: texture_2d<f32>;
@group(0) @binding(1) var frame_sampler: sampler;
@group(0) @binding(2) var<uniform> P: Params;

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

fn tex(uv: vec2<f32>) -> vec3<f32> {
    return textureSampleLevel(frame_tex, frame_sampler, uv, 0.0).rgb;
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
    return vec4<f32>(col, 1.0);
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
