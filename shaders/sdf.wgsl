// Uniform buffer containing the transform matrix
struct Uniforms {
    transform: mat4x4<f32>,
    min_distance: f32,
    max_distance: f32,
    has_color: u32,
    any_color: u32,
};

@group(0) @binding(0) var t_distance: texture_2d<f32>;
@group(0) @binding(1) var s_distance: sampler;
@group(0) @binding(2) var t_color: texture_2d<f32>;
@group(0) @binding(3) var s_color: sampler;
@group(0) @binding(4) var<uniform> uniforms: Uniforms;

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

fn color_orange_to_blue(f: f32) -> vec4<f32> {
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

fn color_stripe(f: f32) -> f32 {
    if f != f {
        return 1.0;
    }

    let s = sign(f);

    let dim = 1.0 - exp(-4.0 * abs(f));
    let bands = 0.8 + 0.2 * cos(140.0 * f);
    let f_abs = abs(f);
    var base: f32;
    if (f < 0.0) {
        base = 1.0;
    } else {
        base = 0.2;
    }

    return run(base, f_abs, dim, bands);
}

struct RgbaDepth {
  @location(0) color: vec4<f32>,
  @builtin(frag_depth) depth: f32
}

// Fragment shader
@fragment
fn fs_main(@location(0) tex_coords: vec2<f32>) -> RgbaDepth {
    var d = textureSample(t_distance, s_distance, tex_coords).r;

    var depth: f32;
    if (d < 0.0) {
        depth = 0.5;
    } else {
        depth = 0.5 + (d - uniforms.min_distance) /
            (uniforms.max_distance - uniforms.min_distance)
            / 2.0;
    }
    var color: vec4<f32>;
    if (uniforms.any_color != 0) {
        let stripes = color_stripe(d);
        if (uniforms.has_color != 0) {
            var rgb = textureSample(t_color, s_color, tex_coords).rgb;
            color = vec4<f32>(rgb * stripes, 1.0);
        } else {
            color = vec4<f32>(stripes * vec3<f32>(1.0), 1.0);
        }
    } else {
        color = color_orange_to_blue(d);
    }
    return RgbaDepth(color, depth);
}

