pub const CRT_SHADER_WGSL: &str = r#"
struct Params {
    scanline_intensity: f32,
    curvature: f32,
    chromatic_aberration: f32,
    flicker: f32,
    vignette: f32,
    time: f32,
    resolution: vec2<f32>,
}

@group(0) @binding(0) var input_texture: texture_2d<f32>;
@group(0) @binding(1) var input_sampler: sampler;
@group(0) @binding(2) var<uniform> params: Params;

fn apply_curvature(uv: vec2<f32>, amount: f32) -> vec2<f32> {
    let centered = uv - vec2(0.5);
    let dist = dot(centered, centered);
    return centered * (1.0 + dist * amount) + vec2(0.5);
}

fn scanlines(uv: vec2<f32>, intensity: f32, res_y: f32) -> f32 {
    let line = sin(uv.y * res_y * 3.14159) * 0.5 + 0.5;
    return mix(1.0, line, intensity);
}

@fragment
fn fs_main(@location(0) uv: vec2<f32>) -> @location(0) vec4<f32> {
    var coords = apply_curvature(uv, params.curvature);

    if coords.x < 0.0 || coords.x > 1.0 || coords.y < 0.0 || coords.y > 1.0 {
        return vec4(0.0, 0.0, 0.0, 1.0);
    }

    let ca = params.chromatic_aberration;
    let r = textureSample(input_texture, input_sampler, coords + vec2(ca, 0.0)).r;
    let g = textureSample(input_texture, input_sampler, coords).g;
    let b = textureSample(input_texture, input_sampler, coords - vec2(ca, 0.0)).b;
    var color = vec3(r, g, b);

    color *= scanlines(coords, params.scanline_intensity, params.resolution.y);
    color *= 1.0 - params.flicker * sin(params.time * 8.0) * 0.5;

    let vig = smoothstep(0.8, 0.3, length(coords - vec2(0.5)));
    color *= mix(1.0, vig, params.vignette);

    return vec4(color, 1.0);
}
"#;

pub const FULLSCREEN_QUAD_WGSL: &str = r#"
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
"#;
