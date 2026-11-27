// Uniform buffer containing the transform matrix
struct Uniforms {
    // Combined transform matrix for canvas + image
    transform: mat4x4<f32>,
    // Transform matrix
    mat: mat4x4<f32>,
    has_color: u32,
    axis_scale: f32,
    image_size: vec2<u32>,
};

@group(0) @binding(0) var t_distance: texture_2d<f32>;
@group(0) @binding(1) var s_distance: sampler;
@group(0) @binding(2) var t_color: texture_2d<f32>;
@group(0) @binding(3) var s_color: sampler;
@group(0) @binding(4) var<uniform> uniforms: Uniforms;

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
    if vertex_index < 6 {
        output.position = uniforms.transform * vec4<f32>(pos[vertex_index % 6], 0.0, 1.0);
        output.position.z = 0.0; // XXX this is a hack
    } else {
        // Use a non-zero Z position to mark axis overlap quad
        output.position = vec4<f32>(pos[vertex_index % 6], 0.5, 1.0);
    }
    output.tex_coords = uv[vertex_index % 6];
    return output;
}


// Fragment shader
@fragment
fn fs_main(
    @builtin(position) position: vec4<f32>,
    @location(0) tex_coords: vec2<f32>
) -> @location(0) vec4<f32> {
    // Float32 distance value, interpolated by the texture sampler
    var dist = textureSample(t_distance, s_distance, tex_coords).r;
    var color: vec4<f32>;
    if uniforms.has_color != 0 {
        color = textureSample(t_color, s_color, tex_coords);
    } else {
        color = vec4<f32>(1.0);
    }

    if position.z == 0.0 {
        if dist > 0.0 {
            discard;
        } else {
            return color;
        }
    }

    if dist > 0.0 {
        color = vec4<f32>(0.0);
    }

    if uniforms.axis_scale > 0.0 {
        let p = vec4<f32>(
            (tex_coords.x - 0.5) *  2.0,
            (tex_coords.y - 0.5) * -2.0, 0.0, 1.0);
        let q = uniforms.mat * p;
        let d = q.xy / q.w;
        let r = sqrt(d.x * d.x + d.y * d.y);

        let scale = min(uniforms.mat[0][0], uniforms.mat[1][1]);
        let pix = f32(min(uniforms.image_size.x, uniforms.image_size.y));

        var valid = false;

        // X axis
        let line_thickness = 10.0 / pix * scale;
        const RED = vec4f(0.95, 0.4, 0.4, 1.0);
        const GREEN = vec4f(0.4, 0.8, 0.4, 1.0);
        if d.x >= 0.0 && d.x <= uniforms.axis_scale && abs(d.y) < line_thickness {
            color = RED;
            valid = true;
        } else if d.x >= 0.0 && d.x / scale <= 0.5 && abs(d.y) < line_thickness {
            color = (RED + 3.0 * color) / 4.0;
            valid = true;
        }

        // Y axis
        if d.y >= 0.0 && d.y <= uniforms.axis_scale && abs(d.x) < line_thickness {
            color = GREEN;
            valid = true;
        } else if d.y >= 0.0 && d.y / scale <= 0.5 && abs(d.x) < line_thickness {
            color = (GREEN + 3.0 * color) / 4.0;
            valid = true;
        }

        // Central white dot, above the axes with a blended outline
        let inner = 20.0 / pix * scale;
        let outer = 30.0 / pix * scale;
        if r < inner {
            color = vec4f(1.0);
            valid = true;
        } else if r < outer {
            let frac = sqrt((r - inner) / (outer - inner));
            color = vec4(0.12, 0.12, 0.12, 1.0) * (1.0 - frac) + color * frac;
            valid = true;
        }

        if valid {
            return color;
        }
    }

    discard;
}
