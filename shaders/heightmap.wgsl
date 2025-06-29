// Uniform buffer containing the transform matrix
struct Uniforms {
    transform: mat4x4<f32>,
    min_depth: f32,
    max_depth: f32,
    has_color: u32,
};

@group(0) @binding(0) var t_depth: texture_2d<f32>;
@group(0) @binding(1) var s_depth: sampler;
@group(0) @binding(2) var t_color: texture_2d<f32>;
@group(0) @binding(3) var s_color: sampler;
@group(0) @binding(4) var<uniform> uniforms: Uniforms;

struct RgbaDepth {
  @location(0) color: vec4<f32>,
  @builtin(frag_depth) depth: f32
}

// Fragment shader
@fragment
fn fs_main(@location(0) tex_coords: vec2<f32>) -> RgbaDepth {
    let depth = textureSample(t_depth, s_depth, tex_coords).r;
    if (depth == 0) {
        discard;
    } else {
        let d = (depth - uniforms.min_depth) / (uniforms.max_depth - uniforms.min_depth);
        var color = vec3<f32>(1.0, 1.0, 1.0);
        if (uniforms.has_color != 0) {
            color = textureSample(t_color, s_color, tex_coords).rgb;
        }
        return RgbaDepth(
            vec4<f32>(color * d, 1.0),
            1.0 - f32(depth) / f32(uniforms.max_depth)
        );
    }
}

