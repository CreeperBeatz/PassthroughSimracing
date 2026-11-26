// Shared depth passthrough uniforms. This snippet is appended to the shaders that use them. The
// including shader must declare:
//   var<uniform> depth_u: DepthUniforms;

// Capsules per hand
const HAND_SEGMENTS: u32 = 28u;
// Boxes of the play area, see play_area.rs
const AREA_BOXES: u32 = 8u;
// Lever windows, see `LeverWindow` in depth_mask.rs
const LEVER_WINDOWS: u32 = 4u;
// Boxes of the green-screen zone, see `MAX_GREEN_WINDOWS` in depth_mask.rs
const GREEN_WINDOWS: u32 = 2u;

// All positions/poses are in the stage reference space
struct DepthEye {
    // world direction = (uv_to_world_dir * vec4f(uv, 1, 0)).xyz, uv is the view-space uv
    uv_to_world_dir: mat4x4f,
    // Current eye position (xyz)
    eye_position: vec4f,
    world_to_depth_view: mat4x4f,
    depth_view_to_world: mat4x4f,
    // Tangents of the depth view fov: left, right, down, up
    depth_tan: vec4f,
}
// Depth camera of the remembered playzone
struct PlayzoneEye {
    world_to_view: mat4x4f,
    view_to_world: mat4x4f,
    // Tangents of the fov: left, right, down, up
    tangents: vec4f,
}
// A level box of the play area, turned about the vertical axis through its center, with a dome on each
// side (see play_area.rs). The dome of a side is a capsule of radius R whose axis lies d below the side.
// Sides, half extents and domes are those of the frame of the box
struct AreaBox {
    // xyz: center (area frame), w: height of the tallest dome (0: no dome)
    center: vec4f,
    // xyz: half extents
    half: vec4f,
    // R of the domes of the +X, +Y, +Z sides (xyz) and of the -X, -Y, -Z sides (0: flat)
    dome_radius_pos: vec4f,
    dome_radius_neg: vec4f,
    // d of the domes of the +X, +Y, +Z sides and of the -X, -Y, -Z sides
    dome_depth_pos: vec4f,
    dome_depth_neg: vec4f,
    // x: cosine, y: sine of the yaw of the box (counter-clockwise seen from above)
    turn: vec4f,
}
// A lever window (`debug.alvr.lever_boxes`): every view ray that crosses this box shows the real room
struct LeverWindow {
    // Stage space to the frame of the box (rigid), the box centered at its origin
    world_to_window: mat4x4f,
    // xyz: half extents, w: how much it shows (0..1)
    half: vec4f,
}
struct DepthUniforms {
    eyes: array<DepthEye, 2>,
    // Stage space to the frame of the play area (or of the box of the settings)
    world_to_area: mat4x4f,
    // x: number of boxes in area_boxes (0 with the distance region), y: how much the real room shows
    // through the game where the mask does not show it fully (0: not, 1: no game; while the play area
    // is edited), z: centre of the cut of the mask edge in stream.wgsl (`debug.alvr.edge_cut`, 0:
    // today's), w: number of lever windows in lever_windows (0: none, the default)
    area: vec4f,
    // x: region mode (0: distance, 1: volume), y: max distance, z: edge feathering, w: debug view
    region: vec4f,
    // x: near, y: far (0 = infinite), z: depth available (0/1), w: flip y (0/1)
    depth: vec4f,
    // x: lighting enabled (0/1), y: base brightness, z: strength, w: reference luminance
    lighting: vec4f,
    // x: min brightness, y: locality, z: color key enabled (0/1), w: how much the game is dimmed (0: not,
    // 1: black; while the play area is edited)
    lighting2: vec4f,
    // Confidence zones of each hand, as distances from its surface (negative inside).
    // x: end of the color keyed rim, y: start of the falloff (solid passthrough before it),
    // z: width of the falloff, w: how far the rim reaches back against the motion (see hand_sweep)
    hand_zones: array<vec4f, 2>,
    // How solid the core of each capsule is (0..1), 4 capsules per vector, left hand first
    hand_solid: array<vec4f, 2u * HAND_SEGMENTS / 4u>,
    // Bounding spheres, xyz: position, w: radius (0: not tracked)
    hand_bounds: array<vec4f, 2>,
    // Start and end of each capsule, xyz: position, w: radius
    hand_points: array<vec4f, 2u * 2u * HAND_SEGMENTS>,
    // Motion of each arm since the depth image was captured: xyz: wrist position, xyz: direction from
    // the wrist to the elbow, xyz: displacement (0: plain depth). Without a motion the wrist is still
    // set when the arm has a wrist gap (nothing else reads it while the displacement is 0)
    arms: array<vec4f, 6>,
    // The wrist gap of each arm (`debug.alvr.arm_gap_mm`), xyz: unit vector from the middle knuckle to
    // the wrist (up the arm), w: its length G in meters (0: off)
    arm_gap: array<vec4f, 2>,
    playzone_eyes: array<PlayzoneEye, 2>,
    // x: available (0/1), y, z: debug view, how much the rim of each hand is narrowed (0..1), w: debug
    // view of the playzone (0/1)
    playzone: vec4f,
    // The room memory (`debug.alvr.mem_roll`, U3 of design/universal/final.md, room_memory.rs), all 0
    // when it is off: today's. x: which texels of the playzone count, 0 all (today's), 1 only frozen
    // ones, 2 only frozen ones not marked moving (`mem_moving 1`); y: `mem_view` (2: what only the
    // memory shows is paler the older it is); z, w: how much farther the zone near the left / right hand
    // reaches than today's 6 cm (m)
    memory: vec4f,
    // The depth cloud (`debug.alvr.cloud`, motion_cloud.rs), all 0 when it is off: today's. x: 0 off, 1
    // the zone of the field (mask g) opens the colour key, 2 the same with the fetch point moved by the
    // arm shift; y: mark of `cloud_ab` (0 none, 1 cloud phase, 2 today's phase); z: `cloud_trail` while
    // the zone is shown (depth texels, 0 off: inside the depth mask, the border of the zone along edges
    // that moved is handed to the colour key); w: `cloud_trail_age` while the trail is on (depth images:
    // only surface that arrived in the mask within these is handed; 0: no age test). Negative:
    // `cloud_trail_hands 0`, the trail also near the hands (as in commit 9abdada4), the age is -1 - w;
    // 0 or more (the default): near the hands the trail is scaled by 1 - the near-hand value (focus area
    // b). Decode with cloud_trail_age() and cloud_trail_spares_hands() of depth_mask.wgsl
    cloud: vec4f,
    // Color key: part of each view where the lower game layer is shown, xy: min, zw: max view uv
    key_lower: array<vec4f, 2>,
    // Rim of a moving hand: the capsules moved anywhere along the motion, from hand_zones.w back to w
    // ahead. xyz: direction of the motion (unit vector), w: how far ahead (0 with hand_zones.w: no sweep)
    hand_sweep: array<vec4f, 2>,
    // One more capsule of each hand, from the wrist towards the forearm, 2 vectors per hand. First:
    // xyz: the wrist, w: radius at the wrist. Second: xyz: the end, w: how solid its core is (0..1)
    hand_bridge: array<vec4f, 4>,
    // Radius at the end of each bridge, x: left hand, y: right hand. It differs from the one at the
    // wrist where a hand limited to a marker box tapers off (both may be negative, the rim still
    // shows down to minus the rim). z, w: 1 when the left / right hand has a bridge
    hand_bridge_end: vec4f,
    // Bounds of the boxes of the play area with their domes, in its frame: center, half extents
    area_bounds: array<vec4f, 2>,
    // Only the first area.x are used
    area_boxes: array<AreaBox, AREA_BOXES>,
    // Only the first area.w are used
    lever_windows: array<LeverWindow, LEVER_WINDOWS>,
    // The green-screen zone (`debug.alvr.green_boxes`, green_screen.rs), all 0 when it is off: today's.
    // x: number of boxes in green_windows
    green: vec4f,
    // Only the first green.x are used, each fully open (half.w 1)
    green_windows: array<LeverWindow, GREEN_WINDOWS>,
    // `debug.alvr.gpu_rect` (gpu_rect.rs, stage S3 of docs/gpu-optimization.md), all 0 when it is off:
    // today's. x: on (0/1), y: 2 draws the borders of the rectangles below in the Tint mask view
    gpu_rect: vec4f,
    // Of each eye, min xy and max zw view uv: the main pass takes the mask tap only inside the first,
    // the focus tap only inside the second. Outside them both textures are 0 where stream.wgsl reads
    // them (gpu_rect.rs bounds every source; the depth mask pass is scissored to the first)
    gpu_rect_mask: array<vec4f, 2>,
    gpu_rect_focus: array<vec4f, 2>,
    // `debug.alvr.cloud_own` (`own_hand` of depth_mask.wgsl, `own_uniforms` in depth_mask.rs), all 0 when
    // it is off: today's. x: distance from the capsules (m, 0: off), y: fade beyond it (m), z, w: gate of
    // the left / right hand (0..1, from its wrist's travel since the depth's capture)
    own: vec4f,
    // Bounding sphere of each hand's capsules at the depth's capture time, xyz: centre, w: radius (0: none)
    capture_bounds: array<vec4f, 2>,
    // As hand_points, at the depth's capture time (from the room memory's capsules)
    capture_points: array<vec4f, 2u * 2u * HAND_SEGMENTS>,
}

// The depth cloud (`debug.alvr.cloud`, motion_cloud.rs, motion_cloud.wgsl). One eye's field: the camera
// of the depth image it was built from, and the one before it
struct CloudEye {
    field: PlayzoneEye,
    // The image before it (the reprojection of mark_main, the history of field_main)
    prev: PlayzoneEye,
    // x: seconds between the two images, y: decay of the held speed (0: none), z: reset (0/1, no
    // motion claimed for this pair), w: the field may open its zone on this display frame (0/1, 0: no
    // field or older than 300 ms; read by the mask only)
    pair: vec4f,
}
// Filled by `cloud_uniforms` in motion_cloud.rs, own buffer
struct CloudUniforms {
    eyes: array<CloudEye, 2>,
    // x: distance at which a display ray is looked up in the field (m), y: deadband (texels per pair),
    // z: largest reach (texels), w: constant reach (texels, 0: speed x age)
    params: vec4f,
    // x, y: reach time of eye 0 / 1 for this display frame (s), z: `cloud_onesided` (0: an edge in one
    // of the two masks only claims no motion, 1: only in the border band of the image, 2: always
    // measured), w: `cloud_window`, half of the window of the field's search (cells, 1..24, 12 by
    // default; phi is stored and read as a share of 2 x this, texels)
    reach: vec4f,
}

// Whether a view ray crosses a box of the green-screen zone in front of the eye (or starts inside it),
// whatever the depth says: the slab test of lever_coverage in depth_mask.wgsl. Read by depth_mask.wgsl
// (the zone) and stream.wgsl (its debug tint)
fn in_green_zone(origin: vec3f, dir: vec3f) -> bool {
    let count = min(u32(depth_u.green.x), GREEN_WINDOWS);
    for (var i = 0u; i < count; i++) {
        let window = depth_u.green_windows[i];
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
            return true;
        }
    }
    return false;
}

// The region of the depth mask, read by depth_mask.wgsl and motion_cloud.wgsl

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
    // Into the frame of the box: turned back by its yaw (turn_y of play_area.rs with minus the sine)
    let offset = point - area_box.center.xyz;
    let c = area_box.turn.x;
    let s = area_box.turn.y;
    let local = vec3f(c * offset.x - s * offset.z, offset.y, c * offset.z + s * offset.x);
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
