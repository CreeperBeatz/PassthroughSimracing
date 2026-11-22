// Note: depth_common.wgsl is appended to this file at pipeline creation.
//
// Grows the depth passthrough mask into a coarse "focus area". The depth map lags behind the
// passthrough image, so with the color key the mask only selects where to look, and the compositor
// color keys the live passthrough image inside it.

@group(0) @binding(0) var<uniform> depth_u: DepthUniforms;
@group(0) @binding(1) var depth_mask: texture_2d<f32>;
@group(0) @binding(2) var mask_sampler: sampler;

struct VertexOutput {
    @builtin(position) position: vec4f,
    @location(0) uv: vec2f,
}

@vertex
fn vertex_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    // Fullscreen triangle, uv origin is top-left
    let xy = vec2f(f32((vertex_index << 1u) & 2u), f32(vertex_index & 2u));

    var result: VertexOutput;
    result.position = vec4f(xy * 2.0 - 1.0, 0.0, 1.0);
    result.uv = vec2f(xy.x, 1.0 - xy.y);
    return result;
}

const RINGS: u32 = 4u;
const SAMPLES_PER_RING: u32 = 6u;

@fragment
fn fragment_main(@location(0) uv: vec2f) -> @location(0) vec4f {
    let growth = depth_u.lighting2.w;

    var coverage = textureSampleLevel(depth_mask, mask_sampler, uv, 0.0).r;
    for (var ring = 1u; ring <= RINGS; ring++) {
        let radius = growth * f32(ring) / f32(RINGS);
        // The outer rings need more samples to leave no gaps
        let count = ring * SAMPLES_PER_RING;
        for (var i = 0u; i < count; i++) {
            let angle = 6.2831853 * f32(i) / f32(count);
            let offset = radius * vec2f(cos(angle), sin(angle));
            coverage = max(coverage, textureSampleLevel(depth_mask, mask_sampler, uv + offset, 0.0).r);
        }
    }

    return vec4f(smoothstep(0.05, 0.35, coverage), 0.0, 0.0, 1.0);
}
