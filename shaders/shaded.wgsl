// Uniform buffer containing the transform matrix
struct Uniforms {
    transform: mat4x4<f32>,
    max_depth: u32,
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

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) tex_coords: vec2<f32>,
}

// Vertex shader to render a full-screen quad
@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    // Create a "full screen quad" from just the vertex_index
    // Maps vertex indices (0,1,2,3) to positions:
    // (-1,1)----(1,1)
    //   |         |
    // (-1,-1)---(1,-1)
    var pos = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(1.0, -1.0),
        vec2<f32>(-1.0, 1.0),
        vec2<f32>(-1.0, 1.0),
        vec2<f32>(1.0, -1.0),
        vec2<f32>(1.0, 1.0),
    );

    // UV coordinates for the quad
    var uv = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 1.0),
        vec2<f32>(0.0, 0.0),
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 1.0),
        vec2<f32>(1.0, 0.0),
    );

    var output: VertexOutput;
    output.position = uniforms.transform * vec4<f32>(pos[vertex_index], 0.0, 1.0);
    output.position.z = 0.0; // XXX this is a hack
    output.tex_coords = uv[vertex_index];
    return output;
}

struct RgbaDepth {
  @location(0) color: vec4<f32>,
  @builtin(frag_depth) depth: f32
}

// Fragment shader
@fragment
fn fs_main(
    @location(0) tex_coords: vec2<f32>,
    @builtin(position) position: vec4<f32>
) -> RgbaDepth {
    var ssao = textureSample(t_ssao, s_ssao, tex_coords);
    var pixel = textureSample(t_pixel, s_pixel, tex_coords);
    let color = textureSample(t_color, s_color, tex_coords).rgb;
    var depth = bitcast<u32>(pixel.r);

    // If depth is 0, this pixel is transparent
    var out = RgbaDepth(vec4(1.0, 1.0, 1.0, 1.0), 0.0);
    if (depth == 0u) {
        discard;
    } else if (depth < uniforms.max_depth) {
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
        let c = vec3<f32>(accum * color);
        out = RgbaDepth(
            vec4<f32>(c, 1.0),
            1.0 - f32(depth) / f32(uniforms.max_depth)
        );
    }
    return out;
}
