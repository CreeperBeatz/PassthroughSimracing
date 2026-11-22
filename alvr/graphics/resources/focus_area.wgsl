// Note: depth_common.wgsl is appended to this file at pipeline creation. Keep `Immediate` the first
// declaration, see stream_shader_source().
//
// Renders the confidence zones of the tracked hands, made of tapered capsules. Hands move faster than
// the depth map can follow, everything else is handled by the depth mask. Hand tracking is only an
// estimate, so the solid inside fades out gradually into the color keyed rim.
//
// Output: r: the hand with its rim (the rim is color keyed), g: solid passthrough

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

const OUTSIDE: f32 = 1000.0;
// Soft edge of the rim
const SOFTNESS: f32 = 0.003;

// Distance between a ray and the surface of a tapered capsule, negative inside
fn ray_capsule_distance(origin: vec3f, dir: vec3f, start: vec4f, end: vec4f) -> f32 {
    let axis = end.xyz - start.xyz;
    let w = origin - start.xyz;
    let b = dot(dir, axis);
    let c = dot(axis, axis);
    let d = dot(dir, w);
    let e = dot(axis, w);

    // Closest point on the segment. Degenerates to the start when parallel or zero length
    let denominator = c - b * b;
    var s = 0.0;
    if denominator > 1e-9 {
        s = clamp((e - b * d) / denominator, 0.0, 1.0);
    }
    let point = start.xyz + axis * s;
    let t = max(dot(point - origin, dir), 0.0);

    return length(origin + dir * t - point) - mix(start.w, end.w, s);
}

@fragment
fn fragment_main(@location(0) uv: vec2f) -> @location(0) vec4f {
    let origin = depth_u.eyes[imm.view_idx].eye_position.xyz;
    let dir = normalize((depth_u.eyes[imm.view_idx].uv_to_world_dir * vec4f(uv, 1.0, 0.0)).xyz);

    var outer = 0.0;
    var inner = 0.0;
    for (var hand = 0u; hand < 2u; hand++) {
        let bounds = depth_u.hand_bounds[hand];
        let zones = depth_u.hand_zones[hand];
        if bounds.w <= 0.0 {
            continue;
        }
        let to_center = bounds.xyz - origin;
        let t = max(dot(to_center, dir), 0.0);
        if length(to_center - dir * t) > bounds.w + zones.x + SOFTNESS {
            continue;
        }

        var distance = OUTSIDE;
        for (var i = 0u; i < HAND_SEGMENTS; i++) {
            let base = (hand * HAND_SEGMENTS + i) * 2u;
            let start = depth_u.hand_points[base];
            let end = depth_u.hand_points[base + 1u];
            let segment_distance = ray_capsule_distance(origin, dir, start, end);
            distance = min(distance, segment_distance);

            let segment = hand * HAND_SEGMENTS + i;
            let solid = depth_u.hand_solid[segment / 4u][segment % 4u];
            if solid > 0.0 {
                let fade = smoothstep(zones.y, zones.y + max(zones.z, 0.0005), segment_distance);
                inner = max(inner, solid * (1.0 - fade));
            }
        }

        // The further from the hand, the less likely that it is there: fade out, so that leaks of the
        // color key are weaker. Full strength up to half of the rim.
        let outer_distance = max(zones.x, 0.0) + SOFTNESS;
        outer = max(outer, 1.0 - smoothstep(0.5 * outer_distance, outer_distance, distance));
    }

    return vec4f(outer, inner, 0.0, 1.0);
}
