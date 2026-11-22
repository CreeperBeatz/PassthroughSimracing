// Shared foveated-encoding decode. This snippet is appended to shaders that sample the stream
// texture. The including shader must declare:
//   var<uniform> foveation: FoveationUniforms;

override ENABLE_FFE: bool = false;

struct FoveationEyeUniforms {
    c1_c2: vec4f,
    lo_hi_bound: vec4f,
    a_left_b_left: vec4f,
    a_right_b_right: vec4f,
    c_right: vec4f,
}
struct FoveationUniforms {
    view_ratio_edge_ratio: vec4f,
    eyes: array<FoveationEyeUniforms, 2>,
}

struct FoveatedUv {
    uv: vec2f,
    // Relative resolution of the source region (used to tell the upscaler about compressed edges)
    resolution_scale: f32,
}

// Converts a view-space uv (0..1, origin top-left) into the uv of the foveated-encoded stream image
fn decode_foveated_uv(view_idx: u32, uv: vec2f) -> FoveatedUv {
    var result: FoveatedUv;
    result.uv = uv;
    result.resolution_scale = 1.0;

    if !ENABLE_FFE {
        return result;
    }

    var corrected_uv = uv;
    let view_size_ratio = foveation.view_ratio_edge_ratio.xy;
    let edge_ratio = foveation.view_ratio_edge_ratio.zw;
    let eye = foveation.eyes[view_idx];

    let c1 = eye.c1_c2.xy;
    let c2 = eye.c1_c2.zw;
    let lo_bound = eye.lo_hi_bound.xy;
    let hi_bound = eye.lo_hi_bound.zw;

    let a_left = eye.a_left_b_left.xy;
    let b_left = eye.a_left_b_left.zw;

    let a_right = eye.a_right_b_right.xy;
    let b_right = eye.a_right_b_right.zw;
    let c_right = eye.c_right.xy;

    if view_idx == 1 {
        corrected_uv.x = 1.0 - corrected_uv.x;
    }

    let center = (corrected_uv - c1) * edge_ratio / c2;
    let left_edge = (-b_left + sqrt(b_left * b_left + 4.0 * a_left * corrected_uv)) / (2.0 * a_left);
    let right_edge = (-b_right + sqrt(b_right * b_right - 4.0 * (c_right - a_right * corrected_uv))) / (2.0 * a_right);

    if corrected_uv.x < lo_bound.x {
        corrected_uv.x = left_edge.x;
        result.resolution_scale = result.resolution_scale * edge_ratio.x;
    } else if corrected_uv.x > hi_bound.x {
        corrected_uv.x = right_edge.x;
        result.resolution_scale = result.resolution_scale * edge_ratio.x;
    } else {
        corrected_uv.x = center.x;
    }

    if corrected_uv.y < lo_bound.y {
        corrected_uv.y = left_edge.y;
        result.resolution_scale = result.resolution_scale * edge_ratio.y;
    } else if corrected_uv.y > hi_bound.y {
        corrected_uv.y = right_edge.y;
        result.resolution_scale = result.resolution_scale * edge_ratio.y;
    } else {
        corrected_uv.y = center.y;
    }

    corrected_uv = corrected_uv * view_size_ratio;

    if view_idx == 1 {
        corrected_uv.x = 1.0 - corrected_uv.x;
    }

    result.uv = corrected_uv;
    return result;
}
