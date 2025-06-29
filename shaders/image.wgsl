// Uniform buffer containing the transform matrix
struct Uniforms {
    transform: mat4x4<f32>,
};

@group(0) @binding(0) var t_diffuse: texture_2d<f32>;
@group(0) @binding(1) var s_diffuse: sampler;
@group(0) @binding(2) var<uniform> uniforms: Uniforms;

// Fragment shader
@fragment
fn fs_main(@location(0) tex_coords: vec2<f32>) -> @location(0) vec4<f32> {
    var rgba = textureSample(t_diffuse, s_diffuse, tex_coords);
    if (rgba.a == 0.0) {
        discard;
    } else {
        return rgba;
    }
}
