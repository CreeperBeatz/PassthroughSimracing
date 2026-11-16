// Note: foveation_common.wgsl is prepended to this file at pipeline creation.
//
// Renders a low resolution map of the (approximately linear) game color, in view-space uv. Each
// output texel averages SAMPLES x SAMPLES points of the stream image inside its cell. The pipeline
// blends the result with the previous content (constant blend factor) for temporal smoothing.

struct Immediate {
    view_idx: u32,
}
var<immediate> imm: Immediate;

override PROBE_SIZE: f32 = 16.0;
const SAMPLES: u32 = 4u;

@group(0) @binding(0) var stream_texture: texture_2d<f32>;
@group(0) @binding(1) var stream_sampler: sampler;
@group(0) @binding(2) var<uniform> foveation: FoveationUniforms;

@vertex
fn vertex_main(@builtin(vertex_index) vertex_index: u32) -> @builtin(position) vec4f {
    // Fullscreen triangle
    let xy = vec2f(f32((vertex_index << 1u) & 2u), f32(vertex_index & 2u));
    return vec4f(xy * 2.0 - 1.0, 0.0, 1.0);
}

@fragment
fn fragment_main(@builtin(position) frag_coord: vec4f) -> @location(0) vec4f {
    let cell = floor(frag_coord.xy);

    var sum = vec3f(0.0);
    for (var y = 0u; y < SAMPLES; y++) {
        for (var x = 0u; x < SAMPLES; x++) {
            let offset = (vec2f(f32(x), f32(y)) + 0.5) / f32(SAMPLES);
            let view_uv = (cell + offset) / PROBE_SIZE;
            let stream_uv = decode_foveated_uv(imm.view_idx, view_uv).uv;
            let color = textureSampleLevel(stream_texture, stream_sampler, stream_uv, 0.0).rgb;
            // The stream is gamma encoded, approximate linearization is enough here
            sum += pow(max(color, vec3f(0.0)), vec3f(2.2));
        }
    }

    return vec4f(sum / f32(SAMPLES * SAMPLES), 1.0);
}
