// Note: depth_common.wgsl is appended to this file at pipeline creation. Keep `Immediate` the first
// declaration, see stream_shader_source().
//
// The depth cloud of `debug.alvr.cloud` (design/depthcloud/final.md, part B.2; the CPU side is
// motion_cloud.rs). Three passes per new depth image and eye, all in the grid of the depth image:
//   mark_main (D x D, Rgba16Float, ping-pong): r: depth (m, 0: no data), g: M, 1 where the surface is
//     in the play region, b: Mp, the same for the previous image on this texel's world ray after the
//     flip test, a: Mp before the flip test. The state is also the copy of the depth for the next image
//   rows_main (D/2 x D/2 cells of 2 x 2 texels, Rgba8Unorm): along the row, the distance (cells,
//     0..12, 13: none in the window) to the nearest cell in and out of M, then of Mp
//   field_main (D/2 x D/2, Rgba8Unorm, ping-pong): the field. r: 0.5 + 0.5 * phi / 24 (phi: signed
//     distance to the edge of M, texels, + on the game side), g: held speed / 510 (texels per second),
//     b: this pair's speed / 510 (see `cloud_onesided` in field_main for an edge in one mask only), a:
//     0.5 + 0.5 * the edge's move towards the game / 24 (not used in v1)

struct Immediate {
    view_idx: u32,
    // Not used in v1 (0)
    mode: u32,
}
var<immediate> imm: Immediate;

@group(0) @binding(0) var<uniform> depth_u: DepthUniforms;
@group(0) @binding(1) var depth_texture: texture_2d_array<f32>;
// mark_main: the state of the image before. rows_main: the state just written. field_main: not read
@group(1) @binding(0) var state_texture: texture_2d<f32>;
// field_main: the rows of this image (rows_main binds a placeholder: it writes them)
@group(1) @binding(1) var rows_texture: texture_2d<f32>;
// field_main: the field of the image before
@group(1) @binding(2) var field_texture: texture_2d<f32>;
@group(1) @binding(3) var linear_sampler: sampler;
@group(1) @binding(4) var<uniform> cloud_u: CloudUniforms;

// Half of the window (cells). Loops over it have constant bounds (GLES)
const WINDOW: i32 = 12;
// The distance of rows_main when no cell of the kind is in the window
const NO_CELL: f32 = 13.0;
// phi is truncated at this (texels: the window)
const TRUNCATION: f32 = 24.0;
// Speeds are stored as a share of this (texels per second)
const SPEED_SCALE: f32 = 510.0;
// Held speeds below this are 0 (texels per second)
const SPEED_FLOOR: f32 = 15.0;
// A cell is in when the mean of its four texels is at least this (two of four)
const CELL_IN: f32 = 0.375;
// Where the search of the previous surface starts without depth (m)
const START_DISTANCE: f32 = 0.7;
const REPROJECT_STEPS: u32 = 3u;
// The previous surface counts if it lies this many texel widths from the ray, as in room_memory.wgsl
const RAY_TOLERANCE: f32 = 1.5;
// Depth values from here on mean "no data", as in depth_mask.wgsl
const MAX_DEPTH: f32 = 0.9999;

@vertex
fn vertex_main(@builtin(vertex_index) vertex_index: u32) -> @builtin(position) vec4f {
    // Fullscreen triangle
    let xy = vec2f(f32((vertex_index << 1u) & 2u), f32(vertex_index & 2u));
    return vec4f(xy * 2.0 - 1.0, 0.0, 1.0);
}

// Tangents of the ray of a point of a texture in the grid of the depth (`coord`: texel units of a
// texture of `size` texels) in a camera: y flipped with the depth. As in room_memory.wgsl
fn texel_tangents(coord: vec2f, size: vec2f, tangents: vec4f) -> vec2f {
    var tex_uv = coord / size;
    if depth_u.depth.w > 0.5 {
        tex_uv.y = 1.0 - tex_uv.y;
    }
    return mix(tangents.xz, tangents.yw, tex_uv);
}

// The texture uv of a camera at these tangents, x -1: outside
fn camera_uv(camera: PlayzoneEye, tan_xy: vec2f) -> vec2f {
    var tex_uv = (tan_xy - camera.tangents.xz) / (camera.tangents.yw - camera.tangents.xz);
    if any(tex_uv < vec2f(0.0)) || any(tex_uv > vec2f(1.0)) {
        return vec2f(-1.0);
    }
    if depth_u.depth.w > 0.5 {
        tex_uv.y = 1.0 - tex_uv.y;
    }
    return tex_uv;
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

struct Previous {
    // The state of the previous image on this ray (g: its M, a: its Mp before the flip test)
    value: vec4f,
    known: bool,
}

// The state of the previous image on the world ray of a texel: fixed-point steps from `start` through
// the previous camera, each one state read (as `reprojected` in room_memory.wgsl). Not known where the
// previous image has no depth there, or where the surface found lies off the ray (a texel uncovered
// beside a near thing is "unknown", not "moved")
fn previous_state(tan_xy: vec2f, start: f32, size: vec2f) -> Previous {
    var result: Previous;
    result.known = false;
    let field = cloud_u.eyes[imm.view_idx].field;
    let prev = cloud_u.eyes[imm.view_idx].prev;

    var z = start;
    var value = vec4f(0.0);
    var seen = vec3f(0.0);
    for (var i = 0u; i < REPROJECT_STEPS; i++) {
        let in_prev = in_camera(prev, camera_point(field, tan_xy, z));
        if in_prev.z <= 0.001 {
            return result;
        }
        let uv = camera_uv(prev, in_prev.xy);
        if uv.x < 0.0 {
            return result;
        }
        let texel = min(vec2i(floor(uv * size)), vec2i(size) - 1);
        value = textureLoad(state_texture, texel, 0);
        if value.r <= 0.0 {
            return result;
        }
        let prev_tan = texel_tangents(vec2f(texel) + 0.5, size, prev.tangents);
        seen = in_camera(field, camera_point(prev, prev_tan, value.r));
        if seen.z <= 0.001 {
            return result;
        }
        z = seen.z;
    }
    let tolerance = RAY_TOLERANCE * (field.tangents.yw - field.tangents.xz) / size;
    if any(abs(seen.xy - tan_xy) > tolerance) {
        return result;
    }

    result.value = value;
    result.known = true;
    return result;
}

// P1: M of this image, and of the previous one on the same world ray (head motion cancelled), with the
// flip test: a texel that changed and changed back within three images claims no change
@fragment
fn mark_main(@builtin(position) frag_coord: vec4f) -> @location(0) vec4f {
    let eye = cloud_u.eyes[imm.view_idx];
    let size = vec2f(textureDimensions(depth_texture));
    let tan_xy = texel_tangents(floor(frag_coord.xy) + 0.5, size, eye.field.tangents);

    var z = 0.0;
    var m = 0.0;
    let depth = load_depth(vec2i(frag_coord.xy), imm.view_idx);
    if depth < MAX_DEPTH {
        z = linearize_depth(depth);
        let inside = region_mask(camera_point(eye.field, tan_xy, z), imm.view_idx) >= 0.5;
        m = select(0.0, 1.0, inside);
    }

    // Unknown or reset: no change claimed
    var mp_raw = m;
    var mpp = m;
    if eye.pair.z < 0.5 {
        let previous = previous_state(tan_xy, select(START_DISTANCE, z, z > 0.0), size);
        if previous.known {
            mp_raw = previous.value.g;
            mpp = previous.value.a;
        }
    }
    var mp = mp_raw;
    if m == mpp && m != mp_raw {
        mp = m;
    }
    return vec4f(z, m, mp, mp_raw);
}

// P2: along the row of this cell, the distance to the nearest cell in and out of M (state g) and of Mp
// (state b), 0..12 cells, 13: none in the window. Cells outside the image are neither. A cell's value is
// the mean of its four texels: one bilinear tap at their common corner
@fragment
fn rows_main(@builtin(position) frag_coord: vec4f) -> @location(0) vec4f {
    let state_size = vec2i(textureDimensions(state_texture));
    let cells = (state_size + 1) / 2;
    let cell = vec2i(frag_coord.xy);
    let y = f32(2 * cell.y + 1) / f32(state_size.y);

    var result = vec4f(NO_CELL);
    for (var dx = -WINDOW; dx <= WINDOW; dx++) {
        let x = cell.x + dx;
        if x < 0 || x >= cells.x {
            continue;
        }
        let uv = vec2f(f32(2 * x + 1) / f32(state_size.x), y);
        let mean = textureSampleLevel(state_texture, linear_sampler, uv, 0.0);
        let distance = f32(abs(dx));
        if mean.g >= CELL_IN {
            result.x = min(result.x, distance);
        } else {
            result.y = min(result.y, distance);
        }
        if mean.b >= CELL_IN {
            result.z = min(result.z, distance);
        } else {
            result.w = min(result.w, distance);
        }
    }
    return result / 255.0;
}

// The signed distance of a cell (texels, + on the game side), not truncated: `own_in` its row distance
// to an in cell (0: it is in), `in2` / `out2` the squared distance (cells) to the nearest in / out cell
// (1e9: none in the window). Below TRUNCATION in size only where the edge is inside the window
fn signed_distance(own_in: f32, in2: f32, out2: f32) -> f32 {
    if own_in < 0.5 {
        return -2.0 * (sqrt(out2) - 0.5);
    }
    return 2.0 * (sqrt(in2) - 0.5);
}

// P3: the exact Euclidean distance inside the window from the rows, for M (phi) and Mp (phip); the
// speed of the edge from their difference; the held speed carried from the previous field on the same
// world ray (at the fetch distance, as the mask looks it up)
@fragment
fn field_main(@builtin(position) frag_coord: vec4f) -> @location(0) vec4f {
    let eye = cloud_u.eyes[imm.view_idx];
    let cells = vec2i(textureDimensions(rows_texture));
    let cell = vec2i(frag_coord.xy);

    // Squared distances (cells) to the nearest in and out cell of M (x, y) and of Mp (z, w)
    var nearest = vec4f(1e9);
    var own = vec4f(0.0);
    for (var dy = -WINDOW; dy <= WINDOW; dy++) {
        let y = cell.y + dy;
        if y < 0 || y >= cells.y {
            continue;
        }
        let row = round(textureLoad(rows_texture, vec2i(cell.x, y), 0) * 255.0);
        if dy == 0 {
            own = row;
        }
        let found = row < vec4f(NO_CELL);
        nearest = select(nearest, min(nearest, f32(dy * dy) + row * row), found);
    }
    let phi_raw = signed_distance(own.x, nearest.x, nearest.y);
    let phip_raw = signed_distance(own.z, nearest.z, nearest.w);
    let phi = clamp(phi_raw, -TRUNCATION, TRUNCATION);
    let phip = clamp(phip_raw, -TRUNCATION, TRUNCATION);

    // An edge in only one of the two masks inside the window (it entered or left the window or the
    // image, a lone cell that flipped in a uniform area, or a fast edge that moved farther than the
    // window) reads up to the truncation. `cloud_onesided` (reach.z): 0 a cell claims motion only where
    // both masks have an edge inside the window, 1 a cell with an edge in one of them only claims none
    // in the border band (cells whose window reaches outside the image), 2 every cell is measured
    let inside = abs(phi_raw) < TRUNCATION;
    let inside_p = abs(phip_raw) < TRUNCATION;
    let border = any(cell < vec2i(WINDOW)) || any(cell >= cells - WINDOW);
    let onesided_mode = cloud_u.reach.z;
    let refused = (onesided_mode < 0.5 && !(inside && inside_p))
        || (onesided_mode > 0.5 && onesided_mode < 1.5 && inside != inside_p && border);
    let delta = abs(phip - phi);
    var speed = 0.0;
    if !refused && delta >= cloud_u.params.y {
        speed = min(delta / max(eye.pair.x, 0.001), SPEED_SCALE);
    }

    // The ray of this cell at the fetch distance, in the camera of the previous field
    var held = 0.0;
    if eye.pair.y > 0.0 {
        let tan_xy = texel_tangents(vec2f(cell) + 0.5, vec2f(cells), eye.field.tangents);
        let origin = eye.field.view_to_world[3].xyz;
        let dir = normalize((eye.field.view_to_world * vec4f(tan_xy, -1.0, 0.0)).xyz);
        let in_prev = in_camera(eye.prev, origin + dir * cloud_u.params.x);
        if in_prev.z > 0.001 {
            let uv = camera_uv(eye.prev, in_prev.xy);
            if uv.x >= 0.0 {
                let before = textureSampleLevel(field_texture, linear_sampler, uv, 0.0).g;
                held = before * SPEED_SCALE * eye.pair.y;
            }
        }
    }
    var held_speed = max(speed, held);
    if held_speed < SPEED_FLOOR {
        held_speed = 0.0;
    }

    return vec4f(
        0.5 + 0.5 * phi / TRUNCATION,
        held_speed / SPEED_SCALE,
        speed / SPEED_SCALE,
        0.5 + 0.5 * clamp((phip - phi) / TRUNCATION, -1.0, 1.0),
    );
}
