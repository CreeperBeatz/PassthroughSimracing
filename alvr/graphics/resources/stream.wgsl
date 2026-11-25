// Note: foveation_common.wgsl and depth_common.wgsl are appended to this file at pipeline creation.
// Keep `Immediate` the first declaration, see stream_shader_source().

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
// Low resolution depth passthrough mask (view-space uv). See depth_mask.wgsl
@group(0) @binding(5) var depth_mask: texture_2d<f32>;
// Shape of the hands, only used with the color key. See focus_area.wgsl
@group(0) @binding(6) var focus_area: texture_2d<f32>;

@group(1) @binding(0) var<uniform> depth_u: DepthUniforms;

// `debug.alvr.problem_view`, see problem_view.rs. All zero (off) unless it is on
struct ProblemView {
    // xyz: anchor of the leak map (stage space), w: on (0/1)
    anchor: vec4f,
    // x: how much the room shows through the game, y: how much the game is dimmed
    look: vec4f,
}
// One texel per cell of the leak map (octahedral, 96 x 96): round(r * 3) is 0 nothing, 1 medium, 2 high,
// 3 unknown
@group(2) @binding(0) var problem_cells: texture_2d<f32>;
@group(2) @binding(1) var<uniform> problem_u: ProblemView;

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
        return depth_passthrough(color, uv);
    }

    return vec4f(color, alpha);
}

//============================================================================================================
// Depth passthrough
//
// Where the mask is set the game is cut out and the passthrough layer below shows through, optionally
// darkened to match the game lighting.
//============================================================================================================

const DEBUG_VIEW_TINT_MASK: u32 = 1u;
const DEBUG_VIEW_SHOW_DEPTH: u32 = 2u;

// The mask edge is where the coverage crosses this range. It is centered below 0.5 to slightly grow
// near objects (fingers) instead of eroding them.
const MASK_EDGE_MIN: f32 = 0.05;
const MASK_EDGE_MAX: f32 = 0.35;

// The mask (0..1) from the coverage of the depth mask. `debug.alvr.edge_cut` (step 1 of
// docs/mask-v2-design.md) moves the centre of the cut, in area.z (0: today's range above, so an unset
// uniform draws what it always did). Fingers are drawn by the hand zone now, so the growth only leaves a
// rim of room around every still object (4-6 mm measured). A straight depth border moves inward by 0.37
// depth texel from a centre of 0.2 to 0.5 and by 0.74 to 0.8; a line one texel wide fades above 0.6.
// It is global: the soft borders of the play-area boxes and of the remembered zone move inward too
fn mask_edge(coverage: f32) -> f32 {
    let cut = depth_u.area.z;
    if cut <= 0.0 {
        return smoothstep(MASK_EDGE_MIN, MASK_EDGE_MAX, coverage);
    }
    let low = clamp(cut - 0.15, 0.02, 0.69);
    return smoothstep(low, low + 0.3, coverage);
}

// The mark of `debug.alvr.cloud_ab`: a square of this size (view uv) around this point
const CLOUD_MARK_CENTRE: vec2f = vec2f(0.5, 0.4);
const CLOUD_MARK_SIZE: f32 = 0.02;

fn depth_passthrough(stream_color: vec3f, uv: vec2f) -> vec4f {
    let debug_view = u32(depth_u.region.w + 0.5);
    // Dimmed while the play area is edited
    var game_color = stream_color * (1.0 - depth_u.lighting2.w);
    // While the play area is edited the real room shows through the game by this much where the mask
    // does not show it fully (outside the area). The overlay is drawn after this, opaque
    var see = depth_u.area.y;
    let problem_on = problem_u.anchor.w > 0.5;
    var problem = vec4f(0.0);
    if problem_on {
        // The problem view: the room through the dimmed game, with the tint of the leak map over it
        see = max(see, problem_u.look.x);
        game_color = game_color * (1.0 - problem_u.look.y);
        problem = problem_tint(uv);
    }

    let mask_sample = textureSampleLevel(depth_mask, stream_sampler, uv, 0.0);
    let mask = mask_edge(mask_sample.r);

    if debug_view == DEBUG_VIEW_SHOW_DEPTH {
        if mask_sample.b < 0.5 {
            // No depth data for this pixel
            return vec4f(0.3, 0.0, 0.0, 1.0);
        }
        let gray = mask_sample.g;
        // Masked pixels are tinted green
        return vec4f(gray * (1.0 - 0.6 * mask), gray, gray * (1.0 - 0.6 * mask), 1.0);
    }

    if depth_u.lighting2.z > 0.5 {
        // Color key. The layers are, from the bottom:
        // 1. passthrough
        // 2. this image, with a hole in the mask and inside of the hands
        // 3. color keyed passthrough
        // 4. this image again, with a hole that also includes the rim of the hands
        // Both alpha values are packed in one channel: 0 in the first holes, 0.5 in the rim, 1
        // elsewhere. They are unpacked by the color scale and bias of the layers (2a and 2a - 1).
        // Alpha is not premultiplied.
        let hand = textureSampleLevel(focus_area, stream_sampler, uv, 0.0);
        let lower_rect = depth_u.key_lower[imm.view_idx];
        let in_lower = all(uv >= lower_rect.xy) && all(uv < lower_rect.zw);
        var focus = hand.r;
        // The zone of the depth cloud (`debug.alvr.cloud`, mask g) is keyed like the rim of the hands.
        // Only where the lower layer is used: outside it a zone pixel would pack to the game
        if depth_u.cloud.x > 0.5 && in_lower {
            focus = max(focus, mask_sample.g);
        }
        let solid = max(mask, hand.g);
        var lower_alpha = 1.0 - solid;
        var upper_alpha = 1.0 - max(solid, focus);
        var rgb = game_color;
        if debug_view == DEBUG_VIEW_TINT_MASK {
            rgb = mix(rgb, vec3f(0.0, 0.8, 0.0), 0.5 * focus);
            // Where the rim is narrowed
            rgb = mix(rgb, vec3f(0.9, 0.0, 0.0), 0.7 * hand.a);
            if depth_u.playzone.w > 0.5 {
                // Shown only because of the remembered playzone: half opaque blue over the passthrough.
                // Faintly blue where it is used, near the hands
                let remembered = mask - mask_edge(mask_sample.r - mask_sample.a);
                rgb = mix(rgb, vec3f(0.0, 0.3, 1.0), max(0.25 * hand.b, remembered));
                // Only the lower layer: both at 0.5 would pack to 0.5, an opaque lower layer
                lower_alpha = max(lower_alpha, 0.5 * remembered);
            }
        }
        if see > 0.0 {
            // The room through the game: only the lower layer can be part way (the packed alpha holds
            // one layer between 0 and 1 while the other is 0 or 1): the upper one opens. Colors the key
            // keeps then show fully where the lower layer is used (around the hands); outside it the
            // upper layer alone shows the same 1 - see below
            lower_alpha = min(lower_alpha, 1.0 - see);
            upper_alpha = 0.0;
        }
        if problem_on {
            // In the lower layer, which alone can be part way (upper is 0 while the room shows)
            rgb = mix(rgb, problem.rgb, problem.a);
            lower_alpha = max(lower_alpha, problem.a);
        }
        if depth_u.cloud.y > 0.5 && all(abs(uv - CLOUD_MARK_CENTRE) < vec2f(0.5 * CLOUD_MARK_SIZE)) {
            // `cloud_ab`: green while the cloud is shown, red in today's phase
            rgb = select(vec3f(0.0, 0.8, 0.0), vec3f(0.9, 0.0, 0.0), depth_u.cloud.y > 1.5);
        }
        var alpha = 0.5 * (lower_alpha + upper_alpha);
        // Where the lower layer is cropped away or not used, the upper one shows what both would show
        // over colors that the key doesn't keep: upper + (1 - upper) * lower
        if !in_lower {
            let lower = min(2.0 * alpha, 1.0);
            let upper = max(2.0 * alpha - 1.0, 0.0);
            alpha = 0.5 + 0.5 * (upper + (1.0 - upper) * lower);
        }
        return vec4f(rgb, alpha);
    }

    // The room shown: by the mask, at least `see` while editing
    let shown = max(mask, see);
    var brightness = 1.0;
    if shown > 0.0 && depth_u.lighting.x > 0.5 {
        brightness = passthrough_brightness(uv);
    }

    // Premultiplied: out = rgb + (1 - a) * passthrough
    var rgb = game_color * (1.0 - shown);
    if debug_view == DEBUG_VIEW_TINT_MASK {
        rgb = rgb + vec3f(0.0, 0.25, 0.0) * mask;
    }
    if problem_on {
        // Premultiplied: the tint covers problem.a of the pixel
        return vec4f(mix(rgb, problem.rgb, problem.a), mix(1.0 - shown * brightness, 1.0, problem.a));
    }

    return vec4f(rgb, 1.0 - shown * brightness);
}

// The rays of the leak map start at its anchor and see the room this far away, as in leak_map.rs
const PROBLEM_DEPTH: f32 = 0.8;
const PROBLEM_MAP_SIZE: f32 = 96.0;
// Hatching lines per cell (a cell is about 2 degrees)
const PROBLEM_LINES: f32 = 4.0;

fn sign_or_one(value: f32) -> f32 {
    return select(-1.0, 1.0, value >= 0.0);
}

// Tint of the problem view along the view ray of uv: rgb, and how much of the pixel it covers.
// Nothing under the low threshold, amber hatching (medium), red cross-hatching (high), grey dots
// (unknown). Same octahedral layout as `grid_position` in leak_map.rs
fn problem_tint(uv: vec2f) -> vec4f {
    let eye = depth_u.eyes[imm.view_idx];
    let direction = normalize((eye.uv_to_world_dir * vec4f(uv, 1.0, 0.0)).xyz);
    let v = eye.eye_position.xyz + direction * PROBLEM_DEPTH - problem_u.anchor.xyz;
    let sum = abs(v.x) + abs(v.y) + abs(v.z);
    if sum < 1e-6 {
        return vec4f(0.0);
    }
    var p = vec2f(v.x, v.z) / sum;
    if v.y > 0.0 {
        // Upper hemisphere, folded outwards
        p = vec2f((1.0 - abs(p.y)) * sign_or_one(p.x), (1.0 - abs(p.x)) * sign_or_one(p.y));
    }
    let grid = (p * 0.5 + 0.5) * PROBLEM_MAP_SIZE;
    let cell = clamp(vec2i(floor(grid)), vec2i(0), vec2i(i32(PROBLEM_MAP_SIZE) - 1));
    let kind = u32(round(textureLoad(problem_cells, cell, 0).r * 3.0));

    let f = grid * PROBLEM_LINES;
    let hatch = step(0.7, fract(f.x + f.y));
    if kind == 1u {
        return vec4f(1.0, 0.7, 0.1, 0.8 * hatch);
    }
    if kind == 2u {
        let crossed = max(hatch, step(0.7, fract(f.x - f.y)));
        return vec4f(1.0, 0.1, 0.1, 0.85 * crossed);
    }
    if kind == 3u {
        let dots = step(length(fract(f * 0.5) - 0.5), 0.15);
        return vec4f(0.6, 0.6, 0.6, 0.7 * dots);
    }
    return vec4f(0.0);
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
