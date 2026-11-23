// Outline overlay: thin opaque lines drawn into the main pass of the stream, over the stream image. See
// overlay.rs. One instance per segment, 4 vertices each (a quad facing the screen).
// Keep `Immediate` the first declaration, see stream_shader_source().

struct Immediate {
    view_proj: mat4x4f,
    // Size of the target in pixels
    viewport: vec2f,
    // Of the target at the center of the view, for the width of the lines
    pixels_per_radian: f32,
    // Index of the first segment of this eye
    first: u32,
}
var<immediate> imm: Immediate;

const MAX_SEGMENTS: u32 = 256u;

const DIV12: f32 = 1.0 / 12.92;
const DIV1: f32 = 1.0 / 1.055;
const THRESHOLD: f32 = 0.04045;
const GAMMA: vec3f = vec3f(2.4);

// As in stream.wgsl: the target encodes linear colors to sRGB
override ENABLE_SRGB_CORRECTION: bool;

struct Segment {
    // xyz: stage space, w: width on screen (radians)
    start: vec4f,
    end: vec4f,
    color: vec4f,
}
struct Segments {
    items: array<Segment, MAX_SEGMENTS>,
}
@group(0) @binding(0) var<uniform> segments: Segments;

struct VertexOutput {
    @builtin(position) position: vec4f,
    @location(0) color: vec3f,
}

@vertex
fn vertex_main(
    @builtin(vertex_index) vertex_index: u32,
    @builtin(instance_index) instance_index: u32,
) -> VertexOutput {
    let segment = segments.items[imm.first + instance_index];
    // Both ends are in front of the near plane (clipped on the CPU): w > 0
    let a = imm.view_proj * vec4f(segment.start.xyz, 1.0);
    let b = imm.view_proj * vec4f(segment.end.xyz, 1.0);

    let half_viewport = 0.5 * imm.viewport;
    let pixels_a = a.xy / a.w * half_viewport;
    let pixels_b = b.xy / b.w * half_viewport;
    var direction = vec2f(1.0, 0.0);
    let length_pixels = length(pixels_b - pixels_a);
    if length_pixels > 1e-4 {
        direction = (pixels_b - pixels_a) / length_pixels;
    }
    let normal = vec2f(-direction.y, direction.x);

    // Vertices 0, 1 at the start, 2, 3 at the end; even ones on one side. The ends reach half the
    // width further, so that lines meeting at a corner close it
    let at_end = vertex_index >= 2u;
    let side = select(-1.0, 1.0, (vertex_index & 1u) == 1u);
    let along = select(-1.0, 1.0, at_end);
    let half_width = 0.5 * segment.start.w * imm.pixels_per_radian;
    let offset = (normal * side + direction * along) * half_width / half_viewport;

    let base = select(a, b, at_end);
    var result: VertexOutput;
    result.position = vec4f(base.xy + offset * base.w, base.zw);

    var color = segment.color.rgb;
    if ENABLE_SRGB_CORRECTION {
        let condition = vec3f(f32(color.r < THRESHOLD), f32(color.g < THRESHOLD), f32(color.b < THRESHOLD));
        let lowValues = color * DIV12;
        let highValues = pow((color + vec3f(0.055)) * DIV1, GAMMA);
        color = condition * lowValues + (1.0 - condition) * highValues;
    }
    result.color = color;

    return result;
}

// Opaque: alpha 1 is "game" in every mode of stream.wgsl (premultiplied, and both packed layer alphas
// of the color key)
@fragment
fn fragment_main(@location(0) color: vec3f) -> @location(0) vec4f {
    return vec4f(color, 1.0);
}
