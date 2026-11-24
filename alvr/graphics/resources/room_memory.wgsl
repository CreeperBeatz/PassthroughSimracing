// Note: depth_common.wgsl is appended to this file at pipeline creation. Keep `Immediate` the first
// declaration, see stream_shader_source().
//
// The room memory of `debug.alvr.mem_roll` (U3 of design/universal/final.md; step 3 of
// docs/mask-v2-design.md, section 4.3; the CPU side is room_memory.rs). A depth map of the rig seen by a
// wide camera fixed in stage space, one texture per eye, each fed by that eye's depth image. It is written
// from the live depth on every new depth image, except near the hands, where the runtime removes the hands
// and what they hold: there it is kept ("frozen"). The mask reads only frozen texels (a texel that is not
// frozen equals the live depth by construction). Channels (Rgba16Float):
//   r: distance from the camera plane (m, 0: nothing known)
//   g: moving: 1 when the live depth changed by more than 3 cm with no hand within 20 cm (legs, the
//      seat; final.md 2.1), falling to 0 over 5 s
//   b: frozen: 1 when the texel was kept at the last update because a hand was near
//   a: seconds since the last live write (above about 2 minutes not exact: 16 bit float)
//
// Entry points: `update_main` (mode 0: one update; mode 1: the memory of the `source` camera re-projected
// into `camera`, every channel kept), `stats_main` (shares for the `memory:` line), `dump_main` (the four
// channels as 16 bit floats in an RGBA8 target twice as wide, for the depth dump: GLES can't read float
// textures back on every device)

struct Immediate {
    view_idx: u32,
    // update_main: 0 update, 1 re-project from `source`
    mode: u32,
}
var<immediate> imm: Immediate;

@group(0) @binding(0) var<uniform> depth_u: DepthUniforms;
@group(0) @binding(1) var depth_texture: texture_2d_array<f32>;
// The memory before this pass (update, re-projection), or the one to read (stats, dump)
@group(1) @binding(0) var memory_texture: texture_2d<f32>;
@group(1) @binding(1) var<uniform> mem_u: MemoryUniforms;

// Filled by `memory_uniforms` in room_memory.rs
struct MemoryUniforms {
    camera: PlayzoneEye,
    // The camera of the memory that is re-projected (mode 1)
    source: PlayzoneEye,
    // x: margin (m): a remembered point whose depth ray passes nearer a hand capsule is kept; y: outer
    // ring (m, 0: off); z: seconds since the last update of the memory; w: how far the live surface
    // may lie from the ray of a texel (tangent units, 1.5 texel widths)
    params: vec4f,
    // Bounding sphere of the capsules of each hand at the capture time of the depth (xyz, radius; 0:
    // no hand)
    hand_bounds: array<vec4f, 2>,
    // Start and end of each capsule at that time, xyz: position, w: radius
    hand_points: array<vec4f, 2u * 2u * HAND_SEGMENTS>,
}

// Where the search of the live surface starts without a remembered one (m)
const START_DISTANCE: f32 = 0.7;
const REPROJECT_STEPS: u32 = 3u;
// In the outer ring a remembered point is kept only if the live depth is this much farther: the
// runtime's fill is the far background (11 cm to 1 m behind, measured)
const FILL_BEHIND: f32 = 0.08;
// A change of the live depth by more than this with no hand within MOVING_HAND marks the texel moving
// [assumed, final.md 2.1]
const MOVING_CHANGE: f32 = 0.03;
const MOVING_HAND: f32 = 0.2;
const MOVING_HOLD_S: f32 = 5.0;
const MAX_AGE: f32 = 60000.0;
const FAR: f32 = 1e9;
// Depth values from here on mean "no data", as in depth_mask.wgsl
const MAX_DEPTH: f32 = 0.9999;
// `stats_main`: cells per side
const STATS_CELLS: i32 = 16;

@vertex
fn vertex_main(@builtin(vertex_index) vertex_index: u32) -> @builtin(position) vec4f {
    // Fullscreen triangle
    let xy = vec2f(f32((vertex_index << 1u) & 2u), f32(vertex_index & 2u));
    return vec4f(xy * 2.0 - 1.0, 0.0, 1.0);
}

// Tangents of the ray of a point of a texture of `size` texels (`coord`: texel units) in a camera. The
// inverse of project_to_playzone in depth_mask.wgsl, which reads this texture: y flipped with the depth
fn texel_tangents(coord: vec2f, size: vec2f, tangents: vec4f) -> vec2f {
    var tex_uv = coord / size;
    if depth_u.depth.w > 0.5 {
        tex_uv.y = 1.0 - tex_uv.y;
    }
    return mix(tangents.xz, tangents.yw, tex_uv);
}

// The texel of a camera's texture at these tangents, x -1: outside
fn camera_texel(camera: PlayzoneEye, tan_xy: vec2f, size: vec2f) -> vec2i {
    var tex_uv = (tan_xy - camera.tangents.xz) / (camera.tangents.yw - camera.tangents.xz);
    if any(tex_uv < vec2f(0.0)) || any(tex_uv > vec2f(1.0)) {
        return vec2i(-1);
    }
    if depth_u.depth.w > 0.5 {
        tex_uv.y = 1.0 - tex_uv.y;
    }
    return min(vec2i(floor(tex_uv * size)), vec2i(size) - 1);
}

// The world point at distance `z` from the camera plane along the ray of these tangents
fn camera_point(camera: PlayzoneEye, tan_xy: vec2f, z: f32) -> vec3f {
    return (camera.view_to_world * vec4f(tan_xy * z, -z, 1.0)).xyz;
}

// A world point seen by a camera: xy its tangents, z its distance from the camera plane (0 or less:
// behind it)
fn in_camera(camera: PlayzoneEye, world_point: vec3f) -> vec3f {
    let view_point = (camera.world_to_view * vec4f(world_point, 1.0)).xyz;
    return vec3f(view_point.xy / max(-view_point.z, 1e-6), -view_point.z);
}

struct DepthLookup {
    texel: vec2f,
    tan_xy: vec2f,
    valid: bool,
}

// Same as project_to_depth in depth_mask.wgsl
fn project_to_depth(world_point: vec3f, view_idx: u32) -> DepthLookup {
    var result: DepthLookup;
    result.valid = false;

    let view_point = (depth_u.eyes[view_idx].world_to_depth_view * vec4f(world_point, 1.0)).xyz;
    if view_point.z > -0.001 {
        return result;
    }

    let depth_tan = depth_u.eyes[view_idx].depth_tan;
    result.tan_xy = view_point.xy / -view_point.z;
    let ndc_uv = (result.tan_xy - depth_tan.xz) / (depth_tan.yw - depth_tan.xz);
    if any(ndc_uv < vec2f(0.0)) || any(ndc_uv > vec2f(1.0)) {
        return result;
    }
    var tex_uv = ndc_uv;
    if depth_u.depth.w > 0.5 {
        tex_uv.y = 1.0 - tex_uv.y;
    }

    result.texel = tex_uv * vec2f(textureDimensions(depth_texture));
    result.valid = true;
    return result;
}

// Same as lookup_to_world in depth_mask.wgsl
fn lookup_to_world(lookup: DepthLookup, z: f32, view_idx: u32) -> vec3f {
    let view_point = vec4f(lookup.tan_xy * z, -z, 1.0);
    return (depth_u.eyes[view_idx].depth_view_to_world * view_point).xyz;
}

// Same as load_depth in depth_mask.wgsl
fn load_depth(coord: vec2i, layer: u32) -> f32 {
    let dims = vec2i(textureDimensions(depth_texture));
    let value = textureLoad(depth_texture, clamp(coord, vec2i(0), dims - 1), layer, 0).r;
    return select(value, 1.0, value <= 0.0);
}

// Same as linearize_depth in depth_mask.wgsl
fn linearize_depth(depth: f32) -> f32 {
    let near = depth_u.depth.x;
    let far = depth_u.depth.y;
    let ndc = depth * 2.0 - 1.0;
    if far <= near {
        return 2.0 * near / max(1.0 - ndc, 1e-6);
    }
    return 2.0 * near * far / ((far + near) - ndc * (far - near));
}

struct Live {
    point: vec3f,
    // Distance from the plane of the memory camera
    z: f32,
    valid: bool,
}

// The live depth surface along the ray of a memory texel: two fixed-point steps from `start`, each one
// depth read. No value (valid false) without depth there, or if the surface found lies more than the
// tolerance off the ray (the head's offset from the anchor hides or uncovers it)
fn live_surface(tan_xy: vec2f, start: f32, view_idx: u32) -> Live {
    var result: Live;
    result.valid = false;

    var z = start;
    var hit = vec3f(0.0);
    var seen = vec3f(0.0);
    for (var i = 0u; i < 2u; i++) {
        let lookup = project_to_depth(camera_point(mem_u.camera, tan_xy, z), view_idx);
        if !lookup.valid {
            return result;
        }
        let depth = load_depth(vec2i(floor(lookup.texel)), view_idx);
        if depth >= MAX_DEPTH {
            return result;
        }
        hit = lookup_to_world(lookup, linearize_depth(depth), view_idx);
        seen = in_camera(mem_u.camera, hit);
        if seen.z <= 0.001 {
            return result;
        }
        z = seen.z;
    }
    if any(abs(seen.xy - tan_xy) > vec2f(mem_u.params.w)) {
        return result;
    }

    result.point = hit;
    result.z = seen.z;
    result.valid = true;
    return result;
}

// Distance between the segment from `origin` (`dir` a unit vector, `len` long) and the surface of a
// tapered capsule. The nearest points of the line and the axis, then of the end of the segment if it was
// cut there: both are distances between real points of the two, the smaller one is used (a bound that is
// exact unless the segment ends beside the capsule)
fn segment_capsule_distance(origin: vec3f, dir: vec3f, len: f32, start: vec4f, end: vec4f) -> f32 {
    let axis = end.xyz - start.xyz;
    let w = origin - start.xyz;
    let b = dot(dir, axis);
    let c = dot(axis, axis);
    let denominator = c - b * b;
    var s = 0.0;
    if denominator > 1e-9 {
        s = clamp((dot(axis, w) - b * dot(dir, w)) / denominator, 0.0, 1.0);
    }
    let point = start.xyz + axis * s;
    let q = origin + dir * clamp(dot(point - origin, dir), 0.0, len);
    let first = length(q - point) - mix(start.w, end.w, s);

    var s2 = 0.0;
    if c > 1e-9 {
        s2 = clamp(dot(q - start.xyz, axis) / c, 0.0, 1.0);
    }
    let second = length(q - (start.xyz + axis * s2)) - mix(start.w, end.w, s2);
    return min(first, second);
}

// How near the hands at the capture time of the depth come to the segment from the depth camera to a
// point: x: the nearest capsule surface, y: the nearest bounding sphere (FAR: no hand). The capsules are
// only tested if a sphere is within `reach`, so far from the hands this is two sphere tests
fn hand_distance(camera: vec3f, point: vec3f, reach: f32) -> vec2f {
    let to_point = point - camera;
    let len = length(to_point);
    let dir = to_point / max(len, 1e-6);

    var capsule = FAR;
    var sphere = FAR;
    for (var hand = 0u; hand < 2u; hand++) {
        let bounds = mem_u.hand_bounds[hand];
        if bounds.w <= 0.0 {
            continue;
        }
        let t = clamp(dot(bounds.xyz - camera, dir), 0.0, len);
        let distance = length(camera + dir * t - bounds.xyz) - bounds.w;
        sphere = min(sphere, distance);
        if distance >= reach {
            continue;
        }
        for (var i = 0u; i < HAND_SEGMENTS; i++) {
            let base = (hand * HAND_SEGMENTS + i) * 2u;
            capsule = min(
                capsule,
                segment_capsule_distance(
                    camera,
                    dir,
                    len,
                    mem_u.hand_points[base],
                    mem_u.hand_points[base + 1u],
                ),
            );
        }
    }
    return vec2f(capsule, sphere);
}

// Mode 1: the texel of `source` the ray of this texel meets (fixed-point steps through the source
// memory), every channel kept. Nothing (0) where the source has no value or its surface lies off the ray
fn reprojected(tan_xy: vec2f, size: vec2f) -> vec4f {
    var z = START_DISTANCE;
    var value = vec4f(0.0);
    var seen = vec3f(0.0);
    for (var i = 0u; i < REPROJECT_STEPS; i++) {
        let in_source = in_camera(mem_u.source, camera_point(mem_u.camera, tan_xy, z));
        if in_source.z <= 0.001 {
            return vec4f(0.0);
        }
        let texel = camera_texel(mem_u.source, in_source.xy, size);
        if texel.x < 0 {
            return vec4f(0.0);
        }
        value = textureLoad(memory_texture, texel, 0);
        if value.r <= 0.0 {
            return vec4f(0.0);
        }
        let source_tan = texel_tangents(vec2f(texel) + 0.5, size, mem_u.source.tangents);
        seen = in_camera(mem_u.camera, camera_point(mem_u.source, source_tan, value.r));
        if seen.z <= 0.001 {
            return vec4f(0.0);
        }
        z = seen.z;
    }
    if any(abs(seen.xy - tan_xy) > vec2f(mem_u.params.w)) {
        return vec4f(0.0);
    }
    return vec4f(seen.z, value.g, value.b, value.a);
}

// The update rule of section 4.3 of docs/mask-v2-design.md, with the corrections of the build notes:
// - near a hand, with a remembered point: kept, frozen (also without a live value there: the runtime's
//   fill near a hand may be no data);
// - no live value: kept, not frozen;
// - in the outer ring (`mem_outer_mm`): kept, frozen, only if the live depth is the far fill;
// - else the live depth is written, not frozen, age 0 (also near a hand with nothing remembered: the
//   fill is better than nothing).
// "Near" is tested on the segment from this eye's depth camera to the remembered point (else the live
// one) against the hand capsules at the capture time of the depth
@fragment
fn update_main(@builtin(position) frag_coord: vec4f) -> @location(0) vec4f {
    let size = vec2f(textureDimensions(memory_texture));
    let coord = vec2i(frag_coord.xy);
    let tan_xy = texel_tangents(floor(frag_coord.xy) + 0.5, size, mem_u.camera.tangents);
    if imm.mode == 1u {
        return reprojected(tan_xy, size);
    }

    let old = textureLoad(memory_texture, coord, 0);
    let dt = mem_u.params.z;
    let aged = min(old.a + dt, MAX_AGE);
    let moving = max(old.g - dt / MOVING_HOLD_S, 0.0);
    let z_old = old.r;
    let remembered = z_old > 0.0;
    let live = live_surface(tan_xy, select(START_DISTANCE, z_old, remembered), imm.view_idx);

    let margin = mem_u.params.x;
    let outer = mem_u.params.y;
    var hand = vec2f(FAR);
    if remembered || live.valid {
        var point = live.point;
        if remembered {
            point = camera_point(mem_u.camera, tan_xy, z_old);
        }
        let camera = depth_u.eyes[imm.view_idx].depth_view_to_world[3].xyz;
        hand = hand_distance(camera, point, max(margin, outer));
    }

    if remembered && hand.x < margin {
        return vec4f(z_old, moving, 1.0, aged);
    }
    if !live.valid {
        return vec4f(z_old, moving, 0.0, aged);
    }
    if remembered && hand.x < outer && live.z > z_old + FILL_BEHIND {
        return vec4f(z_old, moving, 1.0, aged);
    }
    var moved = moving;
    if remembered && abs(live.z - z_old) > MOVING_CHANGE && hand.y > MOVING_HAND {
        moved = 1.0;
    }
    return vec4f(live.z, moved, 0.0, 0.0);
}

// Shares of one cell of STATS_CELLS x STATS_CELLS, in a target 2 * STATS_CELLS wide. Even columns: x
// frozen, y no live value at the last update (age above 0 and not frozen; includes texels never seen),
// z nothing known, w moving. Odd columns: x the age of the oldest frozen texel / 60 s (0: none), y the
// oldest of any texel / 60 s
@fragment
fn stats_main(@builtin(position) frag_coord: vec4f) -> @location(0) vec4f {
    let size = vec2i(textureDimensions(memory_texture));
    let block = max(size / STATS_CELLS, vec2i(1));
    let cell = vec2i(i32(frag_coord.x) / 2, i32(frag_coord.y));
    let start = cell * block;

    var frozen = 0.0;
    var stale = 0.0;
    var empty = 0.0;
    var moving = 0.0;
    var oldest_frozen = 0.0;
    var oldest = 0.0;
    for (var y = 0; y < block.y; y++) {
        for (var x = 0; x < block.x; x++) {
            let value = textureLoad(memory_texture, min(start + vec2i(x, y), size - 1), 0);
            let is_frozen = value.b > 0.5;
            frozen += select(0.0, 1.0, is_frozen);
            stale += select(0.0, 1.0, value.a > 0.0 && !is_frozen);
            empty += select(0.0, 1.0, value.r <= 0.0);
            moving += select(0.0, 1.0, value.g > 0.0);
            if is_frozen {
                oldest_frozen = max(oldest_frozen, value.a);
            }
            oldest = max(oldest, value.a);
        }
    }
    if (i32(frag_coord.x) & 1) == 1 {
        return vec4f(min(oldest_frozen / 60.0, 1.0), min(oldest / 60.0, 1.0), 0.0, 0.0);
    }
    let count = f32(block.x * block.y);
    return vec4f(frozen, stale, empty, moving) / count;
}

// The four channels of a memory texel as 16 bit floats, two per output texel (r, g then b, a), each byte
// a quarter step above its value, so that both rounding and truncation give it back (as depth_dump.wgsl)
@fragment
fn dump_main(@builtin(position) frag_coord: vec4f) -> @location(0) vec4f {
    let coord = vec2i(frag_coord.xy);
    let value = textureLoad(memory_texture, vec2i(coord.x / 2, coord.y), 0);
    var bits = pack2x16float(value.rg);
    if (coord.x & 1) == 1 {
        bits = pack2x16float(value.ba);
    }
    let bytes = vec4f(
        f32(bits & 255u),
        f32((bits >> 8u) & 255u),
        f32((bits >> 16u) & 255u),
        f32((bits >> 24u) & 255u),
    );
    return (bytes + 0.25) / 255.0;
}
