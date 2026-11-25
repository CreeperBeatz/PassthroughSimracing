// Note: depth_common.wgsl is appended to this file at pipeline creation. Keep `Immediate` the first
// declaration, see stream_shader_source().
//
// Computes the depth passthrough mask at low resolution, in view-space uv. The depth map is much
// coarser than the display, so running this for every display pixel would be wasted work. stream.wgsl
// upscales the result with a linear filter.
//
// For each texel, find the real-world surface seen along the view ray using the headset depth map,
// then decide whether that surface belongs to the "keep real" region (close to the eyes, or inside
// the boxes of the play area or the cockpit box of the settings).
//
// Output: r: region coverage (0..1), g: proximity for the debug view (1: at the eye, 0: 3 m or
// more), or with the depth cloud (`debug.alvr.cloud`) its zone, where the colour key decides (0..1),
// b: whether there is depth data (0/1; read by the Show depth view only, which hides the cloud), or
// with the cloud's trail (`cloud_trail`, depth_u.cloud.z above 0) how much of the solid mask the
// stream hands to the key (0..1, see cloud_zone; less near the hands with `cloud_trail_hands`, none on
// what only the remembered playzone shows), a: how much of r comes from the remembered playzone
// (for the debug view)

struct Immediate {
    view_idx: u32,
}
var<immediate> imm: Immediate;

@group(0) @binding(0) var<uniform> depth_u: DepthUniforms;
// Environment depth (GL_DEPTH_COMPONENT16 on Quest), one layer per eye. Bound as unfilterable
// float instead of a depth texture: naga would otherwise emit a shadow sampler, which cannot be
// used with texelFetch in GLSL ES.
@group(0) @binding(1) var depth_texture: texture_2d_array<f32>;
// Remembered depth map without hands, in meters (0: no data). See playzone_copy.wgsl, or with
// `debug.alvr.mem_roll` the room memory of room_memory.wgsl
@group(1) @binding(0) var playzone_texture: texture_2d<f32>;
@group(1) @binding(1) var focus_area: texture_2d<f32>;
@group(1) @binding(2) var linear_sampler: sampler;
// The depth cloud (`debug.alvr.cloud`, motion_cloud.wgsl): the field of this eye and its cameras. A 1 x 1
// field with nothing in it and zeros while it is off; not read unless depth_u.cloud.x is set
@group(2) @binding(0) var cloud_field: texture_2d<f32>;
@group(2) @binding(1) var<uniform> cloud_u: CloudUniforms;

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
    if depth_u.depth.z < 0.5 {
        return vec4f(0.0);
    }

    var result = current_mask(uv);

    // The depth cloud replaces the proximity, which only the Show depth view reads (the cloud is off
    // with it), and with `cloud_trail` the depth data flag, read by that view only too. Off (the
    // default): nothing is read, the mask is today's
    if depth_u.cloud.x > 0.5 {
        let zone = cloud_zone(uv);
        result.g = zone.x;
        if depth_u.cloud.z > 0.0 {
            result.b = zone.y;
        }
    }

    // The strip the runtime removes behind each wrist. Only adds, and with both gaps off (the default)
    // nothing is done, so the mask is today's
    if (depth_u.arm_gap[0].w > 0.0 || depth_u.arm_gap[1].w > 0.0) && result.r < 1.0 {
        let eye = depth_u.eyes[imm.view_idx];
        let origin = eye.eye_position.xyz;
        let dir = normalize((eye.uv_to_world_dir * vec4f(uv, 1.0, 0.0)).xyz);
        let gap = max(gap_coverage(origin, dir, 0u), gap_coverage(origin, dir, 1u));
        result.r = max(result.r, gap);
    }

    // Near the hands, objects may be missing in the depth map
    var near_hand = 0.0;
    var near_hand_read = false;
    // How much of r only the remembered playzone shows (not paled by `mem_view 2`), for the trail
    var memory_only = 0.0;
    if depth_u.lighting2.z > 0.5 && depth_u.playzone.x > 0.5 && result.r < 1.0 {
        near_hand = textureSampleLevel(focus_area, linear_sampler, uv, 0.0).b;
        near_hand_read = true;
        if near_hand > 0.0 {
            let remembered = playzone_coverage(uv);
            let coverage = max(result.r, near_hand * remembered.x);
            memory_only = coverage - result.r;
            result.a = memory_only;
            // `mem_view 2`: the debug tint of what only the memory shows is paler the older it is
            if depth_u.memory.y > 1.5 && remembered.x > 0.0 {
                result.a *= remembered.y / remembered.x;
            }
            result.r = coverage;
        }
    }

    // `cloud_trail` near the hands: the runtime removes the hands and a few cm around them from the
    // depth, so real static surface there leaves the live depth and arrives again after the hand passed
    // (its arrival count restarts). (1) `cloud_trail_hands 1` (the default): the trail is scaled by 1 -
    // the near-hand value; 0: not (as in commit 9abdada4). (2) Always: the part of the coverage only the
    // remembered playzone shows is never handed to the key (it has no arrival count). Off (cloud_trail
    // 0): nothing is read, b stays the depth data flag
    if depth_u.cloud.x > 0.5 && depth_u.cloud.z > 0.0 {
        if cloud_trail_spares_hands() {
            if !near_hand_read {
                near_hand = textureSampleLevel(focus_area, linear_sampler, uv, 0.0).b;
            }
            result.b *= 1.0 - clamp(near_hand, 0.0, 1.0);
        }
        if memory_only > 0.0 {
            result.b *= 1.0 - clamp(memory_only / max(result.r, 1e-3), 0.0, 1.0);
        }
    }

    // The windows of the levers a hand is near (`debug.alvr.lever_boxes`, U2 of
    // design/universal/final.md). Only adds, and without a window (the default) nothing is done. The
    // part only the remembered playzone adds (a, for the debug tint) shrinks by what the window shows
    if depth_u.area.w > 0.0 && result.r < 1.0 {
        let eye = depth_u.eyes[imm.view_idx];
        let origin = eye.eye_position.xyz;
        let dir = normalize((eye.uv_to_world_dir * vec4f(uv, 1.0, 0.0)).xyz);
        let lever = lever_coverage(origin, dir);
        let coverage = max(result.r, lever);
        result.a = coverage - max(result.r - result.a, lever);
        result.r = coverage;
    }

    return result;
}

// The field stores speeds as a share of this (texels per second) and phi as a share of 2 x its window
// (cloud_u.reach.w, cells: texels), see motion_cloud.wgsl
const CLOUD_SPEED_SCALE: f32 = 510.0;
// `cloud_fixed`: the full reach from this speed on (texels per second)
const CLOUD_FIXED_SPEED: f32 = 30.0;
// The zone fades out over this much of its reach (texels)
const CLOUD_SOFT_EDGE: f32 = 1.5;
// The arrival counts of the field are stored as a share of this (depth images)
const CLOUD_OLD_COUNT: f32 = 255.0;

// The zone of the depth cloud along the view ray of uv (0..1, design/depthcloud/final.md B.3): the ray
// is looked up in the field of this eye at a fixed distance (independent of the depth lookup), and the
// zone reaches speed x reach time (at most the cap) from the nearest edge of the field, on both sides.
// With `cloud 2` the lookup point is moved back by the arm shift, as the depth is. Outside the field's
// camera, for an eye without a field (zero matrices) or with a field older than 300 ms (pair.w 0, the
// depth stalled; also with `cloud_fixed`), no zone. With `cloud_trail` (depth_u.cloud.z) the reach
// inside the mask is at most the trail. x: the zone. y: the trail, how much of the solid mask the stream
// hands to the key: the zone, inside the mask (phi < 0) only where the surface arrived within the last
// `cloud_trail_age` depth images (cloud_trail_age(), 0: no age test, the zone): a static surface beside
// an edge that only seems to move (depth noise, the hole the runtime cuts around a hand) stays solid
fn cloud_zone(uv: vec2f) -> vec2f {
    let view_idx = imm.view_idx;
    if cloud_u.eyes[view_idx].pair.w < 0.5 {
        return vec2f(0.0);
    }
    let eye = depth_u.eyes[view_idx];
    let origin = eye.eye_position.xyz;
    let dir = normalize((eye.uv_to_world_dir * vec4f(uv, 1.0, 0.0)).xyz);
    var point = origin + dir * cloud_u.params.x;
    if depth_u.cloud.x > 1.5 {
        point -= arm_displacement(point);
    }

    let camera = cloud_u.eyes[view_idx].field;
    let view_point = (camera.world_to_view * vec4f(point, 1.0)).xyz;
    if view_point.z > -0.001 {
        return vec2f(0.0);
    }
    let tan_xy = view_point.xy / -view_point.z;
    var tex_uv = (tan_xy - camera.tangents.xz) / (camera.tangents.yw - camera.tangents.xz);
    if any(tex_uv < vec2f(0.0)) || any(tex_uv > vec2f(1.0)) {
        return vec2f(0.0);
    }
    if depth_u.depth.w > 0.5 {
        tex_uv.y = 1.0 - tex_uv.y;
    }

    let field = textureSampleLevel(cloud_field, linear_sampler, tex_uv, 0.0);
    // The window the field was built with (a change of `cloud_window` starts the cloud over), as
    // window_cells() of motion_cloud.wgsl: 0 is 12 (24 texels), today's
    let window = round(cloud_u.reach.w);
    let truncation = 2.0 * select(clamp(window, 1.0, 24.0), 12.0, window <= 0.0);
    let phi = (2.0 * field.r - 1.0) * truncation;
    let speed = field.g * CLOUD_SPEED_SCALE;
    var reach = min(speed * cloud_u.reach[view_idx], cloud_u.params.z);
    if cloud_u.params.w > 0.0 {
        reach = cloud_u.params.w * clamp(speed / CLOUD_FIXED_SPEED, 0.0, 1.0);
    }
    // `cloud_trail`: inside the mask (phi < 0) the zone is at most this deep, as the stream hands it to
    // the colour key there. Off (0): the zone of both sides is today's
    if depth_u.cloud.z > 0.0 && phi < 0.0 {
        reach = min(reach, depth_u.cloud.z);
    }
    if reach <= 0.0 {
        return vec2f(0.0);
    }
    let zone = 1.0 - smoothstep(reach - CLOUD_SOFT_EDGE, reach, abs(phi));
    // `cloud_trail_age`: the arrival count of the field (blended across cells like the rest)
    var recent = 1.0;
    let age = cloud_trail_age();
    if age > 0.0 && phi < 0.0 {
        recent = 1.0 - smoothstep(age - 2.0, age, field.a * CLOUD_OLD_COUNT);
    }
    return vec2f(zone, zone * recent);
}

// `cloud_trail_age` from depth_u.cloud.w (negative with `cloud_trail_hands 0`: -1 - the age)
fn cloud_trail_age() -> f32 {
    let w = depth_u.cloud.w;
    return select(w, -1.0 - w, w < 0.0);
}

// `cloud_trail_hands`: whether the trail is scaled down near the hands (depth_u.cloud.w not negative)
fn cloud_trail_spares_hands() -> bool {
    return depth_u.cloud.w >= 0.0;
}

// How much the lever windows show along a view ray: the most open of those it crosses in front of the
// eye (or starts inside), whatever the depth says. Slab test in the frame of each box. A component of
// the direction near 0 is replaced by a tiny positive one (no division by 0): inside that pair of sides
// the ray then crosses it at every distance, outside of them never near. Same as
// `LeverWindow::ray_coverage` in depth_mask.rs
fn lever_coverage(origin: vec3f, dir: vec3f) -> f32 {
    var coverage = 0.0;
    let count = min(u32(depth_u.area.w), LEVER_WINDOWS);
    for (var i = 0u; i < count; i++) {
        let window = depth_u.lever_windows[i];
        let o = (window.world_to_window * vec4f(origin, 1.0)).xyz;
        let d = (window.world_to_window * vec4f(dir, 0.0)).xyz;
        let tiny = vec3f(1e-6);
        let safe = select(d, tiny, abs(d) < tiny);
        let half = window.half.xyz;
        let t1 = (-half - o) / safe;
        let t2 = (half - o) / safe;
        let t_min = min(t1, t2);
        let t_max = max(t1, t2);
        let near = max(max(t_min.x, t_min.y), t_min.z);
        let far = min(min(t_max.x, t_max.y), t_max.z);
        if far >= max(near, 0.0) {
            coverage = max(coverage, window.half.w);
        }
    }
    return coverage;
}

// The tint of `mem_view 2` falls from full to this over MEMORY_VIEW_AGE seconds
const MEMORY_VIEW_FADED: f32 = 0.3;
const MEMORY_VIEW_AGE: f32 = 5.0;

// Coverage of the region in the remembered playzone. x: the coverage, y: the same with each texel weighted
// by its age for `mem_view 2` (equal to x otherwise). With the room memory (`debug.alvr.mem_roll`,
// depth_u.memory.x above 0) only texels it kept near a hand count (b, frozen): the others equal the live
// depth, so nothing is lost, and the arm (written outside the margin) is never drawn twice. With
// `mem_moving 1` (2) not those marked moving either (g)
fn playzone_coverage(uv: vec2f) -> vec2f {
    let view_idx = imm.view_idx;
    let origin = depth_u.eyes[view_idx].eye_position.xyz;
    let dir = normalize((depth_u.eyes[view_idx].uv_to_world_dir * vec4f(uv, 1.0, 0.0)).xyz);

    var t = 0.7;
    for (var i = 0u; i < 2u; i++) {
        let guess = project_to_playzone(origin + dir * t, view_idx);
        if !guess.valid {
            return vec2f(0.0);
        }
        let z = load_playzone(vec2i(floor(guess.texel)));
        if z <= 0.0 {
            return vec2f(0.0);
        }
        t = max(dot(playzone_to_world(guess, z, view_idx) - origin, dir), 0.05);
    }

    let lookup = project_to_playzone(origin + dir * t, view_idx);
    if !lookup.valid {
        return vec2f(0.0);
    }

    let read = depth_u.memory.x;
    let ages = depth_u.memory.y > 1.5;
    // 2x2 neighborhood, blend the results
    let corner = floor(lookup.texel - 0.5);
    let f = lookup.texel - 0.5 - corner;
    var coverage = 0.0;
    var faded = 0.0;
    for (var y = 0; y < 2; y++) {
        for (var x = 0; x < 2; x++) {
            let texel = load_playzone_texel(vec2i(corner) + vec2i(x, y));
            let z = texel.r;
            let counts = read < 0.5
                || (texel.b > 0.5 && (read < 1.5 || texel.g <= 0.0));
            if z > 0.0 && counts {
                let weight = mix(1.0 - f.x, f.x, f32(x)) * mix(1.0 - f.y, f.y, f32(y));
                let part = weight * region_mask(playzone_to_world(lookup, z, view_idx), view_idx);
                coverage += part;
                if ages {
                    faded += part
                        * mix(1.0, MEMORY_VIEW_FADED, clamp(texel.a / MEMORY_VIEW_AGE, 0.0, 1.0));
                }
            }
        }
    }
    if !ages {
        faded = coverage;
    }
    return vec2f(coverage, faded);
}

fn current_mask(uv: vec2f) -> vec4f {

    let view_idx = imm.view_idx;
    let eye = depth_u.eyes[view_idx];
    let origin = eye.eye_position.xyz;
    let dir = normalize((eye.uv_to_world_dir * vec4f(uv, 1.0, 0.0)).xyz);

    // The depth camera is close to (but not exactly at) the eye. Project a guess point along the
    // ray into the depth map, read the real surface there, and refine the guess distance.
    var t = initial_distance(origin, dir);
    for (var i = 0u; i < 2u; i++) {
        let point = origin + dir * t;
        let displacement = arm_displacement(point);
        let guess = project_to_depth(point - displacement, view_idx);
        if !guess.valid {
            return vec4f(0.0);
        }
        let depth = load_depth(vec2i(floor(guess.texel)), view_idx);
        if depth >= MAX_DEPTH {
            return vec4f(0.0);
        }
        let hit = lookup_to_world(guess, linearize_depth(depth), view_idx) + displacement;
        t = max(dot(hit - origin, dir), 0.05);
    }

    let displacement = arm_displacement(origin + dir * t);
    let lookup = project_to_depth(origin + dir * t - displacement, view_idx);
    if !lookup.valid {
        return vec4f(0.0);
    }

    // Classify the 3x3 neighborhood and blend the results (not the depths) with quadratic B-spline
    // weights. This hides the depth texel grid, without inventing surfaces between hand and wall.
    let center = floor(lookup.texel);
    let d = lookup.texel - center - 0.5;
    var weights: array<vec2f, 3>;
    weights[0] = 0.5 * (0.5 - d) * (0.5 - d);
    weights[1] = 0.75 - d * d;
    weights[2] = 0.5 * (0.5 + d) * (0.5 + d);

    // The 9 depths (0: no depth). All 9 points lie on the ray of `lookup`, each at its own depth
    var depths: array<f32, 9>;
    var z_min = 1e9;
    var z_max = 0.0;
    var weight_with_depth = 0.0;
    for (var y = 0; y < 3; y++) {
        for (var x = 0; x < 3; x++) {
            let depth = load_depth(vec2i(center) + vec2i(x - 1, y - 1), view_idx);
            var z = 0.0;
            if depth < MAX_DEPTH {
                z = linearize_depth(depth);
                z_min = min(z_min, z);
                z_max = max(z_max, z);
                weight_with_depth += weights[x].x * weights[y].y;
            }
            depths[y * 3 + x] = z;
        }
    }

    // A distance changes by at most the distance moved. When the point in the middle of the depths
    // is farther from the soft edge than their spread, all 9 are on the same side of it: all outside
    // (0) or all inside (1 each), as the loop below would give. Only near the edge each point is
    // classified. The region is evaluated about once per texel instead of 9 times
    var coverage = 0.0;
    if weight_with_depth > 0.0 {
        let feathering = region_feathering();
        let spread = 0.5 * (z_max - z_min) * length(vec3f(lookup.tan_xy, 1.0));
        let middle = lookup_to_world(lookup, 0.5 * (z_min + z_max), view_idx) + displacement;
        let distance = region_signed_distance(middle, view_idx);
        if distance <= -(feathering + spread) {
            coverage = weight_with_depth;
        } else if distance < feathering + spread {
            for (var y = 0; y < 3; y++) {
                for (var x = 0; x < 3; x++) {
                    let z = depths[y * 3 + x];
                    if z > 0.0 {
                        let hit = lookup_to_world(lookup, z, view_idx) + displacement;
                        coverage += weights[x].x * weights[y].y * region_mask(hit, view_idx);
                    }
                }
            }
        }
    }

    var proximity = 0.0;
    var has_depth = 0.0;
    let center_depth = load_depth(vec2i(center), view_idx);
    if center_depth < MAX_DEPTH {
        let hit = lookup_to_world(lookup, linearize_depth(center_depth), view_idx) + displacement;
        proximity = clamp(1.0 - length(hit - origin) / 3.0, 0.0, 1.0);
        has_depth = 1.0;
    }

    return vec4f(coverage, proximity, has_depth, 0.0);
}

// Depth values from here on mean "no data"
const MAX_DEPTH: f32 = 0.9999;

// Where to start the search of the real surface along the ray. At the arm if the ray passes near it, so
// that its motion is taken into account.
fn initial_distance(origin: vec3f, dir: vec3f) -> f32 {
    var t = 0.7;
    var nearest = ARM_RADIUS;
    for (var hand = 0u; hand < 2u; hand++) {
        let displacement = depth_u.arms[hand * 3u + 2u].xyz;
        if dot(displacement, displacement) < 1e-6 {
            continue;
        }
        let axis = depth_u.arms[hand * 3u + 1u].xyz * ARM_LENGTH;
        // Middle between the old and the current position
        let start = depth_u.arms[hand * 3u].xyz - 0.5 * displacement;

        // Closest point between the ray and the arm
        let w = origin - start;
        let b = dot(dir, axis);
        let c = dot(axis, axis);
        let denominator = c - b * b;
        var s = 0.0;
        if denominator > 1e-9 {
            s = clamp((dot(axis, w) - b * dot(dir, w)) / denominator, 0.0, 1.0);
        }
        let point = start + axis * s;
        let arm_t = max(dot(point - origin, dir), 0.05);
        let distance = length(origin + dir * arm_t - point) - 0.5 * length(displacement);
        if distance < nearest {
            nearest = distance;
            t = arm_t;
        }
    }
    return t;
}

const ARM_LENGTH: f32 = 0.45;
const ARM_RADIUS: f32 = 0.14;

// The depth map is late: how much the real surface at this point has moved since it was captured. The
// arm moves with the wrist, less towards the elbow. Zero without arm motions (plain depth).
fn arm_displacement(point: vec3f) -> vec3f {
    // Past the wrist the hand is moved too, but with the color key the runtime removes the hands from
    // the depth map: there it would move what is around the palm
    var hand_edge = vec2f(-0.2, -0.1);
    if depth_u.lighting2.z > 0.5 {
        hand_edge = vec2f(-0.03, 0.02);
    }
    var result = vec3f(0.0);
    for (var hand = 0u; hand < 2u; hand++) {
        let displacement = depth_u.arms[hand * 3u + 2u].xyz;
        // Still or not tracked
        if dot(displacement, displacement) < 1e-6 {
            continue;
        }
        let from_wrist = point - depth_u.arms[hand * 3u].xyz;
        let direction = depth_u.arms[hand * 3u + 1u].xyz;
        let along = dot(from_wrist, direction);

        // Distance from the axis of the arm, anywhere between its old and its current position
        let offset = from_wrist - direction * along;
        let motion = displacement - direction * dot(displacement, direction);
        let s = clamp(-dot(offset, motion) / max(dot(motion, motion), 1e-6), 0.0, 1.0);
        let aside = length(offset + motion * s);

        // `debug.alvr.arm_shift 1`: a smoothstep to 45 cm. 2: linear over `arm_weight_mm`, in the
        // w of the displacement (0 in mode 1). Offline the linear weight of about 30 cm followed the real
        // arm best: the smoothstep keeps moving the elbow end too much
        let weight_len = depth_u.arms[hand * 3u + 2u].w;
        var along_weight = 1.0 - smoothstep(0.0, ARM_LENGTH, along);
        if weight_len > 0.0 {
            along_weight = clamp(1.0 - along / weight_len, 0.0, 1.0);
        }
        let weight = along_weight
            * smoothstep(hand_edge.x, hand_edge.y, along)
            * (1.0 - smoothstep(0.6 * ARM_RADIUS, ARM_RADIUS, aside));
        result += displacement * weight;
    }
    return result;
}

// Rays farther than this from the gap segment of an arm get nothing from it
const GAP_REACH: f32 = 0.055;
// The surface found for the gap must lie this near the lookup point (fading over the last cm): then it
// is the forearm, not the wall behind it
const GAP_SURFACE: f32 = 0.06;
// Rays that pass this far in front of the wrist (towards the fingers) get nothing; less: a part
const GAP_FRONT: f32 = 0.005;

// Step 2 of docs/mask-v2-design.md (4.2), `debug.alvr.arm_gap_mm`: the runtime removes the forearm
// 0.6-2.6 cm behind the wrist (5-8 cm with the hands on a wheel, measured), and moving the arm
// (arm_displacement) cannot bring back what is not in the depth. A ray that passes just behind the wrist
// is given the forearm's surface from G further up the arm, moved back by the same amount: the arm is
// continued to the hand like a tube. Coverage of the region there, 0 where the ray is far from the gap
// or the depth up the arm is not an arm. The caller takes the max with the plain mask, so the background
// beside the wrist is never moved; it also works for a still arm and without arm motions (arm_shift 0)
fn gap_coverage(origin: vec3f, dir: vec3f, hand: u32) -> f32 {
    let gap = depth_u.arm_gap[hand];
    let gap_len = gap.w;
    if gap_len <= 0.0 {
        return 0.0;
    }
    let view_idx = imm.view_idx;
    // From the wrist up the arm to wrist + G * axis. The wrist is the one the arm is placed with
    let wrist = depth_u.arms[hand * 3u].xyz;
    let axis = gap.xyz;

    // The point of the segment nearest the ray (`along` from the wrist), and the ray's point nearest it
    let w = origin - wrist;
    let b = dot(dir, axis);
    let denominator = 1.0 - b * b;
    var along = 0.0;
    if denominator > 1e-6 {
        along = (dot(axis, w) - b * dot(dir, w)) / denominator;
    }
    // Only behind the wrist: a ray past it, towards the fingers, would be looked up G further up the arm
    // and land on the arm too. Fades out over GAP_FRONT so that the end is not a hard line
    let front = 1.0 - smoothstep(0.0, GAP_FRONT, -along);
    if front <= 0.0 {
        return 0.0;
    }
    along = clamp(along, 0.0, gap_len);
    let on_segment = wrist + axis * along;
    let near_point = origin + dir * max(dot(on_segment - origin, dir), 0.05);
    if distance(near_point, on_segment) > GAP_REACH {
        return 0.0;
    }

    // Look G - along further up the arm, where the depth still shows the arm before arm_displacement
    // moved it
    let shift = axis * (gap_len - along);
    let lookup_point = near_point + shift;
    let displacement = arm_displacement(lookup_point);
    let lookup = project_to_depth(lookup_point - displacement, view_idx);
    if !lookup.valid {
        return 0.0;
    }

    // 2x2 neighborhood, blended so that the edge is not blocky. Each surface counts only if it is near
    // the lookup point, and is classified where it is moved back to, next to the wrist
    let corner = floor(lookup.texel - 0.5);
    let f = lookup.texel - 0.5 - corner;
    var coverage = 0.0;
    for (var y = 0; y < 2; y++) {
        for (var x = 0; x < 2; x++) {
            let depth = load_depth(vec2i(corner) + vec2i(x, y), view_idx);
            if depth < MAX_DEPTH {
                let hit = lookup_to_world(lookup, linearize_depth(depth), view_idx) + displacement;
                let arm_like =
                    1.0 - smoothstep(GAP_SURFACE - 0.01, GAP_SURFACE, distance(hit, lookup_point));
                if arm_like > 0.0 {
                    let weight = mix(1.0 - f.x, f.x, f32(x)) * mix(1.0 - f.y, f.y, f32(y));
                    coverage += weight * arm_like * region_mask(hit - shift, view_idx);
                }
            }
        }
    }
    return coverage * front;
}

struct DepthLookup {
    // Continuous texel coordinates
    texel: vec2f,
    // Tangents of the direction in the depth view
    tan_xy: vec2f,
    valid: bool,
}

fn project_to_depth(world_point: vec3f, view_idx: u32) -> DepthLookup {
    var result: DepthLookup;
    result.valid = false;

    let view_point = (depth_u.eyes[view_idx].world_to_depth_view * vec4f(world_point, 1.0)).xyz;
    if view_point.z > -0.001 {
        return result;
    }

    let depth_tan = depth_u.eyes[view_idx].depth_tan;
    result.tan_xy = view_point.xy / -view_point.z;
    let tan_min = depth_tan.xz; // left, down
    let tan_max = depth_tan.yw; // right, up
    let ndc_uv = (result.tan_xy - tan_min) / (tan_max - tan_min); // origin bottom-left
    if any(ndc_uv < vec2f(0.0)) || any(ndc_uv > vec2f(1.0)) {
        return result;
    }

    // GL convention: texel row 0 is the bottom of the image
    var tex_uv = ndc_uv;
    if depth_u.depth.w > 0.5 {
        tex_uv.y = 1.0 - tex_uv.y;
    }

    result.texel = tex_uv * vec2f(textureDimensions(depth_texture));
    result.valid = true;
    return result;
}

fn project_to_playzone(world_point: vec3f, view_idx: u32) -> DepthLookup {
    var result: DepthLookup;
    result.valid = false;

    let view_point = (depth_u.playzone_eyes[view_idx].world_to_view * vec4f(world_point, 1.0)).xyz;
    if view_point.z > -0.001 {
        return result;
    }

    let tangents = depth_u.playzone_eyes[view_idx].tangents;
    result.tan_xy = view_point.xy / -view_point.z;
    var tex_uv = (result.tan_xy - tangents.xz) / (tangents.yw - tangents.xz);
    if any(tex_uv < vec2f(0.0)) || any(tex_uv > vec2f(1.0)) {
        return result;
    }
    if depth_u.depth.w > 0.5 {
        tex_uv.y = 1.0 - tex_uv.y;
    }

    result.texel = tex_uv * vec2f(textureDimensions(playzone_texture));
    result.valid = true;
    return result;
}

fn load_playzone(coord: vec2i) -> f32 {
    return load_playzone_texel(coord).r;
}

// Every channel (today's copy: r only; the room memory: see room_memory.wgsl)
fn load_playzone_texel(coord: vec2i) -> vec4f {
    let dims = vec2i(textureDimensions(playzone_texture));
    return textureLoad(playzone_texture, clamp(coord, vec2i(0), dims - 1), 0);
}

fn playzone_to_world(lookup: DepthLookup, z: f32, view_idx: u32) -> vec3f {
    let view_point = vec4f(lookup.tan_xy * z, -z, 1.0);
    return (depth_u.playzone_eyes[view_idx].view_to_world * view_point).xyz;
}

// Returns the world-space point at distance `z` from the depth camera plane
fn lookup_to_world(lookup: DepthLookup, z: f32, view_idx: u32) -> vec3f {
    let view_point = vec4f(lookup.tan_xy * z, -z, 1.0);
    return (depth_u.eyes[view_idx].depth_view_to_world * view_point).xyz;
}

fn load_depth(coord: vec2i, layer: u32) -> f32 {
    let dims = vec2i(textureDimensions(depth_texture));
    let value = textureLoad(depth_texture, clamp(coord, vec2i(0), dims - 1), layer, 0).r;
    // Treat 0 as "no data"
    return select(value, 1.0, value <= 0.0);
}

// Converts a [0, 1] depth buffer value (OpenGL convention) to a positive view-space distance
fn linearize_depth(depth: f32) -> f32 {
    let near = depth_u.depth.x;
    let far = depth_u.depth.y;
    let ndc = depth * 2.0 - 1.0;

    if far <= near {
        // Infinite far plane
        return 2.0 * near / max(1.0 - ndc, 1e-6);
    }

    return 2.0 * near * far / ((far + near) - ndc * (far - near));
}
