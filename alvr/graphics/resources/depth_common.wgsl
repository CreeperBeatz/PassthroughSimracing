// Shared depth passthrough uniforms. This snippet is appended to the shaders that use them. The
// including shader must declare:
//   var<uniform> depth_u: DepthUniforms;

// Capsules per hand
const HAND_SEGMENTS: u32 = 28u;
// Boxes of the play area, see play_area.rs
const AREA_BOXES: u32 = 8u;

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
// A level box of the play area, axis aligned in the area frame, with a dome on each side (see
// play_area.rs). The dome of a side is a capsule of radius R whose axis lies d below the side
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
}
struct DepthUniforms {
    eyes: array<DepthEye, 2>,
    // Stage space to the frame of the play area (or of the box of the settings)
    world_to_area: mat4x4f,
    // x: number of boxes in area_boxes (0 with the distance region)
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
    // the wrist to the elbow, xyz: displacement (0: plain depth)
    arms: array<vec4f, 6>,
    playzone_eyes: array<PlayzoneEye, 2>,
    // x: available (0/1), y, z: debug view, how much the rim of each hand is narrowed (0..1), w: debug
    // view of the playzone (0/1)
    playzone: vec4f,
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
}
