// Note: depth_common.wgsl is appended to this file at pipeline creation. Keep `Immediate` the first
// declaration, see stream_shader_source().
//
// Renders the area where the live passthrough is color keyed: the surroundings of the tracked hands.
// Hands move faster than the depth map can follow, everything else is handled by the depth mask.

struct Immediate {
    view_idx: u32,
}
var<immediate> imm: Immediate;

@group(0) @binding(0) var<uniform> depth_u: DepthUniforms;

struct VertexOutput {
    @builtin(position) position: vec4f,
    @location(0) uv: vec2f,
}

@vertex
fn vertex_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    // Fullscreen triangle, uv origin is top-left
    let xy = vec2f(f32((vertex_index << 1u) & 2u), f32(vertex_index & 2u));

    var result: VertexOutput;
    result.position = vec4f(xy * 2.0 - 1.0, 0.0, 1.0);
    result.uv = vec2f(xy.x, 1.0 - xy.y);
    return result;
}

@fragment
fn fragment_main(@location(0) uv: vec2f) -> @location(0) vec4f {
    let origin = depth_u.eyes[imm.view_idx].eye_position.xyz;
    let dir = normalize((depth_u.eyes[imm.view_idx].uv_to_world_dir * vec4f(uv, 1.0, 0.0)).xyz);
    let count = min(u32(depth_u.lighting2.w + 0.5), MAX_HAND_POINTS);

    var focus = 0.0;
    for (var i = 0u; i < count; i++) {
        let hand_point = depth_u.hand_points[i];
        let to_point = hand_point.xyz - origin;
        let t = max(dot(to_point, dir), 0.0);
        let distance = length(to_point - dir * t);
        focus = max(focus, 1.0 - smoothstep(hand_point.w * 0.8, hand_point.w * 1.2, distance));
    }

    return vec4f(focus, 0.0, 0.0, 1.0);
}
