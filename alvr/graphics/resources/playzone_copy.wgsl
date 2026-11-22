// Note: depth_common.wgsl is appended to this file at pipeline creation. Keep `Immediate` the first
// declaration, see stream_shader_source().
//
// Remembers the depth map without hands: the runtime removes the tracked hands from the depth map, and
// with them parts of the objects that they hold. Output: distance from the depth camera plane in meters,
// 0: no data.

struct Immediate {
    view_idx: u32,
}
var<immediate> imm: Immediate;

@group(0) @binding(0) var<uniform> depth_u: DepthUniforms;
@group(0) @binding(1) var depth_texture: texture_2d_array<f32>;

@vertex
fn vertex_main(@builtin(vertex_index) vertex_index: u32) -> @builtin(position) vec4f {
    // Fullscreen triangle
    let xy = vec2f(f32((vertex_index << 1u) & 2u), f32(vertex_index & 2u));
    return vec4f(xy * 2.0 - 1.0, 0.0, 1.0);
}

@fragment
fn fragment_main(@builtin(position) frag_coord: vec4f) -> @location(0) vec4f {
    let depth = textureLoad(depth_texture, vec2i(frag_coord.xy), imm.view_idx, 0).r;
    if depth <= 0.0 || depth >= 0.9999 {
        return vec4f(0.0);
    }

    // Same as linearize_depth in depth_mask.wgsl
    let near = depth_u.depth.x;
    let far = depth_u.depth.y;
    let ndc = depth * 2.0 - 1.0;
    if far <= near {
        return vec4f(2.0 * near / max(1.0 - ndc, 1e-6), 0.0, 0.0, 1.0);
    }
    return vec4f(2.0 * near * far / ((far + near) - ndc * (far - near)), 0.0, 0.0, 1.0);
}
