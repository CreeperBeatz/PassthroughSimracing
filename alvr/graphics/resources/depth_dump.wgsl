// Note: depth_common.wgsl is appended to this file at pipeline creation. Keep `Immediate` the first
// declaration, see stream_shader_source().
//
// For `debug.alvr.key_dump`: the environment depth and the remembered playzone of one eye, texel for
// texel, in a texture that GLES can read back (RGBA8; depth textures and float textures can't be read
// with glReadPixels on all devices). Output: r, g: the depth value as 16 bits (r: low byte), b, a: the
// remembered distance as the bits of a 16 bit float (b: low byte, 0: no data or no playzone).

struct Immediate {
    view_idx: u32,
}
var<immediate> imm: Immediate;

@group(0) @binding(0) var<uniform> depth_u: DepthUniforms;
@group(0) @binding(1) var depth_texture: texture_2d_array<f32>;
@group(1) @binding(0) var playzone_texture: texture_2d<f32>;

@vertex
fn vertex_main(@builtin(vertex_index) vertex_index: u32) -> @builtin(position) vec4f {
    // Fullscreen triangle
    let xy = vec2f(f32((vertex_index << 1u) & 2u), f32(vertex_index & 2u));
    return vec4f(xy * 2.0 - 1.0, 0.0, 1.0);
}

// Two bytes for a normalized target. A quarter step above the byte, so that both rounding and
// truncation give it back
fn bytes(value: u32) -> vec2f {
    return (vec2f(f32(value & 255u), f32((value >> 8u) & 255u)) + 0.25) / 255.0;
}

@fragment
fn fragment_main(@builtin(position) frag_coord: vec4f) -> @location(0) vec4f {
    let coord = vec2i(frag_coord.xy);
    // Unsigned 16 bit normalized on Quest: exact
    let depth = textureLoad(depth_texture, coord, imm.view_idx, 0).r;
    let raw = u32(round(clamp(depth, 0.0, 1.0) * 65535.0));

    var remembered = 0u;
    if depth_u.playzone.x > 0.5 && all(coord < vec2i(textureDimensions(playzone_texture))) {
        let distance = textureLoad(playzone_texture, coord, 0).r;
        remembered = pack2x16float(vec2f(distance, 0.0)) & 0xffffu;
    }

    return vec4f(bytes(raw), bytes(remembered));
}
