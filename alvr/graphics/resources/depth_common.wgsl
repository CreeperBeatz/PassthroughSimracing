// Shared depth passthrough uniforms. This snippet is appended to the shaders that use them. The
// including shader must declare:
//   var<uniform> depth_u: DepthUniforms;

// Capsules per hand. The last ones are the forearm
const HAND_SEGMENTS: u32 = 32u;
const ARM_SEGMENTS: u32 = 4u;

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
struct DepthUniforms {
    eyes: array<DepthEye, 2>,
    world_to_volume: mat4x4f,
    volume_half_extents: vec4f,
    // x: region mode (0: distance, 1: volume), y: max distance, z: edge feathering, w: debug view
    region: vec4f,
    // x: near, y: far (0 = infinite), z: depth available (0/1), w: flip y (0/1)
    depth: vec4f,
    // x: lighting enabled (0/1), y: base brightness, z: strength, w: reference luminance
    lighting: vec4f,
    // x: min brightness, y: locality, z: color key enabled (0/1)
    lighting2: vec4f,
    // Confidence zones of each hand, as distances from its surface (negative inside).
    // x: end of the color keyed rim, y: start of the falloff (solid passthrough before it),
    // z: width of the falloff, w: end of the color keyed rim of the forearm
    hand_zones: array<vec4f, 2>,
    // How solid the core of each capsule is (0..1), 4 capsules per vector, left hand first
    hand_solid: array<vec4f, 2u * HAND_SEGMENTS / 4u>,
    // Bounding spheres, xyz: position, w: radius (0: not tracked)
    hand_bounds: array<vec4f, 2>,
    // Start and end of each capsule, xyz: position, w: radius
    hand_points: array<vec4f, 2u * 2u * HAND_SEGMENTS>,
    // Forearms at the time when the depth image was captured: xyz: wrist, xyz: elbow, and how much of the
    // depth of each quarter (from the wrist) is replaced by the tracked arm (0..1)
    arms: array<vec4f, 6>,
}
