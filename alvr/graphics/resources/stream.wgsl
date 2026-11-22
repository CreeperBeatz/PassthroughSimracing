// Note: foveation_common.wgsl is appended to this file at pipeline creation. Keep `Immediate` the first
// declaration, see stream_shader_source().

struct Immediate {
    reprojection_transform: mat4x4f,
    view_idx: u32,
    passthrough_mode: u32, // 0: Blend, 1: RGB chroma key, 2: HSV chroma key, 3: Depth
    blend_alpha: f32,
    _align: u32,
    ck_channel0: vec4f,
    ck_channel1: vec4f,
    ck_channel2: vec4f,
}
var<immediate> imm: Immediate;

const PASSTHROUGH_MODE_RGB_CHROMA_KEY: u32 = 1u;
const PASSTHROUGH_MODE_HSV_CHROMA_KEY: u32 = 2u;
const PASSTHROUGH_MODE_DEPTH: u32 = 3u;

const DIV12: f32 = 1.0 / 12.92;
const DIV1: f32 = 1.0 / 1.055;
const THRESHOLD: f32 = 0.04045;
const GAMMA: vec3f = vec3f(2.4);

override ENABLE_SRGB_CORRECTION: bool;
override ENCODING_GAMMA: f32;

override ENABLE_UPSCALING: bool = false;
override UPSCALE_USE_EDGE_DIRECTION: bool = true;
override UPSCALE_EDGE_THRESHOLD: f32 = 4.0/255.0;
override UPSCALE_EDGE_SHARPNESS: f32 = 2.0;

@group(0) @binding(0) var stream_texture: texture_2d<f32>;
@group(0) @binding(1) var stream_sampler: sampler;
@group(0) @binding(2) var<uniform> foveation: FoveationUniforms;
// Low resolution, temporally smoothed, linear game color (view-space uv). See light_probe_*.wgsl
@group(0) @binding(3) var light_probe_local: texture_2d<f32>;
// 1x1 average of light_probe_local
@group(0) @binding(4) var light_probe_global: texture_2d<f32>;

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
    // x: min brightness, y: locality
    lighting2: vec4f,
}
@group(1) @binding(0) var<uniform> depth_u: DepthUniforms;
// Environment depth (GL_DEPTH_COMPONENT16 on Quest), one layer per eye. Bound as unfilterable
// float instead of a depth texture: naga would otherwise emit a shadow sampler, which cannot be
// used with texelFetch in GLSL ES.
@group(1) @binding(1) var depth_texture: texture_2d_array<f32>;

struct VertexOutput {
    @builtin(position) position: vec4f,
    @location(0) uv: vec2f,
}

@vertex
fn vertex_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    var result: VertexOutput;

    result.uv = vec2f(f32(vertex_index & 1), f32(vertex_index >> 1));
    result.position = imm.reprojection_transform * vec4f(result.uv.x - 0.5, 0.5 - result.uv.y, 0.0, 1.0);

    return result;
}

@fragment
fn fragment_main(@location(0) uv: vec2f) -> @location(0) vec4f {
    let foveated = decode_foveated_uv(imm.view_idx, uv);
    let corrected_uv = foveated.uv;

    var color: vec3f;
    if ENABLE_UPSCALING {
        color = sgsr(vec4f(corrected_uv.x, corrected_uv.y, 0.0, 0.0), foveated.resolution_scale).xyz;
    } else {
        color = textureSample(stream_texture, stream_sampler, corrected_uv).rgb;
    }

    if ENABLE_SRGB_CORRECTION {
        let condition = vec3f(f32(color.r < THRESHOLD), f32(color.g < THRESHOLD), f32(color.b < THRESHOLD));
        let lowValues = color * DIV12;
        let highValues = pow((color + vec3f(0.055)) * DIV1, GAMMA);
        color = condition * lowValues + (1.0 - condition) * highValues;
    }

    if ENCODING_GAMMA != 0.0 {
        let enc_condition = vec3f(f32(color.r < 0.0), f32(color.g < 0.0), f32(color.b < 0.0));
        let enc_lowValues = color;
        let enc_highValues = pow(color, vec3f(ENCODING_GAMMA));
        color = enc_condition * enc_lowValues + (1.0 - enc_condition) * enc_highValues;
    }

    var alpha = imm.blend_alpha; // Default to Blend passthrough mode
    if imm.passthrough_mode == PASSTHROUGH_MODE_RGB_CHROMA_KEY
        || imm.passthrough_mode == PASSTHROUGH_MODE_HSV_CHROMA_KEY {
        var current = color;
        if imm.passthrough_mode == PASSTHROUGH_MODE_HSV_CHROMA_KEY {
            current = rgb_to_hsv(color);
        }
        let mask = chroma_key_mask(current);

        // Note: because of this calculation, we require premultiplied alpha option in the XR layer
        color = max(color * mask, vec3f(0.0));
        alpha = mask;
    } else if imm.passthrough_mode == PASSTHROUGH_MODE_DEPTH {
        // Note: requires premultiplied alpha in the XR layer
        return depth_passthrough(color, uv, imm.view_idx);
    }

    return vec4f(color, alpha);
}

//============================================================================================================
// Depth passthrough
//
// For each pixel, find the real-world surface seen along the pixel ray using the headset depth
// map, then decide whether that surface belongs to the "keep real" region (close to the eyes, or
// inside the cockpit box). Inside the region the game is cut out and the passthrough layer below
// shows through, optionally darkened to match the game lighting.
//============================================================================================================

const DEBUG_VIEW_TINT_MASK: u32 = 1u;
const DEBUG_VIEW_SHOW_DEPTH: u32 = 2u;

fn depth_passthrough(game_color: vec3f, uv: vec2f, view_idx: u32) -> vec4f {
    let debug_view = u32(depth_u.region.w + 0.5);

    let real_point = find_real_point(uv, view_idx);
    var mask = 0.0;
    if real_point.w > 0.5 {
        mask = region_mask(real_point.xyz, view_idx);
    }

    if debug_view == DEBUG_VIEW_SHOW_DEPTH {
        if real_point.w < 0.5 {
            // No depth data for this pixel
            return vec4f(0.3, 0.0, 0.0, 1.0);
        }
        let dist = length(real_point.xyz - depth_u.eyes[view_idx].eye_position.xyz);
        let gray = clamp(1.0 - dist / 3.0, 0.0, 1.0);
        // Masked pixels are tinted green
        return vec4f(gray * (1.0 - 0.6 * mask), gray, gray * (1.0 - 0.6 * mask), 1.0);
    }

    var brightness = 1.0;
    if mask > 0.0 && depth_u.lighting.x > 0.5 {
        brightness = passthrough_brightness(uv);
    }

    // Premultiplied: out = rgb + (1 - a) * passthrough
    var rgb = game_color * (1.0 - mask);
    if debug_view == DEBUG_VIEW_TINT_MASK {
        rgb = rgb + vec3f(0.0, 0.25, 0.0) * mask;
    }

    return vec4f(rgb, 1.0 - mask * brightness);
}

// Returns the world-space real surface point seen through view-space `uv`, w = 1 if valid
fn find_real_point(uv: vec2f, view_idx: u32) -> vec4f {
    if depth_u.depth.z < 0.5 {
        return vec4f(0.0);
    }

    let eye = depth_u.eyes[view_idx];
    let origin = eye.eye_position.xyz;
    let dir = normalize((eye.uv_to_world_dir * vec4f(uv, 1.0, 0.0)).xyz);

    // The depth camera is close to (but not exactly at) the eye. Project a guess point along the
    // ray into the depth map, read the real surface there, and refine the guess distance.
    var t = 0.7;
    for (var i = 0u; i < 2u; i++) {
        let hit = sample_real_point(origin + dir * t, view_idx, false);
        if hit.w < 0.5 {
            return vec4f(0.0);
        }
        t = max(dot(hit.xyz - origin, dir), 0.05);
    }

    // Final lookup takes the closest depth in a 3x3 neighborhood, to slightly grow near objects
    // (fingers) instead of eroding them
    return sample_real_point(origin + dir * t, view_idx, true);
}

fn load_depth(coord: vec2i, layer: u32) -> f32 {
    let value = textureLoad(depth_texture, coord, layer, 0).r;
    // Treat 0 as "no data"
    return select(value, 1.0, value <= 0.0);
}

// Projects `world_point` into the depth view and returns the real surface along that direction
fn sample_real_point(world_point: vec3f, view_idx: u32, dilate: bool) -> vec4f {
    let eye = depth_u.eyes[view_idx];

    let view_point = (eye.world_to_depth_view * vec4f(world_point, 1.0)).xyz;
    if view_point.z > -0.001 {
        return vec4f(0.0);
    }

    let tan_xy = view_point.xy / -view_point.z;
    let tan_min = eye.depth_tan.xz; // left, down
    let tan_max = eye.depth_tan.yw; // right, up
    let ndc_uv = (tan_xy - tan_min) / (tan_max - tan_min); // origin bottom-left
    if any(ndc_uv < vec2f(0.0)) || any(ndc_uv > vec2f(1.0)) {
        return vec4f(0.0);
    }

    // GL convention: texel row 0 is the bottom of the image
    var tex_uv = ndc_uv;
    if depth_u.depth.w > 0.5 {
        tex_uv.y = 1.0 - tex_uv.y;
    }

    let dims = vec2i(textureDimensions(depth_texture));
    let coord = clamp(vec2i(tex_uv * vec2f(dims)), vec2i(0), dims - 1);

    var depth = load_depth(coord, view_idx);
    if dilate {
        for (var dy = -1; dy <= 1; dy++) {
            for (var dx = -1; dx <= 1; dx++) {
                let c = clamp(coord + vec2i(dx, dy), vec2i(0), dims - 1);
                depth = min(depth, load_depth(c, view_idx));
            }
        }
    }

    if depth >= 0.9999 {
        return vec4f(0.0);
    }

    let z = linearize_depth(depth);
    let world_hit = (eye.depth_view_to_world * vec4f(tan_xy * z, -z, 1.0)).xyz;

    return vec4f(world_hit, 1.0);
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

// 1 inside the region, 0 outside, with a soft edge of `feathering` meters
fn region_mask(world_point: vec3f, view_idx: u32) -> f32 {
    let feathering = max(depth_u.region.z, 0.0005);

    var signed_distance: f32;
    if depth_u.region.x < 0.5 {
        let dist = length(world_point - depth_u.eyes[view_idx].eye_position.xyz);
        signed_distance = dist - depth_u.region.y;
    } else {
        let local_point = (depth_u.world_to_volume * vec4f(world_point, 1.0)).xyz;
        let e = abs(local_point) - depth_u.volume_half_extents.xyz;
        signed_distance = length(max(e, vec3f(0.0))) + min(max(e.x, max(e.y, e.z)), 0.0);
    }

    return 1.0 - smoothstep(-feathering, feathering, signed_distance);
}

// Brightness factor (0..1) applied to the passthrough, following the game lighting
fn passthrough_brightness(uv: vec2f) -> f32 {
    let local_light = textureSampleLevel(light_probe_local, stream_sampler, uv, 0.0).rgb;
    let global_light = textureLoad(light_probe_global, vec2i(0), 0).rgb;
    let light = mix(global_light, local_light, depth_u.lighting2.y);
    let luminance = dot(light, vec3f(0.2126, 0.7152, 0.0722));

    let brightness = depth_u.lighting.y
        * pow(max(luminance, 1e-4) / max(depth_u.lighting.w, 1e-4), depth_u.lighting.z);

    return clamp(brightness, depth_u.lighting2.x, 1.0);
}

fn chroma_key_mask(color: vec3f) -> f32 {
    let start_max = vec3f(imm.ck_channel0.x, imm.ck_channel1.x, imm.ck_channel2.x);
    let start_min = vec3f(imm.ck_channel0.y, imm.ck_channel1.y, imm.ck_channel2.y);
    let end_min = vec3f(imm.ck_channel0.z, imm.ck_channel1.z, imm.ck_channel2.z);
    let end_max = vec3f(imm.ck_channel0.w, imm.ck_channel1.w, imm.ck_channel2.w);

    let start_mask = smoothstep(start_min.yz, start_max.yz, color.yz);
    let end_mask = smoothstep(end_min.yz, end_max.yz, color.yz);
    let sv_mask = max(start_mask, end_mask);
    // create t1 < t2 < t3, based on the rotated hue starting with start_max.x representing 0.
    let t3 = end_max.x - start_max.x;
    // check hue range
    if t3 > 1.0 {
        return max(sv_mask.x, sv_mask.y);
    }
    let x = fract(color.x - start_max.x);
    var t1 = fract(start_min.x - start_max.x);
    var t2 = fract(end_min.x - start_max.x);

    return max(max(smoothstep(t1, 0.0, x), smoothstep(t2, t3, x)), max(sv_mask.x, sv_mask.y));
}

fn rgb_to_hsv(rgb: vec3f) -> vec3f {
    let cmax = max(rgb.r, max(rgb.g, rgb.b));
    let cmin = min(rgb.r, min(rgb.g, rgb.b));
    let delta = cmax - cmin;

    var h = 0.0;
    var s = 0.0;
    let v = cmax;

    if cmax > cmin {
        s = delta / cmax;

        if rgb.r == cmax {
            h = (rgb.g - rgb.b) / delta;
        } else if rgb.g == cmax {
            h = 2.0 + (rgb.b - rgb.r) / delta;
        } else {
            h = 4.0 + (rgb.r - rgb.g) / delta;
        }
        h = fract(h / 6.0);
    }

    return vec3f(h, s, v);
}

//============================================================================================================
//
//
//                  Copyright (c) 2023, Qualcomm Innovation Center, Inc. All rights reserved.
//                              SPDX-License-Identifier: BSD-3-Clause
//
//============================================================================================================

fn fastLanczos2(x: f32) -> f32
{
    var wA: f32 = x - 4.0;
    let wB: f32 = x * wA - wA;
    wA *= wA;
    return wB * wA;
}

fn weightY(dx: f32, dy: f32, c: f32, data: vec3f) -> vec2f {
    let stdA: f32 = data.x;
    let dir: vec2f = data.yz;
    let edgeDis: f32 = ((dx * dir.y) + (dy * dir.x));
    let x: f32 = (((dx * dx) + (dy * dy)) + ((edgeDis * edgeDis) * ((clamp(((c * c) * stdA), 0.0, 1.0) * 0.7) + -1.0)));
    let w: f32 = fastLanczos2(x);
    return vec2f(w, w * c);
}

fn weightYned(dx: f32, dy: f32, c: f32, data: f32) -> vec2f {
    let stdA: f32 = data;
    let x: f32 = ((dx * dx) + (dy * dy)) * 0.55 + clamp(abs(c) * stdA, 0.0, 1.0);
    let w: f32 = fastLanczos2(x);
    return vec2f(w, w * c);
}

fn edgeDirection(left: vec4f, right: vec4f) -> vec2f
{
    var dir: vec2f;
    let RxLz: f32 = (right.x + (-left.z));
    let RwLy: f32 = (right.w + (-left.y));
    var delta: vec2f;
    delta.x = (RxLz + RwLy);
    delta.y = (RxLz + (-RwLy));
    let lengthInv: f32 = inverseSqrt((delta.x * delta.x + 3.075740e-05) + (delta.y * delta.y));
    dir.x = (delta.x * lengthInv);
    dir.y = (delta.y * lengthInv);
    return dir;
}

fn sgsr(in_TEXCOORD0: vec4f, source_resolution_multiplier: f32) -> vec4f {
    // https://github.com/SnapdragonStudios/snapdragon-gsr/issues/2
    let dim = vec2f(textureDimensions(stream_texture)) * source_resolution_multiplier;
    let viewport_info = vec4f(1/dim.x, 1/dim.y, dim.x, dim.y);

    var color: vec4f;
    let texSample = textureSampleLevel(stream_texture, stream_sampler, in_TEXCOORD0.xy, 0.0);
    color.x = texSample.x;
    color.y = texSample.y;
    color.z = texSample.z;

    // all of these 1 values are the OperationMode
    // see https://github.com/SnapdragonStudios/snapdragon-gsr/tree/main/sgsr/v1#operation-mode
    let imgCoord: vec2f = (in_TEXCOORD0.xy * viewport_info.zw) + vec2f(-0.5, 0.5);
    let imgCoordPixel: vec2f = floor(imgCoord);
    var coord: vec2f = (imgCoordPixel * viewport_info.xy);
    let pl: vec2f = (imgCoord + (-imgCoordPixel));
    var left: vec4f = textureGather(1, stream_texture, stream_sampler, coord);

    let edgeVote: f32 = abs(left.z - left.y) + abs(color[1] - left.y) + abs(color[1] - left.z);
    if edgeVote > UPSCALE_EDGE_THRESHOLD {
        coord.x += viewport_info.x;

        var right: vec4f = textureGather(1, stream_texture, stream_sampler, coord + vec2f(viewport_info.x, 0.0));
        var upDown: vec4f;
        let texGatherA = textureGather(1, stream_texture, stream_sampler, coord + vec2f(0.0, -viewport_info.y));
        upDown.x = texGatherA.w;
        upDown.y = texGatherA.z;
        let texGatherB = textureGather(1, stream_texture, stream_sampler, coord + vec2f(0.0, viewport_info.y));
        upDown.z = texGatherB.y;
        upDown.w = texGatherB.x;

        let mean: f32 = (left.y + left.z + right.x + right.w) * 0.25;
        left = left - vec4(mean);
        right = right - vec4(mean);
        upDown = upDown - vec4(mean);
        color.w = color[1] - mean;

        let sum: f32 = (((((abs(left.x) + abs(left.y)) + abs(left.z)) + abs(left.w)) + (((abs(right.x) + abs(right.y)) + abs(right.z)) + abs(right.w))) + (((abs(upDown.x) + abs(upDown.y)) + abs(upDown.z)) + abs(upDown.w)));
        let sumMean: f32 = 1.014185e+01 / sum;
        let stdA: f32 = (sumMean * sumMean);

        var aWY: vec2f;
        if UPSCALE_USE_EDGE_DIRECTION {
            let data = vec3f(stdA, edgeDirection(left, right));
            aWY = weightY(pl.x, pl.y + 1.0, upDown.x, data);
            aWY += weightY(pl.x - 1.0, pl.y + 1.0, upDown.y, data);
            aWY += weightY(pl.x - 1.0, pl.y - 2.0, upDown.z, data);
            aWY += weightY(pl.x, pl.y - 2.0, upDown.w, data);
            aWY += weightY(pl.x + 1.0, pl.y - 1.0, left.x, data);
            aWY += weightY(pl.x, pl.y - 1.0, left.y, data);
            aWY += weightY(pl.x, pl.y, left.z, data);
            aWY += weightY(pl.x + 1.0, pl.y, left.w, data);
            aWY += weightY(pl.x - 1.0, pl.y - 1.0, right.x, data);
            aWY += weightY(pl.x - 2.0, pl.y - 1.0, right.y, data);
            aWY += weightY(pl.x - 2.0, pl.y, right.z, data);
            aWY += weightY(pl.x - 1.0, pl.y, right.w, data);
        } else {
            let data: f32 = stdA;
            aWY = weightYned(pl.x, pl.y + 1.0, upDown.x, data);
            aWY += weightYned(pl.x - 1.0, pl.y + 1.0, upDown.y, data);
            aWY += weightYned(pl.x - 1.0, pl.y - 2.0, upDown.z, data);
            aWY += weightYned(pl.x, pl.y - 2.0, upDown.w, data);
            aWY += weightYned(pl.x + 1.0, pl.y - 1.0, left.x, data);
            aWY += weightYned(pl.x, pl.y - 1.0, left.y, data);
            aWY += weightYned(pl.x, pl.y, left.z, data);
            aWY += weightYned(pl.x + 1.0, pl.y, left.w, data);
            aWY += weightYned(pl.x - 1.0, pl.y - 1.0, right.x, data);
            aWY += weightYned(pl.x - 2.0, pl.y - 1.0, right.y, data);
            aWY += weightYned(pl.x - 2.0, pl.y, right.z, data);
            aWY += weightYned(pl.x - 1.0, pl.y, right.w, data);
        }

        let finalY: f32 = aWY.y / aWY.x;
        let maxY: f32 = max(max(left.y, left.z), max(right.x, right.w));
        let minY: f32 = min(min(left.y, left.z), min(right.x, right.w));
        var deltaY: f32 = clamp(UPSCALE_EDGE_SHARPNESS * finalY, minY, maxY) - color.w;

        //smooth high contrast input
        deltaY = clamp(deltaY, -23.0 / 255.0, 23.0 / 255.0);

        color.x = clamp((color.x + deltaY), 0.0, 1.0);
        color.y = clamp((color.y + deltaY), 0.0, 1.0);
        color.z = clamp((color.z + deltaY), 0.0, 1.0);
    }

    color.w = 1.0; //assume alpha channel is not used

    return color;
}
