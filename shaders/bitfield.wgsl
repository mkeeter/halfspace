// Uniform buffer containing the transform matrix
struct Uniforms {
    transform: mat4x4<f32>,
    has_color: u32,
};

@group(0) @binding(0) var t_distance: texture_2d<f32>;
@group(0) @binding(1) var s_distance: sampler;
@group(0) @binding(2) var t_color: texture_2d<f32>;
@group(0) @binding(3) var s_color: sampler;
@group(0) @binding(4) var<uniform> uniforms: Uniforms;

// Fragment shader
@fragment
fn fs_main(@location(0) tex_coords: vec2<f32>) -> @location(0) vec4<f32> {
    // Float32 distance value, interpolated by the texture sampler
    var dist = textureSample(t_distance, s_distance, tex_coords).r;
    if (dist > 0.0) {
        discard;
    } else if (uniforms.has_color != 0) {
        return textureSample(t_color, s_color, tex_coords);
    } else {
        return vec4<f32>(1.0);
    }
}
