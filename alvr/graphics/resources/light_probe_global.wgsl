// Averages the local light probe into a single texel (global exposure of the game image)

@group(0) @binding(0) var light_probe_local: texture_2d<f32>;

@vertex
fn vertex_main(@builtin(vertex_index) vertex_index: u32) -> @builtin(position) vec4f {
    // Fullscreen triangle
    let xy = vec2f(f32((vertex_index << 1u) & 2u), f32(vertex_index & 2u));
    return vec4f(xy * 2.0 - 1.0, 0.0, 1.0);
}

@fragment
fn fragment_main() -> @location(0) vec4f {
    let dims = vec2i(textureDimensions(light_probe_local));

    var sum = vec3f(0.0);
    for (var y = 0; y < dims.y; y++) {
        for (var x = 0; x < dims.x; x++) {
            sum += textureLoad(light_probe_local, vec2i(x, y), 0).rgb;
        }
    }

    return vec4f(sum / f32(dims.x * dims.y), 1.0);
}
