// Shared depth passthrough uniforms. This snippet is appended to the shaders that use them. The
// including shader must declare:
//   var<uniform> depth_u: DepthUniforms;

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
    // x: min brightness, y: locality, z: color key enabled (0/1), w: focus area growth (view uv)
    lighting2: vec4f,
}
