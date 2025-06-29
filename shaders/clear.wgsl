// Fragment shader
@fragment
fn fs_main(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let x = ((position.x / 40.0) % 1.0) > 0.5;
    let y = ((position.y / 40.0) % 1.0) > 0.5;
    if (x != y) {
        return vec4(0.1, 0.1, 0.1, 1.0);
    } else {
        return vec4(0.2, 0.2, 0.2, 1.0);
    }
}

