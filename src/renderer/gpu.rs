//! wgpu GPU rendering pipeline (Phase 2).
//!
//! CPU text rendering (fontdue) produces a pixel buffer, which is uploaded
//! as a GPU texture. Two render pipelines are available:
//!   - **blit**: passthrough (no effect)
//!   - **crt**: CRT post-processing (scanlines, curvature, chromatic aberration)
//!
//! Enable with `cargo build --features gpu`.

#![allow(dead_code)]

#[cfg(feature = "gpu")]
use std::sync::Arc;
#[cfg(feature = "gpu")]
use winit::window::Window;

#[cfg(feature = "gpu")]
use crate::effects::ShaderEffect;

// ── Uniform data sent to the CRT shader ──

#[cfg(feature = "gpu")]
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct ShaderUniforms {
    scanline_intensity: f32,
    curvature: f32,
    chromatic_aberration: f32,
    flicker: f32,
    vignette: f32,
    time: f32,
    resolution_x: f32,
    resolution_y: f32,
}

// ── GPU Pipeline ──

#[cfg(feature = "gpu")]
pub struct GpuPipeline {
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface: wgpu::Surface<'static>,
    surface_config: wgpu::SurfaceConfiguration,

    // Two pipelines
    blit_pipeline: wgpu::RenderPipeline,
    crt_pipeline: wgpu::RenderPipeline,

    // Shared bind group layouts
    blit_bgl: wgpu::BindGroupLayout,
    crt_bgl: wgpu::BindGroupLayout,

    // Resources
    sampler: wgpu::Sampler,
    pixel_texture: wgpu::Texture,
    pixel_view: wgpu::TextureView,
    uniform_buffer: wgpu::Buffer,
    blit_bind_group: wgpu::BindGroup,
    crt_bind_group: wgpu::BindGroup,

    width: u32,
    height: u32,
    rgba_buffer: Vec<u8>,
}

#[cfg(feature = "gpu")]
impl GpuPipeline {
    pub fn new(window: Arc<Window>) -> Result<Self, String> {
        let size = window.inner_size();
        let (width, height) = (size.width.max(1), size.height.max(1));

        // Instance / Surface / Adapter / Device
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
        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("rift"),
                ..Default::default()
            },
        ))
        .map_err(|e| format!("request_device: {e}"))?;

        // Surface config
        let mut surface_config = surface
            .get_default_config(&adapter, width, height)
            .ok_or_else(|| "Surface not supported by adapter".to_string())?;
        surface_config.present_mode = wgpu::PresentMode::Fifo;
        surface.configure(&device, &surface_config);
        let format = surface_config.format;

        // Texture + sampler
        let pixel_texture = create_texture(&device, width, height);
        let pixel_view = pixel_texture.create_view(&Default::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        // Uniform buffer for CRT params
        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("shader_uniforms"),
            size: std::mem::size_of::<ShaderUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // ── Blit pipeline (texture + sampler only) ──

        let blit_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("blit_bgl"),
            entries: &[
                tex_entry(0),
                sampler_entry(1),
            ],
        });

        let blit_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("blit_bg"),
            layout: &blit_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&pixel_view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&sampler) },
            ],
        });

        let blit_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("blit_shader"),
            source: wgpu::ShaderSource::Wgsl(BLIT_WGSL.into()),
        });

        let blit_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("blit_layout"),
            bind_group_layouts: &[Some(&blit_bgl)],
            immediate_size: 0,
        });

        let blit_pipeline = create_pipeline(&device, &blit_layout, &blit_shader, &blit_shader, format, "blit");

        // ── CRT pipeline (texture + sampler + uniforms) ──

        let crt_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("crt_bgl"),
            entries: &[
                tex_entry(0),
                sampler_entry(1),
                uniform_entry(2),
            ],
        });

        let crt_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("crt_bg"),
            layout: &crt_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&pixel_view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&sampler) },
                wgpu::BindGroupEntry { binding: 2, resource: uniform_buffer.as_entire_binding() },
            ],
        });

        // Combined CRT shader: vertex (fullscreen tri) + fragment (CRT effect)
        let crt_wgsl = format!("{}\n{}", crate::effects::FULLSCREEN_QUAD_WGSL, crate::effects::CRT_SHADER_WGSL);
        let crt_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("crt_shader"),
            source: wgpu::ShaderSource::Wgsl(crt_wgsl.into()),
        });

        let crt_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("crt_layout"),
            bind_group_layouts: &[Some(&crt_bgl)],
            immediate_size: 0,
        });

        let crt_pipeline = create_pipeline(&device, &crt_layout, &crt_shader, &crt_shader, format, "crt");

        Ok(Self {
            device, queue, surface, surface_config,
            blit_pipeline, crt_pipeline,
            blit_bgl, crt_bgl,
            sampler, pixel_texture, pixel_view, uniform_buffer,
            blit_bind_group, crt_bind_group,
            width, height,
            rgba_buffer: Vec::new(),
        })
    }

    /// Render a frame. When `effect` is `Some(Crt(..))`, the GPU CRT shader runs;
    /// otherwise the blit (passthrough) pipeline is used. All other effects still
    /// run on the CPU before the pixel buffer reaches this method.
    pub fn render_frame(
        &mut self,
        pixels: &[u32],
        width: u32,
        height: u32,
        effect: Option<&ShaderEffect>,
        time: f32,
    ) {
        if width == 0 || height == 0 { return; }
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
            self.rgba_buffer[off]     = ((px >> 16) & 0xff) as u8;
            self.rgba_buffer[off + 1] = ((px >>  8) & 0xff) as u8;
            self.rgba_buffer[off + 2] = ( px        & 0xff) as u8;
            self.rgba_buffer[off + 3] = 255;
        }

        // Upload texture
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

        // Select pipeline + bind group based on effect
        let use_crt = matches!(effect, Some(ShaderEffect::Crt(_)));

        if use_crt {
            if let Some(ShaderEffect::Crt(params)) = effect {
                let uniforms = ShaderUniforms {
                    scanline_intensity: params.scanline_intensity,
                    curvature: params.curvature,
                    chromatic_aberration: params.chromatic_aberration,
                    flicker: params.flicker,
                    vignette: params.vignette,
                    time,
                    resolution_x: width as f32,
                    resolution_y: height as f32,
                };
                self.queue.write_buffer(
                    &self.uniform_buffer,
                    0,
                    bytemuck::bytes_of(&uniforms),
                );
            }
        }

        let pipeline = if use_crt { &self.crt_pipeline } else { &self.blit_pipeline };
        let bind_group = if use_crt { &self.crt_bind_group } else { &self.blit_bind_group };

        // Acquire frame
        let output = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t)
            | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.surface_config);
                return;
            }
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                return;
            }
            _ => {
                log::error!("wgpu surface: unexpected status");
                return;
            }
        };
        let view = output.texture.create_view(&Default::default());

        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("render_enc"),
        });

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

        // Recreate both bind groups with new texture view
        self.blit_bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("blit_bg"),
            layout: &self.blit_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&self.pixel_view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.sampler) },
            ],
        });
        self.crt_bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("crt_bg"),
            layout: &self.crt_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&self.pixel_view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                wgpu::BindGroupEntry { binding: 2, resource: self.uniform_buffer.as_entire_binding() },
            ],
        });
    }
}

// ── Helper functions ──

#[cfg(feature = "gpu")]
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

#[cfg(feature = "gpu")]
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

#[cfg(feature = "gpu")]
fn sampler_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    }
}

#[cfg(feature = "gpu")]
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

#[cfg(feature = "gpu")]
fn create_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    vs_module: &wgpu::ShaderModule,
    fs_module: &wgpu::ShaderModule,
    format: wgpu::TextureFormat,
    label: &str,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: vs_module,
            entry_point: Some("vs_main"),
            buffers: &[],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: fs_module,
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

// ── Blit shader (passthrough) ──

#[cfg(feature = "gpu")]
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
    return textureSample(frame_tex, frame_sampler, uv);
}
"#;
