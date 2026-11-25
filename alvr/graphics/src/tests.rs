//! Shader tests that don't need a headset:
//! - the WGSL shaders must translate to GLSL ES 3.2 (the backend used on Quest)
//! - the depth passthrough shader is run on the desktop GPU against a synthetic depth scene

use crate::{
    depth_dump::{DumpReadback, DumpSource},
    depth_mask::{
        self, ArmGap, ArmMotion, ColorKeyInput, DepthFrame, DepthRenderInput, HAND_SEGMENTS,
        HandShape, HandSweep, HandZones, LeverWindow, MAX_LEVER_WINDOWS, RegionOverride,
        SweepParams, WristBridge,
    },
    motion_cloud::{
        CLOUD_EYE_FLOAT_COUNT, CLOUD_SPEED_FLOOR, CLOUD_SPEED_SCALE, CLOUD_TRUNCATION,
        CLOUD_UNIFORMS_SIZE, CLOUD_WINDOW_CELLS, CloudEyeUniform, CloudInput, CloudParams, EyePass,
        ResetReason, cells_inside, cloud_uniforms, pair_speed, window_distance,
    },
    motion_cloud_gpu::{CloudPipelines, MotionCloud},
    overlay::{self, OVERLAY_AMBER, OVERLAY_SLOTS, OverlayEye, OverlaySegment},
    play_area::{self, AreaBox, PlacedArea, PlayArea, Side},
    room_memory::{
        CaptureHand, MEMORY_UNIFORMS_SIZE, MemoryCamera, MemoryInput, MemoryParams, MemoryStats,
        memory_uniforms,
    },
    room_memory_gpu::{
        MODE_REPROJECT, MODE_UPDATE, MemoryPipelines, create_memory_bind_group,
        create_memory_texture, create_memory_uniform_buffer, encode_memory_pass,
    },
    stream::{self, FOVEATION_UNIFORM_SIZE, LIGHT_PROBE_SIZE, StreamPipelines, floats_to_bytes},
};
use alvr_common::{
    Fov, Pose, ViewParams,
    glam::{Mat4, Quat, UVec2, Vec2, Vec3, Vec4},
};
use alvr_session::{
    CockpitVolumeConfig, DepthMaskRegion, DepthPassthroughConfig, DepthPassthroughDebugView,
    PassthroughLightingConfig, settings_schema::Switch,
};
use wgpu::{
    BindGroupDescriptor, BindGroupEntry, BindingResource, BufferDescriptor, BufferUsages, Color,
    Device, Extent3d, LoadOp, Origin3d, Queue, RenderPassColorAttachment, RenderPassDescriptor,
    SamplerDescriptor, StoreOp, TexelCopyBufferInfo, TexelCopyBufferLayout, TexelCopyTextureInfo,
    TextureAspect, TextureDescriptor, TextureDimension, TextureFormat, TextureUsages,
    TextureViewDescriptor,
};

// ---------------------------------------------------------------------------------------------
// GLSL ES translation
// ---------------------------------------------------------------------------------------------

fn translate_to_gles(name: &str, source: &str, constants: &[(&str, f64)]) {
    use naga::{
        back::glsl,
        valid::{Capabilities, ValidationFlags, Validator},
    };

    let source_module = naga::front::wgsl::parse_str(source)
        .unwrap_or_else(|e| panic!("{name}: {}", e.emit_to_string(source)));
    let info = Validator::new(ValidationFlags::all(), Capabilities::all())
        .validate(&source_module)
        .unwrap_or_else(|e| panic!("{name}: {}", e.emit_to_string(source)));

    let mut pipeline_constants = naga::back::PipelineConstants::default();
    for (key, value) in constants {
        pipeline_constants.insert((*key).to_owned(), *value);
    }

    for entry_point in &source_module.entry_points {
        let (module, info) = naga::back::pipeline_constants::process_overrides(
            &source_module,
            &info,
            Some((entry_point.stage, &entry_point.name)),
            &pipeline_constants,
        )
        .unwrap_or_else(|e| panic!("{name}: {e}"));

        let options = glsl::Options {
            version: glsl::Version::Embedded {
                version: 320,
                is_webgl: false,
            },
            ..Default::default()
        };
        let pipeline_options = glsl::PipelineOptions {
            shader_stage: entry_point.stage,
            entry_point: entry_point.name.clone(),
            multiview: None,
        };

        let mut output = String::new();
        let reflection = glsl::Writer::new(
            &mut output,
            &module,
            &info,
            &options,
            &pipeline_options,
            Default::default(),
        )
        .and_then(|mut writer| writer.write())
        .unwrap_or_else(|e| panic!("{name} {}: {e}", entry_point.name));

        // wgpu-hal (GLES) looks up the type of each immediate in the *source* module, using handles
        // that belong to the module compacted by process_overrides(). If the types used by the
        // immediates are not the first ones in the shader, the lookup returns the wrong type, and
        // wgpu panics at submit time or uploads garbage.
        for item in &reflection.immediates_items {
            assert_eq!(
                source_module.types[item.ty].inner, module.types[item.ty].inner,
                "{name} {}: wgpu would use the wrong type for `{}`. Declare immediates first",
                entry_point.name, item.access_path
            );
        }

        // texelFetch on shadow samplers is invalid in GLSL ES
        assert!(
            !output.contains("Shadow"),
            "{name}: unexpected shadow sampler:\n{output}"
        );
    }
}

#[test]
fn stream_shader_translates_to_gles() {
    for ffe in [false, true] {
        for upscaling in [false, true] {
            translate_to_gles(
                "stream",
                &stream::stream_shader_source(),
                &[
                    ("ENABLE_SRGB_CORRECTION", 1.0),
                    ("ENCODING_GAMMA", 0.0),
                    ("ENABLE_FFE", f64::from(u8::from(ffe))),
                    ("ENABLE_UPSCALING", f64::from(u8::from(upscaling))),
                ],
            );
        }
    }
}

#[test]
fn depth_mask_shader_translates_to_gles() {
    translate_to_gles("depth mask", &stream::depth_mask_shader_source(), &[]);
}

// The uniform block as naga lays it out is the one `depth_uniforms` fills: the same size, and the
// play area at the end where the Rust side writes it
#[test]
fn depth_uniforms_layout_matches_the_shader() {
    let source = stream::depth_mask_shader_source();
    let module = naga::front::wgsl::parse_str(&source).unwrap();
    let struct_of = |name: &str| {
        module
            .types
            .iter()
            .find_map(|(_, ty)| match &ty.inner {
                naga::TypeInner::Struct { members, span } if ty.name.as_deref() == Some(name) => {
                    Some((members.clone(), *span as usize))
                }
                _ => None,
            })
            .unwrap()
    };
    let (members, span) = struct_of("DepthUniforms");
    assert_eq!(span, depth_mask::DEPTH_UNIFORMS_SIZE);
    let offset = |name: &str| {
        members
            .iter()
            .find(|member| member.name.as_deref() == Some(name))
            .unwrap()
            .offset as usize
    };
    let (_, box_size) = struct_of("AreaBox");
    assert_eq!(box_size, 4 * play_area::AREA_BOX_FLOAT_COUNT);
    // The lever windows last, after the play area
    let (_, lever_size) = struct_of("LeverWindow");
    assert_eq!(lever_size, 4 * depth_mask::LEVER_WINDOW_FLOAT_COUNT);
    let levers_start = depth_mask::DEPTH_UNIFORMS_SIZE - 4 * depth_mask::LEVER_FLOAT_COUNT;
    assert_eq!(offset("lever_windows"), levers_start);
    let area_start = levers_start - 4 * play_area::AREA_FLOAT_COUNT;
    assert_eq!(offset("area_bounds"), area_start);
    assert_eq!(offset("area_boxes"), area_start + 32);
    assert_eq!(offset("hand_bridge_end"), area_start - 16);
    // The wrist gaps right after the 6 vectors of the arms, where `depth_uniforms` writes them
    assert_eq!(offset("arm_gap"), offset("arms") + 6 * 16);
    assert_eq!(offset("playzone_eyes"), offset("arm_gap") + 2 * 16);
    // The room memory's vector right after the playzone's, then the depth cloud's, where
    // `depth_uniforms` writes them
    assert_eq!(offset("memory"), offset("playzone") + 16);
    assert_eq!(offset("cloud"), offset("memory") + 16);
    assert_eq!(offset("key_lower"), offset("cloud") + 16);
    assert_eq!(depth_mask::DEPTH_UNIFORMS_SIZE, 4528);
    // Where the box of the settings was: 2 eyes, then the transform and the number of boxes
    let eyes = 2 * 4 * (16 + 4 + 16 + 16 + 4);
    assert_eq!(offset("world_to_area"), eyes);
    assert_eq!(offset("area"), eyes + 64);
    assert_eq!(offset("region"), eyes + 80);
}

#[test]
fn playzone_copy_shader_translates_to_gles() {
    translate_to_gles("playzone copy", &stream::playzone_copy_shader_source(), &[]);
}

#[test]
fn room_memory_shader_translates_to_gles() {
    translate_to_gles("room memory", &stream::room_memory_shader_source(), &[]);
}

// The uniform block of room_memory.wgsl is the one `memory_uniforms` fills
#[test]
fn memory_uniforms_layout_matches_the_shader() {
    let source = stream::room_memory_shader_source();
    let module = naga::front::wgsl::parse_str(&source).unwrap();
    let (members, span) = module
        .types
        .iter()
        .find_map(|(_, ty)| match &ty.inner {
            naga::TypeInner::Struct { members, span }
                if ty.name.as_deref() == Some("MemoryUniforms") =>
            {
                Some((members.clone(), *span as usize))
            }
            _ => None,
        })
        .unwrap();
    assert_eq!(span, MEMORY_UNIFORMS_SIZE);
    let offset = |name: &str| {
        members
            .iter()
            .find(|member| member.name.as_deref() == Some(name))
            .unwrap()
            .offset as usize
    };
    assert_eq!(offset("source"), 4 * 36);
    assert_eq!(offset("params"), 4 * 72);
    assert_eq!(offset("hand_bounds"), 4 * 76);
    assert_eq!(offset("hand_points"), 4 * 84);
}

#[test]
fn motion_cloud_shader_translates_to_gles() {
    translate_to_gles("motion cloud", &stream::motion_cloud_shader_source(), &[]);
}

// The uniform block of the depth cloud is the one `cloud_uniforms` fills
#[test]
fn cloud_uniforms_layout_matches_the_shader() {
    let source = stream::motion_cloud_shader_source();
    let module = naga::front::wgsl::parse_str(&source).unwrap();
    let struct_of = |name: &str| {
        module
            .types
            .iter()
            .find_map(|(_, ty)| match &ty.inner {
                naga::TypeInner::Struct { members, span } if ty.name.as_deref() == Some(name) => {
                    Some((members.clone(), *span as usize))
                }
                _ => None,
            })
            .unwrap()
    };
    let offset = |members: &[naga::StructMember], name: &str| {
        members
            .iter()
            .find(|member| member.name.as_deref() == Some(name))
            .unwrap()
            .offset as usize
    };
    let (members, span) = struct_of("CloudUniforms");
    assert_eq!(span, CLOUD_UNIFORMS_SIZE);
    assert_eq!(span, 640);
    let (eye_members, eye_span) = struct_of("CloudEye");
    assert_eq!(eye_span, 4 * CLOUD_EYE_FLOAT_COUNT);
    assert_eq!(offset(&eye_members, "field"), 0);
    assert_eq!(offset(&eye_members, "prev"), 4 * 36);
    assert_eq!(offset(&eye_members, "pair"), 4 * 72);
    assert_eq!(offset(&members, "eyes"), 0);

    // Where `cloud_uniforms` writes the params, the reach times and the pair of eye 1
    let params = CloudParams {
        fetch_m: 0.6,
        dead: 3.0,
        cap: 9.0,
        fixed: 7.0,
        ..Default::default()
    };
    let eye = CloudEyeUniform {
        field: view(Vec3::ZERO),
        prev: view(Vec3::X),
        dt_s: 0.05,
        decay: 0.5,
        reset: true,
    };
    let data = cloud_uniforms(&[None, Some(eye)], &params, [0.11, 0.12], [true, true]);
    let params_at = offset(&members, "params") / 4;
    assert_eq!(&data[params_at..params_at + 4], &[0.6, 3.0, 9.0, 7.0]);
    let reach_at = offset(&members, "reach") / 4;
    // z: `onesided`, 1 by default
    assert_eq!(&data[reach_at..reach_at + 4], &[0.11, 0.12, 1.0, 0.0]);
    // w: the field may open its zone (the mask's stale test)
    let pair_at = (eye_span + offset(&eye_members, "pair")) / 4;
    assert_eq!(&data[pair_at..pair_at + 4], &[0.05, 0.5, 1.0, 1.0]);
    let data = cloud_uniforms(&[None, Some(eye)], &params, [0.11, 0.12], [true, false]);
    assert_eq!(&data[pair_at..pair_at + 4], &[0.05, 0.5, 1.0, 0.0]);
    // The view_to_world of the previous camera of eye 1 ends with its position
    let prev_at = (eye_span + offset(&eye_members, "prev")) / 4 + 16;
    assert_eq!(&data[prev_at + 12..prev_at + 16], &[1.0, 0.0, 0.0, 1.0]);
}

#[test]
fn depth_dump_shader_translates_to_gles() {
    translate_to_gles("depth dump", &stream::depth_dump_shader_source(), &[]);
}

#[test]
fn focus_area_shader_translates_to_gles() {
    translate_to_gles("focus area", &stream::focus_area_shader_source(), &[]);
}

#[test]
fn light_probe_shaders_translate_to_gles() {
    translate_to_gles(
        "light probe local",
        &stream::light_probe_local_shader_source(),
        &[("ENABLE_FFE", 1.0), ("PROBE_SIZE", 16.0)],
    );
    translate_to_gles(
        "light probe global",
        &stream::light_probe_global_shader_source(),
        &[],
    );
}

#[test]
fn overlay_shader_translates_to_gles() {
    for srgb in [false, true] {
        translate_to_gles(
            "overlay",
            &stream::overlay_shader_source(),
            &[("ENABLE_SRGB_CORRECTION", f64::from(u8::from(srgb)))],
        );
    }
    // The uniform buffer holds the segments of both eyes
    assert!(stream::OVERLAY_WGSL.contains(&format!("const MAX_SEGMENTS: u32 = {OVERLAY_SLOTS}u;")));
    // The dashes as in overlay.rs
    assert!(stream::OVERLAY_WGSL.contains(&format!(
        "const DASH_PERIOD: f32 = {:?};",
        overlay::DASH_PERIOD
    )));
    assert!(
        stream::OVERLAY_WGSL.contains(&format!("const DASH_ON: f32 = {:?};", overlay::DASH_ON))
    );
}

// The camera of the overlay (as `StreamRenderer::write_overlay` builds it) puts a point where the
// stream shows the ray of the depth mask that hit it: the mask casts the ray of a pixel of the stream
// view from the eye position of the depth input in the direction of `uv_to_world_dir` (input view),
// and the main pass draws that pixel into the output view with `reprojection_transform`. Input and
// output views differ here in orientation, position and fov (as with the custom reprojection), and the
// eye is away from both
#[test]
fn overlay_camera_matches_the_rays_of_the_mask() {
    let input = ViewParams {
        pose: Pose {
            orientation: Quat::from_euler(alvr_common::glam::EulerRot::YXZ, 0.3, -0.2, 0.05),
            position: Vec3::new(0.02, 1.2, 0.1),
        },
        fov: Fov {
            left: -0.9,
            right: 0.7,
            up: 0.8,
            down: -0.95,
        },
    };
    let output = ViewParams {
        pose: Pose {
            orientation: input.pose.orientation
                * Quat::from_rotation_y(0.04)
                * Quat::from_rotation_x(-0.02),
            position: input.pose.position + Vec3::new(0.01, 0.0, -0.005),
        },
        fov: Fov {
            left: -0.85,
            right: 0.75,
            up: 0.82,
            down: -0.9,
        },
    };
    let eye_position = Vec3::new(0.035, 1.21, 0.09);
    let transform = stream::reprojection_transform(&stream::StreamViewParams {
        swapchain_index: 0,
        input_view_params: input,
        output_view_params: output,
    });
    let uv_to_dir = depth_mask::uv_to_world_dir(&input);
    let eye = OverlayEye::new(
        eye_position,
        output.pose.orientation,
        output.fov,
        Vec2::splat(TARGET_SIZE as f32),
    );

    for (u, v) in [
        (0.5, 0.5),
        (0.1, 0.2),
        (0.9, 0.8),
        (0.3, 0.95),
        (0.75, 0.05),
    ] {
        let direction = (uv_to_dir * Vec4::new(u, v, 1.0, 0.0))
            .truncate()
            .normalize();
        for distance in [0.3, 0.8, 2.5] {
            let point = eye_position + direction * distance;
            // As in stream.wgsl vertex_main
            let stream = transform * Vec4::new(u - 0.5, 0.5 - v, 0.0, 1.0);
            let overlay = eye.view_proj * point.extend(1.0);
            let stream = Vec2::new(stream.x, stream.y) / stream.w;
            let overlay = Vec2::new(overlay.x, overlay.y) / overlay.w;
            // The quad of the stream is 1 km away: the offset of the views moves it by 1e-5 rad
            assert!(
                stream.distance(overlay) < 1e-4,
                "{u} {v} {distance}: {stream} {overlay}"
            );
        }
    }
}

// ---------------------------------------------------------------------------------------------
// GPU tests
// ---------------------------------------------------------------------------------------------

const TARGET_SIZE: u32 = 64;
const DEPTH_SIZE: u32 = 64;
const NEAR: f32 = 0.1;
const STREAM_GRAY: u8 = 128;

fn gpu() -> Option<(Device, Queue)> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::PRIMARY,
        flags: wgpu::InstanceFlags::VALIDATION,
        memory_budget_thresholds: Default::default(),
        backend_options: Default::default(),
        display: Default::default(),
    });
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .ok()?;
    if !adapter.features().contains(wgpu::Features::IMMEDIATES) {
        eprintln!("GPU does not support immediates, skipping");
        return None;
    }

    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_features: wgpu::Features::IMMEDIATES,
        required_limits: wgpu::Limits {
            max_immediate_size: crate::MAX_IMMEDIATE_SIZE,
            ..wgpu::Limits::default()
        },
        ..Default::default()
    }))
    .ok()
}

fn fov() -> Fov {
    // 90° symmetric: tangents are ±1
    let a = std::f32::consts::FRAC_PI_4;
    Fov {
        left: -a,
        right: a,
        up: a,
        down: -a,
    }
}

fn view(position: Vec3) -> ViewParams {
    ViewParams {
        pose: Pose {
            orientation: Default::default(),
            position,
        },
        fov: fov(),
    }
}

fn base_config(region: DepthMaskRegion) -> DepthPassthroughConfig {
    DepthPassthroughConfig {
        region,
        edge_feathering: 0.01,
        lighting: Switch::Disabled,
        debug_view: DepthPassthroughDebugView::Off,
        flip_depth_y: false,
    }
}

struct Scene {
    /// Depth texture rows in memory order (row 0 = bottom of the depth view)
    depth: Vec<f32>,
}

impl Scene {
    /// Background at `far_distance`, plus a near rectangle (in depth-view ndc uv with origin at
    /// the bottom-left) at `near_distance`. Distances are along the view axis.
    fn new(far_distance: f32, near_rect: [f32; 4], near_distance: f32) -> Self {
        let mut depth = vec![0.0; (DEPTH_SIZE * DEPTH_SIZE) as usize];
        for y in 0..DEPTH_SIZE {
            for x in 0..DEPTH_SIZE {
                let u = (x as f32 + 0.5) / DEPTH_SIZE as f32;
                let v = (y as f32 + 0.5) / DEPTH_SIZE as f32;
                let [x0, y0, x1, y1] = near_rect;
                let distance = if u >= x0 && u <= x1 && v >= y0 && v <= y1 {
                    near_distance
                } else {
                    far_distance
                };
                depth[(y * DEPTH_SIZE + x) as usize] =
                    depth_mask::encode_depth(distance, NEAR, f32::INFINITY);
            }
        }
        Self { depth }
    }
}

struct Output {
    pixels: Vec<[u8; 4]>,
    /// The depth mask (TARGET_SIZE x TARGET_SIZE, rows from the top of the view)
    mask: Vec<[u8; 4]>,
}

impl Output {
    /// Pixel at view-space uv (origin top-left)
    fn at(&self, u: f32, v: f32) -> [u8; 4] {
        let x = ((u * TARGET_SIZE as f32) as u32).min(TARGET_SIZE - 1);
        let y = ((v * TARGET_SIZE as f32) as u32).min(TARGET_SIZE - 1);
        self.pixels[(y * TARGET_SIZE + x) as usize]
    }

    fn alpha(&self, u: f32, v: f32) -> f32 {
        f32::from(self.at(u, v)[3]) / 255.0
    }

    /// Mean u of the pixels of the row at `v` where the room shows (alpha below 0.5). NaN: none
    fn room_centre(&self, v: f32) -> f32 {
        let room = (0..TARGET_SIZE)
            .map(|x| (x as f32 + 0.5) / TARGET_SIZE as f32)
            .filter(|u| self.alpha(*u, v) < 0.5)
            .collect::<Vec<_>>();

        room.iter().sum::<f32>() / room.len() as f32
    }
}

fn write_texture_2d(
    queue: &Queue,
    texture: &wgpu::Texture,
    data: &[u8],
    bytes_per_row: u32,
    rows: u32,
    layer: u32,
) {
    queue.write_texture(
        TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: Origin3d {
                x: 0,
                y: 0,
                z: layer,
            },
            aspect: TextureAspect::All,
        },
        data,
        TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(bytes_per_row),
            rows_per_image: Some(rows),
        },
        Extent3d {
            width: bytes_per_row / 4,
            height: rows,
            depth_or_array_layers: 1,
        },
    );
}

fn render(
    device: &Device,
    queue: &Queue,
    config: &DepthPassthroughConfig,
    scene: &Scene,
    eye_position: Vec3,
    depth_camera_position: Vec3,
    depth_available: bool,
) -> Output {
    render_with_key(
        device,
        queue,
        config,
        scene,
        eye_position,
        depth_camera_position,
        depth_available,
        None,
    )
}

/// Run the stream (and light probe) pipelines for the left eye and read back the result
#[expect(clippy::too_many_arguments)]
fn render_with_key(
    device: &Device,
    queue: &Queue,
    config: &DepthPassthroughConfig,
    scene: &Scene,
    eye_position: Vec3,
    depth_camera_position: Vec3,
    depth_available: bool,
    color_key: Option<ColorKeyInput>,
) -> Output {
    render_full(
        device,
        queue,
        config,
        scene,
        eye_position,
        depth_camera_position,
        depth_available,
        color_key,
        [None; 2],
        None,
        None,
        false,
        &[],
    )
}

#[expect(clippy::too_many_arguments)]
fn render_full(
    device: &Device,
    queue: &Queue,
    config: &DepthPassthroughConfig,
    scene: &Scene,
    eye_position: Vec3,
    depth_camera_position: Vec3,
    depth_available: bool,
    color_key: Option<ColorKeyInput>,
    arms: [Option<ArmMotion>; 2],
    playzone: Option<&Scene>,
    key_lower_rects: Option<[(Vec2, Vec2); 2]>,
    playzone_view: bool,
    overlay: &[OverlaySegment],
) -> Output {
    render_with_area(
        device,
        queue,
        config,
        scene,
        eye_position,
        depth_camera_position,
        depth_available,
        color_key,
        arms,
        playzone,
        key_lower_rects,
        playzone_view,
        overlay,
        None,
        0.0,
        0.0,
    )
}

/// `render_full` with a play area instead of the region of the settings
#[expect(clippy::too_many_arguments)]
fn render_with_area(
    device: &Device,
    queue: &Queue,
    config: &DepthPassthroughConfig,
    scene: &Scene,
    eye_position: Vec3,
    depth_camera_position: Vec3,
    depth_available: bool,
    color_key: Option<ColorKeyInput>,
    arms: [Option<ArmMotion>; 2],
    playzone: Option<&Scene>,
    key_lower_rects: Option<[(Vec2, Vec2); 2]>,
    playzone_view: bool,
    overlay: &[OverlaySegment],
    region_override: Option<PlacedArea>,
    game_dim: f32,
    game_see: f32,
) -> Output {
    render_v2(
        device,
        queue,
        config,
        scene,
        eye_position,
        depth_camera_position,
        depth_available,
        color_key,
        arms,
        playzone,
        key_lower_rects,
        playzone_view,
        overlay,
        region_override,
        game_dim,
        game_see,
        MaskV2::default(),
    )
}

/// The switches of steps 1 and 2 of docs/mask-v2-design.md. Default: off, today's mask
#[derive(Clone, Copy, Default)]
struct MaskV2 {
    edge_cut: f32,
    arm_gaps: [Option<ArmGap>; 2],
    /// The depth camera turned about +y by this (radians). With the eye at its position nothing moves
    /// by parallax: the depth texels only slide under the pixels, by a part of a texel
    depth_yaw: f32,
    /// The lever windows (U2 of design/universal/final.md). Default: none
    lever_windows: [Option<LeverWindow>; MAX_LEVER_WINDOWS],
    /// The room memory's input (U3). Default: none, today's playzone
    memory: Option<MemoryInput>,
    /// The depth cloud's input. Default: none, off
    cloud: Option<CloudInput>,
    /// The mask is given a field that would open the zone everywhere in view (see `cloud_probe_group`)
    /// instead of the dummy of a renderer without the cloud
    cloud_probe: bool,
    /// The cell (r: phi, g: held speed) of that field. None: on an edge at the largest speed
    cloud_probe_cell: Option<[u8; 4]>,
}

/// `render_with_area` with the edge cut and the wrist gaps
#[expect(clippy::too_many_arguments)]
fn render_v2(
    device: &Device,
    queue: &Queue,
    config: &DepthPassthroughConfig,
    scene: &Scene,
    eye_position: Vec3,
    depth_camera_position: Vec3,
    depth_available: bool,
    color_key: Option<ColorKeyInput>,
    arms: [Option<ArmMotion>; 2],
    playzone: Option<&Scene>,
    key_lower_rects: Option<[(Vec2, Vec2); 2]>,
    playzone_view: bool,
    overlay: &[OverlaySegment],
    region_override: Option<PlacedArea>,
    game_dim: f32,
    game_see: f32,
    mask_v2: MaskV2,
) -> Output {
    let target_format = TextureFormat::Rgba8Unorm;
    let constants = [
        ("ENABLE_SRGB_CORRECTION", 0.0),
        ("ENCODING_GAMMA", 0.0),
        ("ENABLE_FFE", 0.0),
    ];
    let pipelines = StreamPipelines::new(device, target_format, &constants, false);

    // Uniform gray game image
    let stream_texture = device.create_texture(&TextureDescriptor {
        label: None,
        size: Extent3d {
            width: 32,
            height: 32,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: TextureDimension::D2,
        format: TextureFormat::Rgba8Unorm,
        usage: TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST,
        view_formats: &[],
    });
    write_texture_2d(
        queue,
        &stream_texture,
        &[STREAM_GRAY, STREAM_GRAY, STREAM_GRAY, 255].repeat(32 * 32),
        32 * 4,
        32,
        0,
    );
    let stream_view = stream_texture.create_view(&TextureViewDescriptor::default());

    let sampler = device.create_sampler(&SamplerDescriptor {
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });
    let foveation_buffer = device.create_buffer(&BufferDescriptor {
        label: None,
        size: FOVEATION_UNIFORM_SIZE as u64,
        usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let probe_local = stream::create_light_probe_texture(device, LIGHT_PROBE_SIZE)
        .create_view(&TextureViewDescriptor::default());
    let probe_global = stream::create_light_probe_texture(device, 1)
        .create_view(&TextureViewDescriptor::default());

    let depth_mask = stream::create_depth_mask_texture(device, UVec2::splat(TARGET_SIZE * 5));
    let depth_mask_view = depth_mask.create_view(&TextureViewDescriptor::default());

    let focus_area_view = stream::create_focus_area_texture(device, UVec2::splat(TARGET_SIZE * 5))
        .create_view(&TextureViewDescriptor::default());

    let view_bind_group = device.create_bind_group(&BindGroupDescriptor {
        label: None,
        layout: &pipelines.view_layout,
        entries: &[
            BindGroupEntry {
                binding: 0,
                resource: BindingResource::TextureView(&stream_view),
            },
            BindGroupEntry {
                binding: 1,
                resource: BindingResource::Sampler(&sampler),
            },
            BindGroupEntry {
                binding: 2,
                resource: foveation_buffer.as_entire_binding(),
            },
            BindGroupEntry {
                binding: 3,
                resource: BindingResource::TextureView(&probe_local),
            },
            BindGroupEntry {
                binding: 4,
                resource: BindingResource::TextureView(&probe_global),
            },
            BindGroupEntry {
                binding: 5,
                resource: BindingResource::TextureView(&depth_mask_view),
            },
            BindGroupEntry {
                binding: 6,
                resource: BindingResource::TextureView(&focus_area_view),
            },
        ],
    });
    let probe_local_bind_group = device.create_bind_group(&BindGroupDescriptor {
        label: None,
        layout: &pipelines.probe_local_layout,
        entries: &[
            BindGroupEntry {
                binding: 0,
                resource: BindingResource::TextureView(&stream_view),
            },
            BindGroupEntry {
                binding: 1,
                resource: BindingResource::Sampler(&sampler),
            },
            BindGroupEntry {
                binding: 2,
                resource: foveation_buffer.as_entire_binding(),
            },
        ],
    });
    let probe_global_bind_group = device.create_bind_group(&BindGroupDescriptor {
        label: None,
        layout: &pipelines.probe_global_layout,
        entries: &[BindGroupEntry {
            binding: 0,
            resource: BindingResource::TextureView(&probe_local),
        }],
    });

    // Synthetic depth: same image in both layers
    let create_depth = |scene: &Scene| {
        let depth_texture = device.create_texture(&TextureDescriptor {
            label: None,
            size: Extent3d {
                width: DEPTH_SIZE,
                height: DEPTH_SIZE,
                depth_or_array_layers: 2,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: TextureDimension::D2,
            format: TextureFormat::R32Float,
            usage: TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let depth_bytes = floats_to_bytes(&scene.depth);
        for layer in 0..2 {
            write_texture_2d(
                queue,
                &depth_texture,
                &depth_bytes,
                DEPTH_SIZE * 4,
                DEPTH_SIZE,
                layer,
            );
        }
        depth_texture
    };
    let depth_texture = create_depth(scene);
    let uniform_buffer = stream::create_depth_uniform_buffer(device);
    let depth_bind_group = stream::create_depth_bind_group(
        device,
        &pipelines,
        &uniform_buffer,
        &stream::depth_array_view(&depth_texture),
    );

    let input = DepthRenderInput {
        frame: depth_available.then_some(DepthFrame {
            swapchain_index: 0,
            near: NEAR,
            far: f32::INFINITY,
            depth_views: [ViewParams {
                pose: Pose {
                    orientation: Quat::from_rotation_y(mask_v2.depth_yaw),
                    position: depth_camera_position,
                },
                fov: fov(),
            }; 2],
        }),
        eye_positions: [eye_position; 2],
        color_key,
        arms,
        arm_gaps: mask_v2.arm_gaps,
        edge_cut: mask_v2.edge_cut,
        lever_windows: mask_v2.lever_windows,
        capture_playzone: false,
        playzone_view,
        memory: mask_v2.memory,
        cloud: mask_v2.cloud,
        key_lower_rects,
        region_override,
        game_dim,
        game_see,
    };
    let lighting_enabled = config.lighting.as_option().is_some();
    let playzone_views = [view(depth_camera_position); 2];
    let uniforms = depth_mask::depth_uniforms(
        config,
        &[view(eye_position); 2],
        &input,
        playzone.map(|_| &playzone_views),
        lighting_enabled,
    );
    queue.write_buffer(&uniform_buffer, 0, &floats_to_bytes(&uniforms));

    let target = device.create_texture(&TextureDescriptor {
        label: None,
        size: Extent3d {
            width: TARGET_SIZE,
            height: TARGET_SIZE,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: TextureDimension::D2,
        format: target_format,
        usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let target_view = target.create_view(&TextureViewDescriptor::default());

    // Drawn after the stream in the same pass, with the camera of the eye
    let overlay_eye = OverlayEye::new(
        eye_position,
        Quat::IDENTITY,
        fov(),
        Vec2::splat(TARGET_SIZE as f32),
    );
    let overlay_buffer = stream::create_overlay_buffer(device);
    let overlay_bind_group = stream::create_overlay_bind_group(device, &pipelines, &overlay_buffer);
    let mut overlay_data = vec![];
    let overlay_count = overlay::pack_segments(&overlay_eye, overlay, &mut overlay_data);
    if overlay_count > 0 {
        queue.write_buffer(&overlay_buffer, 0, &floats_to_bytes(&overlay_data));
    }

    let mut encoder = device.create_command_encoder(&Default::default());

    if lighting_enabled {
        for (view, bind_group, pipeline, load, blend) in [
            (
                &probe_local,
                &probe_local_bind_group,
                &pipelines.probe_local,
                LoadOp::Load,
                true,
            ),
            (
                &probe_global,
                &probe_global_bind_group,
                &pipelines.probe_global,
                LoadOp::Clear(Color::BLACK),
                false,
            ),
        ] {
            let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load,
                        store: StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, bind_group, &[]);
            if blend {
                pass.set_immediates(0, &0_u32.to_le_bytes());
                pass.set_blend_constant(Color::WHITE);
            }
            pass.draw(0..3, 0..1);
        }
    }

    let problem_view = crate::problem_view::ProblemViewGpu::new(device, &pipelines.problem_layout);
    let playzone_view = stream::create_playzone_texture(device, UVec2::splat(DEPTH_SIZE))
        .create_view(&TextureViewDescriptor::default());
    let playzone_bind_group = stream::create_playzone_bind_group(
        device,
        &pipelines,
        &playzone_view,
        &focus_area_view,
        &sampler,
    );
    if let Some(playzone) = playzone {
        let texture = create_depth(playzone);
        let bind_group = stream::create_depth_bind_group(
            device,
            &pipelines,
            &uniform_buffer,
            &stream::depth_array_view(&texture),
        );
        stream::encode_playzone_copy(&mut encoder, &pipelines, &playzone_view, &bind_group, 0);
    }
    if let Some(key) = color_key {
        let bind_group = stream::create_focus_area_bind_group(device, &pipelines, &uniform_buffer);
        let rect = depth_mask::hands_view_rect(&view(eye_position), eye_position, &key);
        stream::encode_focus_area(
            &mut encoder,
            &pipelines,
            &focus_area_view,
            &bind_group,
            0,
            stream::focus_area_resolution(UVec2::splat(TARGET_SIZE * 5)),
            rect,
        );
    }
    let cloud_group = if mask_v2.cloud_probe {
        let depth_views = input
            .frame
            .map_or([view(depth_camera_position); 2], |frame| frame.depth_views);
        let cell = mask_v2.cloud_probe_cell.unwrap_or(CLOUD_PROBE_CELL);
        cloud_probe_group(device, queue, &pipelines, depth_views[0], cell)
    } else {
        stream::create_cloud_dummy_read_bind_group(device, queue, &pipelines)
    };
    stream::encode_depth_mask(
        &mut encoder,
        &pipelines,
        &depth_mask_view,
        &depth_bind_group,
        &playzone_bind_group,
        &cloud_group,
        0,
    );

    {
        let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
            label: None,
            color_attachments: &[Some(RenderPassColorAttachment {
                view: &target_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: LoadOp::Clear(Color::TRANSPARENT),
                    store: StoreOp::Store,
                },
            })],
            ..Default::default()
        });

        // Maps the stream quad onto the whole target: uv (0,0) is the top-left corner
        let transform = Mat4::from_scale(Vec3::new(2.0, 2.0, 1.0));

        // Immediate layout: transform, view_idx, passthrough_mode, blend_alpha
        pass.set_pipeline(&pipelines.main);
        pass.set_immediates(0, &floats_to_bytes(&transform.to_cols_array()));
        pass.set_immediates(64, &0_u32.to_le_bytes());
        pass.set_immediates(68, &3_u32.to_le_bytes());
        pass.set_immediates(72, &1_f32.to_le_bytes());
        pass.set_bind_group(0, &view_bind_group, &[]);
        pass.set_bind_group(1, &depth_bind_group, &[]);
        pass.set_bind_group(2, &problem_view.bind_group, &[]);
        pass.draw(0..4, 0..1);

        if overlay_count > 0 {
            stream::encode_overlay(
                &mut pass,
                &pipelines,
                &overlay_bind_group,
                &overlay_eye,
                0,
                overlay_count,
            );
        }
    }

    let readback = device.create_buffer(&BufferDescriptor {
        label: None,
        size: u64::from(TARGET_SIZE * TARGET_SIZE * 4),
        usage: BufferUsages::COPY_DST | BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    encoder.copy_texture_to_buffer(
        target.as_image_copy(),
        TexelCopyBufferInfo {
            buffer: &readback,
            layout: TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(TARGET_SIZE * 4),
                rows_per_image: Some(TARGET_SIZE),
            },
        },
        Extent3d {
            width: TARGET_SIZE,
            height: TARGET_SIZE,
            depth_or_array_layers: 1,
        },
    );
    queue.submit([encoder.finish()]);

    readback
        .slice(..)
        .map_async(wgpu::MapMode::Read, |r| r.unwrap());
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    let data = readback.slice(..).get_mapped_range().to_vec();
    let mask = read_rgba(
        device,
        queue,
        device.create_command_encoder(&Default::default()),
        &depth_mask,
        UVec2::splat(TARGET_SIZE),
    );

    Output {
        pixels: data
            .chunks_exact(4)
            .map(|c| [c[0], c[1], c[2], c[3]])
            .collect(),
        mask: mask
            .chunks_exact(4)
            .map(|c| [c[0], c[1], c[2], c[3]])
            .collect(),
    }
}

/// The cell of `cloud_probe_group` that opens the zone everywhere: on an edge (phi 0) at the largest
/// speed
const CLOUD_PROBE_CELL: [u8; 4] = [128, 255, 255, 128];

/// Group 2 of the depth mask with a field that is `cell` everywhere it is looked up (CLOUD_PROBE_CELL
/// opens the zone everywhere): a 1 x 1 field, both eyes with the camera `camera`, a reach time of 1 s
/// and the default cap (14 texels)
fn cloud_probe_group(
    device: &Device,
    queue: &Queue,
    pipelines: &StreamPipelines,
    camera: ViewParams,
    cell: [u8; 4],
) -> wgpu::BindGroup {
    let texture = device.create_texture(&TextureDescriptor {
        label: None,
        size: Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: TextureDimension::D2,
        format: TextureFormat::Rgba8Unorm,
        usage: TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST,
        view_formats: &[],
    });
    write_texture_2d(queue, &texture, &cell, 4, 1, 0);
    let eye = CloudEyeUniform {
        field: camera,
        prev: camera,
        dt_s: 0.067,
        decay: 0.7,
        reset: false,
    };
    let buffer = crate::motion_cloud_gpu::create_cloud_uniform_buffer(device);
    let data = cloud_uniforms(
        &[Some(eye); 2],
        &CloudParams::default(),
        [1.0; 2],
        [true; 2],
    );
    queue.write_buffer(&buffer, 0, &floats_to_bytes(&data));
    crate::motion_cloud_gpu::create_cloud_read_bind_group(
        device,
        &pipelines.cloud_read_layout,
        &texture.create_view(&TextureViewDescriptor::default()),
        &buffer,
    )
}

macro_rules! gpu_or_skip {
    () => {
        match gpu() {
            Some(gpu) => gpu,
            None => {
                eprintln!("No suitable GPU, skipping");
                return;
            }
        }
    };
}

/// A "hand" in the top half of the view at 0.4 m, background at 3 m
fn hand_scene() -> Scene {
    // Depth rows start at the bottom: the top half of the view is v in [0.5, 1]
    Scene::new(3.0, [0.3, 0.55, 0.7, 0.9], 0.4)
}

#[test]
fn distance_mode_shows_near_objects_only() {
    let (device, queue) = gpu_or_skip!();
    let config = base_config(DepthMaskRegion::Distance { max_distance: 0.9 });

    let out = render(
        &device,
        &queue,
        &config,
        &hand_scene(),
        Vec3::ZERO,
        Vec3::ZERO,
        true,
    );

    // Hand (upper half of the view, view uv origin is top-left): passthrough fully visible
    assert_eq!(out.at(0.5, 0.25), [0, 0, 0, 0]);
    // Background: opaque game image
    let background = out.at(0.5, 0.75);
    assert_eq!(background[3], 255);
    assert!((i32::from(background[0]) - i32::from(STREAM_GRAY)).abs() <= 2);
    // Left of the hand: background
    assert_eq!(out.alpha(0.1, 0.25), 1.0);
}

#[test]
fn color_key_packs_mask_and_hand_area() {
    let (device, queue) = gpu_or_skip!();
    let config = base_config(DepthMaskRegion::Distance { max_distance: 0.9 });

    // A tracked hand (a horizontal bar) below the near object, 0.5 m away: view uv v = 0.75
    let segment = [
        Vec4::new(-0.1, -0.25, -0.5, 0.05),
        Vec4::new(0.1, -0.25, -0.5, 0.05),
    ];
    let out = render_with_key(
        &device,
        &queue,
        &config,
        &hand_scene(),
        Vec3::ZERO,
        Vec3::ZERO,
        true,
        Some(ColorKeyInput {
            hands: [
                None,
                Some((
                    HandShape {
                        segments: [segment; HAND_SEGMENTS],
                    },
                    HandZones {
                        core: -0.01,
                        falloff: 0.01,
                        rim: 0.04,
                        solid: [1.0; HAND_SEGMENTS],
                        sweep: HandSweep::NONE,
                    },
                )),
            ],
            leak: [0.0; 2],
            bridges: [None; 2],
        }),
    );

    // Near object: plain passthrough
    assert_eq!(out.alpha(0.5, 0.25), 0.0);
    // Inside of the hand: also plain passthrough
    for u in [0.4, 0.5, 0.6] {
        assert!(out.alpha(u, 0.75) < 0.01, "{}", out.alpha(u, 0.75));
    }
    // Rim: keyed passthrough. The radius is 0.05 m, about 0.045 in view uv at this angle
    assert!((out.alpha(0.5, 0.835) - 0.5).abs() < 0.02);
    // Game
    assert_eq!(out.alpha(0.85, 0.75), 1.0);
    assert_eq!(out.alpha(0.5, 0.95), 1.0);
    // Alpha is not premultiplied
    for (u, v) in [(0.5, 0.25), (0.5, 0.75), (0.5, 0.835), (0.85, 0.75)] {
        let gray = i32::from(out.at(u, v)[0]);
        assert!((gray - i32::from(STREAM_GRAY)).abs() <= 2, "{gray}");
    }
}

#[test]
fn color_key_upper_layer_alone_matches_both() {
    let (device, queue) = gpu_or_skip!();
    let config = base_config(DepthMaskRegion::Distance { max_distance: 0.9 });
    let no_hands = ColorKeyInput {
        hands: [None; 2],
        leak: [0.0; 2],
        bridges: [None; 2],
    };
    let render_rects = |rects| {
        render_full(
            &device,
            &queue,
            &config,
            &hand_scene(),
            Vec3::ZERO,
            Vec3::ZERO,
            true,
            Some(no_hands),
            [None; 2],
            None,
            rects,
            false,
            &[],
        )
    };
    let whole = render_rects(None);
    let full = render_rects(Some([(Vec2::ZERO, Vec2::ONE); 2]));
    // Only the left half has the lower layer
    let half = render_rects(Some([(Vec2::ZERO, Vec2::new(0.5, 1.0)); 2]));
    // No lower layer (two layers)
    let none = render_rects(Some([(Vec2::ZERO, Vec2::ZERO); 2]));

    // The packed alpha as the compositor shows it, over colors the key doesn't keep
    let lower = |a: f32| (2.0 * a).min(1.0);
    let upper = |a: f32| (2.0 * a - 1.0).max(0.0);
    let both = |a: f32| upper(a) + (1.0 - upper(a)) * lower(a);
    for v in (0..64).map(|i| (i as f32 + 0.5) / 64.0) {
        for u in [0.3, 0.7] {
            assert_eq!(full.at(u, v), whole.at(u, v));
            let a = whole.alpha(u, v);
            let alone = upper(none.alpha(u, v));
            assert!(
                (alone - both(a)).abs() < 0.02,
                "{u} {v}: {alone} {}",
                both(a)
            );
        }
        assert_eq!(half.at(0.3, v), whole.at(0.3, v));
        assert_eq!(half.at(0.7, v), none.at(0.7, v));
    }
    // Game, near object
    assert_eq!(none.alpha(0.5, 0.95), 1.0);
    assert!(upper(none.alpha(0.5, 0.25)) < 0.01);
}

#[test]
fn hand_core_fades_out_gradually() {
    let (device, queue) = gpu_or_skip!();
    let config = base_config(DepthMaskRegion::Distance { max_distance: 0.9 });

    let segment = [
        Vec4::new(-0.1, -0.25, -0.5, 0.05),
        Vec4::new(0.1, -0.25, -0.5, 0.05),
    ];
    let shape = HandShape {
        segments: [segment; HAND_SEGMENTS],
    };
    let zones = HandZones {
        core: -0.03,
        falloff: 0.06,
        rim: 0.06,
        solid: [1.0; HAND_SEGMENTS],
        sweep: HandSweep::NONE,
    };
    let render_zones = |zones| {
        render_with_key(
            &device,
            &queue,
            &config,
            &hand_scene(),
            Vec3::ZERO,
            Vec3::ZERO,
            true,
            Some(ColorKeyInput {
                hands: [Some((shape, zones)), None],
                leak: [0.0; 2],
                bridges: [None; 2],
            }),
        )
    };

    // From the center to the outside: never jumps from solid (0) to keyed (0.5)
    let out = render_zones(zones);
    let alphas = (0..20)
        .map(|i| out.alpha(0.5, 0.75 + i as f32 * 0.01))
        .collect::<Vec<_>>();
    assert!(alphas[0] < 0.01);
    assert!(
        alphas.iter().any(|a| (0.1..0.4).contains(a)),
        "no falloff: {alphas:?}"
    );
    for pair in alphas.windows(2).take(9) {
        assert!((-0.01..0.25).contains(&(pair[1] - pair[0])), "{alphas:?}");
    }

    // Without solid segments everything is keyed
    let keyed = render_zones(HandZones {
        solid: [0.0; HAND_SEGMENTS],
        ..zones
    });
    assert!((keyed.alpha(0.5, 0.75) - 0.5).abs() < 0.01);

    // A half solid segment, all segments of the test hand are the same
    let mut solid = [0.0; HAND_SEGMENTS];
    solid[0] = 0.5;
    let half = render_zones(HandZones { solid, ..zones });
    assert!((half.alpha(0.5, 0.75) - 0.25).abs() < 0.03);

    // A fast hand is keyed, not solid, also further away
    let fast = render_zones(zones.for_speed(Vec3::X * 10.0, 0.02, 0.004, 0.05, None));
    assert!(fast.alpha(0.5, 0.75) > out.alpha(0.5, 0.75) + 0.1);
    assert_eq!(out.alpha(0.5, 0.9), 1.0);
    // Near the end of the rim the key fades out
    let faded = fast.alpha(0.5, 0.9);
    assert!((0.55..0.95).contains(&faded), "{faded}");
    assert!(fast.alpha(0.5, 0.85) < faded);
}

#[test]
fn negative_rim_ends_inside_the_capsule() {
    let (device, queue) = gpu_or_skip!();
    let config = base_config(DepthMaskRegion::Distance { max_distance: 0.9 });

    // Same bar as in color_key_packs_mask_and_hand_area, nothing solid
    let segment = [
        Vec4::new(-0.1, -0.25, -0.5, 0.05),
        Vec4::new(0.1, -0.25, -0.5, 0.05),
    ];
    let render_rim = |rim| {
        render_with_key(
            &device,
            &queue,
            &config,
            &hand_scene(),
            Vec3::ZERO,
            Vec3::ZERO,
            true,
            Some(ColorKeyInput {
                hands: [
                    None,
                    Some((
                        HandShape {
                            segments: [segment; HAND_SEGMENTS],
                        },
                        HandZones {
                            core: -0.01,
                            falloff: 0.01,
                            rim,
                            solid: [0.0; HAND_SEGMENTS],
                            sweep: HandSweep::NONE,
                        },
                    )),
                ],
                leak: [0.0; 2],
                bridges: [None; 2],
            }),
        )
    };

    let narrow = render_rim(-0.01);
    let normal = render_rim(0.0);
    // Center of the bar: keyed
    assert!((narrow.alpha(0.5, 0.75) - 0.5).abs() < 0.01);
    // About 3 mm inside of the edge of the bar: game, only keyed with the rim of 0
    assert_eq!(narrow.alpha(0.5, 0.8), 1.0);
    assert!((normal.alpha(0.5, 0.8) - 0.5).abs() < 0.01);
}

#[test]
fn hand_zones_follow_the_speed() {
    let zones = HandZones {
        core: -0.004,
        falloff: 0.008,
        rim: 0.01,
        solid: [1.0; HAND_SEGMENTS],
        sweep: HandSweep::NONE,
    };

    let still = zones.for_speed(Vec3::ZERO, 0.01, 0.04, 0.05, None);
    assert!((still.core - zones.core).abs() < 1e-6 && (still.rim - zones.rim).abs() < 1e-6);

    // 1 m/s: 1 cm and 4 cm
    let moving = zones.for_speed(Vec3::Y, 0.01, 0.04, 0.05, None);
    assert!((moving.core - -0.014).abs() < 1e-6 && (moving.rim - 0.05).abs() < 1e-6);

    // Limited
    let fast = zones.for_speed(Vec3::Z * 100.0, 0.01, 0.04, 0.05, None);
    assert!((fast.core - -0.044).abs() < 1e-6 && (fast.rim - 0.06).abs() < 1e-6);
}

#[test]
fn hand_zones_shrink_over_leaks() {
    let mut zones = HandZones {
        core: -0.004,
        falloff: 0.004,
        rim: 0.0,
        solid: [1.0; HAND_SEGMENTS],
        sweep: HandSweep::NONE,
    };
    zones.solid[3] = 0.5;

    // No leak: the same
    let same = zones.for_leak(0.0, 0.003);
    assert_eq!(same.rim.to_bits(), zones.rim.to_bits());
    assert_eq!(same.core.to_bits(), zones.core.to_bits());

    // A moving hand over a full leak: the rim ends inside the hand, the core stays
    let moving = zones.for_speed(Vec3::X, 0.01, 0.04, 0.016, None);
    let leaky = moving.for_leak(1.0, 0.003);
    assert!((leaky.rim - -0.003).abs() < 1e-6, "{}", leaky.rim);
    assert_eq!(leaky.core, moving.core);
    assert_eq!(leaky.falloff, moving.falloff);
    assert_eq!(leaky.solid, moving.solid);

    // Half: in between
    let half = moving.for_leak(0.5, 0.003);
    assert!(
        (half.rim - (0.5 * 0.016 - 0.5 * 0.003)).abs() < 1e-6,
        "{}",
        half.rim
    );
}

#[test]
fn hand_zones_sweep_along_the_motion() {
    let zones = HandZones {
        core: -0.004,
        falloff: 0.008,
        rim: 0.002,
        solid: [1.0; HAND_SEGMENTS],
        sweep: HandSweep::NONE,
    };
    let params = SweepParams {
        max: 0.04,
        iso: 0.25,
        bias: 0.0,
    };

    // Still: no sweep, the same as without it
    let still = zones.for_speed(Vec3::ZERO, 0.01, 0.045, 0.016, Some(params));
    assert_eq!(still.sweep, HandSweep::NONE);
    assert_eq!(still.rim, zones.rim);

    // Barely moving (less than 1 mm): in all directions, no sweep
    let slow = zones.for_speed(Vec3::X * 0.02, 0.01, 0.045, 0.016, Some(params));
    assert_eq!(slow.sweep, HandSweep::NONE);
    assert!((slow.rim - (0.002 + 0.0009)).abs() < 1e-6, "{}", slow.rim);

    // 0.4 m/s to the right: 18 mm of growth, a quarter in all directions
    let moving = zones.for_speed(Vec3::X * 0.4, 0.01, 0.045, 0.016, Some(params));
    assert!(
        (moving.rim - (0.002 + 0.0045)).abs() < 1e-6,
        "{}",
        moving.rim
    );
    assert!((moving.core - -0.008).abs() < 1e-6);
    assert!((moving.sweep.direction - Vec3::X).length() < 1e-6);
    assert!((moving.sweep.forward - 0.018).abs() < 1e-6);
    assert_eq!(moving.sweep.forward, moving.sweep.backward);

    // Fast: the sweep and the part in all directions are limited
    let fast = zones.for_speed(Vec3::NEG_Y * 4.0, 0.01, 0.045, 0.016, Some(params));
    assert!((fast.rim - (0.002 + 0.016)).abs() < 1e-6, "{}", fast.rim);
    assert!((fast.sweep.forward - 0.04).abs() < 1e-6);
    assert!((fast.sweep.direction - Vec3::NEG_Y).length() < 1e-6);

    // Bias: further ahead than behind
    let ahead = zones.for_speed(
        Vec3::X * 0.4,
        0.01,
        0.045,
        0.016,
        Some(SweepParams {
            bias: 0.5,
            ..params
        }),
    );
    assert!((ahead.sweep.forward - 0.027).abs() < 1e-6);
    assert!((ahead.sweep.backward - 0.009).abs() < 1e-6);

    // Off: growth in all directions as before
    let off = zones.for_speed(Vec3::X * 0.4, 0.01, 0.045, 0.016, None);
    assert_eq!(off.sweep, HandSweep::NONE);
    assert!((off.rim - 0.018).abs() < 1e-6);

    // Over a leak the sweep shrinks like the rim
    let half = moving.for_leak(0.5, 0.003);
    assert!((half.sweep.forward - 0.009).abs() < 1e-6);
    assert!((half.sweep.backward - 0.009).abs() < 1e-6);
    assert_eq!(half.sweep.direction, moving.sweep.direction);
    assert_eq!(moving.for_leak(1.0, 0.003).sweep.forward, 0.0);
}

#[test]
fn hand_zone_sweeps_along_the_motion() {
    let (device, queue) = gpu_or_skip!();
    let config = base_config(DepthMaskRegion::Distance { max_distance: 0.9 });

    // The bar of color_key_packs_mask_and_hand_area: u 0.35..0.65, v 0.70..0.80, solid inside
    let segment = [
        Vec4::new(-0.1, -0.25, -0.5, 0.05),
        Vec4::new(0.1, -0.25, -0.5, 0.05),
    ];
    let shape = HandShape {
        segments: [segment; HAND_SEGMENTS],
    };
    let zones = HandZones {
        core: -0.01,
        falloff: 0.005,
        rim: 0.0,
        solid: [1.0; HAND_SEGMENTS],
        sweep: HandSweep::NONE,
    };
    let render_zones = |zones| {
        render_with_key(
            &device,
            &queue,
            &config,
            &hand_scene(),
            Vec3::ZERO,
            Vec3::ZERO,
            true,
            Some(ColorKeyInput {
                hands: [None, Some((shape, zones))],
                leak: [0.0; 2],
                bridges: [None; 2],
            }),
        )
    };
    // Moving to the right, 4 cm (0.04 in view uv) ahead, 2 cm behind
    let swept = |forward, backward| HandZones {
        sweep: HandSweep {
            direction: Vec3::X,
            forward,
            backward,
        },
        ..zones
    };

    let still = render_zones(zones);
    let moving = render_zones(swept(0.04, 0.02));

    // Ahead of the bar: keyed, only when moving. The solid fill is not moved
    assert_eq!(still.alpha(0.67, 0.75), 1.0);
    assert!(
        (moving.alpha(0.67, 0.75) - 0.5).abs() < 0.01,
        "{}",
        moving.alpha(0.67, 0.75)
    );
    // Behind it, less far
    assert!(
        (moving.alpha(0.34, 0.75) - 0.5).abs() < 0.01,
        "{}",
        moving.alpha(0.34, 0.75)
    );
    assert_eq!(moving.alpha(0.31, 0.75), 1.0);
    // Beside the motion (below the bar): game as before
    assert_eq!(moving.alpha(0.5, 0.83), 1.0);
    assert_eq!(moving.alpha(0.67, 0.83), 1.0);
    // The solid inside stays where it was
    assert!(moving.alpha(0.5, 0.75) < 0.01);

    // A long sweep (key_sweep_bias 100) reaches beyond the bounding sphere moved half of it: the test
    // of the distance to the hand covers all of it
    let far = render_zones(swept(0.08, 0.0));
    assert!(
        (far.alpha(0.72, 0.75) - 0.5).abs() < 0.01,
        "{}",
        far.alpha(0.72, 0.75)
    );
    assert_eq!(far.alpha(0.75, 0.75), 1.0);
    assert_eq!(far.alpha(0.34, 0.75), 1.0);

    // No sweep, or an empty one: exactly as before. A tiny one: the same within a level
    let empty = render_zones(swept(0.0, 0.0));
    let tiny = render_zones(swept(1e-6, 1e-6));
    for v in (0..64).map(|i| (i as f32 + 0.5) / 64.0) {
        for u in (0..64).map(|i| (i as f32 + 0.5) / 64.0) {
            assert_eq!(empty.at(u, v), still.at(u, v), "{u} {v}");
            let (a, b) = (tiny.at(u, v), still.at(u, v));
            for c in 0..4 {
                assert!(a[c].abs_diff(b[c]) <= 1, "{u} {v}: {a:?} {b:?}");
            }
        }
    }
}

#[test]
fn wrist_bridge_extends_the_hand_zone() {
    let (device, queue) = gpu_or_skip!();
    let config = base_config(DepthMaskRegion::Distance { max_distance: 0.9 });

    // A short bar as the hand, 0.5 m away at v = 0.75, the fingers to the left. The wrist is at its
    // right end (u = 0.45), the forearm goes to the right: 1 cm is 0.01 in view uv.
    let wrist = Vec3::new(-0.05, -0.25, -0.5);
    let segment = [Vec4::new(-0.15, -0.25, -0.5, 0.02), wrist.extend(0.02)];
    let shape = HandShape {
        segments: [segment; HAND_SEGMENTS],
    };
    let zones = HandZones {
        core: -0.01,
        falloff: 0.005,
        rim: 0.0,
        solid: [0.0; HAND_SEGMENTS],
        sweep: HandSweep::NONE,
    };
    let render_bridge = |zones, bridge| {
        render_with_key(
            &device,
            &queue,
            &config,
            &hand_scene(),
            Vec3::ZERO,
            Vec3::ZERO,
            true,
            Some(ColorKeyInput {
                hands: [None, Some((shape, zones))],
                leak: [0.0; 2],
                bridges: [None, bridge],
            }),
        )
    };
    let bridge = |length, solid| WristBridge::new(wrist, Vec3::X, length, 0.025, solid);

    // 4 cm behind the wrist: game without the bridge, keyed with it
    let without = render_bridge(zones, None);
    let with = render_bridge(zones, bridge(0.08, 0.0));
    assert_eq!(without.alpha(0.49, 0.75), 1.0);
    assert!(
        (with.alpha(0.49, 0.75) - 0.5).abs() < 0.01,
        "{}",
        with.alpha(0.49, 0.75)
    );
    // Past its end and beside it: game
    assert_eq!(with.alpha(0.58, 0.75), 1.0);
    assert_eq!(with.alpha(0.49, 0.81), 1.0);
    // The hand is the same
    assert_eq!(with.at(0.4, 0.75), without.at(0.4, 0.75));

    // Long, far out of the bounding sphere of the hand: 20 cm behind the wrist
    let long = render_bridge(zones, bridge(0.25, 0.0));
    assert!((long.alpha(0.65, 0.75) - 0.5).abs() < 0.01);
    assert_eq!(with.alpha(0.65, 0.75), 1.0);

    // Off (debug.alvr.key_wrist_len 0): no bridge, the output is the same as without one. A bridge of
    // a hand that is not there does nothing either.
    assert_eq!(bridge(0.0, 1.0), None);
    let lone = render_with_key(
        &device,
        &queue,
        &config,
        &hand_scene(),
        Vec3::ZERO,
        Vec3::ZERO,
        true,
        Some(ColorKeyInput {
            hands: [None, Some((shape, zones))],
            leak: [0.0; 2],
            bridges: [bridge(0.08, 1.0), None],
        }),
    );
    for v in (0..64).map(|i| (i as f32 + 0.5) / 64.0) {
        for u in (0..64).map(|i| (i as f32 + 0.5) / 64.0) {
            assert_eq!(lone.at(u, v), without.at(u, v), "{u} {v}");
        }
    }

    // Solid only if the camera is sure enough of the direction
    let unsure = render_bridge(
        zones,
        bridge(0.08, WristBridge::solid_target(0.5, 0.6, 1.0)),
    );
    let sure = render_bridge(
        zones,
        bridge(0.08, WristBridge::solid_target(0.7, 0.6, 1.0)),
    );
    let moving = render_bridge(
        zones,
        bridge(0.08, WristBridge::solid_target(0.7, 0.6, 0.0)),
    );
    assert!((unsure.alpha(0.49, 0.75) - 0.5).abs() < 0.01);
    assert!(sure.alpha(0.49, 0.75) < 0.01, "{}", sure.alpha(0.49, 0.75));
    assert!((moving.alpha(0.49, 0.75) - 0.5).abs() < 0.01);
    // The solid core ends inside, the edge is keyed like for the hand
    assert!((sure.alpha(0.49, 0.77) - 0.5).abs() < 0.01);

    // Moving down in the image: the rim of the bridge is swept like the hand
    let swept = HandZones {
        sweep: HandSweep {
            direction: Vec3::NEG_Y,
            forward: 0.04,
            backward: 0.0,
        },
        ..zones
    };
    assert_eq!(with.alpha(0.49, 0.80), 1.0);
    let moving = render_bridge(swept, bridge(0.08, 0.0));
    assert!((moving.alpha(0.49, 0.80) - 0.5).abs() < 0.01);
    // Over a leak it narrows like the hand
    let narrow = render_bridge(zones.for_leak(1.0, 0.012), bridge(0.08, 0.0));
    assert!((narrow.alpha(0.49, 0.75) - 0.5).abs() < 0.01);
    assert_eq!(narrow.alpha(0.49, 0.77), 1.0);
    assert!((with.alpha(0.49, 0.77) - 0.5).abs() < 0.01);
}

// `debug.alvr.marker_hands`: a hand that reaches out of a marker box shows inside it only, with the
// bridge from the wrist
#[test]
fn hand_limited_to_a_marker_box() {
    let (device, queue) = gpu_or_skip!();
    let config = base_config(DepthMaskRegion::Distance { max_distance: 0.9 });

    // The box holds everything left of x = -0.10 (u = 0.40 at 0.5 m)
    let region = depth_mask::RegionOverride {
        volume_to_world: Mat4::from_translation(Vec3::new(-0.6, 0.0, 0.0)),
        half_extents: Vec3::new(0.5, 2.0, 2.0),
    };
    // A bar of short capsules (2 cm, like the bones of the fingers) from x = -0.25 to -0.05 at
    // v = 0.75, the wrist at x = -0.12 and the bridge from it to x = -0.02
    let joint = |x: f32| Vec4::new(x, -0.25, -0.5, 0.015);
    let shape = HandShape {
        segments: std::array::from_fn(|i| {
            let x = -0.25 + (i % 10) as f32 * 0.02;
            [joint(x), joint(x + 0.02)]
        }),
    };
    let zones = HandZones {
        core: -0.01,
        falloff: 0.005,
        rim: 0.0,
        solid: [0.0; HAND_SEGMENTS],
        sweep: HandSweep::NONE,
    };
    let bridge = WristBridge::new(Vec3::new(-0.12, -0.25, -0.5), Vec3::X, 0.1, 0.015, 0.0);
    let render_hand = |shape, bridge| {
        render_with_key(
            &device,
            &queue,
            &config,
            &hand_scene(),
            Vec3::ZERO,
            Vec3::ZERO,
            true,
            Some(ColorKeyInput {
                hands: [None, Some((shape, zones))],
                leak: [0.0; 2],
                bridges: [None, bridge],
            }),
        )
    };
    let plain = render_hand(shape, bridge);
    let (limited_shape, limited_bridge) = depth_mask::limit_hand_to_region(
        &shape,
        &zones,
        bridge,
        &PlacedArea::from(region),
        depth_mask::HAND_REGION_FEATHER,
    );
    let limited = render_hand(limited_shape, limited_bridge);

    // Inside the box, 3 cm and more from its face: keyed as before
    for u in [0.27, 0.3, 0.37] {
        assert_eq!(limited.at(u, 0.75), plain.at(u, 0.75), "{u}");
        assert!((limited.alpha(u, 0.75) - 0.5).abs() < 0.01, "{u}");
    }
    // Outside, 3 cm and more from the face: the hand and the bridge are gone (the game shows)
    for u in [0.43, 0.46] {
        assert!((plain.alpha(u, 0.75) - 0.5).abs() < 0.01, "{u}");
        assert_eq!(limited.alpha(u, 0.75), 1.0, "{u}");
    }
}

#[test]
fn hands_rect_contains_the_bridge() {
    let joints = std::array::from_fn(|i| Vec4::new(0.05 + i as f32 * 0.004, 0.1, -0.5, 0.01));
    let shape = HandShape::from_joints(&joints);
    let zones = HandZones {
        core: 0.0,
        falloff: 0.004,
        rim: 0.01,
        solid: [0.0; HAND_SEGMENTS],
        sweep: HandSweep::NONE,
    };
    let rect = |bridge| {
        let key = ColorKeyInput {
            hands: [None, Some((shape, zones))],
            leak: [0.0; 2],
            bridges: [None, bridge],
        };
        depth_mask::hands_view_rect(&view(Vec3::ZERO), Vec3::ZERO, &key).unwrap()
    };

    // 25 cm to the left from the wrist: to x = -0.2 m, u = 0.3
    let wrist = joints[0].truncate();
    let (min, max) = rect(WristBridge::new(wrist, Vec3::NEG_X, 0.25, 0.02, 0.0));
    let (hand_min, hand_max) = rect(None);
    assert!(min.x < 0.3 - 0.02, "{min}");
    assert!(hand_min.x > 0.4, "{hand_min}");
    assert_eq!(max, hand_max);
}

#[test]
fn hand_shape_ends_at_the_fingertips() {
    let joints: [Vec4; 26] = std::array::from_fn(|i| Vec4::new(i as f32 * 0.03, 0.0, -0.4, 0.01));
    let shape = HandShape::from_joints(&joints);
    let points = shape.segments.as_flattened();

    for (i, joint) in joints.iter().enumerate() {
        if [5, 10, 15, 20, 25].contains(&i) {
            // The round end of the capsule touches the tip
            let end = *joint - Vec4::new(joint.w, 0.0, 0.0, 0.0);
            assert!(points.iter().any(|p| p.distance(end) < 1e-5), "tip {i}");
            assert!(!points.contains(joint), "tip {i}");
        } else {
            assert!(points.contains(joint), "joint {i}");
        }
    }
}

#[test]
fn late_arms_are_moved() {
    let (device, queue) = gpu_or_skip!();
    let config = base_config(DepthMaskRegion::Distance { max_distance: 0.9 });

    // An arm at 0.5 m: in the depth map it covers u 0.40..0.50, v (from the bottom) 0.2..0.8. Since
    // then it moved 10 cm to the right, 0.1 in view uv.
    let scene = Scene::new(3.0, [0.4, 0.2, 0.5, 0.8], 0.5);
    let render_arm = |arm: Option<ArmMotion>| {
        render_full(
            &device,
            &queue,
            &config,
            &scene,
            Vec3::ZERO,
            Vec3::ZERO,
            true,
            None,
            [None, arm],
            None,
            None,
            false,
            &[],
        )
    };
    let motion = |displacement| arm_motion(Vec3::new(0.05, 0.05, -0.5), displacement, 0.0);

    let still = render_arm(Some(motion(Vec3::ZERO)));
    assert_eq!(still.alpha(0.45, 0.48), 0.0);
    assert_eq!(still.alpha(0.55, 0.48), 1.0);

    // Near the wrist: moved
    let moved = render_arm(Some(motion(Vec3::new(0.1, 0.0, 0.0))));
    assert!(moved.alpha(0.55, 0.48) < 0.1, "{}", moved.alpha(0.55, 0.48));
    assert!(moved.alpha(0.43, 0.48) > 0.9, "{}", moved.alpha(0.43, 0.48));

    // Switched off (debug.alvr.arm_shift 0): plain depth, the same as a still arm
    let plain = render_arm(None);
    assert_eq!(plain.alpha(0.45, 0.48), 0.0);
    assert_eq!(plain.alpha(0.55, 0.48), 1.0);

    // 8 cm past the wrist (the palm) the depth is moved too
    assert!(moved.alpha(0.45, 0.37) > 0.9, "{}", moved.alpha(0.45, 0.37));
    // But not with the color key: the runtime removes the hands, there the depth is the surroundings
    let keyed = render_full(
        &device,
        &queue,
        &config,
        &scene,
        Vec3::ZERO,
        Vec3::ZERO,
        true,
        Some(ColorKeyInput {
            hands: [None; 2],
            leak: [0.0; 2],
            bridges: [None; 2],
        }),
        [None, Some(motion(Vec3::new(0.1, 0.0, 0.0)))],
        None,
        None,
        false,
        &[],
    );
    assert_eq!(keyed.alpha(0.45, 0.37), 0.0);
    assert!(keyed.alpha(0.55, 0.48) < 0.1, "{}", keyed.alpha(0.55, 0.48));

    // `arm_shift 2`: the linear weight over 30 cm. A longer arm (y from -0.4 to 0.4 m, the wrist at
    // y = 0.1) that moved 20 cm to the right
    let long_scene = Scene::new(3.0, [0.4, 0.1, 0.5, 0.9], 0.5);
    let render_long = |arm: ArmMotion| {
        render_full(
            &device,
            &queue,
            &config,
            &long_scene,
            Vec3::ZERO,
            Vec3::ZERO,
            true,
            None,
            [None, Some(arm)],
            None,
            None,
            false,
            &[],
        )
    };
    let displacement = Vec3::new(0.2, 0.0, 0.0);
    let wrist = Vec3::new(0.15, 0.1, -0.5);
    let still = render_long(arm_motion(wrist - displacement, Vec3::ZERO, 0.3));
    let linear = render_long(arm_motion(wrist, displacement, 0.3));
    let today = render_long(arm_motion(wrist, displacement, 0.0));
    // Rows about 20 cm (y = -0.1), 31 cm (y = -0.21) and 35 cm (y = -0.25) along the arm from the
    // wrist (the pixel centres are at 20.2, 31.1 and 35.8 cm). At 0.5 m one meter is one view uv, one
    // pixel 1/64 = 0.0156
    let (row_20, row_31, row_35) = (0.6, 0.71, 0.75);
    let shift = |output: &Output, v| output.room_centre(v) - still.room_centre(v);

    // 20 cm along: moved by 1 - 20 / 30 = 1/3 of the displacement (today's smoothstep: 58 %)
    let shift_20 = shift(&linear, row_20);
    assert!((shift_20 - 0.2 / 3.0).abs() < 0.012, "{shift_20}");
    let today_20 = shift(&today, row_20);
    assert!((today_20 - 0.2 * 0.584).abs() < 0.012, "{today_20}");
    // 31 cm along: past the weight, not moved. Today's smoothstep still moves it by 23 % (0.046, 3
    // pixels; measured 3): at least 2 pixels, so a pixel of rounding does not fail it
    let shift_31 = shift(&linear, row_31);
    assert!(shift_31.abs() < 0.005, "{shift_31}");
    let today_31 = shift(&today, row_31);
    assert!(today_31 > 1.5 / TARGET_SIZE as f32, "{today_31}");
    // 35 cm along: not moved either
    let shift_35 = shift(&linear, row_35);
    assert!(shift_35.abs() < 0.005, "{shift_35}");
}

/// The eye at the depth camera, no color key, no playzone: only the arms and the switches of steps 1
/// and 2 of docs/mask-v2-design.md
fn render_mask_v2(
    device: &Device,
    queue: &Queue,
    config: &DepthPassthroughConfig,
    scene: &Scene,
    arms: [Option<ArmMotion>; 2],
    mask_v2: MaskV2,
) -> Output {
    render_v2(
        device,
        queue,
        config,
        scene,
        Vec3::ZERO,
        Vec3::ZERO,
        true,
        None,
        arms,
        None,
        None,
        false,
        &[],
        None,
        0.0,
        0.0,
        mask_v2,
    )
}

// `debug.alvr.edge_cut`: a straight depth border moves inward by 0.37 depth texel at 50 and by 0.74 at
// 80 (B-spline arithmetic of the 3 x 3 blend, confirmed by the check of the design)
#[test]
fn edge_cut_moves_the_border_inward() {
    let (device, queue) = gpu_or_skip!();
    let config = base_config(DepthMaskRegion::Distance { max_distance: 0.9 });

    // An object at 0.5 m, u 0.3..0.7 (texels 19..44), on a background at 3 m. One pixel of the view is
    // one depth texel
    let scene = Scene::new(3.0, [0.3, 0.3, 0.7, 0.7], 0.5);
    // Width of the room in the middle row (pixels = texels), averaged over 8 positions of the depth
    // texels under the pixels, an eighth of a texel apart (a texel is 0.03125 in tangent, the edges
    // are near a tangent of 0.4: about 1.16 times the angle). Without that the pixels would see the
    // edge at only one position, where a shift of a part of a texel hardly shows
    let width = |edge_cut: f32| {
        (0..8)
            .map(|k| {
                let output = render_mask_v2(
                    &device,
                    &queue,
                    &config,
                    &scene,
                    [None; 2],
                    MaskV2 {
                        edge_cut,
                        depth_yaw: k as f32 * 0.03125 / 8.0 / 1.16,
                        ..Default::default()
                    },
                );
                (0..TARGET_SIZE)
                    .map(|x| 1.0 - output.alpha((x as f32 + 0.5) / TARGET_SIZE as f32, 0.5))
                    .sum::<f32>()
            })
            .sum::<f32>()
            / 8.0
    };

    let today = width(0.0);
    // The object is 26 texels wide, today drawn 0.38 texel too big on each side
    assert!((today - 26.75).abs() < 0.3, "{today}");
    // Two borders in the row: half the change of the width is the shift of one
    let shift = |edge_cut| (today - width(edge_cut)) / 2.0;
    let at_50 = shift(0.5);
    assert!((at_50 - 0.37).abs() < 0.08, "{at_50}");
    let at_80 = shift(0.8);
    assert!((at_80 - 0.74).abs() < 0.08, "{at_80}");
    // 20 as a value is today's within a step of the 8 bit output (0 draws today's exactly, 0.2 goes
    // through the general path)
    let at_20 = shift(0.2);
    assert!(at_20.abs() < 0.02, "{at_20}");
}

// The arm of the wrist gap tests: a forearm at 0.5 m, 8 cm wide (x -0.04..0.04), from the bottom of
// the view up to y = -0.094, on a background at 3 m. The runtime removed it from there up to the wrist
// at y = -0.06: a strip of 3.4 cm, as with the hands on a wheel. A gap of 5 cm looks the depth up at
// y = -0.11, a texel inside the arm (one texel is 1.56 cm at 0.5 m)
const GAP_WRIST: Vec3 = Vec3::new(0.0, -0.06, -0.5);
const GAP_LEN: f32 = 0.05;
// Rows of pixels in the strip: 1 cm behind the wrist (y = -0.070, centre of row 36), and 2.6 cm
// (y = -0.086, row 37, half a texel from the arm's edge in the depth: today's mask shows a little room
// there)
const GAP_ROW_CLEAN: f32 = 36.5 / 64.0;
const GAP_ROW_EDGE: f32 = 37.5 / 64.0;

fn gap_arm_scene() -> Scene {
    Scene::new(3.0, [0.46, 0.0, 0.54, 0.41], 0.5)
}

fn gap(wrist: Vec3, length: f32) -> MaskV2 {
    MaskV2 {
        // From the middle knuckle to the wrist: down, up the arm
        arm_gaps: [
            None,
            Some(ArmGap {
                wrist,
                axis: Vec3::NEG_Y,
                length,
            }),
        ],
        ..Default::default()
    }
}

// `debug.alvr.arm_gap_mm` closes the strip behind the wrist with the forearm's depth from up the arm:
// for a still arm, without arm motions (arm_shift 0) and for a moved arm. Nothing beside the arm
#[test]
fn wrist_gap_closes_the_strip_behind_the_wrist() {
    let (device, queue) = gpu_or_skip!();
    let config = base_config(DepthMaskRegion::Distance { max_distance: 0.9 });
    let scene = gap_arm_scene();
    let render = |arms, mask_v2| render_mask_v2(&device, &queue, &config, &scene, arms, mask_v2);
    let strip = [GAP_ROW_CLEAN, GAP_ROW_EDGE];

    // Today: the game in the strip
    let off = render([None; 2], MaskV2::default());
    assert_eq!(off.alpha(0.5, GAP_ROW_CLEAN), 1.0);
    assert!(off.alpha(0.5, GAP_ROW_EDGE) > 0.8);
    // The arm itself is room
    assert_eq!(off.alpha(0.5, 0.75), 0.0);

    let still = arm_motion(GAP_WRIST, Vec3::ZERO, 0.0);
    for arms in [[None; 2], [None, Some(still)]] {
        let on = render(arms, gap(GAP_WRIST, GAP_LEN));
        for v in strip {
            assert!(on.alpha(0.5, v) < 0.1, "{}", on.alpha(0.5, v));
        }
        // Beside the arm, 1.5 cm outside its edge but within the reach of the gap: the depth up the arm
        // there is the far wall, nothing is added
        for v in strip {
            assert_eq!(on.alpha(0.4453, v), 1.0);
            assert_eq!(on.alpha(0.5547, v), 1.0);
        }
        // Not past the wrist, towards the fingers (the hand zone draws them)
        assert_eq!(on.alpha(0.5, 0.52), 1.0);
        // Only adds near the wrist: everywhere else the picture is the same
        for (i, (a, b)) in off.pixels.iter().zip(&on.pixels).enumerate() {
            let (x, y) = (i as u32 % TARGET_SIZE, i as u32 / TARGET_SIZE);
            let near = (26..=38).contains(&x) && (32..=40).contains(&y);
            assert!(near || a == b, "{x} {y}: {a:?} {b:?}");
        }
    }

    // The arm moved since the depth was captured (arm_shift 1): the gap follows the moved arm (5 cm to
    // the right: x 0.01..0.09). Without subtracting the arm's displacement at the lookup the strip
    // would stay the game
    let moved = arm_motion(
        GAP_WRIST + Vec3::new(0.05, 0.0, 0.0),
        Vec3::new(0.05, 0.0, 0.0),
        0.0,
    );
    let on = render([None, Some(moved)], gap(moved.wrist, GAP_LEN));
    for v in strip {
        assert!(on.alpha(0.5547, v) < 0.1, "{}", on.alpha(0.5547, v));
    }
    // 10 cm to the right (x 0.06..0.14): the moved strip is room, and the gap adds nothing at the arm's
    // old place, which stays the game as without the gap (with 5 cm the old place is inside the moved
    // arm's drawn edge, so it cannot be told)
    let moved = arm_motion(
        GAP_WRIST + Vec3::new(0.1, 0.0, 0.0),
        Vec3::new(0.1, 0.0, 0.0),
        0.0,
    );
    let plain = render([None, Some(moved)], MaskV2::default());
    let on = render([None, Some(moved)], gap(moved.wrist, GAP_LEN));
    for v in strip {
        assert!(on.alpha(0.6016, v) < 0.1, "{}", on.alpha(0.6016, v));
        assert!(plain.alpha(0.6016, v) > 0.8, "{}", plain.alpha(0.6016, v));
        assert!(on.alpha(0.5, v) > 0.8, "{}", on.alpha(0.5, v));
        assert_eq!(on.at(0.5, v), plain.at(0.5, v));
    }

    // At 0: off, today's picture
    let zero = render([None; 2], gap(GAP_WRIST, 0.0));
    assert_eq!(zero.pixels, off.pixels);
}

// A short strip (1.8 cm, as on the lap: the arm up to y = -0.078) with a gap of 5 cm: the strip is
// closed, but rays past the wrist, towards the fingers, get nothing although a lookup G further up the
// arm would land on the arm there too (found by review: before the fix 1-2 cm past the wrist was room)
#[test]
fn wrist_gap_stays_behind_the_wrist() {
    let (device, queue) = gpu_or_skip!();
    let config = base_config(DepthMaskRegion::Distance { max_distance: 0.9 });
    let scene = Scene::new(3.0, [0.46, 0.0, 0.54, 0.425], 0.5);
    let render = |mask_v2| render_mask_v2(&device, &queue, &config, &scene, [None; 2], mask_v2);

    let off = render(MaskV2::default());
    let on = render(gap(GAP_WRIST, GAP_LEN));
    // 1 cm behind the wrist (a texel above the arm's edge: today mostly the game): closed
    assert!(off.alpha(0.5, GAP_ROW_CLEAN) > 0.8);
    assert!(
        on.alpha(0.5, GAP_ROW_CLEAN) < 0.1,
        "{}",
        on.alpha(0.5, GAP_ROW_CLEAN)
    );
    // 2.1 cm (row 34) and 3.7 cm (row 33) past the wrist, in the middle and 2.3 cm to the side: the game
    // as today
    for v in [34.5 / 64.0, 33.5 / 64.0] {
        for u in [0.5, 0.4766] {
            assert_eq!(off.alpha(u, v), 1.0);
            assert_eq!(on.alpha(u, v), 1.0, "{u} {v}");
        }
    }
}

// Where the depth up the arm is not an arm (here a desk at 0.75 m, inside the region, 25 cm behind the
// lookup point) the strip behind the wrist stays the game, although the desk moved back to the strip
// would be inside the region
#[test]
fn wrist_gap_needs_an_arm_up_the_arm() {
    let (device, queue) = gpu_or_skip!();
    let config = base_config(DepthMaskRegion::Distance { max_distance: 0.9 });
    // The lower part of the view (up to y = -0.094 at 0.5 m) is the desk, the rest the far wall
    let desk = Scene::new(3.0, [0.0, 0.0, 1.0, 0.41], 0.75);
    let render = |mask_v2| render_mask_v2(&device, &queue, &config, &desk, [None; 2], mask_v2);

    let off = render(MaskV2::default());
    let on = render(gap(GAP_WRIST, GAP_LEN));
    assert_eq!(off.alpha(0.5, GAP_ROW_CLEAN), 1.0);
    assert_eq!(on.pixels, off.pixels);
}

// Both switches unset: the uniform block has zeros where they go, and the picture of a scene with an
// arm, a moved arm and a play area border is the same as with a gap far away (nothing leaks out of the
// new code) and as with the edge cut at its value of today (20 %) within a step of the 8 bit output
#[test]
fn mask_v2_switches_off_are_today() {
    let (device, queue) = gpu_or_skip!();
    let mut config = base_config(DepthMaskRegion::Volume(CockpitVolumeConfig {
        center_x: 0.05,
        center_y: 0.0,
        center_z: -0.6,
        yaw_deg: 10.0,
        width: 0.5,
        height: 0.4,
        depth: 0.5,
    }));
    config.edge_feathering = 0.03;
    let scene = Scene::new(0.8, [0.1, 0.1, 0.6, 0.7], 0.55);
    let arms = [
        Some(arm_motion(
            Vec3::new(0.0, 0.0, -0.55),
            Vec3::new(0.03, 0.0, 0.0),
            0.3,
        )),
        Some(arm_motion(Vec3::new(-0.1, 0.05, -0.5), Vec3::ZERO, 0.0)),
    ];
    let render = |mask_v2| render_mask_v2(&device, &queue, &config, &scene, arms, mask_v2);

    let off = render(MaskV2::default());
    // A gap on an arm 2 m away: no ray passes within its reach. On an arm without a motion, so that the
    // gap's own wrist is used (with a motion the gap starts at the arm's wrist, in view)
    let one_arm = [arms[0], None];
    let off_one = render_mask_v2(&device, &queue, &config, &scene, one_arm, MaskV2::default());
    let far = render_mask_v2(
        &device,
        &queue,
        &config,
        &scene,
        one_arm,
        gap(Vec3::new(2.0, 0.0, -0.5), 0.04),
    );
    assert_eq!(far.pixels, off_one.pixels);
    // A gap of length 0 on both arms is no gap
    let zero = render(MaskV2 {
        arm_gaps: [Some(ArmGap {
            wrist: Vec3::new(0.0, 0.0, -0.55),
            axis: Vec3::NEG_Y,
            length: 0.0,
        }); 2],
        ..Default::default()
    });
    assert_eq!(zero.pixels, off.pixels);
    // The edge cut as a value of 20 %: the general path, at most one step of the output apart
    let at_20 = render(MaskV2 {
        edge_cut: 0.2,
        ..Default::default()
    });
    for (a, b) in off.pixels.iter().zip(&at_20.pixels) {
        for (a, b) in a.iter().zip(b) {
            assert!(a.abs_diff(*b) <= 1, "{a} {b}");
        }
    }
}

// `debug.alvr.lever_boxes`: an open lever window shows the room along every ray through its box, also
// where the depth sees only the far background (a held lever is removed from the depth with the hand),
// pixel for pixel as `LeverWindow::ray_coverage` says. Off, closed or behind the eye: today's mask
#[test]
fn lever_window_shows_the_room_through_its_box() {
    let (device, queue) = gpu_or_skip!();
    let config = base_config(DepthMaskRegion::Distance { max_distance: 0.9 });
    // Only the far wall: the game everywhere today
    let scene = Scene::new(3.0, [0.0, 0.0, 0.0, 0.0], 3.0);
    let render = |lever_windows| {
        render_mask_v2(
            &device,
            &queue,
            &config,
            &scene,
            [None; 2],
            MaskV2 {
                lever_windows,
                ..Default::default()
            },
        )
    };
    let off = render([None; MAX_LEVER_WINDOWS]);
    assert!(off.pixels.iter().all(|pixel| pixel[3] == 255));

    // A box 20 x 16 x 10 cm, 60 cm ahead, to the lower right, turned by 25 degrees
    let window = LeverWindow {
        region: RegionOverride {
            volume_to_world: Mat4::from_rotation_translation(
                Quat::from_rotation_y(25_f32.to_radians()),
                Vec3::new(0.15, -0.1, -0.6),
            ),
            half_extents: Vec3::new(0.1, 0.08, 0.05),
        },
        open: 1.0,
    };
    let mut windows = [None; MAX_LEVER_WINDOWS];
    windows[2] = Some(window);
    let open = render(windows);
    // Pixel for pixel against the Rust copy, except within a pixel of the outline (the mask is filtered)
    let ray = |x: f32, y: f32| {
        let (u, v) = (x / TARGET_SIZE as f32, y / TARGET_SIZE as f32);
        Vec3::new(2.0 * u - 1.0, 1.0 - 2.0 * v, -1.0).normalize()
    };
    let mut inside = 0;
    for y in 0..TARGET_SIZE {
        for x in 0..TARGET_SIZE {
            let (cx, cy) = (x as f32 + 0.5, y as f32 + 0.5);
            let covered = |dx: f32, dy: f32| window.ray_coverage(Vec3::ZERO, ray(cx + dx, cy + dy));
            let centre = covered(0.0, 0.0);
            let near_outline = [(-1.5, 0.0), (1.5, 0.0), (0.0, -1.5), (0.0, 1.5)]
                .iter()
                .any(|&(dx, dy)| covered(dx, dy) != centre);
            if near_outline {
                continue;
            }
            let alpha = open.pixels[(y * TARGET_SIZE + x) as usize][3];
            if centre > 0.0 {
                inside += 1;
                assert_eq!(alpha, 0, "{x} {y}");
            } else {
                assert_eq!(alpha, 255, "{x} {y}");
            }
        }
    }
    assert!(inside > 40, "{inside}");

    // Closed (open 0), or behind the eye: today's mask
    windows[2] = Some(LeverWindow {
        open: 0.0,
        ..window
    });
    assert_eq!(render(windows).pixels, off.pixels);
    windows[2] = Some(LeverWindow {
        region: RegionOverride {
            volume_to_world: Mat4::from_translation(Vec3::new(0.0, 0.0, 0.6)),
            ..window.region
        },
        ..window
    });
    assert_eq!(render(windows).pixels, off.pixels);

    // Fading after the close: the room shows less than fully (coverage 0.15 is inside the cut of the
    // mask edge, 0.05..0.35)
    windows[2] = Some(LeverWindow {
        open: 0.15,
        ..window
    });
    let fading = render(windows);
    let middle = fading.alpha(0.5 + 0.5 * 0.25, 0.5 + 0.5 * 0.1 / 0.6);
    assert!(middle > 0.0 && middle < 1.0, "{middle}");
}

fn arm_motion(wrist: Vec3, displacement: Vec3, weight_len: f32) -> ArmMotion {
    ArmMotion {
        wrist,
        direction: Vec3::NEG_Y,
        displacement,
        weight_len,
        mode: if weight_len > 0.0 { 2 } else { 1 },
        target_ns: 0,
        history_ns: 0,
        wrist_history: wrist,
        extrapolated: 0.0,
    }
}

#[test]
fn hands_rect_contains_the_hands() {
    let joints = std::array::from_fn(|i| Vec4::new(0.05 + i as f32 * 0.004, 0.1, -0.5, 0.01));
    let shape = HandShape::from_joints(&joints);
    let zones = HandZones {
        core: 0.0,
        falloff: 0.004,
        rim: 0.01,
        solid: [1.0; HAND_SEGMENTS],
        sweep: HandSweep::NONE,
    };
    let key = ColorKeyInput {
        hands: [None, Some((shape, zones))],
        leak: [0.0; 2],
        bridges: [None; 2],
    };
    let (min, max) = depth_mask::hands_view_rect(&view(Vec3::ZERO), Vec3::ZERO, &key).unwrap();

    // Tangents of the view are +-1
    for point in shape.segments.as_flattened() {
        let uv = Vec2::new(
            0.5 + 0.5 * point.x / -point.z,
            0.5 - 0.5 * point.y / -point.z,
        )
        .clamp(Vec2::ZERO, Vec2::ONE);
        assert!(
            uv.cmpge(min).all() && uv.cmple(max).all(),
            "{uv} {min} {max}"
        );
    }
    assert!(min.x > 0.1, "{min}");

    // A moving hand: both ends of the sweep. Longer than the range of the playzone around the hand
    let sweep = HandSweep {
        direction: Vec3::Y,
        forward: 0.2,
        backward: 0.15,
    };
    let swept = ColorKeyInput {
        hands: [None, Some((shape, HandZones { sweep, ..zones }))],
        leak: [0.0; 2],
        bridges: [None; 2],
    };
    let (swept_min, swept_max) =
        depth_mask::hands_view_rect(&view(Vec3::ZERO), Vec3::ZERO, &swept).unwrap();
    for point in shape.segments.as_flattened() {
        for shift in [sweep.forward, -sweep.backward] {
            let point = point.truncate() + sweep.direction * shift;
            let uv = Vec2::new(
                0.5 + 0.5 * point.x / -point.z,
                0.5 - 0.5 * point.y / -point.z,
            );
            assert!(
                uv.cmpge(swept_min).all() && uv.cmple(swept_max).all(),
                "{uv} {swept_min} {swept_max}"
            );
        }
    }
    assert!(
        swept_min.y < min.y - 0.05 && swept_max.y > max.y + 0.05,
        "{min} {max} {swept_min} {swept_max}"
    );

    // Behind the viewer or not tracked
    let none = ColorKeyInput {
        hands: [None; 2],
        leak: [0.0; 2],
        bridges: [None; 2],
    };
    assert!(depth_mask::hands_view_rect(&view(Vec3::ZERO), Vec3::ZERO, &none).is_none());
}

#[test]
fn hands_rect_ignores_hands_out_of_view() {
    let rect = |center: Vec3| {
        let joints = [center.extend(0.01); 26];
        let zones = HandZones {
            core: 0.0,
            falloff: 0.004,
            rim: 0.0,
            solid: [0.0; HAND_SEGMENTS],
            sweep: HandSweep::NONE,
        };
        let key = ColorKeyInput {
            hands: [Some((HandShape::from_joints(&joints), zones)), None],
            leak: [0.0; 2],
            bridges: [None; 2],
        };
        depth_mask::hands_view_rect(&view(Vec3::ZERO), Vec3::ZERO, &key)
    };

    // Behind the eye
    assert!(rect(Vec3::new(0.0, 0.0, 0.3)).is_none());
    // Beside the head, below the view, crossing the near plane
    assert!(rect(Vec3::new(0.0, -0.4, 0.0)).is_none());
    // In front of the eye, crossing the near plane: the whole view
    assert_eq!(
        rect(Vec3::new(0.0, 0.0, -0.05)),
        Some((Vec2::ZERO, Vec2::ONE))
    );
    // In front
    let (min, max) = rect(Vec3::new(0.1, 0.0, -0.5)).unwrap();
    assert!(min.x > 0.5 && max.x < 1.0, "{min} {max}");
}

#[test]
fn crop_view_matches_the_full_view() {
    let fov = Fov {
        left: -0.9,
        right: 0.8,
        up: 0.7,
        down: -0.95,
    };
    let resolution = UVec2::new(2144, 2240);
    let size = resolution.as_vec2();
    let lerp = |a: f32, b: f32, f: f32| a.tan() * (1.0 - f) + b.tan() * f;
    let rect = (Vec2::new(0.3, 0.1), Vec2::new(0.5, 0.4));
    const MARGIN: f32 = 0.03;

    for top_origin in [false, true] {
        let (offset, crop_size, crop) =
            depth_mask::crop_view(fov, resolution, rect, MARGIN, top_origin);
        assert!((offset + crop_size).cmple(resolution).all());
        assert!(crop_size.cmplt(resolution).all(), "{crop_size}");

        // Each pixel edge of the crop looks in the same direction as in the full image
        let from_bottom = |row: f32| if top_origin { 1.0 - row } else { row };
        for pixel in [
            offset,
            offset + crop_size,
            offset + crop_size / 2 + UVec2::new(1, 3),
        ] {
            let full = pixel.as_vec2() / size;
            let local = (pixel - offset).as_vec2() / crop_size.as_vec2();
            let full_tan = Vec2::new(
                lerp(fov.left, fov.right, full.x),
                lerp(fov.down, fov.up, from_bottom(full.y)),
            );
            let crop_tan = Vec2::new(
                lerp(crop.left, crop.right, local.x),
                lerp(crop.down, crop.up, from_bottom(local.y)),
            );
            assert!(
                (full_tan - crop_tan).abs().max_element() < 1e-4,
                "{top_origin} {pixel} {full_tan} {crop_tan}"
            );
        }

        // The rect and its margin are inside (view uv: v from the top)
        let min = rect.0 - MARGIN;
        let max = rect.1 + MARGIN;
        assert!(crop.left.tan() <= lerp(fov.left, fov.right, min.x) + 1e-5);
        assert!(crop.right.tan() >= lerp(fov.left, fov.right, max.x) - 1e-5);
        assert!(crop.up.tan() >= lerp(fov.down, fov.up, 1.0 - min.y) - 1e-5);
        assert!(crop.down.tan() <= lerp(fov.down, fov.up, 1.0 - max.y) + 1e-5);
    }

    // The same pixels in view uv
    for top_origin in [false, true] {
        let (offset, crop_size, _) =
            depth_mask::crop_view(fov, resolution, rect, MARGIN, top_origin);
        let (min, max) = depth_mask::crop_view_uv(resolution, offset, crop_size, top_origin);
        assert!(min.cmple(rect.0 - MARGIN).all() && max.cmpge(rect.1 + MARGIN).all());
        let (start, end) = (offset.as_ivec2(), (offset + crop_size).as_ivec2());
        for pixel in [start, end - 1, start - 1, end] {
            // Pixel center in view uv
            let row = if top_origin {
                pixel.y
            } else {
                resolution.y as i32 - 1 - pixel.y
            };
            let uv = (Vec2::new(pixel.x as f32, row as f32) + 0.5) / size;
            let is_inside = uv.cmpge(min).all() && uv.cmplt(max).all();
            let is_cropped = pixel.cmpge(start).all() && pixel.cmplt(end).all();
            assert_eq!(is_inside, is_cropped, "{top_origin} {pixel}");
        }
    }

    // The top of the view is at the last rows in OpenGL
    let top = (Vec2::new(0.4, 0.0), Vec2::new(0.6, 0.1));
    let (offset, crop_size, _) = depth_mask::crop_view(fov, resolution, top, 0.0, false);
    assert_eq!(offset.y + crop_size.y, resolution.y);
    let (offset, _, _) = depth_mask::crop_view(fov, resolution, top, 0.0, true);
    assert_eq!(offset.y, 0);

    // The whole view
    let (offset, crop_size, full) =
        depth_mask::crop_view(fov, resolution, (Vec2::ZERO, Vec2::ONE), MARGIN, false);
    assert_eq!((offset, crop_size), (UVec2::ZERO, resolution));
    for (a, b) in [
        (full.left, fov.left),
        (full.right, fov.right),
        (full.up, fov.up),
        (full.down, fov.down),
    ] {
        assert!((a - b).abs() < 1e-5);
    }
}

#[test]
fn playzone_fills_objects_removed_near_the_hand() {
    let (device, queue) = gpu_or_skip!();
    let config = base_config(DepthMaskRegion::Distance { max_distance: 0.9 });

    // A wheel: a wide bar at 0.6 m. The runtime removed all of it, because of the hand
    let remembered = Scene::new(3.0, [0.1, 0.4, 0.9, 0.6], 0.6);
    let current = Scene::new(3.0, [2.0, 2.0, 2.0, 2.0], 0.6);

    // The hand is in the middle of the wheel
    let segment = [
        Vec4::new(-0.02, 0.0, -0.55, 0.02),
        Vec4::new(0.02, 0.0, -0.55, 0.02),
    ];
    let segments = [segment; HAND_SEGMENTS];
    let key = ColorKeyInput {
        hands: [
            Some((
                HandShape { segments },
                HandZones {
                    core: 0.0,
                    falloff: 0.004,
                    rim: 0.0,
                    solid: [0.0; HAND_SEGMENTS],
                    sweep: HandSweep::NONE,
                },
            )),
            None,
        ],
        leak: [0.0; 2],
        bridges: [None; 2],
    };
    let render_view = |config: &DepthPassthroughConfig, playzone, playzone_view| {
        render_full(
            &device,
            &queue,
            config,
            &current,
            Vec3::ZERO,
            Vec3::ZERO,
            true,
            Some(key),
            [None; 2],
            playzone,
            None,
            playzone_view,
            &[],
        )
    };
    let render_playzone = |playzone| render_view(&config, playzone, false);

    // Beside the hand, 4 cm from its surface
    let without = render_playzone(None);
    assert_eq!(without.alpha(0.555, 0.5), 1.0);

    let with = render_playzone(Some(&remembered));
    assert!(with.alpha(0.555, 0.5) < 0.1, "{}", with.alpha(0.555, 0.5));
    // Above the wheel there is nothing
    assert_eq!(with.alpha(0.5, 0.36), 1.0);
    // Far from the hand the current depth map is right: somebody moved the wheel
    assert_eq!(with.alpha(0.8, 0.5), 1.0);

    // Debug view: what only the playzone shows is half opaque blue in the lower layer, the upper one
    // stays open. Unpacked as by the layers: lower 2a, upper 2a - 1
    let mut tint = config.clone();
    tint.debug_view = DepthPassthroughDebugView::TintMask;
    let tinted = render_view(&tint, Some(&remembered), true);
    let [red, _, blue, _] = tinted.at(0.555, 0.5);
    assert!(blue > 200 && red < 50, "{red} {blue}");
    let alpha = tinted.alpha(0.555, 0.5);
    let lower = (2.0 * alpha).min(1.0);
    let upper = (2.0 * alpha - 1.0).max(0.0);
    assert!((lower - 0.5).abs() < 0.1, "{lower}");
    assert!(upper < 0.01, "{upper}");
    // Off, and far from the hand: as before
    let plain = render_view(&tint, Some(&remembered), false);
    assert!(plain.alpha(0.555, 0.5) < 0.1);
    let far = render_view(&tint, Some(&remembered), true).at(0.8, 0.5);
    assert_eq!(far, plain.at(0.8, 0.5));
    assert_eq!(far[3], 255);
}

// Two layers, one per eye, like the environment depth swapchain
fn depth_array(device: &Device, queue: &Queue, layers: [&Scene; 2]) -> wgpu::Texture {
    let texture = device.create_texture(&TextureDescriptor {
        label: None,
        size: Extent3d {
            width: DEPTH_SIZE,
            height: DEPTH_SIZE,
            depth_or_array_layers: 2,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: TextureDimension::D2,
        format: TextureFormat::R32Float,
        usage: TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST,
        view_formats: &[],
    });
    for (layer, scene) in layers.iter().enumerate() {
        write_texture_2d(
            queue,
            &texture,
            &floats_to_bytes(&scene.depth),
            DEPTH_SIZE * 4,
            DEPTH_SIZE,
            layer as u32,
        );
    }
    texture
}

// `debug.alvr.key_dump` of the render thread: the right eye of the depth in meters, the playzone, and a
// mask whose rows need padding in the buffer
#[test]
fn depth_dump_reads_back_meters_and_masks() {
    let (device, queue) = gpu_or_skip!();
    let config = base_config(DepthMaskRegion::Distance { max_distance: 0.9 });
    let constants = [
        ("ENABLE_SRGB_CORRECTION", 0.0),
        ("ENCODING_GAMMA", 0.0),
        ("ENABLE_FFE", 0.0),
    ];
    let pipelines = StreamPipelines::new(&device, TextureFormat::Rgba8Unorm, &constants, false);

    // The left eye only sees the background: a wrong layer would show
    let background = Scene::new(3.0, [2.0, 2.0, 2.0, 2.0], 0.4);
    let depth_texture = depth_array(&device, &queue, [&background, &hand_scene()]);
    let remembered = Scene::new(2.0, [0.1, 0.1, 0.4, 0.3], 0.6);
    let playzone_texture = depth_array(&device, &queue, [&background, &remembered]);

    let frame = DepthFrame {
        swapchain_index: 0,
        near: NEAR,
        far: f32::INFINITY,
        depth_views: [view(Vec3::ZERO); 2],
    };
    let input = DepthRenderInput {
        frame: Some(frame),
        eye_positions: [Vec3::ZERO; 2],
        color_key: None,
        arms: [None; 2],
        arm_gaps: [None; 2],
        edge_cut: 0.0,
        lever_windows: [None; MAX_LEVER_WINDOWS],
        capture_playzone: false,
        playzone_view: false,
        memory: None,
        cloud: None,
        key_lower_rects: None,
        region_override: None,
        game_dim: 0.0,
        game_see: 0.0,
    };
    let uniforms = depth_mask::depth_uniforms(
        &config,
        &[view(Vec3::ZERO); 2],
        &input,
        Some(&[view(Vec3::ZERO); 2]),
        false,
    );
    let uniform_buffer = stream::create_depth_uniform_buffer(&device);
    queue.write_buffer(&uniform_buffer, 0, &floats_to_bytes(&uniforms));
    let depth_bind_group = |texture| {
        stream::create_depth_bind_group(
            &device,
            &pipelines,
            &uniform_buffer,
            &stream::depth_array_view(texture),
        )
    };

    let sampler = device.create_sampler(&SamplerDescriptor::default());
    let playzone_view = stream::create_playzone_texture(&device, UVec2::splat(DEPTH_SIZE))
        .create_view(&TextureViewDescriptor::default());
    let focus_area_view = stream::create_focus_area_texture(&device, UVec2::splat(5))
        .create_view(&TextureViewDescriptor::default());
    let playzone_bind_group = stream::create_playzone_bind_group(
        &device,
        &pipelines,
        &playzone_view,
        &focus_area_view,
        &sampler,
    );
    // 50 x 30 texels: 200 bytes per row, 256 in the buffer
    let mask = stream::create_depth_mask_texture(&device, UVec2::new(250, 150));
    let dump_pipeline = stream::create_depth_dump_pipeline(&device, &pipelines);

    let mut encoder = device.create_command_encoder(&Default::default());
    stream::encode_playzone_copy(
        &mut encoder,
        &pipelines,
        &playzone_view,
        &depth_bind_group(&playzone_texture),
        1,
    );
    let depth_bind_group = depth_bind_group(&depth_texture);
    stream::encode_depth_mask(
        &mut encoder,
        &pipelines,
        &mask.create_view(&TextureViewDescriptor::default()),
        &depth_bind_group,
        &playzone_bind_group,
        &stream::create_cloud_dummy_read_bind_group(&device, &queue, &pipelines),
        1,
    );
    let readback = DumpReadback::encode(
        &device,
        &mut encoder,
        &dump_pipeline,
        1,
        vec![
            DumpSource::Depth {
                eye: 1,
                depth_bind_group: &depth_bind_group,
                playzone_bind_group: &playzone_bind_group,
                size: UVec2::splat(DEPTH_SIZE),
                file: "depth".into(),
                playzone_file: Some("playzone".into()),
            },
            DumpSource::Rgba {
                texture: mask,
                file: "mask".into(),
            },
        ],
        &frame,
        ("json".into(), "{}".into()),
    );
    queue.submit([encoder.finish()]);
    let readback = readback.start();
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    let files = readback.poll().unwrap().unwrap().into_files();

    let names = files
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(names, ["depth", "playzone", "mask", "json"]);
    let meters = |bytes: &[u8]| {
        bytes
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect::<Vec<_>>()
    };
    // Rows of the depth texture: row 0 is the bottom of the view
    let at = |values: &[f32], u: f32, v: f32| {
        values[(v * DEPTH_SIZE as f32) as usize * DEPTH_SIZE as usize
            + (u * DEPTH_SIZE as f32) as usize]
    };
    let close = |value: f32, expected: f32| (value - expected).abs() < 1e-3 * expected;

    let depth = meters(&files[0].1);
    assert_eq!(depth.len(), (DEPTH_SIZE * DEPTH_SIZE) as usize);
    assert!(close(at(&depth, 0.5, 0.7), 0.4), "{}", at(&depth, 0.5, 0.7));
    assert!(close(at(&depth, 0.1, 0.1), 3.0), "{}", at(&depth, 0.1, 0.1));
    let playzone = meters(&files[1].1);
    assert!(
        close(at(&playzone, 0.25, 0.2), 0.6),
        "{}",
        at(&playzone, 0.25, 0.2)
    );
    assert!(
        close(at(&playzone, 0.7, 0.7), 2.0),
        "{}",
        at(&playzone, 0.7, 0.7)
    );

    // RGBA, row 0 is the top of the view: the hand is in the upper half
    let mask = &files[2].1;
    assert_eq!(mask.len(), 50 * 30 * 4);
    let texel = |x: usize, y: usize| &mask[(y * 50 + x) * 4..][..4];
    assert_eq!(texel(25, 7)[0], 255);
    assert_eq!(texel(25, 22)[0], 0);
    // Depth data everywhere, nothing from the playzone without the color key
    assert_eq!(texel(25, 22)[2], 255);
    assert_eq!(texel(25, 7)[3], 0);
    assert_eq!(files[3].1, b"{}");
}

#[test]
fn thin_objects_are_not_eroded() {
    let (device, queue) = gpu_or_skip!();
    let config = base_config(DepthMaskRegion::Distance { max_distance: 0.9 });

    // A finger, one depth texel wide
    let center = 32.5 / DEPTH_SIZE as f32;
    let half_width = 0.4 / DEPTH_SIZE as f32;
    let scene = Scene::new(
        3.0,
        [center - half_width, 0.55, center + half_width, 0.9],
        0.4,
    );
    let out = render(
        &device,
        &queue,
        &config,
        &scene,
        Vec3::ZERO,
        Vec3::ZERO,
        true,
    );

    assert!(out.alpha(center, 0.25) < 0.1);
    assert_eq!(out.alpha(center - 0.1, 0.25), 1.0);
    assert_eq!(out.alpha(center + 0.1, 0.25), 1.0);
}

#[test]
fn diagonal_edges_are_smooth() {
    let (device, queue) = gpu_or_skip!();
    let config = base_config(DepthMaskRegion::Distance { max_distance: 0.9 });

    // Near surface left of a slanted edge: depth steps one texel every 4 rows
    let mut scene = Scene::new(3.0, [2.0, 2.0, 2.0, 2.0], 0.4);
    for y in 0..DEPTH_SIZE {
        for x in 0..DEPTH_SIZE {
            if x < 24 + y / 4 {
                scene.depth[(y * DEPTH_SIZE + x) as usize] =
                    depth_mask::encode_depth(0.4, NEAR, f32::INFINITY);
            }
        }
    }
    let out = render(
        &device,
        &queue,
        &config,
        &scene,
        Vec3::ZERO,
        Vec3::ZERO,
        true,
    );

    // Covered width of each row, in texels. A blocky mask jumps by a whole texel at once.
    let widths = (8..TARGET_SIZE - 8)
        .map(|y| {
            (0..TARGET_SIZE)
                .map(|x| 1.0 - f32::from(out.pixels[(y * TARGET_SIZE + x) as usize][3]) / 255.0)
                .sum::<f32>()
        })
        .collect::<Vec<_>>();
    for pair in widths.windows(2) {
        let step = pair[0] - pair[1];
        assert!((-0.05..0.7).contains(&step), "step {step}: {widths:?}");
    }
}

#[test]
fn flip_depth_y_mirrors_the_mask() {
    let (device, queue) = gpu_or_skip!();
    let mut config = base_config(DepthMaskRegion::Distance { max_distance: 0.9 });
    config.flip_depth_y = true;

    let out = render(
        &device,
        &queue,
        &config,
        &hand_scene(),
        Vec3::ZERO,
        Vec3::ZERO,
        true,
    );

    assert_eq!(out.alpha(0.5, 0.25), 1.0);
    assert_eq!(out.alpha(0.5, 0.75), 0.0);
}

#[test]
fn missing_depth_shows_only_the_game() {
    let (device, queue) = gpu_or_skip!();
    let config = base_config(DepthMaskRegion::Distance { max_distance: 0.9 });

    let out = render(
        &device,
        &queue,
        &config,
        &hand_scene(),
        Vec3::ZERO,
        Vec3::ZERO,
        false,
    );

    assert!(out.pixels.iter().all(|p| p[3] == 255));
}

#[test]
fn editing_dims_the_game() {
    let (device, queue) = gpu_or_skip!();
    let config = base_config(DepthMaskRegion::Distance { max_distance: 0.9 });
    let draw = |game_dim: f32| {
        render_with_area(
            &device,
            &queue,
            &config,
            &hand_scene(),
            Vec3::ZERO,
            Vec3::ZERO,
            false,
            None,
            [None; 2],
            None,
            None,
            false,
            &[],
            None,
            game_dim,
            0.0,
        )
    };

    // Only the game (no depth): its color times 1 - dim, still opaque
    let plain = draw(0.0);
    let dimmed = draw(0.6);
    for (plain, dimmed) in plain.pixels.iter().zip(&dimmed.pixels) {
        assert_eq!((plain[3], dimmed[3]), (255, 255));
        for channel in 0..3 {
            let expected = f32::from(plain[channel]) * 0.4;
            assert!(
                (f32::from(dimmed[channel]) - expected).abs() <= 1.5,
                "{plain:?} {dimmed:?}"
            );
        }
    }
    assert!(plain.pixels[0][0] > 100);
}

// While editing the room shows through the game outside the mask (`game_see`), in the plain and the
// color key path; inside the mask it shows fully as before, and the overlay stays opaque
#[test]
fn editing_shows_the_room_through_the_game() {
    let (device, queue) = gpu_or_skip!();
    let config = base_config(DepthMaskRegion::Distance { max_distance: 0.9 });
    let (dim, see) = (0.3, 0.45);
    let no_hands = ColorKeyInput {
        hands: [None; 2],
        leak: [0.0; 2],
        bridges: [None; 2],
    };
    let line = OverlaySegment {
        start: Vec3::new(-0.3, -0.3, -1.0),
        end: Vec3::new(0.3, -0.3, -1.0),
        color: OVERLAY_AMBER,
        width: 0.05,
        dashed: false,
    };
    let draw = |key: Option<ColorKeyInput>,
                rects: Option<[(Vec2, Vec2); 2]>,
                overlay: &[OverlaySegment],
                see: f32| {
        render_with_area(
            &device,
            &queue,
            &config,
            &hand_scene(),
            Vec3::ZERO,
            Vec3::ZERO,
            true,
            key,
            [None; 2],
            None,
            rects,
            false,
            overlay,
            None,
            dim,
            see,
        )
    };
    let gray = f32::from(STREAM_GRAY);
    let near = |value: u8, expected: f32| (f32::from(value) - expected).abs() <= 2.0;

    // Plain path, premultiplied: the background is the dimmed game at 1 - see over the room
    let plain = draw(None, None, &[], see);
    let background = plain.at(0.5, 0.75);
    assert!(near(background[3], 255.0 * (1.0 - see)), "{background:?}");
    assert!(
        near(background[0], gray * (1.0 - dim) * (1.0 - see)),
        "{background:?}"
    );
    // The near object (the mask): the room only, as without editing
    assert_eq!(plain.at(0.5, 0.25), [0, 0, 0, 0]);
    // 0: as before
    assert_eq!(draw(None, None, &[], 0.0).alpha(0.5, 0.75), 1.0);

    // The overlay over it: opaque in its color, nothing else is opaque
    let lined = draw(None, None, &[line], see);
    let opaque = lined
        .pixels
        .iter()
        .filter(|pixel| pixel[3] == 255)
        .collect::<Vec<_>>();
    assert!(opaque.len() >= 15, "{}", opaque.len());
    let amber = OVERLAY_AMBER * 255.0;
    for pixel in opaque {
        assert!(
            near(pixel[0], amber.x) && near(pixel[1], amber.y) && near(pixel[2], amber.z),
            "{pixel:?}"
        );
    }
    assert!(plain.pixels.iter().all(|pixel| pixel[3] < 255));

    // Color key, both layers: the lower one at 1 - see, the upper one open (packed: half of it)
    let lower = |a: f32| (2.0 * a).min(1.0);
    let upper = |a: f32| (2.0 * a - 1.0).max(0.0);
    let keyed = draw(Some(no_hands), None, &[], see);
    let a = keyed.alpha(0.5, 0.75);
    assert!(
        (lower(a) - (1.0 - see)).abs() < 0.01 && upper(a) == 0.0,
        "{a}"
    );
    // Not premultiplied: the dimmed game
    assert!(near(keyed.at(0.5, 0.75)[0], gray * (1.0 - dim)));
    assert!(keyed.alpha(0.5, 0.25) < 0.01);
    // Without the lower layer the upper one alone shows the same
    let alone = draw(
        Some(no_hands),
        Some([(Vec2::ZERO, Vec2::ZERO); 2]),
        &[],
        see,
    );
    let a = alone.alpha(0.5, 0.75);
    assert!((upper(a) - (1.0 - see)).abs() < 0.01, "{a}");
    assert!(upper(alone.alpha(0.5, 0.25)) < 0.01);
    // 0: the game opaque in both layers, as before
    assert_eq!(draw(Some(no_hands), None, &[], 0.0).alpha(0.5, 0.75), 1.0);
}

#[test]
fn eye_offset_from_depth_camera_is_compensated() {
    let (device, queue) = gpu_or_skip!();
    let config = base_config(DepthMaskRegion::Distance { max_distance: 0.9 });

    // Depth cameras are a few cm away from the eyes on a real headset
    let out = render(
        &device,
        &queue,
        &config,
        &hand_scene(),
        Vec3::ZERO,
        Vec3::new(0.03, 0.02, -0.01),
        true,
    );

    assert_eq!(out.alpha(0.5, 0.25), 0.0);
    assert_eq!(out.alpha(0.5, 0.75), 1.0);
}

#[test]
fn volume_mode_selects_by_position() {
    let (device, queue) = gpu_or_skip!();

    // Box around the hand only
    let config = base_config(DepthMaskRegion::Volume(CockpitVolumeConfig {
        center_x: 0.0,
        center_y: 0.1,
        center_z: -0.4,
        yaw_deg: 0.0,
        width: 0.5,
        height: 0.5,
        depth: 0.2,
    }));
    let out = render(
        &device,
        &queue,
        &config,
        &hand_scene(),
        Vec3::ZERO,
        Vec3::ZERO,
        true,
    );
    assert_eq!(out.alpha(0.5, 0.25), 0.0);
    assert_eq!(out.alpha(0.5, 0.75), 1.0);

    // Box around the far wall only: hand is game, wall is passthrough
    let config = base_config(DepthMaskRegion::Volume(CockpitVolumeConfig {
        center_x: 0.0,
        center_y: 0.0,
        center_z: -3.0,
        yaw_deg: 30.0,
        width: 20.0,
        height: 20.0,
        depth: 0.5,
    }));
    let out = render(
        &device,
        &queue,
        &config,
        &hand_scene(),
        Vec3::ZERO,
        Vec3::ZERO,
        true,
    );
    assert_eq!(out.alpha(0.5, 0.25), 1.0);
    assert_eq!(out.alpha(0.5, 0.75), 0.0);
}

// The play area on the GPU against its Rust copy: two boxes, one with a dome, cut by a wall
#[test]
fn play_area_mask_matches_the_rust_distance() {
    let (device, queue) = gpu_or_skip!();
    let config = base_config(DepthMaskRegion::Distance { max_distance: 0.1 });
    // A wall 0.6 m away fills the view: 1.2 x 1.2 m, about 2 cm per pixel
    let wall = 0.6;
    let scene = Scene::new(wall, [0.0; 4], wall);

    let mut area = PlayArea::single(AreaBox::new(
        Vec3::new(-0.2, 0.05, 0.0),
        Vec3::new(0.12, 0.2, 0.3),
    ));
    // A half ball on +X, as high as the rim (0.2 m)
    area.set_dome(0, Side::new(0, true), 1.0);
    let added = area.add_box(0, Side::new(1, false)).unwrap();
    area.drag_side(added, Side::new(0, true), 0.45);
    area.move_box(added, Vec3::new(0.0, -0.05, 0.0));
    // A box turned by 35 deg about +Y, its center 15 cm in front of the wall: the wall cuts it 10 cm
    // to the side of where the box without its yaw would be
    let turned = area.add_box(0, Side::new(1, true)).unwrap();
    area.move_box(
        turned,
        Vec3::new(0.48, 0.0, 0.15) - area.boxes()[turned].center,
    );
    area.drag_side(turned, Side::new(0, true), -0.1);
    area.turn_box(turned, 35_f32.to_radians());
    let turned_box = area.boxes()[turned];
    // The area frame is turned and moved: the wall is at z = 0 in it
    let area_to_world =
        Mat4::from_rotation_translation(Quat::from_rotation_z(0.1), Vec3::new(0.0, 0.0, -wall));
    let placed = PlacedArea {
        area_to_world,
        area,
    };
    let out = render_with_area(
        &device,
        &queue,
        &config,
        &scene,
        Vec3::ZERO,
        Vec3::ZERO,
        true,
        None,
        [None; 2],
        None,
        None,
        false,
        &[],
        Some(placed),
        0.0,
        0.0,
    );

    // Pixel centers on the wall (view uv origin top-left, +Y up)
    let (mut inside, mut outside, mut dome, mut in_turned) = (0, 0, 0, 0);
    for y in 0..TARGET_SIZE {
        for x in 0..TARGET_SIZE {
            let u = (x as f32 + 0.5) / TARGET_SIZE as f32;
            let v = (y as f32 + 0.5) / TARGET_SIZE as f32;
            let point = Vec3::new((2.0 * u - 1.0) * wall, (1.0 - 2.0 * v) * wall, -wall);
            let distance = placed.signed_distance(point);
            // A pixel and a half, and the soft edge
            if distance.abs() < 0.04 {
                continue;
            }
            let alpha = out.alpha(u, v);
            if distance < 0.0 {
                assert_eq!(alpha, 0.0, "inside at {x}, {y}: {distance}");
                inside += 1;
                let local = area_to_world.inverse().transform_point3(point);
                if local.x > -0.08 + 0.04 && local.y > 0.0 && local.x < 0.2 {
                    dome += 1;
                }
                // In the turned box but not where the box would be without its yaw
                let unturned = AreaBox {
                    yaw: 0.0,
                    ..turned_box
                };
                if turned_box.signed_distance(local, f32::INFINITY) < -0.02
                    && unturned.signed_distance(local, f32::INFINITY) > 0.02
                {
                    in_turned += 1;
                }
            } else {
                assert_eq!(alpha, 1.0, "outside at {x}, {y}: {distance}");
                outside += 1;
            }
        }
    }
    assert!(
        inside > 200 && outside > 1000 && dome > 20 && in_turned > 10,
        "{inside} {outside} {dome} {in_turned}"
    );

    // One box: the same mask from the settings and as a placed area
    let volume = CockpitVolumeConfig {
        center_x: 0.1,
        center_y: -0.05,
        center_z: -wall,
        yaw_deg: 20.0,
        width: 0.5,
        height: 0.3,
        depth: 0.4,
    };
    let from_settings = render(
        &device,
        &queue,
        &base_config(DepthMaskRegion::Volume(volume.clone())),
        &scene,
        Vec3::ZERO,
        Vec3::ZERO,
        true,
    );
    let region = RegionOverride {
        volume_to_world: Mat4::from_rotation_translation(
            Quat::from_rotation_y(volume.yaw_deg.to_radians()),
            Vec3::new(volume.center_x, volume.center_y, volume.center_z),
        ),
        half_extents: Vec3::new(volume.width, volume.height, volume.depth) * 0.5,
    };
    let from_area = render_with_area(
        &device,
        &queue,
        &config,
        &scene,
        Vec3::ZERO,
        Vec3::ZERO,
        true,
        None,
        [None; 2],
        None,
        None,
        false,
        &[],
        Some(PlacedArea::from(region)),
        0.0,
        0.0,
    );
    assert!(from_settings.pixels.iter().any(|pixel| pixel[3] == 0));
    assert!(from_settings.pixels == from_area.pixels);
}

#[test]
fn lighting_darkens_passthrough() {
    let (device, queue) = gpu_or_skip!();

    // Linear luminance of the gray stream image, approximated like the probe shader
    let gray_linear = (f32::from(STREAM_GRAY) / 255.0).powf(2.2);

    let mut config = base_config(DepthMaskRegion::Distance { max_distance: 0.9 });
    config.lighting = Switch::Enabled(PassthroughLightingConfig {
        base_brightness: 0.8,
        strength: 1.0,
        // The game is half as bright as the reference: brightness = 0.8 * 0.5
        reference_luminance: gray_linear * 2.0,
        min_brightness: 0.1,
        locality: 0.5,
        response_speed: 1.0,
    });

    let out = render(
        &device,
        &queue,
        &config,
        &hand_scene(),
        Vec3::ZERO,
        Vec3::ZERO,
        true,
    );

    // alpha = 1 - brightness
    let alpha = out.alpha(0.5, 0.25);
    assert!((alpha - 0.6).abs() < 0.03, "alpha {alpha}");
    // Still black: the passthrough below is only dimmed
    assert_eq!(&out.at(0.5, 0.25)[..3], &[0, 0, 0]);
    // Game unaffected
    assert_eq!(out.alpha(0.5, 0.75), 1.0);
}

#[test]
fn tint_debug_view_marks_the_narrowed_rim() {
    let (device, queue) = gpu_or_skip!();
    let mut config = base_config(DepthMaskRegion::Distance { max_distance: 0.9 });
    config.debug_view = DepthPassthroughDebugView::TintMask;

    let segment = [
        Vec4::new(-0.1, -0.25, -0.5, 0.05),
        Vec4::new(0.1, -0.25, -0.5, 0.05),
    ];
    let render_leak = |leak| {
        render_with_key(
            &device,
            &queue,
            &config,
            &hand_scene(),
            Vec3::ZERO,
            Vec3::ZERO,
            true,
            Some(ColorKeyInput {
                hands: [
                    None,
                    Some((
                        HandShape {
                            segments: [segment; HAND_SEGMENTS],
                        },
                        HandZones {
                            core: -0.01,
                            falloff: 0.01,
                            rim: 0.04,
                            solid: [0.0; HAND_SEGMENTS],
                            sweep: HandSweep::NONE,
                        },
                    )),
                ],
                leak,
                bridges: [None; 2],
            }),
        )
    };

    // The rim of the hand
    let (u, v) = (0.5, 0.835);
    let plain = render_leak([0.0; 2]).at(u, v);
    assert!(plain[1] > plain[0], "{plain:?}");
    // The value of the other hand doesn't matter
    assert_eq!(render_leak([1.0, 0.0]).at(u, v), plain);

    let red = render_leak([0.0, 1.0]).at(u, v);
    assert!(red[0] > 2 * red[1], "{red:?}");
    assert_eq!(red[3], plain[3]);
}

#[test]
fn tint_debug_view_marks_the_mask() {
    let (device, queue) = gpu_or_skip!();
    let mut config = base_config(DepthMaskRegion::Distance { max_distance: 0.9 });
    config.debug_view = DepthPassthroughDebugView::TintMask;

    let out = render(
        &device,
        &queue,
        &config,
        &hand_scene(),
        Vec3::ZERO,
        Vec3::ZERO,
        true,
    );

    let hand = out.at(0.5, 0.25);
    assert_eq!(hand[3], 0);
    assert!(hand[1] > 50 && hand[0] == 0);
}

// Distance of a point from a segment (pixels)
fn distance_to_segment(point: Vec2, a: Vec2, b: Vec2) -> f32 {
    let t = ((point - a).dot(b - a) / (b - a).length_squared().max(1e-6)).clamp(0.0, 1.0);
    point.distance(a + (b - a) * t)
}

#[test]
fn overlay_draws_the_box_outline_over_the_stream() {
    let (device, queue) = gpu_or_skip!();
    let config = base_config(DepthMaskRegion::Distance { max_distance: 0.9 });

    // Below the eye: front face 1 m away, back face 2 m
    let region = RegionOverride {
        volume_to_world: Mat4::from_translation(Vec3::new(0.0, -0.5, -1.5)),
        half_extents: Vec3::new(0.3, 0.1, 0.5),
    };
    // 3 pixels: 90 degrees over 64 pixels
    let eye = OverlayEye::new(
        Vec3::ZERO,
        Quat::IDENTITY,
        fov(),
        Vec2::splat(TARGET_SIZE as f32),
    );
    let half_width = 1.5;
    let edges = overlay::box_edges(
        &region,
        OVERLAY_AMBER,
        2.0 * half_width / eye.pixels_per_radian,
    );

    let draw = |overlay: &[OverlaySegment]| {
        render_full(
            &device,
            &queue,
            &config,
            &hand_scene(),
            Vec3::ZERO,
            Vec3::ZERO,
            true,
            None,
            [None; 2],
            None,
            None,
            false,
            overlay,
        )
    };
    let plain = draw(&[]);
    let outlined = draw(&edges);

    // Where the edges are in the target (row 0 at the top). The projection of the stream flips y, so
    // the box below the eye is in the upper half here; on the headset the stream is flipped the same way
    let to_pixels = |point: Vec3| {
        let clip = eye.view_proj * point.extend(1.0);
        Vec2::new(clip.x / clip.w + 1.0, 1.0 - clip.y / clip.w) * 0.5 * TARGET_SIZE as f32
    };
    let projected = edges.map(|edge| (to_pixels(edge.start), to_pixels(edge.end)));

    // The upper front edge crosses the hole of the hand: opaque in the line color there
    let row = to_pixels(Vec3::new(0.0, -0.4, -1.0)).y;
    assert!((row - 19.2).abs() < 1e-3, "{row}");
    let row = row as u32;
    let pixel = |out: &Output, x: u32, y: u32| out.pixels[(y * TARGET_SIZE + x) as usize];
    for x in 26..=38 {
        assert_eq!(pixel(&plain, x, row)[3], 0, "not in the hole at {x}");
        let [r, g, b, a] = pixel(&outlined, x, row);
        let expected = OVERLAY_AMBER * 255.0;
        assert!(
            (f32::from(r) - expected.x).abs() <= 1.0
                && (f32::from(g) - expected.y).abs() <= 1.0
                && (f32::from(b) - expected.z).abs() <= 1.0
                && a == 255,
            "{x}: {:?}",
            [r, g, b, a]
        );
    }

    // Away from every line nothing changes
    let mut away = 0;
    for y in 0..TARGET_SIZE {
        for x in 0..TARGET_SIZE {
            let center = Vec2::new(x as f32 + 0.5, y as f32 + 0.5);
            let distance = projected
                .iter()
                .map(|(a, b)| distance_to_segment(center, *a, *b))
                .fold(f32::INFINITY, f32::min);
            if distance > half_width + 1.5 {
                away += 1;
                assert_eq!(pixel(&outlined, x, y), pixel(&plain, x, y), "at {x}, {y}");
            }
        }
    }
    assert!(away > TARGET_SIZE * TARGET_SIZE / 2, "{away}");
}

// A dashed line: dashes of DASH_ON * DASH_PERIOD in the room, gaps between them
#[test]
fn overlay_draws_dashed_lines() {
    let (device, queue) = gpu_or_skip!();
    let config = base_config(DepthMaskRegion::Distance { max_distance: 0.9 });
    let eye = OverlayEye::new(
        Vec3::ZERO,
        Quat::IDENTITY,
        fov(),
        Vec2::splat(TARGET_SIZE as f32),
    );
    // 15 cm ahead: 64 pixels over 30 cm, a period of 8.5 pixels
    let line = OverlaySegment {
        start: Vec3::new(-0.1, 0.0, -0.15),
        end: Vec3::new(0.1, 0.0, -0.15),
        color: OVERLAY_AMBER,
        width: 3.0 / eye.pixels_per_radian,
        dashed: true,
    };
    let draw = |overlay: &[OverlaySegment]| {
        render_full(
            &device,
            &queue,
            &config,
            &hand_scene(),
            Vec3::ZERO,
            Vec3::ZERO,
            true,
            None,
            [None; 2],
            None,
            None,
            false,
            overlay,
        )
    };
    let plain = draw(&[]);
    let dashed = draw(&[line]);
    let solid = draw(&[OverlaySegment {
        dashed: false,
        ..line
    }]);

    // Along the middle row, where the line is (x from 11 to 53)
    let row = TARGET_SIZE / 2;
    let pixel = |out: &Output, x: u32| out.pixels[(row * TARGET_SIZE + x) as usize];
    let (mut drawn, mut gaps, mut runs) = (0, 0, 0);
    let mut previous = false;
    for x in 12..52 {
        assert_ne!(pixel(&solid, x), pixel(&plain, x), "solid at {x}");
        let on = pixel(&dashed, x) == pixel(&solid, x);
        if on {
            drawn += 1;
            if !previous {
                runs += 1;
            }
        } else {
            assert_eq!(pixel(&dashed, x), pixel(&plain, x), "gap at {x}");
            gaps += 1;
        }
        previous = on;
    }
    // 40 pixels: about 4.7 periods, 60 % drawn
    let fraction = drawn as f32 / (drawn + gaps) as f32;
    assert!((fraction - overlay::DASH_ON).abs() < 0.15, "{drawn} {gaps}");
    assert!((4..=6).contains(&runs), "{runs}");
}

// ---------------------------------------------------------------------------------------------
// Room memory (`debug.alvr.mem_roll`, U3 of design/universal/final.md, room_memory.wgsl)
// ---------------------------------------------------------------------------------------------

const MEMORY_SIZE: u32 = 128;
// About one depth image every 70 ms
const MEMORY_DT: f32 = 0.07;

/// Level, at the origin, looking at -z like the depth camera of the scenes
fn level_memory_camera() -> MemoryCamera {
    MemoryCamera {
        position: Vec3::ZERO,
        yaw: 0.0,
        pitch: 0.0,
    }
}

/// A hand of 28 copies of one capsule (xyz: position, w: radius)
fn memory_hand(start: Vec3, end: Vec3, radius: f32) -> HandShape {
    HandShape {
        segments: [[start.extend(radius), end.extend(radius)]; HAND_SEGMENTS],
    }
}

/// The same hand for the color key, so that the zone near it is drawn
fn memory_key(shape: HandShape) -> ColorKeyInput {
    ColorKeyInput {
        hands: [
            Some((
                shape,
                HandZones {
                    core: 0.0,
                    falloff: 0.004,
                    rim: 0.0,
                    solid: [0.0; HAND_SEGMENTS],
                    sweep: HandSweep::NONE,
                },
            )),
            None,
        ],
        leak: [0.0; 2],
        bridges: [None; 2],
    }
}

fn memory_input(hand: Option<HandShape>, reach: f32) -> MemoryInput {
    MemoryInput {
        params: MemoryParams {
            size: MEMORY_SIZE,
            ..Default::default()
        },
        capture_ns: None,
        hands: [hand, None],
        hand_sources: [CaptureHand::Exact, CaptureHand::None],
        reach: [reach, 0.0],
        hands_away: false,
    }
}

/// The texel of the memory camera that sees a world point (rows in the order of the texture, flip_y
/// off), and the point's distance from the camera plane
fn memory_texel(camera: &MemoryCamera, point: Vec3) -> (usize, usize, f32) {
    let view = camera.view_params();
    let local = view.pose.orientation.inverse() * (point - view.pose.position);
    let tangents = depth_mask::fov_tangents(view.fov);
    let tan = Vec2::new(local.x, local.y) / -local.z;
    let uv = (tan - Vec2::new(tangents.x, tangents.z))
        / (Vec2::new(tangents.y, tangents.w) - Vec2::new(tangents.x, tangents.z));
    let texel = (uv * MEMORY_SIZE as f32).floor();
    (texel.x as usize, texel.y as usize, -local.z)
}

/// The world point of a memory texel at its remembered distance
fn memory_point(camera: &MemoryCamera, x: usize, y: usize, z: f32) -> Vec3 {
    let view = camera.view_params();
    let tangents = depth_mask::fov_tangents(view.fov);
    let uv = (Vec2::new(x as f32, y as f32) + 0.5) / MEMORY_SIZE as f32;
    let tan_min = Vec2::new(tangents.x, tangents.z);
    let tan = tan_min + (Vec2::new(tangents.y, tangents.w) - tan_min) * uv;
    view.pose.position + view.pose.orientation * Vec3::new(tan.x * z, tan.y * z, -z)
}

/// The room memory of one eye (the left) on the desktop GPU, with the passes of the renderer
struct MemoryRig {
    pipelines: StreamPipelines,
    memory: MemoryPipelines,
    textures: [wgpu::TextureView; 2],
    current: usize,
    memory_uniforms: wgpu::Buffer,
    depth_uniforms: wgpu::Buffer,
}

impl MemoryRig {
    fn new(device: &Device) -> Self {
        let constants = [
            ("ENABLE_SRGB_CORRECTION", 0.0),
            ("ENCODING_GAMMA", 0.0),
            ("ENABLE_FFE", 0.0),
        ];
        let pipelines = StreamPipelines::new(device, TextureFormat::Rgba8Unorm, &constants, false);
        let memory = MemoryPipelines::new(device, &pipelines);
        let textures = [0, 1].map(|_| {
            create_memory_texture(device, MEMORY_SIZE)
                .create_view(&TextureViewDescriptor::default())
        });

        Self {
            pipelines,
            memory,
            textures,
            current: 0,
            memory_uniforms: create_memory_uniform_buffer(device),
            depth_uniforms: stream::create_depth_uniform_buffer(device),
        }
    }

    // The depth uniforms of a frame whose depth image was taken from `depth_view`, the eye at the
    // origin, the memory read with `memory` through `camera`
    fn write_depth_uniforms(
        &self,
        queue: &Queue,
        depth_view: ViewParams,
        key: Option<ColorKeyInput>,
        arms: [Option<ArmMotion>; 2],
        memory: Option<MemoryInput>,
        camera: Option<&MemoryCamera>,
    ) {
        let input = DepthRenderInput {
            frame: Some(DepthFrame {
                swapchain_index: 0,
                near: NEAR,
                far: f32::INFINITY,
                depth_views: [depth_view; 2],
            }),
            eye_positions: [Vec3::ZERO; 2],
            color_key: key,
            arms,
            arm_gaps: [None; 2],
            edge_cut: 0.0,
            lever_windows: [None; MAX_LEVER_WINDOWS],
            capture_playzone: false,
            playzone_view: false,
            memory,
            cloud: None,
            key_lower_rects: None,
            region_override: None,
            game_dim: 0.0,
            game_see: 0.0,
        };
        let config = base_config(DepthMaskRegion::Distance { max_distance: 0.9 });
        let playzone = camera.map(|camera| [camera.view_params(); 2]);
        let uniforms = depth_mask::depth_uniforms(
            &config,
            &[view(Vec3::ZERO); 2],
            &input,
            playzone.as_ref(),
            false,
        );
        queue.write_buffer(&self.depth_uniforms, 0, &floats_to_bytes(&uniforms));
    }

    /// One update (or re-projection) of the left eye with the depth image `scene` seen from `depth_view`
    #[expect(clippy::too_many_arguments)]
    fn step(
        &mut self,
        device: &Device,
        queue: &Queue,
        scene: &Scene,
        depth_view: ViewParams,
        camera: &MemoryCamera,
        source: Option<&MemoryCamera>,
        hand: Option<HandShape>,
        mode: u32,
    ) {
        self.write_depth_uniforms(queue, depth_view, None, [None; 2], None, None);
        let params = MemoryParams {
            size: MEMORY_SIZE,
            ..Default::default()
        };
        queue.write_buffer(
            &self.memory_uniforms,
            0,
            &floats_to_bytes(&memory_uniforms(
                camera,
                source,
                &params,
                &[hand, None],
                MEMORY_DT,
            )),
        );
        let depth_texture = depth_array(device, queue, [scene, scene]);
        let depth_bind_group = stream::create_depth_bind_group(
            device,
            &self.pipelines,
            &self.depth_uniforms,
            &stream::depth_array_view(&depth_texture),
        );
        let source_group = create_memory_bind_group(
            device,
            &self.memory,
            &self.textures[self.current],
            &self.memory_uniforms,
        );
        let mut encoder = device.create_command_encoder(&Default::default());
        encode_memory_pass(
            &mut encoder,
            &self.memory.update,
            &self.textures[1 - self.current],
            &depth_bind_group,
            &source_group,
            0,
            mode,
        );
        queue.submit([encoder.finish()]);
        self.current = 1 - self.current;
    }

    fn update(
        &mut self,
        device: &Device,
        queue: &Queue,
        scene: &Scene,
        camera: &MemoryCamera,
        hand: Option<HandShape>,
    ) {
        self.step(
            device,
            queue,
            scene,
            view(Vec3::ZERO),
            camera,
            None,
            hand,
            MODE_UPDATE,
        );
    }

    fn read_target(
        &self,
        device: &Device,
        queue: &Queue,
        pipeline: &wgpu::RenderPipeline,
        size: UVec2,
    ) -> Vec<u8> {
        let target = device.create_texture(&TextureDescriptor {
            label: None,
            size: Extent3d {
                width: size.x,
                height: size.y,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: TextureDimension::D2,
            format: TextureFormat::Rgba8Unorm,
            usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        // Any depth image: these passes don't read it
        let depth_texture = depth_array(device, queue, [&hand_scene(), &hand_scene()]);
        let depth_bind_group = stream::create_depth_bind_group(
            device,
            &self.pipelines,
            &self.depth_uniforms,
            &stream::depth_array_view(&depth_texture),
        );
        let memory_group = create_memory_bind_group(
            device,
            &self.memory,
            &self.textures[self.current],
            &self.memory_uniforms,
        );
        let mut encoder = device.create_command_encoder(&Default::default());
        encode_memory_pass(
            &mut encoder,
            pipeline,
            &target.create_view(&TextureViewDescriptor::default()),
            &depth_bind_group,
            &memory_group,
            0,
            MODE_UPDATE,
        );
        read_rgba(device, queue, encoder, &target, size)
    }

    /// Every texel (r, g, b, a), through the pass of the depth dump. Rows in the order of the texture
    fn read(&self, device: &Device, queue: &Queue) -> Vec<[f32; 4]> {
        let bytes = self.read_target(
            device,
            queue,
            &self.memory.dump,
            UVec2::new(2 * MEMORY_SIZE, MEMORY_SIZE),
        );
        bytes
            .chunks_exact(8)
            .map(|texel| {
                std::array::from_fn(|i| {
                    crate::depth_dump::f16_to_f32(u16::from_le_bytes([
                        texel[2 * i],
                        texel[2 * i + 1],
                    ]))
                })
            })
            .collect()
    }

    fn stats(&self, device: &Device, queue: &Queue) -> MemoryStats {
        let cells = crate::room_memory::MEMORY_STATS_CELLS;
        let bytes = self.read_target(
            device,
            queue,
            &self.memory.stats,
            UVec2::new(2 * cells, cells),
        );
        MemoryStats::decode(&bytes, (2 * cells * 4) as usize).unwrap()
    }

    /// The depth mask of the left eye (r: coverage) over the live depth `live`, with the zone near the
    /// key's hand, the memory read with `memory` (None: every texel, as today's playzone) through
    /// `camera`. Rows from the top of the view
    #[expect(clippy::too_many_arguments)]
    fn mask(
        &self,
        device: &Device,
        queue: &Queue,
        live: &Scene,
        key: ColorKeyInput,
        arms: [Option<ArmMotion>; 2],
        memory: Option<MemoryInput>,
        camera: &MemoryCamera,
        reach: f32,
    ) -> Vec<[u8; 4]> {
        self.write_depth_uniforms(
            queue,
            view(Vec3::ZERO),
            Some(key),
            arms,
            memory,
            Some(camera),
        );
        // With no memory input the zone is not grown in the shader: the rect is today's then
        let resolution = UVec2::splat(TARGET_SIZE * 5);
        let mask = stream::create_depth_mask_texture(device, resolution);
        let focus_area = stream::create_focus_area_texture(device, resolution)
            .create_view(&TextureViewDescriptor::default());
        let sampler = device.create_sampler(&SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let read_group = stream::create_playzone_bind_group(
            device,
            &self.pipelines,
            &self.textures[self.current],
            &focus_area,
            &sampler,
        );
        let depth_texture = depth_array(device, queue, [live, live]);
        let depth_bind_group = stream::create_depth_bind_group(
            device,
            &self.pipelines,
            &self.depth_uniforms,
            &stream::depth_array_view(&depth_texture),
        );
        let focus_group =
            stream::create_focus_area_bind_group(device, &self.pipelines, &self.depth_uniforms);
        let rect = depth_mask::hands_view_rect_reach(
            &view(Vec3::ZERO),
            Vec3::ZERO,
            &key,
            [if memory.is_some() { reach } else { 0.0 }, 0.0],
        );
        let mut encoder = device.create_command_encoder(&Default::default());
        stream::encode_focus_area(
            &mut encoder,
            &self.pipelines,
            &focus_area,
            &focus_group,
            0,
            stream::focus_area_resolution(resolution),
            rect,
        );
        stream::encode_depth_mask(
            &mut encoder,
            &self.pipelines,
            &mask.create_view(&TextureViewDescriptor::default()),
            &depth_bind_group,
            &read_group,
            &stream::create_cloud_dummy_read_bind_group(device, queue, &self.pipelines),
            0,
        );
        read_rgba(device, queue, encoder, &mask, UVec2::splat(TARGET_SIZE))
            .chunks_exact(4)
            .map(|c| [c[0], c[1], c[2], c[3]])
            .collect()
    }
}

// Copies an RGBA8 texture into a buffer after `encoder` and reads it, rows without padding
fn read_rgba(
    device: &Device,
    queue: &Queue,
    mut encoder: wgpu::CommandEncoder,
    texture: &wgpu::Texture,
    size: UVec2,
) -> Vec<u8> {
    let row = (size.x * 4) as usize;
    let bytes_per_row = (size.x * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
    let readback = device.create_buffer(&BufferDescriptor {
        label: None,
        size: u64::from(bytes_per_row * size.y),
        usage: BufferUsages::COPY_DST | BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        TexelCopyBufferInfo {
            buffer: &readback,
            layout: TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: Some(size.y),
            },
        },
        Extent3d {
            width: size.x,
            height: size.y,
            depth_or_array_layers: 1,
        },
    );
    queue.submit([encoder.finish()]);
    readback
        .slice(..)
        .map_async(wgpu::MapMode::Read, |r| r.unwrap());
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    readback
        .slice(..)
        .get_mapped_range()
        .chunks_exact(bytes_per_row as usize)
        .flat_map(|padded| padded[..row].to_vec())
        .collect()
}

/// Coverage of the mask at view uv (origin top-left)
fn mask_at(mask: &[[u8; 4]], u: f32, v: f32) -> u8 {
    let x = ((u * TARGET_SIZE as f32) as usize).min(TARGET_SIZE as usize - 1);
    let y = ((v * TARGET_SIZE as f32) as usize).min(TARGET_SIZE as usize - 1);
    mask[y * TARGET_SIZE as usize + x][0]
}

// The point of the scenes at depth-view ndc uv (origin bottom-left) and distance `z` from the plane of
// the camera at the origin
fn scene_point(u: f32, v: f32, z: f32) -> Vec3 {
    Vec3::new((2.0 * u - 1.0) * z, (2.0 * v - 1.0) * z, -z)
}

// The user's ghost: a bottle (the shifter) moved while the hand stays 15 cm away. After one update its
// old place is the game and its new place the room. Today's copy (taken while no hand was near) would
// still have the bottle at the old place
#[test]
fn memory_drops_a_moved_object_away_from_the_hand() {
    let (device, queue) = gpu_or_skip!();
    let mut rig = MemoryRig::new(&device);
    let camera = level_memory_camera();

    // At 0.6 m: x -0.36..-0.18 before, 0.12..0.30 after
    let before = Scene::new(3.0, [0.2, 0.4, 0.35, 0.6], 0.6);
    let after = Scene::new(3.0, [0.6, 0.4, 0.75, 0.6], 0.6);
    // The hand 15 cm from the old place, 11 cm from the new one, beyond the margin of 6 cm
    let hand = memory_hand(Vec3::new(-0.02, 0.0, -0.6), Vec3::new(0.0, 0.0, -0.6), 0.01);
    rig.update(&device, &queue, &before, &camera, None);
    let old_place = scene_point(0.275, 0.5, 0.6);
    let new_place = scene_point(0.675, 0.5, 0.6);
    let texel = |memory: &[[f32; 4]], point: Vec3| {
        let (x, y, _) = memory_texel(&camera, point);
        memory[y * MEMORY_SIZE as usize + x]
    };
    let memory = rig.read(&device, &queue);
    assert!(
        (texel(&memory, old_place)[0] - 0.6).abs() < 0.01,
        "{:?}",
        texel(&memory, old_place)
    );

    rig.update(&device, &queue, &after, &camera, Some(hand));
    let memory = rig.read(&device, &queue);
    let [z, _, frozen, age] = texel(&memory, old_place);
    assert!(
        (z - 3.0).abs() < 0.05 && frozen == 0.0 && age == 0.0,
        "{z} {frozen} {age}"
    );
    let [z, _, frozen, _] = texel(&memory, new_place);
    assert!((z - 0.6).abs() < 0.01 && frozen == 0.0, "{z} {frozen}");
    // Frozen only where the depth camera looks past the hand: the background behind it, never the bottle
    let frozen = memory
        .iter()
        .filter(|texel| texel[2] == 1.0)
        .collect::<Vec<_>>();
    assert!(!frozen.is_empty());
    assert!(frozen.iter().all(|texel| (texel[0] - 3.0).abs() < 0.05));

    // The mask, with the zone near the hand grown over both places: the old place is the game, the new
    // one the room. Mask rows start at the top of the view (ndc v 0.5 is the middle)
    let input = memory_input(Some(hand), 0.4);
    let mask = rig.mask(
        &device,
        &queue,
        &after,
        memory_key(hand),
        [None; 2],
        Some(input),
        &camera,
        0.4,
    );
    assert_eq!(mask_at(&mask, 0.275, 0.5), 0);
    assert_eq!(mask_at(&mask, 0.675, 0.5), 255);
}

// The held bottle: the runtime removes it with the hand (the far background in its place). Within the
// margin of the hand it stays whole in the memory over 50 updates and is drawn
#[test]
fn memory_keeps_a_held_object_whole() {
    let (device, queue) = gpu_or_skip!();
    let mut rig = MemoryRig::new(&device);
    let camera = level_memory_camera();

    // At 0.5 m: x -0.05..0.05, y -0.15..0.15
    let bottle = Scene::new(3.0, [0.45, 0.35, 0.55, 0.65], 0.5);
    let removed = Scene::new(3.0, [2.0, 2.0, 2.0, 2.0], 0.5);
    // The hand round the bottle: every point of it within 5 cm of the capsule
    let hand = memory_hand(
        Vec3::new(0.0, -0.12, -0.47),
        Vec3::new(0.0, 0.12, -0.47),
        0.02,
    );
    rig.update(&device, &queue, &bottle, &camera, None);
    let bottle_texels = |memory: &[[f32; 4]]| {
        memory
            .iter()
            .filter(|texel| (texel[0] - 0.5).abs() < 0.01)
            .count()
    };
    let whole = bottle_texels(&rig.read(&device, &queue));
    // 0.1 x 0.3 m at 0.5 m: 0.2 x 0.6 in tangents of 3.46 / 128
    assert!((whole as f32 - 7.4 * 22.2).abs() < 40.0, "{whole}");

    for _ in 0..50 {
        rig.update(&device, &queue, &removed, &camera, Some(hand));
    }
    let memory = rig.read(&device, &queue);
    assert_eq!(bottle_texels(&memory), whole);
    let held = memory
        .iter()
        .filter(|texel| (texel[0] - 0.5).abs() < 0.01)
        .collect::<Vec<_>>();
    assert!(held.iter().all(|texel| texel[2] == 1.0));
    // 50 updates of 70 ms since the last live write
    assert!(
        held.iter().all(|texel| (texel[3] - 3.5).abs() < 0.1),
        "{:?}",
        held[0]
    );

    // The left eye's stats: the frozen share (the bottle and the background just behind the hand)
    let stats = rig.stats(&device, &queue);
    let frozen = memory.iter().filter(|texel| texel[2] == 1.0).count();
    let share = frozen as f32 / (MEMORY_SIZE * MEMORY_SIZE) as f32;
    assert!(
        (stats.frozen - share).abs() < 0.002,
        "{} {share}",
        stats.frozen
    );
    assert!(
        (stats.oldest_frozen_s.unwrap() - 3.5).abs() < 0.3,
        "{stats:?}"
    );

    // Drawn: the whole bottle, also towards its ends and sides (2 memory texels from its edges)
    let input = memory_input(Some(hand), 0.0);
    let mask = rig.mask(
        &device,
        &queue,
        &removed,
        memory_key(hand),
        [None; 2],
        Some(input),
        &camera,
        0.0,
    );
    for v in [0.5, 0.4, 0.62] {
        assert_eq!(mask_at(&mask, 0.5, v), 255, "{v}");
        assert!(mask_at(&mask, 0.52, v) > 250, "{v}");
    }
    // Beside it the game
    assert_eq!(mask_at(&mask, 0.6, 0.5), 0);
}

// The arm (here the depth's forearm below the wrist) is written outside the margin, never frozen: when
// the mask moves the live arm by the wrist's motion (step 1 of the arm plan), the memory doesn't draw
// it a second time at its old place. Read like today's copy (every texel) it would
#[test]
fn memory_never_draws_the_arm_twice() {
    let (device, queue) = gpu_or_skip!();
    let mut rig = MemoryRig::new(&device);
    let camera = level_memory_camera();

    // A forearm at 0.5 m, x -0.05..0.05, from the bottom of the view up to the wrist at y -0.05
    let arm = Scene::new(3.0, [0.45, 0.0, 0.55, 0.45], 0.5);
    let hand = memory_hand(
        Vec3::new(0.0, -0.04, -0.5),
        Vec3::new(0.0, 0.04, -0.5),
        0.01,
    );
    for _ in 0..5 {
        rig.update(&device, &queue, &arm, &camera, Some(hand));
    }
    let memory = rig.read(&device, &queue);
    // 15 cm and more below the wrist: never frozen. Next to the hand a stub may be
    for v in [0.05, 0.15, 0.25, 0.3] {
        let (x, y, _) = memory_texel(&camera, scene_point(0.5, v, 0.5));
        let texel = memory[y * MEMORY_SIZE as usize + x];
        assert!(
            (texel[0] - 0.5).abs() < 0.01 && texel[2] == 0.0,
            "{v}: {texel:?}"
        );
    }
    let (x, y, _) = memory_texel(&camera, scene_point(0.5, 0.44, 0.5));
    assert_eq!(memory[y * MEMORY_SIZE as usize + x][2], 1.0);

    // The arm moved 10 cm to the right since the depth was captured
    let motion = ArmMotion {
        wrist: Vec3::new(0.1, -0.05, -0.5),
        direction: Vec3::NEG_Y,
        displacement: Vec3::new(0.1, 0.0, 0.0),
        weight_len: 1.0,
        mode: 2,
        target_ns: 0,
        history_ns: 0,
        wrist_history: Vec3::ZERO,
        extrapolated: 0.0,
    };
    let moved_hand = memory_hand(
        Vec3::new(0.1, -0.04, -0.5),
        Vec3::new(0.1, 0.04, -0.5),
        0.01,
    );
    let input = memory_input(Some(hand), 0.3);
    let render = |memory| {
        rig.mask(
            &device,
            &queue,
            &arm,
            memory_key(moved_hand),
            [Some(motion), None],
            memory,
            &camera,
            0.3,
        )
    };
    // 15 cm below the wrist (ndc v 0.3): the moved arm is drawn (moved 8.5 cm at that weight), its old
    // place is the game
    let mask = render(Some(input));
    assert_eq!(mask_at(&mask, 0.5, 0.7), 0);
    assert_eq!(mask_at(&mask, 0.585, 0.7), 255);
    // Every texel read: the old arm too (the zone near the hand reaches it with the reach of the input,
    // which the rect of today's read doesn't have, so it is given here as the grown key)
    let grown = memory_key(memory_hand(
        Vec3::new(0.1, -0.35, -0.5),
        Vec3::new(0.1, 0.04, -0.5),
        0.15,
    ));
    let mask = rig.mask(
        &device,
        &queue,
        &arm,
        grown,
        [Some(motion), None],
        None,
        &camera,
        0.0,
    );
    assert_eq!(mask_at(&mask, 0.5, 0.7), 255);
}

// With `mem_roll` off (no memory input) the playzone is read as before: every texel, the zone near the
// hand as before, no other uniform. Today's copy (frozen 0 in every texel) shows the wheel next to the
// hand; read as the memory (only frozen texels) it would not
#[test]
fn memory_off_reads_the_playzone_as_today() {
    let (device, queue) = gpu_or_skip!();
    let config = base_config(DepthMaskRegion::Distance { max_distance: 0.9 });
    let remembered = Scene::new(3.0, [0.1, 0.4, 0.9, 0.6], 0.6);
    let current = Scene::new(3.0, [2.0, 2.0, 2.0, 2.0], 0.6);
    let shape = memory_hand(
        Vec3::new(-0.02, 0.0, -0.55),
        Vec3::new(0.02, 0.0, -0.55),
        0.02,
    );
    let render = |memory| {
        render_v2(
            &device,
            &queue,
            &config,
            &current,
            Vec3::ZERO,
            Vec3::ZERO,
            true,
            Some(memory_key(shape)),
            [None; 2],
            Some(&remembered),
            None,
            false,
            &[],
            None,
            0.0,
            0.0,
            MaskV2 {
                memory,
                ..Default::default()
            },
        )
    };
    let off = render(None);
    // As in playzone_fills_objects_removed_near_the_hand
    assert!(off.alpha(0.555, 0.5) < 0.1);
    assert_eq!(off.alpha(0.8, 0.5), 1.0);
    let on = render(Some(memory_input(Some(shape), 0.0)));
    assert_eq!(on.alpha(0.555, 0.5), 1.0);
    // Away from the hand both are the same, pixel for pixel
    for u in [0.1, 0.3, 0.8] {
        assert_eq!(off.at(u, 0.2), on.at(u, 0.2));
    }

    // The uniforms: without the memory its vector is zeros and nothing else differs from with it
    let input = |memory| DepthRenderInput {
        frame: None,
        eye_positions: [Vec3::ZERO; 2],
        color_key: Some(memory_key(shape)),
        arms: [None; 2],
        arm_gaps: [None; 2],
        edge_cut: 0.0,
        lever_windows: [None; MAX_LEVER_WINDOWS],
        capture_playzone: false,
        playzone_view: false,
        memory,
        cloud: None,
        key_lower_rects: None,
        region_override: None,
        game_dim: 0.0,
        game_see: 0.0,
    };
    let uniforms = |memory| {
        depth_mask::depth_uniforms(&config, &[view(Vec3::ZERO); 2], &input(memory), None, false)
    };
    let off = uniforms(None);
    let on = uniforms(Some(memory_input(Some(shape), 0.05)));
    let differ = (0..off.len())
        .filter(|&i| off[i] != on[i])
        .collect::<Vec<_>>();
    // `memory` is the 4 floats after the playzone's vector: x and z are set
    assert_eq!(differ.len(), 2, "{differ:?}");
    let slot = differ[0];
    assert_eq!(&off[slot..slot + 4], &[0.0; 4]);
    assert_eq!(&on[slot..slot + 4], &[1.0, 0.0, 0.05, 0.0]);
}

// The memory camera: a point seen by a depth camera turned 30 degrees and moved 10 cm from the anchor
// lands on the texel of its direction, and that texel gives the point back. A re-projection into another
// camera keeps it, and its frozen flag and age
#[test]
fn memory_camera_maps_points_both_ways() {
    let (device, queue) = gpu_or_skip!();
    let mut rig = MemoryRig::new(&device);
    // Aimed 35 degrees down, as by default
    let camera = MemoryCamera {
        position: Vec3::ZERO,
        yaw: 0.0,
        pitch: 35_f32.to_radians(),
    };
    let depth_view = ViewParams {
        pose: Pose {
            orientation: Quat::from_rotation_y(30_f32.to_radians()),
            position: Vec3::new(0.1, 0.0, 0.0),
        },
        fov: fov(),
    };
    // An object at 0.5 m in the depth view, a little left and down
    let scene = Scene::new(3.0, [0.4, 0.3, 0.5, 0.4], 0.5);
    let local = scene_point(0.45, 0.35, 0.5);
    let point = depth_view.pose.position + depth_view.pose.orientation * local;

    rig.step(
        &device,
        &queue,
        &scene,
        depth_view,
        &camera,
        None,
        None,
        MODE_UPDATE,
    );
    let memory = rig.read(&device, &queue);
    let (x, y, z) = memory_texel(&camera, point);
    let texel = memory[y * MEMORY_SIZE as usize + x];
    // The point lies in the plane of the object, the texel's ray passes within half a texel of it (3.46
    // / 128 at 0.5 m: 7 mm), and the plane is turned against the memory camera
    assert!((texel[0] - z).abs() < 0.015, "{texel:?} {z}");
    let back = memory_point(&camera, x, y, texel[0]);
    assert!(back.distance(point) < 0.02, "{back} {point}");

    // A hand at the object: frozen at the next update, then re-projected into a camera moved 13 cm and
    // turned 23 degrees
    let hand = memory_hand(
        point + Vec3::new(-0.01, 0.0, 0.03),
        point + Vec3::new(0.01, 0.0, 0.03),
        0.01,
    );
    rig.step(
        &device,
        &queue,
        &scene,
        depth_view,
        &camera,
        None,
        Some(hand),
        MODE_UPDATE,
    );
    let moved = MemoryCamera {
        position: Vec3::new(0.13, 0.0, 0.0),
        yaw: 0.4,
        pitch: 35_f32.to_radians(),
    };
    rig.step(
        &device,
        &queue,
        &scene,
        depth_view,
        &moved,
        Some(&camera),
        None,
        MODE_REPROJECT,
    );
    let memory = rig.read(&device, &queue);
    let (x, y, z) = memory_texel(&moved, point);
    let texel = memory[y * MEMORY_SIZE as usize + x];
    assert!((texel[0] - z).abs() < 0.025, "{texel:?} {z}");
    assert_eq!(texel[2], 1.0, "{texel:?}");
    assert!((texel[3] - MEMORY_DT).abs() < 1e-3, "{texel:?}");
    let back = memory_point(&moved, x, y, texel[0]);
    assert!(back.distance(point) < 0.025, "{back} {point}");
}

// ---------------------------------------------------------------------------------------------
// Depth cloud (`debug.alvr.cloud`, design/depthcloud/final.md, motion_cloud.wgsl)
// ---------------------------------------------------------------------------------------------

/// Texels of the depth images of the cloud tests: 64 x 64 cells, so that a box and its moved edges fit
/// with the window of 12 cells on each side
const CLOUD_DEPTH_SIZE: u32 = 128;
const CLOUD_CELLS: usize = CLOUD_DEPTH_SIZE as usize / 2;
/// Two intervals of the 33.3 ms grid of the depth
const CLOUD_DT_NS: i64 = 66_666_667;
/// The wall behind the box, out of the region
const CLOUD_WALL: f32 = 3.0;
/// The box, in the region
const CLOUD_BOX: f32 = 0.5;
/// Texels of the box in the depth camera at the origin, [x0, y0, x1, y1), rows from the bottom: cells
/// 12..42 x 8..56
const CLOUD_BOX_TEXELS: [i32; 4] = [24, 16, 84, 112];
/// One 8 bit step of a stored speed (depth texels per second)
const CLOUD_SPEED_STEP: f32 = CLOUD_SPEED_SCALE / 255.0;

/// The region of the cloud tests: 0.25 to 0.75 m in front of the origin, 4 m wide and high
fn cloud_config() -> DepthPassthroughConfig {
    base_config(DepthMaskRegion::Volume(CockpitVolumeConfig {
        center_x: 0.0,
        center_y: 0.0,
        center_z: -0.5,
        yaw_deg: 0.0,
        width: 4.0,
        height: 4.0,
        depth: 0.5,
    }))
}

/// A depth camera of 90 degrees, turned about +y then +x
fn cloud_camera(yaw_deg: f32, pitch_deg: f32, position: Vec3) -> ViewParams {
    ViewParams {
        pose: Pose {
            orientation: Quat::from_rotation_y(yaw_deg.to_radians())
                * Quat::from_rotation_x(pitch_deg.to_radians()),
            position,
        },
        fov: fov(),
    }
}

/// A rectangle facing the depth camera at the origin, `z` m away, over its texels [x0, y0, x1, y1)
/// (rows from the bottom; beyond 0..CLOUD_DEPTH_SIZE: outside that camera's view): its edges lie on
/// texel borders there
#[derive(Clone, Copy)]
struct CloudRect {
    z: f32,
    texels: [i32; 4],
}

impl CloudRect {
    /// The texels it covers in the depth camera at the origin, rows from the bottom
    fn inside(&self) -> Vec<bool> {
        let size = CLOUD_DEPTH_SIZE as i32;
        let [x0, y0, x1, y1] = self.texels;
        (0..size * size)
            .map(|index| {
                let (x, y) = (index % size, index / size);
                (x0..x1).contains(&x) && (y0..y1).contains(&y)
            })
            .collect()
    }
}

/// The box of the tests at `z`, moved by `dx` texels to the right
fn cloud_box(z: f32, dx: i32) -> CloudRect {
    let [x0, y0, x1, y1] = CLOUD_BOX_TEXELS;
    CloudRect {
        z,
        texels: [x0 + dx, y0, x1 + dx, y1],
    }
}

/// The tangent of a texel border of the depth (0..CLOUD_DEPTH_SIZE)
fn cloud_tangent(texel: f32) -> f32 {
    -1.0 + 2.0 * texel / CLOUD_DEPTH_SIZE as f32
}

/// The depth image seen from `camera` of the rectangles in front of a wall `wall` m away, both facing
/// -z of the world. Rows from the bottom
fn cloud_depth(camera: &ViewParams, wall: f32, rects: &[CloudRect]) -> Vec<f32> {
    let origin = camera.pose.position;
    let mut depth = Vec::with_capacity((CLOUD_DEPTH_SIZE * CLOUD_DEPTH_SIZE) as usize);
    for y in 0..CLOUD_DEPTH_SIZE {
        for x in 0..CLOUD_DEPTH_SIZE {
            let dir = camera.pose.orientation
                * Vec3::new(
                    cloud_tangent(x as f32 + 0.5),
                    cloud_tangent(y as f32 + 0.5),
                    -1.0,
                );
            // Distance from the camera plane to the plane z = -d (the ray is 1 long along the axis)
            let along = |d: f32| (-d - origin.z) / dir.z;
            let mut distance = along(wall);
            for rect in rects {
                let s = along(rect.z);
                let hit = origin + dir * s;
                let [x0, y0, x1, y1] = rect.texels.map(|t| cloud_tangent(t as f32) * rect.z);
                if s > 0.0 && s < distance && (x0..x1).contains(&hit.x) && (y0..y1).contains(&hit.y)
                {
                    distance = s;
                }
            }
            depth.push(depth_mask::encode_depth(distance, NEAR, f32::INFINITY));
        }
    }
    depth
}

/// The depth image of the camera at the origin where the texels of `inside` show the box's distance
/// and the others the wall
fn cloud_mask_depth(inside: &[bool]) -> Vec<f32> {
    inside
        .iter()
        .map(|inside| {
            let distance = if *inside { CLOUD_BOX } else { CLOUD_WALL };
            depth_mask::encode_depth(distance, NEAR, f32::INFINITY)
        })
        .collect()
}

/// phi of a field cell (texels, + on the game side)
fn cloud_phi(cell: [u8; 4]) -> f32 {
    (f32::from(cell[0]) / 255.0 * 2.0 - 1.0) * CLOUD_TRUNCATION
}

/// The held speed (g) and this pair's speed (b) of a field cell (texels per second)
fn cloud_speeds(cell: [u8; 4]) -> (f32, f32) {
    (
        f32::from(cell[1]) * CLOUD_SPEED_STEP,
        f32::from(cell[2]) * CLOUD_SPEED_STEP,
    )
}

/// The depth cloud of both eyes on the desktop GPU, with `MotionCloud` as the renderer drives it
struct CloudRig {
    pipelines: StreamPipelines,
    cloud_pipelines: CloudPipelines,
    cloud: MotionCloud,
    depth_uniforms: wgpu::Buffer,
    params: CloudParams,
    capture_ns: i64,
    // The last depth image: its frame, the depth uniforms of its passes, its bind group
    last: Option<(DepthFrame, Vec<f32>, wgpu::BindGroup)>,
}

/// A colour key without hands: the depth cloud runs only with the key
fn cloud_key() -> ColorKeyInput {
    ColorKeyInput {
        hands: [None; 2],
        leak: [0.0; 2],
        bridges: [None; 2],
    }
}

impl CloudRig {
    fn new(device: &Device, queue: &Queue, params: CloudParams) -> Self {
        let constants = [
            ("ENABLE_SRGB_CORRECTION", 0.0),
            ("ENCODING_GAMMA", 0.0),
            ("ENABLE_FFE", 0.0),
        ];
        let pipelines = StreamPipelines::new(device, TextureFormat::Rgba8Unorm, &constants, false);
        let cloud_pipelines = CloudPipelines::new(device, &pipelines);
        let cloud = MotionCloud::new(
            device,
            queue,
            &cloud_pipelines,
            &pipelines.cloud_read_layout,
            UVec2::splat(CLOUD_DEPTH_SIZE),
        );
        assert_eq!(cloud.field_size, UVec2::splat(CLOUD_CELLS as u32));

        Self {
            pipelines,
            cloud_pipelines,
            cloud,
            depth_uniforms: stream::create_depth_uniform_buffer(device),
            params,
            capture_ns: 0,
            last: None,
        }
    }

    /// `image_frames` over the two display frames that do both eyes
    fn image(
        &mut self,
        device: &Device,
        queue: &Queue,
        depth: &[f32],
        camera: ViewParams,
        dt_ns: i64,
    ) -> [EyePass; 2] {
        self.image_frames(device, queue, depth, camera, dt_ns, 2)
            .map(|pass| pass.expect("an eye without passes"))
    }

    /// A new depth image `depth` (both eyes) seen from `camera`, captured `dt_ns` after the one
    /// before, over `displays` display frames (eye 0's passes run on the display frame it arrives,
    /// eye 1's on the next one, as `plan_eyes` schedules them). The passes of each eye
    fn image_frames(
        &mut self,
        device: &Device,
        queue: &Queue,
        depth: &[f32],
        camera: ViewParams,
        dt_ns: i64,
        displays: i64,
    ) -> [Option<EyePass>; 2] {
        self.stereo_frames(device, queue, [depth; 2], [camera; 2], dt_ns, displays)
    }

    /// `image` with a depth image and a depth camera of its own for each eye (layer 0: eye 0)
    fn stereo_image(
        &mut self,
        device: &Device,
        queue: &Queue,
        depths: [&[f32]; 2],
        cameras: [ViewParams; 2],
        dt_ns: i64,
    ) -> [EyePass; 2] {
        self.stereo_frames(device, queue, depths, cameras, dt_ns, 2)
            .map(|pass| pass.expect("an eye without passes"))
    }

    /// `image_frames` with a depth image and a depth camera of its own for each eye
    fn stereo_frames(
        &mut self,
        device: &Device,
        queue: &Queue,
        depths: [&[f32]; 2],
        cameras: [ViewParams; 2],
        dt_ns: i64,
        displays: i64,
    ) -> [Option<EyePass>; 2] {
        self.capture_ns += dt_ns;
        let frame = DepthFrame {
            swapchain_index: 0,
            near: NEAR,
            far: f32::INFINITY,
            depth_views: cameras,
        };
        let input = DepthRenderInput {
            frame: Some(frame),
            eye_positions: cameras.map(|camera| camera.pose.position),
            color_key: None,
            arms: [None; 2],
            arm_gaps: [None; 2],
            edge_cut: 0.0,
            lever_windows: [None; MAX_LEVER_WINDOWS],
            capture_playzone: false,
            playzone_view: false,
            memory: None,
            cloud: None,
            key_lower_rects: None,
            region_override: None,
            game_dim: 0.0,
            game_see: 0.0,
        };
        let uniforms = depth_mask::depth_uniforms(&cloud_config(), &cameras, &input, None, false);
        queue.write_buffer(&self.depth_uniforms, 0, &floats_to_bytes(&uniforms));

        let texture = device.create_texture(&TextureDescriptor {
            label: None,
            size: Extent3d {
                width: CLOUD_DEPTH_SIZE,
                height: CLOUD_DEPTH_SIZE,
                depth_or_array_layers: 2,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: TextureDimension::D2,
            format: TextureFormat::R32Float,
            usage: TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST,
            view_formats: &[],
        });
        for (layer, depth) in depths.into_iter().enumerate() {
            write_texture_2d(
                queue,
                &texture,
                &floats_to_bytes(depth),
                CLOUD_DEPTH_SIZE * 4,
                CLOUD_DEPTH_SIZE,
                layer as u32,
            );
        }
        let depth_bind_group = stream::create_depth_bind_group(
            device,
            &self.pipelines,
            &self.depth_uniforms,
            &stream::depth_array_view(&texture),
        );

        let mut done = [None; 2];
        for display in 0..displays {
            let cloud_input = CloudInput {
                params: self.params,
                capture_ns: Some(self.capture_ns),
                display_ns: self.capture_ns + 100_000_000 + display * 13_888_889,
                shown: true,
            };
            let passes = self
                .cloud
                .plan(queue, &cloud_input, Some(&frame), &uniforms);
            let mut encoder = device.create_command_encoder(&Default::default());
            self.cloud.encode(
                &mut encoder,
                &self.cloud_pipelines,
                &depth_bind_group,
                &passes,
            );
            queue.submit([encoder.finish()]);
            for (eye, pass) in passes.eyes.into_iter().enumerate() {
                if let Some(pass) = pass {
                    assert!(done[eye].is_none(), "eye {eye} twice");
                    done[eye] = Some(pass);
                }
            }
        }
        self.last = Some((frame, uniforms.to_vec(), depth_bind_group));
        done
    }

    /// A display frame without a new depth image at which each eye's reach time is `reach_s` older
    /// than its field's, less the latency: only the uniforms are written (no eye may be waiting)
    fn display(&mut self, queue: &Queue, reach_s: f32) {
        let lead_ns = (reach_s / (self.params.gain / 100.0) * 1e9) as i64;
        self.display_at(queue, (self.params.lambda_ms * 1e6) as i64 + lead_ns, true);
    }

    /// A display frame `age_ns` after the capture of the last depth image, with that image again
    /// (`same_frame`) or without a depth frame (the depth stalled): only the uniforms are written
    fn display_at(&mut self, queue: &Queue, age_ns: i64, same_frame: bool) {
        let (frame, uniforms, _) = self.last.as_ref().unwrap();
        let input = CloudInput {
            params: self.params,
            capture_ns: Some(self.capture_ns),
            display_ns: self.capture_ns + age_ns,
            shown: true,
        };
        let frame = same_frame.then_some(frame);
        let passes = self.cloud.plan(queue, &input, frame, uniforms);
        assert_eq!(passes.eyes, [None; 2]);
    }

    /// The depth mask of an eye over the last depth image, with the colour key on (no hands) and
    /// `cloud` (None: off), seen from the depth camera at the origin: one pixel per depth texel. Rows
    /// from the top of the view
    fn mask(
        &self,
        device: &Device,
        queue: &Queue,
        eye: usize,
        cloud: Option<CloudInput>,
    ) -> Vec<[u8; 4]> {
        self.mask_with_arms(device, queue, eye, cloud, [None; 2])
    }

    /// `mask` with the arm shift of `arms`
    fn mask_with_arms(
        &self,
        device: &Device,
        queue: &Queue,
        eye: usize,
        cloud: Option<CloudInput>,
        arms: [Option<ArmMotion>; 2],
    ) -> Vec<[u8; 4]> {
        let (frame, _, depth_bind_group) = self.last.as_ref().unwrap();
        let stream_view = cloud_camera(0.0, 0.0, Vec3::ZERO);
        let input = DepthRenderInput {
            frame: Some(*frame),
            eye_positions: [stream_view.pose.position; 2],
            color_key: Some(cloud_key()),
            arms,
            arm_gaps: [None; 2],
            edge_cut: 0.0,
            lever_windows: [None; MAX_LEVER_WINDOWS],
            capture_playzone: false,
            playzone_view: false,
            memory: None,
            cloud,
            key_lower_rects: None,
            region_override: None,
            game_dim: 0.0,
            game_see: 0.0,
        };
        let uniforms =
            depth_mask::depth_uniforms(&cloud_config(), &[stream_view; 2], &input, None, false);
        queue.write_buffer(&self.depth_uniforms, 0, &floats_to_bytes(&uniforms));

        let resolution = UVec2::splat(CLOUD_DEPTH_SIZE * 5);
        let mask = stream::create_depth_mask_texture(device, resolution);
        let focus_area = stream::create_focus_area_texture(device, resolution)
            .create_view(&TextureViewDescriptor::default());
        let playzone = stream::create_playzone_texture(device, UVec2::splat(CLOUD_DEPTH_SIZE))
            .create_view(&TextureViewDescriptor::default());
        let sampler = device.create_sampler(&SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let playzone_group = stream::create_playzone_bind_group(
            device,
            &self.pipelines,
            &playzone,
            &focus_area,
            &sampler,
        );
        let mut encoder = device.create_command_encoder(&Default::default());
        stream::encode_depth_mask(
            &mut encoder,
            &self.pipelines,
            &mask.create_view(&TextureViewDescriptor::default()),
            depth_bind_group,
            &playzone_group,
            self.cloud.read_bind_group(eye),
            eye as u32,
        );
        read_rgba(
            device,
            queue,
            encoder,
            &mask,
            UVec2::splat(CLOUD_DEPTH_SIZE),
        )
        .chunks_exact(4)
        .map(|c| [c[0], c[1], c[2], c[3]])
        .collect()
    }

    /// The input of the cloud as shown, for `mask`
    fn shown(&self) -> CloudInput {
        CloudInput {
            params: self.params,
            capture_ns: Some(self.capture_ns),
            display_ns: self.capture_ns,
            shown: true,
        }
    }
}

/// The zone (mask g) of `CloudRig::mask` at the depth texel x, y (rows from the bottom)
fn cloud_zone_at(mask: &[[u8; 4]], x: usize, y: usize) -> u8 {
    let size = CLOUD_DEPTH_SIZE as usize;
    mask[(size - 1 - y) * size + x][1]
}

impl CloudRig {
    /// The current field of an eye, (r, g, b, a) per cell, rows from the bottom
    fn field(&self, device: &Device, queue: &Queue, eye: usize) -> Vec<[u8; 4]> {
        let encoder = device.create_command_encoder(&Default::default());
        read_rgba(
            device,
            queue,
            encoder,
            self.cloud.field_texture(eye),
            self.cloud.field_size,
        )
        .chunks_exact(4)
        .map(|c| [c[0], c[1], c[2], c[3]])
        .collect()
    }
}

// Nothing moves: three identical depth images measure no speed anywhere, and the mask of each eye has
// no zone. The pairs are measured (not reset), so the zero is the measurement's
#[test]
fn cloud_still_scene_has_no_zone() {
    let (device, queue) = gpu_or_skip!();
    let mut rig = CloudRig::new(&device, &queue, CloudParams::default());
    let camera = cloud_camera(0.0, 0.0, Vec3::ZERO);
    let depth = cloud_depth(&camera, CLOUD_WALL, &[cloud_box(CLOUD_BOX, 0)]);
    for image in 0..3 {
        let passes = rig.image(&device, &queue, &depth, camera, CLOUD_DT_NS);
        let expected = if image == 0 {
            Some(ResetReason::Start)
        } else {
            None
        };
        assert_eq!(passes.map(|pass| pass.reason), [expected; 2]);
        for eye in 0..2 {
            let field = rig.field(&device, &queue, eye);
            assert!(
                field.iter().all(|cell| cell[1] == 0 && cell[2] == 0),
                "image {image} eye {eye}"
            );
            // The box is in the field, with its edge
            assert!(field.iter().any(|cell| cloud_phi(*cell) < -20.0));
            assert!(field.iter().any(|cell| cloud_phi(*cell).abs() < 1.5));
        }
    }
    let status = rig.cloud.status();
    assert_eq!(status.pairs, [3, 3]);
    assert_eq!(status.resets, [[1, 0, 0, 0]; 2]);
    // The mask: the zone replaces the proximity, so all 0
    rig.display(&queue, 0.2);
    for eye in 0..2 {
        let mask = rig.mask(&device, &queue, eye, Some(rig.shown()));
        assert!(mask.iter().all(|texel| texel[1] == 0), "eye {eye}");
        // The box is drawn
        assert!(mask.iter().any(|texel| texel[0] == 255));
    }
}

// The head turns 5 degrees and moves 2 cm between depth images (75 deg/s, under the guard), nothing
// else moves: the rays are compared in the world, so no speed anywhere. The box keeps its edges 10
// texels or more inside the image; a strip beyond the left border of the first camera comes into view
// when the head turns left and goes out of view when it turns back. The texels it uncovers or covers
// at the border have no known previous state on their world ray (outside the other camera, or the
// surface found there lies off the ray), so mark_main takes the previous mask equal to the new one
// there: both masks have the same edge and nothing is
// claimed, in every `cloud_onesided` mode. This does not test the border band of `cloud_onesided` (no
// cell here has an edge in one mask only): that is cloud_onesided_modes (a)
#[test]
fn cloud_head_motion_alone_has_no_zone() {
    let (device, queue) = gpu_or_skip!();
    let mut rig = CloudRig::new(&device, &queue, CloudParams::default());
    let cameras = [
        cloud_camera(0.0, 0.0, Vec3::ZERO),
        cloud_camera(5.0, 0.0, Vec3::new(0.02, 0.0, 0.0)),
        cloud_camera(5.0, 5.0, Vec3::new(0.02, 0.02, 0.0)),
        cloud_camera(0.0, 5.0, Vec3::new(0.02, 0.02, -0.02)),
    ];
    let box_rect = CloudRect {
        z: CLOUD_BOX,
        texels: [32, 32, 96, 96],
    };
    let strip = CloudRect {
        z: CLOUD_BOX,
        texels: [-60, 40, -4, 88],
    };
    for (image, camera) in cameras.into_iter().enumerate() {
        let depth = cloud_depth(&camera, CLOUD_WALL, &[box_rect, strip]);
        // The strip is in view only while the head is turned left
        let near = depth_mask::encode_depth(1.0, NEAR, f32::INFINITY);
        let strip_seen = (0..8).any(|x| depth[64 * CLOUD_DEPTH_SIZE as usize + x] < near);
        assert_eq!(strip_seen, image == 1 || image == 2, "image {image}");
        let passes = rig.image(&device, &queue, &depth, camera, CLOUD_DT_NS);
        if image > 0 {
            for pass in passes {
                assert_eq!(pass.reason, None, "image {image}");
                assert!((pass.head_deg_s - 75.0).abs() < 1.0, "{}", pass.head_deg_s);
            }
        }
        for eye in 0..2 {
            let field = rig.field(&device, &queue, eye);
            let moving = field
                .iter()
                .enumerate()
                .filter(|(_, cell)| cell[1] != 0 || cell[2] != 0)
                .map(|(index, cell)| (index % CLOUD_CELLS, index / CLOUD_CELLS, cell[1], cell[2]))
                .collect::<Vec<_>>();
            assert!(moving.is_empty(), "image {image} eye {eye}: {moving:?}");
        }
    }

    // The control: the same head motion while the box moves 6 texels (4.7 cm) is measured, mostly at
    // 90 texels per second, at most 120 (seen from the turned camera 2 cm nearer, 6 texels of the
    // camera at the origin are 6 to 8), so the images before were reprojected, not ignored
    let camera = cloud_camera(-5.0, 5.0, Vec3::new(0.0, 0.02, -0.02));
    let moved = CloudRect {
        texels: [38, 32, 102, 96],
        ..box_rect
    };
    let depth = cloud_depth(&camera, CLOUD_WALL, &[moved, strip]);
    let passes = rig.image(&device, &queue, &depth, camera, CLOUD_DT_NS);
    assert_eq!(passes.map(|pass| pass.reason), [None; 2]);
    for eye in 0..2 {
        let field = rig.field(&device, &queue, eye);
        let speeds = field.iter().map(|cell| cloud_speeds(*cell).0);
        let measured = speeds
            .clone()
            .filter(|speed| (90.0 - CLOUD_SPEED_STEP..=90.0 + CLOUD_SPEED_STEP).contains(speed));
        assert!(measured.count() > 300, "eye {eye}");
        assert!(
            speeds
                .clone()
                .all(|speed| speed <= 120.0 + CLOUD_SPEED_STEP),
            "eye {eye}"
        );
    }
}

// The box moves 6 texels to the right in 67 ms: the cells near the two edges that moved (leading and
// trailing) read 90 texels per second, held and of this pair. Cells nearest the edges it slid along
// read 0, and no cell reads more than the edges moved
#[test]
fn cloud_moving_edge_measures_its_speed() {
    let (device, queue) = gpu_or_skip!();
    let mut rig = CloudRig::new(&device, &queue, CloudParams::default());
    let camera = cloud_camera(0.0, 0.0, Vec3::ZERO);
    let before = cloud_depth(&camera, CLOUD_WALL, &[cloud_box(CLOUD_BOX, 0)]);
    let after = cloud_depth(&camera, CLOUD_WALL, &[cloud_box(CLOUD_BOX, 6)]);
    rig.image(&device, &queue, &before, camera, CLOUD_DT_NS);
    let passes = rig.image(&device, &queue, &after, camera, CLOUD_DT_NS);
    for pass in passes {
        assert_eq!(pass.reason, None);
        assert!((pass.dt_s - 0.0667).abs() < 1e-3, "{}", pass.dt_s);
    }
    let expected = 6.0 / (CLOUD_DT_NS as f32 / 1e9);
    assert!((expected - 90.0).abs() < 0.01);

    for eye in 0..2 {
        let field = rig.field(&device, &queue, eye);
        let cell = |x: usize, y: usize| field[y * CLOUD_CELLS + x];
        // The edges moved from cells 12 and 42 to 15 and 45. Within 6 cells (12 texels) of them, in
        // the rows 12 cells or more from the top and bottom edges. There the nearest cell of the other
        // kind is along the row in both masks: the distances are whole cells, phi moved by exactly 6
        // texels, and 6 texels in 66.67 ms (89.999996 texels per second) is 45 steps of 2 of the
        // 8 bit encoding (44.999998; 45 is the only step within the 0.6 of a step a GPU may be off when
        // it stores it): exact
        for y in 20..44 {
            for x in (9..21).chain(39..51) {
                assert_eq!(
                    cloud_speeds(cell(x, y)),
                    (90.0, 90.0),
                    "eye {eye} cell {x}, {y}"
                );
            }
        }
        // Above and below the box, nearest the edges it slid along
        for y in (4..12).chain(52..60) {
            for x in 24..34 {
                assert_eq!(
                    cloud_speeds(cell(x, y)),
                    (0.0, 0.0),
                    "eye {eye} cell {x}, {y}"
                );
            }
        }
        assert!(
            field
                .iter()
                .all(|cell| cloud_speeds(*cell).0 <= expected + CLOUD_SPEED_STEP)
        );
    }
}

// The same box moves in front of a wall that is in the region too: no edge against the game, no speed
#[test]
fn cloud_ignores_motion_in_front_of_the_region() {
    let (device, queue) = gpu_or_skip!();
    let mut rig = CloudRig::new(&device, &queue, CloudParams::default());
    let camera = cloud_camera(0.0, 0.0, Vec3::ZERO);
    let wall = 0.65;
    let before = cloud_depth(&camera, wall, &[cloud_box(0.4, 0)]);
    let after = cloud_depth(&camera, wall, &[cloud_box(0.4, 6)]);
    rig.image(&device, &queue, &before, camera, CLOUD_DT_NS);
    let passes = rig.image(&device, &queue, &after, camera, CLOUD_DT_NS);
    assert_eq!(passes.map(|pass| pass.reason), [None; 2]);
    for eye in 0..2 {
        let field = rig.field(&device, &queue, eye);
        // Everything is in the region, far from any edge
        assert!(field.iter().all(|cell| cell[0] == 0), "eye {eye}");
        assert!(
            field.iter().all(|cell| cell[1] == 0 && cell[2] == 0),
            "eye {eye}"
        );
    }
}

// An edge that moves 2 texels between two depth images stays under the deadband (2.5): no speed, also
// with the edges between two cells
#[test]
fn cloud_deadband() {
    let (device, queue) = gpu_or_skip!();
    for offset in [0, 1] {
        let mut rig = CloudRig::new(&device, &queue, CloudParams::default());
        let camera = cloud_camera(0.0, 0.0, Vec3::ZERO);
        let before = cloud_depth(&camera, CLOUD_WALL, &[cloud_box(CLOUD_BOX, offset)]);
        let after = cloud_depth(&camera, CLOUD_WALL, &[cloud_box(CLOUD_BOX, offset + 2)]);
        rig.image(&device, &queue, &before, camera, CLOUD_DT_NS);
        let passes = rig.image(&device, &queue, &after, camera, CLOUD_DT_NS);
        assert_eq!(passes.map(|pass| pass.reason), [None; 2]);
        for eye in 0..2 {
            let field = rig.field(&device, &queue, eye);
            assert!(
                field.iter().all(|cell| cell[1] == 0 && cell[2] == 0),
                "offset {offset} eye {eye}"
            );
        }
    }
}

// A 4 x 4 texel patch is in one depth image only (noise): its arrival is measured, but the pair that
// removes it again adds no speed (the flip test)
#[test]
fn cloud_flip_is_ignored() {
    let (device, queue) = gpu_or_skip!();
    let mut rig = CloudRig::new(&device, &queue, CloudParams::default());
    let camera = cloud_camera(0.0, 0.0, Vec3::ZERO);
    let still = cloud_depth(&camera, CLOUD_WALL, &[cloud_box(CLOUD_BOX, 0)]);
    let patch = CloudRect {
        z: CLOUD_BOX,
        texels: [100, 60, 104, 64],
    };
    let with_patch = cloud_depth(&camera, CLOUD_WALL, &[cloud_box(CLOUD_BOX, 0), patch]);
    rig.image(&device, &queue, &still, camera, CLOUD_DT_NS);
    let passes = rig.image(&device, &queue, &with_patch, camera, CLOUD_DT_NS);
    assert_eq!(passes.map(|pass| pass.reason), [None; 2]);
    for eye in 0..2 {
        let field = rig.field(&device, &queue, eye);
        assert!(field.iter().any(|cell| cell[2] > 0), "eye {eye}");
    }
    let passes = rig.image(&device, &queue, &still, camera, CLOUD_DT_NS);
    assert_eq!(passes.map(|pass| pass.reason), [None; 2]);
    for eye in 0..2 {
        let field = rig.field(&device, &queue, eye);
        assert!(field.iter().all(|cell| cell[2] == 0), "eye {eye}");
    }
}

// After the box stops, the held speed of each cell decays by exp(-dt / hold) per depth image, within
// one 8 bit step, and is 0 once below 15 texels per second
#[test]
fn cloud_holds_then_forgets() {
    let (device, queue) = gpu_or_skip!();
    let params = CloudParams::default();
    let mut rig = CloudRig::new(&device, &queue, params);
    let camera = cloud_camera(0.0, 0.0, Vec3::ZERO);
    let before = cloud_depth(&camera, CLOUD_WALL, &[cloud_box(CLOUD_BOX, 0)]);
    let after = cloud_depth(&camera, CLOUD_WALL, &[cloud_box(CLOUD_BOX, 6)]);
    rig.image(&device, &queue, &before, camera, CLOUD_DT_NS);
    rig.image(&device, &queue, &after, camera, CLOUD_DT_NS);
    let mut held = [0, 1].map(|eye| rig.field(&device, &queue, eye));
    assert!(held[0].iter().any(|cell| cloud_speeds(*cell).0 > 80.0));

    let decay = (-(CLOUD_DT_NS as f32 / 1e6) / params.hold_ms).exp();
    let mut decayed_above_floor = 0;
    for image in 0..8 {
        let passes = rig.image(&device, &queue, &after, camera, CLOUD_DT_NS);
        for pass in passes {
            assert_eq!(pass.reason, None);
            assert!((pass.decay(&params) - decay).abs() < 1e-5);
        }
        for (eye, held) in held.iter_mut().enumerate() {
            let field = rig.field(&device, &queue, eye);
            for (index, (cell, before)) in field.iter().zip(held.iter()).enumerate() {
                let (now, speed) = cloud_speeds(*cell);
                let expected = cloud_speeds(*before).0 * decay;
                let at = (index % CLOUD_CELLS, index / CLOUD_CELLS);
                assert_eq!(speed, 0.0, "image {image} eye {eye} cell {at:?}");
                if expected < CLOUD_SPEED_FLOOR - CLOUD_SPEED_STEP {
                    assert_eq!(now, 0.0, "image {image} eye {eye} cell {at:?}: {expected}");
                } else if expected > CLOUD_SPEED_FLOOR + CLOUD_SPEED_STEP {
                    assert!(
                        (now - expected).abs() <= CLOUD_SPEED_STEP,
                        "image {image} eye {eye} cell {at:?}: {now} {expected}"
                    );
                    decayed_above_floor += 1;
                } else {
                    assert!(
                        now == 0.0 || (now - expected).abs() <= CLOUD_SPEED_STEP,
                        "image {image} eye {eye} cell {at:?}: {now} {expected}"
                    );
                }
            }
            *held = field;
        }
    }
    // It decayed over several images, and is gone after 8 (90 x 0.72^6 is below 15)
    assert!(decayed_above_floor > 100, "{decayed_above_floor}");
    assert!(held.iter().flatten().all(|cell| cell[1] == 0));
}

// The signed distance of the field on the GPU (rows_main, field_main) against its Rust reference, on
// a random mask of blobs, some cut by the border of the image
#[test]
fn cloud_window_distance_matches_rust() {
    let (device, queue) = gpu_or_skip!();
    let size = CLOUD_DEPTH_SIZE as usize;
    // xorshift: the same blobs on every run
    let mut state = 0x2545_f491_u32;
    let mut random = |range: u32| {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        state % range
    };
    // One blob large enough to be beyond the window deep inside
    let mut blobs = vec![(Vec2::new(40.0, 80.0), 34.0)];
    blobs.extend((0..14).map(|_| {
        let center = Vec2::new(random(128) as f32, random(128) as f32);
        (center, 2.0 + random(13) as f32)
    }));
    let inside = (0..size * size)
        .map(|index| {
            let texel = Vec2::new((index % size) as f32 + 0.5, (index / size) as f32 + 0.5);
            blobs
                .iter()
                .any(|(center, radius)| texel.distance(*center) < *radius)
        })
        .collect::<Vec<_>>();
    let (cells, width, height) = cells_inside(&inside, size, size);
    assert_eq!((width, height), (CLOUD_CELLS, CLOUD_CELLS));
    let reference = window_distance(&cells, width, height);
    // Edges, cells deep inside and cells beyond the window
    assert!(reference.contains(&-CLOUD_TRUNCATION));
    assert!(reference.contains(&CLOUD_TRUNCATION));
    assert!(reference.iter().filter(|phi| phi.abs() < 2.0).count() > 100);

    let mut rig = CloudRig::new(&device, &queue, CloudParams::default());
    let camera = cloud_camera(0.0, 0.0, Vec3::ZERO);
    rig.image(
        &device,
        &queue,
        &cloud_mask_depth(&inside),
        camera,
        CLOUD_DT_NS,
    );
    // Within one 8 bit step of the encoding, not half of one: GPUs need not round to the nearest step
    // when they store a float in an Rgba8Unorm target (D3D and Vulkan allow 0.6 of a step; on the
    // desktop GPU of the tests 19.59 texels, 231.58 steps, was stored as 231)
    let step = 2.0 * CLOUD_TRUNCATION / 255.0;
    for eye in 0..2 {
        let field = rig.field(&device, &queue, eye);
        for (index, (cell, expected)) in field.iter().zip(&reference).enumerate() {
            let phi = cloud_phi(*cell);
            assert!(
                (phi - expected).abs() <= step,
                "eye {eye} cell {}, {}: {phi} {expected}",
                index % width,
                index / width
            );
        }
    }
}

// `cloud_onesided 0`: a box appears where nothing was within the window (12 cells): only the new mask
// has an edge there (deep inside it neither has one), so that pair claims no speed anywhere, not even
// at its edges, where only one mask has an edge. On the next pair it moves 6 texels to the right and
// its leading edge is measured (its trailing side is not: the texels it leaves were in for one image
// only, which the flip test takes as noise)
#[test]
fn cloud_edge_entering_the_window_claims_no_speed() {
    let (device, queue) = gpu_or_skip!();
    let params = CloudParams {
        onesided: 0,
        ..Default::default()
    };
    let mut rig = CloudRig::new(&device, &queue, params);
    let camera = cloud_camera(0.0, 0.0, Vec3::ZERO);
    let empty = cloud_depth(&camera, CLOUD_WALL, &[]);
    rig.image(&device, &queue, &empty, camera, CLOUD_DT_NS);
    let appeared = cloud_depth(&camera, CLOUD_WALL, &[cloud_box(CLOUD_BOX, 0)]);
    let passes = rig.image(&device, &queue, &appeared, camera, CLOUD_DT_NS);
    assert_eq!(passes.map(|pass| pass.reason), [None; 2]);
    for eye in 0..2 {
        let field = rig.field(&device, &queue, eye);
        assert!(field.iter().all(|cell| cell[2] == 0), "eye {eye}");
        // The box is in the field
        assert!(field.iter().any(|cell| cloud_phi(*cell) < -20.0));
    }

    let moved = cloud_depth(&camera, CLOUD_WALL, &[cloud_box(CLOUD_BOX, 6)]);
    let passes = rig.image(&device, &queue, &moved, camera, CLOUD_DT_NS);
    assert_eq!(passes.map(|pass| pass.reason), [None; 2]);
    for eye in 0..2 {
        let field = rig.field(&device, &queue, eye);
        // Within 6 cells of the leading edge (cells 42 to 45): exactly 90, as in
        // cloud_moving_edge_measures_its_speed
        for y in 20..44 {
            for x in 39..51 {
                let (_, speed) = cloud_speeds(field[y * CLOUD_CELLS + x]);
                assert_eq!(speed, 90.0, "eye {eye} cell {x}, {y}");
            }
        }
    }
}

// `cloud_onesided`: (a) a box leaves through the left border while the head turns right (1 degree per
// image, 30 deg/s): on the pair where its last texels leave, only the earlier mask has an edge, in the
// border band. Modes 0 and 1 claim nothing, mode 2 claims a speed there. (b) A box in the middle of
// the image (more than 12 cells from every border) jumps 30 texels (15 cells) in one pair: the cells
// that see only one of its edges in the window claim a speed in modes 1 and 2 and none in mode 0. Each
// cell is checked against `pair_speed` on the masks of the two images (the camera stands still)
#[test]
fn cloud_onesided_modes() {
    let (device, queue) = gpu_or_skip!();
    let params = |onesided| CloudParams {
        onesided,
        ..Default::default()
    };

    // (a)
    for onesided in 0..3 {
        let mut rig = CloudRig::new(&device, &queue, params(onesided));
        for (image, x0) in [-2, -2, -6, -10, -14].into_iter().enumerate() {
            let camera = cloud_camera(-(image as f32), 0.0, Vec3::ZERO);
            let rect = CloudRect {
                z: CLOUD_BOX,
                texels: [x0, 40, x0 + 20, 88],
            };
            let depth = cloud_depth(&camera, CLOUD_WALL, &[rect]);
            let passes = rig.image(&device, &queue, &depth, camera, CLOUD_DT_NS / 2);
            if image == 0 {
                continue;
            }
            assert_eq!(passes.map(|pass| pass.reason), [None; 2], "image {image}");
            let near = depth_mask::encode_depth(1.0, NEAR, f32::INFINITY);
            let seen = depth.iter().any(|depth| *depth < near);
            // In view up to the last image
            assert_eq!(seen, image < 4, "image {image}");
            if image < 4 {
                continue;
            }
            for eye in 0..2 {
                let field = rig.field(&device, &queue, eye);
                let claimed = field.iter().filter(|cell| cell[2] > 0).count();
                if onesided < 2 {
                    assert_eq!(claimed, 0, "mode {onesided} eye {eye}");
                } else {
                    assert!(claimed > 100, "mode {onesided} eye {eye}: {claimed}");
                    // Only in the border band
                    for (index, cell) in field.iter().enumerate() {
                        assert!(cell[2] == 0 || index % CLOUD_CELLS < 12, "cell {index}");
                    }
                }
            }
        }
    }

    // (b)
    let camera = cloud_camera(0.0, 0.0, Vec3::ZERO);
    let before = CloudRect {
        z: CLOUD_BOX,
        texels: [40, 40, 60, 88],
    };
    let after = CloudRect {
        texels: [70, 40, 90, 88],
        ..before
    };
    let phi_of = |rect: &CloudRect| {
        let (cells, width, height) = cells_inside(
            &rect.inside(),
            CLOUD_DEPTH_SIZE as usize,
            CLOUD_DEPTH_SIZE as usize,
        );
        window_distance(&cells, width, height)
    };
    let (phi, phip) = (phi_of(&after), phi_of(&before));
    let window = CLOUD_WINDOW_CELLS as usize;
    let border = |index: usize| {
        let (x, y) = (index % CLOUD_CELLS, index / CLOUD_CELLS);
        x.min(y) < window || x.max(y) >= CLOUD_CELLS - window
    };
    let inside = |phi: f32| phi.abs() < CLOUD_TRUNCATION;
    let one_sided = |index: usize| inside(phi[index]) != inside(phip[index]);
    let dt = CLOUD_DT_NS as f32 / 1e9;
    for onesided in 0..3 {
        let params = params(onesided);
        let mut rig = CloudRig::new(&device, &queue, params);
        let old = cloud_depth(&camera, CLOUD_WALL, &[before]);
        rig.image(&device, &queue, &old, camera, CLOUD_DT_NS);
        rig.image(&device, &queue, &old, camera, CLOUD_DT_NS);
        let new = cloud_depth(&camera, CLOUD_WALL, &[after]);
        let passes = rig.image(&device, &queue, &new, camera, CLOUD_DT_NS);
        assert_eq!(passes.map(|pass| pass.reason), [None; 2]);
        for eye in 0..2 {
            let field = rig.field(&device, &queue, eye);
            let mut interior_claims = 0;
            for (index, cell) in field.iter().enumerate() {
                let expected = pair_speed(phi[index], phip[index], border(index), dt, &params);
                // Not where the difference is at the deadband
                if ((phi[index] - phip[index]).abs() - params.dead).abs() < 0.05 {
                    continue;
                }
                let (_, speed) = cloud_speeds(*cell);
                assert!(
                    (speed - expected).abs() <= CLOUD_SPEED_STEP,
                    "mode {onesided} eye {eye} cell {}, {}: {speed} {expected}",
                    index % CLOUD_CELLS,
                    index / CLOUD_CELLS
                );
                if one_sided(index) && !border(index) && speed > 0.0 {
                    interior_claims += 1;
                }
                if one_sided(index) && onesided == 0 {
                    assert_eq!(speed, 0.0);
                }
            }
            if onesided == 0 {
                assert_eq!(interior_claims, 0);
            } else {
                assert!(interior_claims > 100, "mode {onesided}: {interior_claims}");
            }
        }
    }
}

// The box of the tests moved 6 texels to the right (90 texels per second): the mask's coverage and
// depth flag (r, b, a) are the same with the cloud shown, in its today's phase and off; only g (the
// zone in place of the proximity) differs
#[test]
fn cloud_never_changes_coverage() {
    let (device, queue) = gpu_or_skip!();
    let mut rig = CloudRig::new(&device, &queue, CloudParams::default());
    let camera = cloud_camera(0.0, 0.0, Vec3::ZERO);
    let before = cloud_depth(&camera, CLOUD_WALL, &[cloud_box(CLOUD_BOX, 0)]);
    let after = cloud_depth(&camera, CLOUD_WALL, &[cloud_box(CLOUD_BOX, 6)]);
    rig.image(&device, &queue, &before, camera, CLOUD_DT_NS);
    rig.image(&device, &queue, &after, camera, CLOUD_DT_NS);
    for eye in 0..2 {
        let off = rig.mask(&device, &queue, eye, None);
        let today_phase = rig.mask(
            &device,
            &queue,
            eye,
            Some(CloudInput {
                shown: false,
                ..rig.shown()
            }),
        );
        assert_eq!(today_phase, off);
        let on = rig.mask(&device, &queue, eye, Some(rig.shown()));
        for (a, b) in off.iter().zip(&on) {
            assert_eq!([a[0], a[2], a[3]], [b[0], b[2], b[3]]);
        }
        let zone = on.iter().filter(|texel| texel[1] == 255).count();
        assert!(zone > 500, "eye {eye}: {zone}");
        // The box and the room both show, with an edge
        assert!(on.iter().any(|texel| texel[0] == 255));
        assert!(on.iter().any(|texel| texel[0] == 0));
    }
}

// The box moved 6 texels (90 texels per second). A pixel 4 texels outside a moved edge (phi 3.5) is
// outside the zone at a reach time of 0.03 s (2.7 texels) and inside it at 0.10 s (9 texels), on the
// leading and the trailing side; a pixel 15.5 texels out is beyond the cap (14) at any age
#[test]
fn cloud_zone_grows_with_the_age() {
    let (device, queue) = gpu_or_skip!();
    let mut rig = CloudRig::new(&device, &queue, CloudParams::default());
    let camera = cloud_camera(0.0, 0.0, Vec3::ZERO);
    let before = cloud_depth(&camera, CLOUD_WALL, &[cloud_box(CLOUD_BOX, 0)]);
    let after = cloud_depth(&camera, CLOUD_WALL, &[cloud_box(CLOUD_BOX, 6)]);
    rig.image(&device, &queue, &before, camera, CLOUD_DT_NS);
    rig.image(&device, &queue, &after, camera, CLOUD_DT_NS);
    // The edges are now at texels 30 (trailing) and 90 (leading), the middle row is 64
    let (leading, trailing, beyond, y) = (93, 26, 105, 64);
    for (reach_s, inside) in [(0.03, false), (0.10, true), (0.3, true)] {
        rig.display(&queue, reach_s);
        let reach_ms = rig.cloud.status().eyes.map(|eye| eye.reach_ms);
        for eye in 0..2 {
            assert!(
                (reach_ms[eye] - reach_s * 1000.0).abs() < 0.01,
                "{reach_ms:?}"
            );
            let mask = rig.mask(&device, &queue, eye, Some(rig.shown()));
            let expected = if inside { 255 } else { 0 };
            for x in [leading, trailing] {
                assert_eq!(
                    cloud_zone_at(&mask, x, y),
                    expected,
                    "reach {reach_s} eye {eye} x {x}"
                );
            }
            assert_eq!(cloud_zone_at(&mask, beyond, y), 0, "reach {reach_s}");
        }
    }
}

// `cloud_fixed 8`: the zone reaches 8 texels from the edges that moved whatever the age (phi 5.5 in,
// 9.5 out, on both sides), and nothing from the edges the box slid along
#[test]
fn cloud_fixed_reach() {
    let (device, queue) = gpu_or_skip!();
    let params = CloudParams {
        fixed: 8.0,
        ..Default::default()
    };
    let mut rig = CloudRig::new(&device, &queue, params);
    let camera = cloud_camera(0.0, 0.0, Vec3::ZERO);
    let before = cloud_depth(&camera, CLOUD_WALL, &[cloud_box(CLOUD_BOX, 0)]);
    let after = cloud_depth(&camera, CLOUD_WALL, &[cloud_box(CLOUD_BOX, 6)]);
    rig.image(&device, &queue, &before, camera, CLOUD_DT_NS);
    rig.image(&device, &queue, &after, camera, CLOUD_DT_NS);
    for reach_s in [0.0, 0.03, 0.3] {
        rig.display(&queue, reach_s);
        for eye in 0..2 {
            let mask = rig.mask(&device, &queue, eye, Some(rig.shown()));
            let zone = |x, y| cloud_zone_at(&mask, x, y);
            assert_eq!(zone(95, 64), 255, "reach {reach_s} eye {eye}");
            assert_eq!(zone(25, 64), 255, "reach {reach_s} eye {eye}");
            assert_eq!(zone(99, 64), 0, "reach {reach_s} eye {eye}");
            assert_eq!(zone(21, 64), 0, "reach {reach_s} eye {eye}");
            // 1.5 texels above the top edge and below the bottom one, in the middle of the box
            assert_eq!(zone(60, 113), 0, "reach {reach_s} eye {eye}");
            assert_eq!(zone(60, 14), 0, "reach {reach_s} eye {eye}");
        }
    }
}

// `cloud 2`: the field is looked up where the arm shift takes the depth from, so the zone moves with the
// arm's displacement; `cloud 1` does not read the arm. An arm whose wrist is 5 cm above the fetch point of
// texel 100 of the middle row, moved 7 cm to the right (`cloud_fixed 8`, the box moved 6 texels: its
// leading edge at texel 90). Texel 85 (5 in) is in the zone of `cloud 1` only; with `cloud 2` it reads
// the field about 10 texels further left, and texel 105 (15 out) reads it near the edge
#[test]
fn cloud_2_moves_the_zone_with_the_arm() {
    let (device, queue) = gpu_or_skip!();
    let camera = cloud_camera(0.0, 0.0, Vec3::ZERO);
    let before = cloud_depth(&camera, CLOUD_WALL, &[cloud_box(CLOUD_BOX, 0)]);
    let after = cloud_depth(&camera, CLOUD_WALL, &[cloud_box(CLOUD_BOX, 6)]);
    let fetch = CloudParams::default().fetch_m;
    let wrist = Vec3::new(cloud_tangent(100.5), 0.0, -1.0).normalize() * fetch + Vec3::Y * 0.05;
    let arm = ArmMotion {
        wrist,
        direction: Vec3::NEG_Y,
        displacement: Vec3::new(0.07, 0.0, 0.0),
        weight_len: 0.0,
        mode: 1,
        target_ns: 0,
        history_ns: 0,
        wrist_history: wrist,
        extrapolated: 0.0,
    };
    for mode in [1, 2] {
        let params = CloudParams {
            mode,
            fixed: 8.0,
            ..Default::default()
        };
        let mut rig = CloudRig::new(&device, &queue, params);
        rig.image(&device, &queue, &before, camera, CLOUD_DT_NS);
        rig.image(&device, &queue, &after, camera, CLOUD_DT_NS);
        rig.display(&queue, 0.1);
        for eye in 0..2 {
            let mask =
                rig.mask_with_arms(&device, &queue, eye, Some(rig.shown()), [Some(arm), None]);
            let zone = [85, 105].map(|x| cloud_zone_at(&mask, x, 64));
            let expected = if mode == 1 { [255, 0] } else { [0, 255] };
            assert_eq!(zone, expected, "cloud {mode} eye {eye}");
        }
    }
}

// Eye 1 is still on the field of image B while eye 0 is on that of image C (the display frame of C,
// eye 1 waits for the next one). B and C come from different depth cameras (C turned 3 degrees): each
// eye's zone is placed through its own camera, at its own edge (B: texel 90, C: 96; `cloud_fixed 3`),
// and each eye has the reach time of its own image
#[test]
fn cloud_field_follows_its_own_eye() {
    let (device, queue) = gpu_or_skip!();
    let params = CloudParams {
        fixed: 3.0,
        ..Default::default()
    };
    let mut rig = CloudRig::new(&device, &queue, params);
    let origin = cloud_camera(0.0, 0.0, Vec3::ZERO);
    for dx in [0, 6] {
        let depth = cloud_depth(&origin, CLOUD_WALL, &[cloud_box(CLOUD_BOX, dx)]);
        rig.image(&device, &queue, &depth, origin, CLOUD_DT_NS);
    }
    let capture_b = rig.capture_ns;
    let turned = cloud_camera(3.0, 0.0, Vec3::ZERO);
    let depth = cloud_depth(&turned, CLOUD_WALL, &[cloud_box(CLOUD_BOX, 12)]);
    let passes = rig.image_frames(&device, &queue, &depth, turned, CLOUD_DT_NS, 1);
    assert!(passes[0].is_some_and(|pass| pass.reason.is_none()));
    assert!(passes[1].is_none());

    let status = rig.cloud.status();
    assert_eq!(status.eyes[0].field_capture_ns, Some(rig.capture_ns));
    assert_eq!(status.eyes[1].field_capture_ns, Some(capture_b));
    // Eye 1's image is 67 ms older: its reach time is 1.5 x that longer
    let older = status.eyes[1].reach_ms - status.eyes[0].reach_ms;
    assert!((older - 100.0).abs() < 0.1, "{older}");

    let eye0 = rig.mask(&device, &queue, 0, Some(rig.shown()));
    let eye1 = rig.mask(&device, &queue, 1, Some(rig.shown()));
    for y in [48, 64, 80] {
        assert_eq!(cloud_zone_at(&eye1, 89, y), 255, "{y}");
        assert_eq!(cloud_zone_at(&eye1, 95, y), 0, "{y}");
        assert_eq!(cloud_zone_at(&eye0, 95, y), 255, "{y}");
        assert_eq!(cloud_zone_at(&eye0, 89, y), 0, "{y}");
    }
}

// Eye 1 has a depth camera and a depth image of its own: 6.25 cm to the right of eye 0's, so that the
// box at 0.5 m lies exactly 8 texels further left in its image (cells 4 further left). The box moves 6
// texels to the right. Each eye's field has the distances of its own image and the speeds `pair_speed`
// gives on its own masks: a pass that read eye 0's depth layer or camera for eye 1 would put eye 1's
// edges at eye 0's cells. The mask looks eye 1's field up through eye 1's camera (`cloud_fixed 3`):
// from the view at the origin, at the fetch distance (0.45 m along the ray), its leading edge shows at
// about 91.7 (eye 0's at 90), not at 82 where eye 0's camera would put it
#[test]
fn cloud_eye_1_measures_its_own_image() {
    let (device, queue) = gpu_or_skip!();
    let params = CloudParams {
        fixed: 3.0,
        ..Default::default()
    };
    let mut rig = CloudRig::new(&device, &queue, params);
    // 8 texels of 1 / 64 of a tangent each, at the box's distance
    let baseline = 8.0 / 64.0 * CLOUD_BOX;
    let cameras = [
        cloud_camera(0.0, 0.0, Vec3::ZERO),
        cloud_camera(0.0, 0.0, Vec3::new(baseline, 0.0, 0.0)),
    ];
    for (image, dx) in [0, 6].into_iter().enumerate() {
        let depths =
            cameras.map(|camera| cloud_depth(&camera, CLOUD_WALL, &[cloud_box(CLOUD_BOX, dx)]));
        // The box is 8 texels further left in eye 1's image (row 64: its texels 16..76 at first)
        let near = depth_mask::encode_depth(1.0, NEAR, f32::INFINITY);
        let row = 64 * CLOUD_DEPTH_SIZE as usize;
        for (eye, depth) in depths.iter().enumerate() {
            let left = (0..CLOUD_DEPTH_SIZE as usize)
                .position(|x| depth[row + x] < near)
                .unwrap();
            assert_eq!(left as i32, 24 + dx - 8 * eye as i32, "eye {eye}");
        }
        let passes = rig.stereo_image(
            &device,
            &queue,
            [&depths[0], &depths[1]],
            cameras,
            CLOUD_DT_NS,
        );
        let expected = (image == 0).then_some(ResetReason::Start);
        assert_eq!(passes.map(|pass| pass.reason), [expected; 2]);
    }

    let dt = CLOUD_DT_NS as f32 / 1e9;
    // One step of the encoding of phi, as in cloud_window_distance_matches_rust
    let step = 2.0 * CLOUD_TRUNCATION / 255.0;
    let window = CLOUD_WINDOW_CELLS as usize;
    let border = |index: usize| {
        let (x, y) = (index % CLOUD_CELLS, index / CLOUD_CELLS);
        x.min(y) < window || x.max(y) >= CLOUD_CELLS - window
    };
    for eye in 0..2 {
        // This eye's masks: eye 0's box moved by -8 texels for eye 1
        let phi_of = |dx: i32| {
            let rect = cloud_box(CLOUD_BOX, dx - 8 * eye as i32);
            let (cells, width, height) = cells_inside(
                &rect.inside(),
                CLOUD_DEPTH_SIZE as usize,
                CLOUD_DEPTH_SIZE as usize,
            );
            window_distance(&cells, width, height)
        };
        let (phi, phip) = (phi_of(6), phi_of(0));
        let field = rig.field(&device, &queue, eye);
        let mut measured = 0;
        for (index, cell) in field.iter().enumerate() {
            let at = (index % CLOUD_CELLS, index / CLOUD_CELLS);
            assert!(
                (cloud_phi(*cell) - phi[index]).abs() <= step,
                "eye {eye} cell {at:?}: {} {}",
                cloud_phi(*cell),
                phi[index]
            );
            // Not where the difference is at the deadband
            if ((phi[index] - phip[index]).abs() - params.dead).abs() < 0.05 {
                continue;
            }
            let expected = pair_speed(phi[index], phip[index], border(index), dt, &params);
            let (held, speed) = cloud_speeds(*cell);
            assert!(
                (speed - expected).abs() <= CLOUD_SPEED_STEP && held == speed,
                "eye {eye} cell {at:?}: {held} {speed} {expected}"
            );
            if speed > 0.0 {
                measured += 1;
            }
        }
        assert!(measured > 500, "eye {eye}: {measured}");
        // The leading edge: eye 0's between cells 44 and 45, eye 1's between 40 and 41
        let cell = |x: usize| cloud_phi(field[32 * CLOUD_CELLS + x]);
        if eye == 0 {
            assert!(cell(44) < 0.0 && cell(45) > 0.0);
        } else {
            assert!(cell(40) < 0.0 && cell(41) > 0.0 && cell(44) > 0.0);
        }
    }

    rig.display(&queue, 0.1);
    for eye in 0..2 {
        let mask = rig.mask(&device, &queue, eye, Some(rig.shown()));
        let zone = [82, 90, 97].map(|x| cloud_zone_at(&mask, x, 64));
        assert_eq!(zone, [0, 255, 0], "eye {eye}");
    }
}

// The depth stalls: no new image, or the runtime keeps returning the last one. A field up to 300 ms old
// still opens its zone (speed x age, and with `cloud_fixed 8`); older it opens none, in both eyes, and
// the status marks it stale with a reach of 0. `forget` empties the records of the eyes, not the counts
#[test]
fn cloud_stale_field_has_no_zone() {
    let (device, queue) = gpu_or_skip!();
    let ms = 1_000_000;
    for fixed in [0.0, 8.0] {
        let params = CloudParams {
            fixed,
            ..Default::default()
        };
        let mut rig = CloudRig::new(&device, &queue, params);
        let camera = cloud_camera(0.0, 0.0, Vec3::ZERO);
        let before = cloud_depth(&camera, CLOUD_WALL, &[cloud_box(CLOUD_BOX, 0)]);
        let after = cloud_depth(&camera, CLOUD_WALL, &[cloud_box(CLOUD_BOX, 6)]);
        rig.image(&device, &queue, &before, camera, CLOUD_DT_NS);
        rig.image(&device, &queue, &after, camera, CLOUD_DT_NS);
        // (age, the same depth frame again or none, open)
        for (age_ms, same_frame, open) in [
            (299, true, true),
            (301, true, false),
            (299, false, true),
            (301, false, false),
            (2000, false, false),
        ] {
            rig.display_at(&queue, age_ms * ms, same_frame);
            let status = rig.cloud.status();
            let what = format!("fixed {fixed} age {age_ms} same frame {same_frame}");
            for eye in 0..2 {
                assert_eq!(status.eyes[eye].stale, !open, "{what} eye {eye}");
                if !open {
                    assert_eq!(status.eyes[eye].reach_ms, 0.0, "{what} eye {eye}");
                }
                let mask = rig.mask(&device, &queue, eye, Some(rig.shown()));
                // 3.5 texels outside the leading and 3.5 outside the trailing edge
                let zone = [93, 26].map(|x| cloud_zone_at(&mask, x, 64));
                let expected = if open { 255 } else { 0 };
                assert_eq!(zone, [expected; 2], "{what} eye {eye}");
                if !open {
                    assert!(mask.iter().all(|texel| texel[1] == 0), "{what} eye {eye}");
                }
            }
        }

        rig.cloud.forget();
        let status = rig.cloud.status();
        assert_eq!(status.eyes, [crate::CloudEyeRecord::default(); 2]);
        assert_eq!(status.pairs, [2, 2]);
    }
}

// The scenes of mask_v2_switches_off_are_today, with and without the colour key: with the cloud off
// (None), in its today's phase, without the key, with the distance region and with the Show depth
// view the mask (all four channels) and the picture are those of a renderer without the cloud, also
// when the mask is given a field that would open the zone everywhere. The control: shown, that field
// changes only mask g, and the picture
#[test]
fn cloud_off_is_today() {
    let (device, queue) = gpu_or_skip!();
    let mut config = base_config(DepthMaskRegion::Volume(CockpitVolumeConfig {
        center_x: 0.05,
        center_y: 0.0,
        center_z: -0.6,
        yaw_deg: 10.0,
        width: 0.5,
        height: 0.4,
        depth: 0.5,
    }));
    config.edge_feathering = 0.03;
    let scene = Scene::new(0.8, [0.1, 0.1, 0.6, 0.7], 0.55);
    let arms = [
        Some(arm_motion(
            Vec3::new(0.0, 0.0, -0.55),
            Vec3::new(0.03, 0.0, 0.0),
            0.3,
        )),
        Some(arm_motion(Vec3::new(-0.1, 0.05, -0.5), Vec3::ZERO, 0.0)),
    ];
    let render = |config: &DepthPassthroughConfig, color_key, mask_v2| {
        render_v2(
            &device,
            &queue,
            config,
            &scene,
            Vec3::ZERO,
            Vec3::ZERO,
            true,
            color_key,
            arms,
            None,
            None,
            false,
            &[],
            None,
            0.0,
            0.0,
            mask_v2,
        )
    };
    let shown = CloudInput {
        params: CloudParams::default(),
        capture_ns: Some(0),
        display_ns: 100_000_000,
        shown: true,
    };
    let today_phase = CloudInput {
        shown: false,
        ..shown
    };
    let probe = |cloud| MaskV2 {
        cloud,
        cloud_probe: true,
        ..Default::default()
    };
    let same = |a: &Output, b: &Output, what: &str| {
        assert!(a.mask == b.mask, "{what}: mask");
        assert!(a.pixels == b.pixels, "{what}: picture");
    };

    let key = Some(cloud_key());
    for color_key in [None, key] {
        let off = render(&config, color_key, MaskV2::default());
        same(&off, &render(&config, color_key, probe(None)), "off");
        same(
            &off,
            &render(&config, color_key, probe(Some(today_phase))),
            "today's phase",
        );
        if color_key.is_none() {
            same(
                &off,
                &render(&config, color_key, probe(Some(shown))),
                "no key",
            );
        }
    }
    let distance = base_config(DepthMaskRegion::Distance { max_distance: 0.7 });
    same(
        &render(&distance, key, MaskV2::default()),
        &render(&distance, key, probe(Some(shown))),
        "distance region",
    );
    let show_depth = DepthPassthroughConfig {
        debug_view: DepthPassthroughDebugView::ShowDepth,
        ..config.clone()
    };
    same(
        &render(&show_depth, key, MaskV2::default()),
        &render(&show_depth, key, probe(Some(shown))),
        "show depth",
    );

    // The control
    let off = render(&config, key, MaskV2::default());
    let on = render(&config, key, probe(Some(shown)));
    for (a, b) in off.mask.iter().zip(&on.mask) {
        assert_eq!([a[0], a[2], a[3]], [b[0], b[2], b[3]]);
    }
    assert!(on.mask.iter().all(|texel| texel[1] == 255));
    assert!(off.pixels != on.pixels);
}

// The zone opens the key only where the lower layer is used (here the left half of the view): outside
// it the picture is the same with and without a zone (a field that opens it everywhere)
#[test]
fn cloud_zone_is_inert_outside_the_lower_layer() {
    let (device, queue) = gpu_or_skip!();
    let config = cloud_config();
    // A box at 0.5 m in the region in the middle of the view, the room behind it out of the region
    let scene = Scene::new(3.0, [0.3, 0.3, 0.7, 0.7], 0.5);
    let lower = (Vec2::ZERO, Vec2::new(0.5, 1.0));
    let render = |mask_v2| {
        render_v2(
            &device,
            &queue,
            &config,
            &scene,
            Vec3::ZERO,
            Vec3::ZERO,
            true,
            Some(cloud_key()),
            [None; 2],
            None,
            Some([lower; 2]),
            false,
            &[],
            None,
            0.0,
            0.0,
            mask_v2,
        )
    };
    let without = render(MaskV2::default());
    let with = render(MaskV2 {
        cloud: Some(CloudInput {
            params: CloudParams::default(),
            capture_ns: Some(0),
            display_ns: 100_000_000,
            shown: true,
        }),
        cloud_probe: true,
        ..Default::default()
    });
    let mut changed_inside = 0;
    for (index, (a, b)) in without.pixels.iter().zip(&with.pixels).enumerate() {
        let u = (index as u32 % TARGET_SIZE) as f32 + 0.5;
        if u >= 0.5 * TARGET_SIZE as f32 {
            assert_eq!(a, b, "pixel {index}");
        } else if a != b {
            changed_inside += 1;
        }
    }
    // Inside it the game where the mask does not show the room is now keyed: half the packed alpha
    assert!(changed_inside > 100, "{changed_inside}");
    assert!(
        (with.alpha(0.1, 0.1) - 0.5).abs() < 0.003,
        "{}",
        with.alpha(0.1, 0.1)
    );
    assert_eq!(without.alpha(0.1, 0.1), 1.0);
}

/// Probe cells for `cloud_trail`: inside the mask 2.8 and 10.1 texels from the edge at the largest
/// speed, and 2.8 texels inside at rest
const CLOUD_TRAIL_NEAR: [u8; 4] = [112, 255, 255, 128];
const CLOUD_TRAIL_DEEP: [u8; 4] = [74, 255, 255, 128];
const CLOUD_TRAIL_STILL: [u8; 4] = [112, 0, 0, 128];

/// The picture and the mask with a probe field of `cell` and `cloud_trail` `trail`: a box at 0.5 m in
/// the region in the middle of the view (0.3..0.7), the room behind it out of the region, the lower
/// layer in the left half of the view
fn cloud_trail_render(device: &Device, queue: &Queue, cell: [u8; 4], trail: f32) -> Output {
    let scene = Scene::new(3.0, [0.3, 0.3, 0.7, 0.7], 0.5);
    let lower = (Vec2::ZERO, Vec2::new(0.5, 1.0));
    render_v2(
        device,
        queue,
        &cloud_config(),
        &scene,
        Vec3::ZERO,
        Vec3::ZERO,
        true,
        Some(cloud_key()),
        [None; 2],
        None,
        Some([lower; 2]),
        false,
        &[],
        None,
        0.0,
        0.0,
        MaskV2 {
            cloud: Some(CloudInput {
                params: CloudParams {
                    trail,
                    ..Default::default()
                },
                capture_ns: Some(0),
                display_ns: 100_000_000,
                shown: true,
            }),
            cloud_probe: true,
            cloud_probe_cell: Some(cell),
            ..Default::default()
        },
    )
}

/// The box of `cloud_zone_grows_with_the_age` moved 6 texels (edges now at texels 30, trailing, and
/// 90, leading), with `cloud_trail` `trail`, at a reach time of `reach_s`: the masks of both eyes
fn cloud_trail_masks(
    device: &Device,
    queue: &Queue,
    trail: f32,
    reach_s: f32,
) -> [Vec<[u8; 4]>; 2] {
    let params = CloudParams {
        trail,
        ..Default::default()
    };
    let mut rig = CloudRig::new(device, queue, params);
    let camera = cloud_camera(0.0, 0.0, Vec3::ZERO);
    let before = cloud_depth(&camera, CLOUD_WALL, &[cloud_box(CLOUD_BOX, 0)]);
    let after = cloud_depth(&camera, CLOUD_WALL, &[cloud_box(CLOUD_BOX, 6)]);
    rig.image(device, queue, &before, camera, CLOUD_DT_NS);
    rig.image(device, queue, &after, camera, CLOUD_DT_NS);
    rig.display(queue, reach_s);
    [0, 1].map(|eye| rig.mask(device, queue, eye, Some(rig.shown())))
}

// `cloud_trail 0` (the default) is the picture of the cloud without it: (a) on the moving box the zone
// inside the mask reaches as far as outside (14 texels at 0.3 s: 10.5 in at both moved edges); (b)
// with the probe field, a cell inside the mask (2.8 or 10.1 texels) gives the same mask (all channels)
// and picture as a cell on the edge, as the zone is the same on both sides. The control: with
// `cloud_trail 6` the picture changes, and the cell 10.1 texels in differs from the one on the edge
#[test]
fn cloud_trail_off_is_the_same_picture() {
    let (device, queue) = gpu_or_skip!();
    assert_eq!(CloudParams::default().trail, 0.0);

    // (a)
    for (eye, mask) in cloud_trail_masks(&device, &queue, 0.0, 0.3)
        .iter()
        .enumerate()
    {
        for x in [26, 33, 40, 80, 87, 93] {
            assert_eq!(cloud_zone_at(mask, x, 64), 255, "eye {eye} x {x}");
        }
    }

    // (b)
    let edge = cloud_trail_render(&device, &queue, CLOUD_PROBE_CELL, 0.0);
    for cell in [CLOUD_TRAIL_NEAR, CLOUD_TRAIL_DEEP] {
        let inside = cloud_trail_render(&device, &queue, cell, 0.0);
        assert!(inside.mask == edge.mask, "{cell:?}: mask");
        assert!(inside.pixels == edge.pixels, "{cell:?}: picture");
    }
    // Inside the box the mask shows the room: both layers open
    assert_eq!(edge.alpha(0.4, 0.5), 0.0);
    // The control: the trail changes the picture, and the cell 10.1 texels in is cut off by it
    let edge_trail = cloud_trail_render(&device, &queue, CLOUD_PROBE_CELL, 6.0);
    assert!(edge_trail.pixels != edge.pixels);
    let deep = cloud_trail_render(&device, &queue, CLOUD_TRAIL_DEEP, 6.0);
    assert!(deep.mask != edge_trail.mask);
    assert!(deep.pixels != edge_trail.pixels);
}

// `cloud_trail 6` at a reach of 14 texels. (a) The mask of the moving box: the zone inside the mask is
// at most 6 texels deep (3.5 texels in at both moved edges: open, 10.5 in: none), outside it as
// without the trail; none inside the top edge the box slid along; only mask g changes, and only
// downwards. (b) The picture with the probe field: inside the mask 2.8 texels from a moving edge the
// lower game layer is opaque and the upper one open (packed 0.5: the key decides), 10.1 texels in or at
// rest the mask still shows the room (0); outside the lower layer (right half) the picture is that of
// `cloud_trail 0`
#[test]
fn cloud_trail_hands_the_inner_border_to_the_key() {
    let (device, queue) = gpu_or_skip!();

    // (a)
    let today = cloud_trail_masks(&device, &queue, 0.0, 0.3);
    let trail = cloud_trail_masks(&device, &queue, 6.0, 0.3);
    for eye in 0..2 {
        let zone = |x, y| cloud_zone_at(&trail[eye], x, y);
        for x in [33, 87] {
            assert_eq!(zone(x, 64), 255, "eye {eye} x {x}");
        }
        for x in [40, 80] {
            assert_eq!(zone(x, 64), 0, "eye {eye} x {x}");
        }
        for x in [26, 93] {
            assert_eq!(zone(x, 64), 255, "eye {eye} x {x}");
        }
        assert_eq!(zone(105, 64), 0, "eye {eye}");
        // 3.5 texels inside the top edge, in the middle of the box
        assert_eq!(zone(60, 108), 0, "eye {eye}");
        for (a, b) in today[eye].iter().zip(&trail[eye]) {
            assert_eq!([a[0], a[2], a[3]], [b[0], b[2], b[3]], "eye {eye}");
            assert!(b[1] <= a[1], "eye {eye}");
        }
        assert!(today[eye] != trail[eye], "eye {eye}");
    }

    // (b)
    let near = cloud_trail_render(&device, &queue, CLOUD_TRAIL_NEAR, 6.0);
    assert!(
        (near.alpha(0.4, 0.5) - 0.5).abs() < 0.003,
        "{}",
        near.alpha(0.4, 0.5)
    );
    assert_eq!(
        cloud_trail_render(&device, &queue, CLOUD_TRAIL_NEAR, 0.0).alpha(0.4, 0.5),
        0.0
    );
    for cell in [CLOUD_TRAIL_DEEP, CLOUD_TRAIL_STILL] {
        assert_eq!(
            cloud_trail_render(&device, &queue, cell, 6.0).alpha(0.4, 0.5),
            0.0,
            "{cell:?}"
        );
    }
    for cell in [CLOUD_TRAIL_NEAR, CLOUD_TRAIL_DEEP] {
        let off = cloud_trail_render(&device, &queue, cell, 0.0);
        let on = cloud_trail_render(&device, &queue, cell, 6.0);
        for (index, (a, b)) in off.pixels.iter().zip(&on.pixels).enumerate() {
            if index as u32 % TARGET_SIZE >= TARGET_SIZE / 2 {
                assert_eq!(a, b, "{cell:?} pixel {index}");
            }
        }
    }
}

// The trail is at most the reach: at a reach time of 0.022 s (2 texels at 90 texels per second)
// `cloud_trail 6` gives the mask of `cloud_trail 0` (all channels), open right inside the moved edges
// and closed 3.5 texels in, where a border of 6 would still be open (as it is at 0.3 s)
#[test]
fn cloud_trail_is_capped_by_the_reach() {
    let (device, queue) = gpu_or_skip!();
    let today = cloud_trail_masks(&device, &queue, 0.0, 0.022);
    let trail = cloud_trail_masks(&device, &queue, 6.0, 0.022);
    let wide = cloud_trail_masks(&device, &queue, 6.0, 0.3);
    for eye in 0..2 {
        assert!(today[eye] == trail[eye], "eye {eye}");
        let zone = |x| cloud_zone_at(&trail[eye], x, 64);
        for x in [30, 89] {
            assert!(zone(x) > 0, "eye {eye} x {x}");
        }
        for x in [33, 86] {
            assert_eq!(zone(x), 0, "eye {eye} x {x}");
            assert_eq!(cloud_zone_at(&wide[eye], x, 64), 255, "eye {eye} x {x}");
        }
    }
}

// `cloud_ab`: a square of 2 % of the view at (0.5, 0.4), green while the cloud is shown, red in today's
// phase, nothing without `cloud_ab`
#[test]
fn cloud_ab_marks_the_phase() {
    let (device, queue) = gpu_or_skip!();
    let config = cloud_config();
    let scene = Scene::new(3.0, [0.0, 0.0, 0.2, 0.2], 0.5);
    let render = |ab, shown| {
        let cloud = CloudInput {
            params: CloudParams {
                ab,
                ..Default::default()
            },
            capture_ns: Some(0),
            display_ns: 100_000_000,
            shown,
        };
        render_v2(
            &device,
            &queue,
            &config,
            &scene,
            Vec3::ZERO,
            Vec3::ZERO,
            true,
            Some(cloud_key()),
            [None; 2],
            None,
            None,
            false,
            &[],
            None,
            0.0,
            0.0,
            MaskV2 {
                cloud: Some(cloud),
                ..Default::default()
            },
        )
    };
    let gray = [STREAM_GRAY; 3];
    for (ab, shown, mark) in [
        (true, true, [0, 204, 0]),
        (true, false, [229, 0, 0]),
        (false, true, gray),
        (false, false, gray),
    ] {
        let output = render(ab, shown);
        assert_eq!(output.at(0.5, 0.4)[..3], mark, "ab {ab} shown {shown}");
        // 2 pixels (3 %) away: the game
        assert_eq!(output.at(0.5 + 2.0 / 64.0, 0.4)[..3], gray);
        assert_eq!(output.at(0.5, 0.4 - 2.0 / 64.0)[..3], gray);
    }
}

// The `cloud` vector of the depth uniforms, at the place of the shader: zeros while off; the mode
// while shown with the key and a region of boxes; 0 in today's phase; the mark of `cloud_ab`
#[test]
fn cloud_vector_in_the_depth_uniforms() {
    let source = stream::depth_mask_shader_source();
    let module = naga::front::wgsl::parse_str(&source).unwrap();
    let at = module
        .types
        .iter()
        .find_map(|(_, ty)| match &ty.inner {
            naga::TypeInner::Struct { members, .. }
                if ty.name.as_deref() == Some("DepthUniforms") =>
            {
                members
                    .iter()
                    .find(|member| member.name.as_deref() == Some("cloud"))
                    .map(|member| member.offset as usize / 4)
            }
            _ => None,
        })
        .unwrap();
    let camera = cloud_camera(0.0, 0.0, Vec3::ZERO);
    let input = |key: bool, cloud: Option<CloudInput>| DepthRenderInput {
        frame: None,
        eye_positions: [Vec3::ZERO; 2],
        color_key: key.then(cloud_key),
        arms: [None; 2],
        arm_gaps: [None; 2],
        edge_cut: 0.0,
        lever_windows: [None; MAX_LEVER_WINDOWS],
        capture_playzone: false,
        playzone_view: false,
        memory: None,
        cloud,
        key_lower_rects: None,
        region_override: None,
        game_dim: 0.0,
        game_see: 0.0,
    };
    let vector = |config: &DepthPassthroughConfig, key: bool, cloud: Option<CloudInput>| {
        let data =
            depth_mask::depth_uniforms(config, &[camera; 2], &input(key, cloud), None, false);
        <[f32; 4]>::try_from(&data[at..at + 4]).unwrap()
    };
    let cloud = |mode, ab, shown| {
        Some(CloudInput {
            params: CloudParams {
                mode,
                ab,
                ..Default::default()
            },
            capture_ns: None,
            display_ns: 0,
            shown,
        })
    };
    let volume = cloud_config();
    let distance = base_config(DepthMaskRegion::Distance { max_distance: 0.9 });
    let show_depth = DepthPassthroughConfig {
        debug_view: DepthPassthroughDebugView::ShowDepth,
        ..cloud_config()
    };
    assert_eq!(vector(&volume, true, None), [0.0; 4]);
    assert_eq!(
        vector(&volume, true, cloud(1, false, true)),
        [1.0, 0.0, 0.0, 0.0]
    );
    assert_eq!(
        vector(&volume, true, cloud(2, false, true)),
        [2.0, 0.0, 0.0, 0.0]
    );
    assert_eq!(
        vector(&volume, true, cloud(1, true, true)),
        [1.0, 1.0, 0.0, 0.0]
    );
    assert_eq!(
        vector(&volume, true, cloud(1, true, false)),
        [0.0, 2.0, 0.0, 0.0]
    );
    assert_eq!(vector(&volume, true, cloud(1, false, false)), [0.0; 4]);
    assert_eq!(vector(&volume, true, cloud(0, false, true)), [0.0; 4]);
    assert_eq!(vector(&volume, false, cloud(1, true, true)), [0.0; 4]);
    assert_eq!(vector(&distance, true, cloud(1, true, true)), [0.0; 4]);
    assert_eq!(vector(&show_depth, true, cloud(1, false, true)), [0.0; 4]);
    // `cloud_trail` in z, only while the zone is shown
    let trail = |cloud: Option<CloudInput>, trail| {
        cloud.map(|cloud| CloudInput {
            params: CloudParams {
                trail,
                ..cloud.params
            },
            ..cloud
        })
    };
    assert_eq!(
        vector(&volume, true, trail(cloud(1, false, true), 6.0)),
        [1.0, 0.0, 6.0, 0.0]
    );
    assert_eq!(
        vector(&volume, true, trail(cloud(2, true, true), 6.0)),
        [2.0, 1.0, 6.0, 0.0]
    );
    assert_eq!(
        vector(&volume, true, trail(cloud(1, true, false), 6.0)),
        [0.0, 2.0, 0.0, 0.0]
    );
    for config in [&distance, &show_depth] {
        assert_eq!(
            vector(config, true, trail(cloud(1, false, true), 6.0)),
            [0.0; 4]
        );
    }
    assert_eq!(
        vector(&volume, false, trail(cloud(1, false, true), 6.0)),
        [0.0; 4]
    );
    // Show depth only hides the zone: the passes run
    assert!(depth_mask::cloud_runs(
        &show_depth,
        &input(true, cloud(1, false, true))
    ));
    assert!(!depth_mask::cloud_runs(
        &distance,
        &input(true, cloud(1, false, true))
    ));
    assert!(!depth_mask::cloud_runs(
        &volume,
        &input(false, cloud(2, false, true))
    ));
    assert!(!depth_mask::cloud_runs(&volume, &input(true, None)));

    // `cloud_shown`, which the client's arm shift follows, holds exactly where the vector has a mode:
    // not with Show depth, not in today's phase, not where the cloud does not run
    for config in [&volume, &distance, &show_depth] {
        for key in [false, true] {
            for cloud in [
                None,
                cloud(0, false, true),
                cloud(1, false, true),
                cloud(1, true, false),
                cloud(1, true, true),
                cloud(2, false, true),
            ] {
                let runs = depth_mask::cloud_runs(config, &input(key, cloud));
                assert_eq!(
                    depth_mask::cloud_shown(config, cloud.as_ref(), runs),
                    vector(config, key, cloud)[0] > 0.0,
                    "{:?} key {key} {cloud:?}",
                    config.debug_view
                );
            }
        }
    }
    assert!(!depth_mask::cloud_shown(
        &show_depth,
        cloud(1, false, true).as_ref(),
        true
    ));
    assert!(depth_mask::cloud_shown(
        &volume,
        cloud(1, false, true).as_ref(),
        true
    ));
}
