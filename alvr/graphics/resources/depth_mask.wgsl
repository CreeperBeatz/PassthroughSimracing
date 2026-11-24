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
// more), b: whether there is depth data (0/1), a: how much of r comes from the remembered playzone
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
// Remembered depth map without hands, in meters (0: no data). See playzone_copy.wgsl
@group(1) @binding(0) var playzone_texture: texture_2d<f32>;
@group(1) @binding(1) var focus_area: texture_2d<f32>;
@group(1) @binding(2) var linear_sampler: sampler;

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

    // Near the hands, objects may be missing in the depth map
    if depth_u.lighting2.z > 0.5 && depth_u.playzone.x > 0.5 && result.r < 1.0 {
        let near_hand = textureSampleLevel(focus_area, linear_sampler, uv, 0.0).b;
        if near_hand > 0.0 {
            let coverage = max(result.r, near_hand * playzone_coverage(uv));
            result.a = coverage - result.r;
            result.r = coverage;
        }
    }

    return result;
}

// Coverage of the region in the remembered playzone
fn playzone_coverage(uv: vec2f) -> f32 {
    let view_idx = imm.view_idx;
    let origin = depth_u.eyes[view_idx].eye_position.xyz;
    let dir = normalize((depth_u.eyes[view_idx].uv_to_world_dir * vec4f(uv, 1.0, 0.0)).xyz);

    var t = 0.7;
    for (var i = 0u; i < 2u; i++) {
        let guess = project_to_playzone(origin + dir * t, view_idx);
        if !guess.valid {
            return 0.0;
        }
        let z = load_playzone(vec2i(floor(guess.texel)));
        if z <= 0.0 {
            return 0.0;
        }
        t = max(dot(playzone_to_world(guess, z, view_idx) - origin, dir), 0.05);
    }

    let lookup = project_to_playzone(origin + dir * t, view_idx);
    if !lookup.valid {
        return 0.0;
    }

    // 2x2 neighborhood, blend the results
    let corner = floor(lookup.texel - 0.5);
    let f = lookup.texel - 0.5 - corner;
    var coverage = 0.0;
    for (var y = 0; y < 2; y++) {
        for (var x = 0; x < 2; x++) {
            let z = load_playzone(vec2i(corner) + vec2i(x, y));
            if z > 0.0 {
                let weight = mix(1.0 - f.x, f.x, f32(x)) * mix(1.0 - f.y, f.y, f32(y));
                coverage += weight * region_mask(playzone_to_world(lookup, z, view_idx), view_idx);
            }
        }
    }
    return coverage;
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

        let weight = (1.0 - smoothstep(0.0, ARM_LENGTH, along))
            * smoothstep(hand_edge.x, hand_edge.y, along)
            * (1.0 - smoothstep(0.6 * ARM_RADIUS, ARM_RADIUS, aside));
        result += displacement * weight;
    }
    return result;
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
    let dims = vec2i(textureDimensions(playzone_texture));
    return textureLoad(playzone_texture, clamp(coord, vec2i(0), dims - 1), 0).r;
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

// The soft edge of the region (meters, on each side of its surface)
fn region_feathering() -> f32 {
    return max(depth_u.region.z, 0.0005);
}

// Distance from the surface of the region (meters, negative inside). Beyond the soft edge on either
// side it may be only a bound, which gives the same mask (see area_signed_distance)
fn region_signed_distance(world_point: vec3f, view_idx: u32) -> f32 {
    if depth_u.region.x < 0.5 {
        let dist = length(world_point - depth_u.eyes[view_idx].eye_position.xyz);
        return dist - depth_u.region.y;
    }
    let area_point = (depth_u.world_to_area * vec4f(world_point, 1.0)).xyz;
    return area_signed_distance(area_point, region_feathering());
}

// 1 inside the region, 0 outside, with a soft edge of `feathering` meters
fn region_mask(world_point: vec3f, view_idx: u32) -> f32 {
    let feathering = region_feathering();
    let signed_distance = region_signed_distance(world_point, view_idx);
    return 1.0 - smoothstep(-feathering, feathering, signed_distance);
}

// Larger than any distance in the room
const FAR_DISTANCE: f32 = 1e9;

// Distance from the surface of the union of the boxes of the play area (area frame), see play_area.rs.
// A line-by-line copy of PlayArea::signed_distance: keep both the same. Below -feathering the value can
// be too large, above +feathering too small: the mask is the same
fn area_signed_distance(point: vec3f, feathering: f32) -> f32 {
    // Outside the bounds of all boxes by the soft edge: that distance is a lower bound
    let e = abs(point - depth_u.area_bounds[0].xyz) - depth_u.area_bounds[1].xyz;
    let outside = length(max(e, vec3f(0.0)));
    if outside >= feathering {
        return outside;
    }

    var distance = FAR_DISTANCE;
    let count = min(u32(depth_u.area.x), AREA_BOXES);
    for (var i = 0u; i < count; i++) {
        distance = min(distance, area_box_distance(point, i, feathering));
        // Inside by the soft edge: the mask is 1 whatever the other boxes give
        if distance <= -feathering {
            break;
        }
    }
    return distance;
}

// A box with its domes, a copy of AreaBox::signed_distance
fn area_box_distance(point: vec3f, index: u32, feathering: f32) -> f32 {
    let area_box = depth_u.area_boxes[index];
    let local = point - area_box.center.xyz;
    let half = area_box.half.xyz;
    let e = abs(local) - half;
    let plain = length(max(e, vec3f(0.0))) + min(max(e.x, max(e.y, e.z)), 0.0);
    let tallest = area_box.center.w;
    // Deep inside, a dome only makes it more negative. Far outside no dome is within the soft edge,
    // and every dome is within its height of the box: minus that is still a lower bound
    if tallest <= 0.0 || plain <= -feathering {
        return plain;
    }
    if plain >= tallest + feathering {
        return plain - tallest;
    }

    // Of each axis only the side the point is on: the other one is farther than the box
    let positive = local >= vec3f(0.0);
    let radius = select(area_box.dome_radius_neg.xyz, area_box.dome_radius_pos.xyz, positive);
    let depth = select(area_box.dome_depth_neg.xyz, area_box.dome_depth_pos.xyz, positive);
    // Rim radius of the sides of each axis: half their shorter edge
    let rim = vec3f(min(half.y, half.z), min(half.x, half.z), min(half.x, half.y));
    var distance = plain;
    distance = min(distance, area_dome_distance(e.x, e.yz, rim.x, radius.x, depth.x, 2.0 * half.x));
    distance = min(distance, area_dome_distance(e.y, e.xz, rim.y, radius.y, depth.y, 2.0 * half.y));
    distance = min(distance, area_dome_distance(e.z, e.xy, rim.z, radius.z, depth.z, 2.0 * half.z));
    return distance;
}

// Distance from the dome of a side, a copy of dome_distance in play_area.rs: `height` above the plane
// of the side, `aside` how far outside its edges in its plane (per in-plane axis), the rim radius, the
// capsule (radius 0: flat), the edge of the box across the side. FAR_DISTANCE where the box itself is
// the answer
fn area_dome_distance(
    height: f32,
    aside: vec2f,
    rim: f32,
    radius: f32,
    depth: f32,
    thickness: f32,
) -> f32 {
    if radius <= 0.0 {
        return FAR_DISTANCE;
    }
    // Distance from the axis in the plane of the side
    let rho = length(max(aside + rim, vec2f(0.0)));
    let y = height + depth;
    // Inside the cone from the axis through the rim the nearest point of the capsule is on the dome.
    // Under the side, inside the box, the capsule counts too, so that there is no seam at the side;
    // never deeper than the opposite side (thickness + height below the point)
    if depth * rho < rim * y || (height <= 0.0 && max(aside.x, aside.y) < 0.0) {
        return max(max(length(vec2f(rho, y)) - radius, max(aside.x, aside.y)), -(thickness + height));
    }
    return FAR_DISTANCE;
}
