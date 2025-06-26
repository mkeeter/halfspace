// Uniform buffer containing the transform matrix
struct Uniforms {
    transform: mat4x4<f32>,
    color: vec4<f32>,
};

@group(0) @binding(0) var t_diffuse: texture_2d<f32>;
@group(0) @binding(1) var s_diffuse: sampler;
@group(0) @binding(2) var<uniform> uniforms: Uniforms;

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


fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = clamp((x - edge0) / (edge1 - edge0), 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
}

fn mix(x: f32, y: f32, a: f32) -> f32 {
    return x * (1.0 - a) + y * a;
}

fn run(v: f32, f_abs: f32, dim: f32, bands: f32) -> f32 {
    var v_mod = v * dim * bands;
    v_mod = mix(v_mod, 1.0, 1.0 - smoothstep(0.0, 0.015, f_abs));
    v_mod = mix(v_mod, 1.0, 1.0 - smoothstep(0.0, 0.005, f_abs));
    return clamp(v_mod, 0.0, 1.0);
}

fn color(f: f32) -> vec4<f32> {
    if f != f {
        return vec4<f32>(1.0, 0.0, 0.0, 1.0); // red for NaN
    }

    let s = sign(f);
    let r = 1.0 - 0.1 * s;
    let g = 1.0 - 0.4 * s;
    let b = 1.0 - 0.7 * s;

    let dim = 1.0 - exp(-4.0 * abs(f));
    let bands = 0.8 + 0.2 * cos(140.0 * f);

    let f_abs = abs(f);
    return vec4<f32>(
        run(r, f_abs, dim, bands),
        run(g, f_abs, dim, bands),
        run(b, f_abs, dim, bands),
        1.0 // alpha
    );
}

// Fragment shader
@fragment
fn fs_main(@location(0) tex_coords: vec2<f32>) -> @location(0) vec4<f32> {
    var rgba = textureSample(t_diffuse, s_diffuse, tex_coords);
    return color(rgba.r);
}

