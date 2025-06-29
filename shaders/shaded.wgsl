// Uniform buffer containing the transform matrix
struct Uniforms {
    transform: mat4x4<f32>,
    max_depth: u32,
    has_color: u32,
}

struct Light {
    position: vec3<f32>,
    intensity: f32,
}

@group(0) @binding(0) var t_ssao: texture_2d<f32>;
@group(0) @binding(1) var s_ssao: sampler;
@group(0) @binding(2) var t_pixel: texture_2d<f32>;
@group(0) @binding(3) var s_pixel: sampler;
@group(0) @binding(4) var t_color: texture_2d<f32>;
@group(0) @binding(5) var s_color: sampler;
@group(0) @binding(6) var<uniform> uniforms: Uniforms;

struct RgbaDepth {
  @location(0) color: vec4<f32>,
  @builtin(frag_depth) depth: f32
}

// Fragment shader
@fragment
fn fs_main(@location(0) tex_coords: vec2<f32>) -> RgbaDepth {
    var ssao = textureSample(t_ssao, s_ssao, tex_coords);
    var pixel = textureSample(t_pixel, s_pixel, tex_coords);
    var depth = bitcast<u32>(pixel.r);

    // If depth is 0, this pixel is transparent
    if (depth == 0u) {
        discard;
    }

    // Pixel position (for lighting calculations)
    let p = vec3<f32>(
        (tex_coords.xy - 0.5) * 2.0,
        2.0 * (f32(depth) / f32(uniforms.max_depth) - 0.5)
    );

    let normal = vec3<f32>(pixel.yzw);
    let n = normalize(normal);
    const LIGHTS = array<Light, 3>(
        Light(vec3<f32>(5.0, -5.0, 10.0), 0.5),
        Light(vec3<f32>(-5.0, 0.0, 10.0), 0.15),
        Light(vec3<f32>(0.0, -5.0, 10.0), 0.15)
    );
    var accum: f32 = 0.2;
    for (var i = 0u; i < 3u; i = i + 1u) {
        let light = LIGHTS[i];
        let light_dir = normalize(light.position - p);
        accum = accum + max(dot(light_dir, n), 0.0) * light.intensity;
    }
    accum = clamp(accum * (ssao.r * 0.6 + 0.4), 0.0, 1.0);
    var color = vec3<f32>(1.0);
    if (uniforms.has_color != 0) {
        color = textureSample(t_color, s_color, tex_coords).rgb;
    }
    let c = vec3<f32>(accum * color);
    return RgbaDepth(
        vec4<f32>(c, 1.0),
        1.0 - f32(depth) / f32(uniforms.max_depth)
    );
}
