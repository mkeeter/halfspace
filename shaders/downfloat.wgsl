// Converts from f32 -> f16; canonicalizing NaN values along the way
//
// We have to canonicalize NaN values because otherwise we do
// - f32 -> f16 (here)
// - f16 -> f32 (in texture sampling)
// ...and our sneaky NaN encoding scheme doesn't persist.  However, we only have
// NaN-boxed values when doing bitfield rendering, where we don't care about
// value, so we can canonicalize to -1.0 / 1.0 instead.
@group(0) @binding(0) var<storage, read> f32_values: array<vec2<f32>>;
@group(0) @binding(1) var<storage, read_write> f16_values: array<u32>;

@compute @workgroup_size(64, 1, 1)
fn float_main(@builtin(global_invocation_id) global_id: vec3u) {
    let i = global_id.x;
    if i < arrayLength(&f32_values) && i < arrayLength(&f16_values) {
        let v = f32_values[i];
        let a = process(v.x);
        let b = process(v.y);
        f16_values[i] = pack2x16float(vec2f(a, b));
    }
}

fn process(v: f32) -> f32 {
    if distance_pixel_is_fill(bitcast<u32>(v)) {
        if (bitcast<u32>(v) & 1u) != 0 {
            return -1.0;
        } else {
            return 1.0;
        }
    } else {
        return v;
    }
}


////////////////////////////////////////////////////////////////////////////////
// Copied from Fidget; TODO make this public somehow?

// NAN with characteristic bit pattern in mantissa
const DISTANCE_PIXEL_KEY: u32 = 0x7fc1ec00;

fn distance_pixel_is_fill(d: u32) -> bool {
    return (d & DISTANCE_PIXEL_KEY) == DISTANCE_PIXEL_KEY;
}
