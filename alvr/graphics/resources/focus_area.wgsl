// Note: depth_common.wgsl is appended to this file at pipeline creation. Keep `Immediate` the first
// declaration, see stream_shader_source().
//
// Renders the confidence zones of the tracked hands, made of tapered capsules. Hands move faster than
// the depth map can follow, everything else is handled by the depth mask. Hand tracking is only an
// estimate, so the solid inside fades out gradually into the color keyed rim.
//
// Output: r: the hand with its rim (the rim is color keyed), g: solid passthrough, b: near the hand, where
// the runtime may remove objects from the depth map, a: debug view, how much the rim is narrowed

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
// Range of the remembered playzone around the hands. See also hands_view_rect()
const PLAYZONE_RANGE: f32 = 0.06;

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

// Same, for the capsule moved anywhere from -sweep.x to +sweep.y along the unit vector `motion` (a
// moving hand, while its pose may be off). `across`: xyz: the part of `motion` across the ray, w: 1 / its
// squared length (0 if it is almost along the ray). Seen along the ray, the ray is a point and the moved
// axis covers a parallelogram. Its closest point is found like the closest points of two segments, the
// axis and the motion (Ericson, Real-Time Collision Detection, 5.1.9): exact for a capsule of constant
// radius in front of the eye. Like above, a tapered capsule takes the radius at that point.
fn ray_swept_capsule_distance(
    origin: vec3f,
    dir: vec3f,
    start: vec4f,
    end: vec4f,
    motion: vec3f,
    across: vec4f,
    sweep: vec2f,
) -> f32 {
    let axis = end.xyz - start.xyz;
    let w = origin - start.xyz;
    let b = dot(dir, axis);
    let d = dot(dir, w);
    // Across the ray: axis . axis, axis . w, axis . motion, motion . w
    let aa = dot(axis, axis) - b * b;
    let aw = dot(axis, w) - b * d;
    let am = dot(axis, across.xyz);
    let mw = dot(w, across.xyz);

    var inv_aa = 0.0;
    if aa > 1e-9 {
        inv_aa = 1.0 / aa;
    }
    // The point of the axis where it crosses the line of the motion (the closest point to the ray if
    // they are parallel), then the shift for it, then the point of the axis for that shift
    let det = aa - am * am * across.w;
    var s = aw * inv_aa;
    if det > 1e-7 * aa {
        s = (aw - am * mw * across.w) / det;
    }
    s = clamp(s, 0.0, 1.0);
    let shift = clamp((mw - s * am) * across.w, -sweep.x, sweep.y);
    s = clamp((aw - shift * am) * inv_aa, 0.0, 1.0);

    let point = start.xyz + axis * s + motion * shift;
    let t = max(dot(point - origin, dir), 0.0);

    return length(origin + dir * t - point) - mix(start.w, end.w, s);
}

@fragment
fn fragment_main(@location(0) uv: vec2f) -> @location(0) vec4f {
    let origin = depth_u.eyes[imm.view_idx].eye_position.xyz;
    let dir = normalize((depth_u.eyes[imm.view_idx].uv_to_world_dir * vec4f(uv, 1.0, 0.0)).xyz);

    var outer = 0.0;
    var inner = 0.0;
    var near = 0.0;
    var tint = 0.0;
    for (var hand = 0u; hand < 2u; hand++) {
        let bounds = depth_u.hand_bounds[hand];
        let zones = depth_u.hand_zones[hand];
        if bounds.w <= 0.0 {
            continue;
        }
        let to_center = bounds.xyz - origin;
        let t = max(dot(to_center, dir), 0.0);
        let center_distance = length(to_center - dir * t);
        // A moving hand: the rim covers the capsules moved along the motion, the solid fill doesn't
        let motion = depth_u.hand_sweep[hand];
        let sweep = vec2f(zones.w, motion.w);
        let sweeps = sweep.x + sweep.y > 0.0;
        let across = motion.xyz - dir * dot(motion.xyz, dir);
        let across_length2 = dot(across, across);
        let across_info = vec4f(across, select(0.0, 1.0 / across_length2, across_length2 > 1e-8));
        let is_near_hand =
            center_distance < bounds.w + max(zones.x, 0.0) + max(sweep.x, sweep.y) + SOFTNESS;

        var distance = OUTSIDE;
        // Skip the hand if it is far away
        let last = select(0u, HAND_SEGMENTS, is_near_hand);
        for (var i = 0u; i < last; i++) {
            let base = (hand * HAND_SEGMENTS + i) * 2u;
            let start = depth_u.hand_points[base];
            let end = depth_u.hand_points[base + 1u];
            let segment = hand * HAND_SEGMENTS + i;
            let solid = depth_u.hand_solid[segment / 4u][segment % 4u];

            var segment_distance = 0.0;
            if sweeps {
                distance = min(
                    distance,
                    ray_swept_capsule_distance(origin, dir, start, end, motion.xyz, across_info, sweep),
                );
                if solid > 0.0 {
                    segment_distance = ray_capsule_distance(origin, dir, start, end);
                }
            } else {
                segment_distance = ray_capsule_distance(origin, dir, start, end);
                distance = min(distance, segment_distance);
            }

            if solid > 0.0 {
                let fade = smoothstep(zones.y, zones.y + max(zones.z, 0.0005), segment_distance);
                inner = max(inner, solid * (1.0 - fade));
            }
        }

        // The bounding sphere is good enough here
        near = max(
            near,
            1.0 - smoothstep(bounds.w + 0.5 * PLAYZONE_RANGE, bounds.w + PLAYZONE_RANGE, center_distance),
        );

        // The further from the hand, the less likely that it is there: fade out, so that leaks of the
        // color key are weaker. Full strength up to half of the rim. A negative rim ends inside the
        // hand, with the soft edge only.
        let rim_end = zones.x + SOFTNESS;
        let rim_start = min(0.5 * rim_end, rim_end - 0.5 * SOFTNESS);
        let hand_outer = 1.0 - smoothstep(rim_start, rim_end, distance);
        outer = max(outer, hand_outer);
        tint = max(tint, depth_u.playzone[1u + hand] * hand_outer);
    }

    return vec4f(outer, inner, near, tint);
}
