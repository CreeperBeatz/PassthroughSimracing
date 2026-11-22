//! Shader tests that don't need a headset:
//! - the WGSL shaders must translate to GLSL ES 3.2 (the backend used on Quest)
//! - the depth passthrough shader is run on the desktop GPU against a synthetic depth scene

use crate::{
    depth_mask::{
        self, ArmMotion, ColorKeyInput, DepthFrame, DepthRenderInput, HAND_SEGMENTS, HandShape,
        HandZones,
    },
    stream::{self, FOVEATION_UNIFORM_SIZE, LIGHT_PROBE_SIZE, StreamPipelines, floats_to_bytes},
};
use alvr_common::{
    Fov, Pose, ViewParams,
    glam::{Mat4, UVec2, Vec3, Vec4},
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

    let depth_mask_view = stream::create_depth_mask_texture(device, UVec2::splat(TARGET_SIZE * 4))
        .create_view(&TextureViewDescriptor::default());

    let focus_area_view = stream::create_focus_area_texture(device, UVec2::splat(TARGET_SIZE * 4))
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
            depth_views: [view(depth_camera_position); 2],
        }),
        eye_positions: [eye_position; 2],
        color_key,
        arms,
    };
    let lighting_enabled = config.lighting.as_option().is_some();
    let uniforms =
        depth_mask::depth_uniforms(config, &[view(eye_position); 2], &input, lighting_enabled);
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

    stream::encode_depth_mask(
        &mut encoder,
        &pipelines,
        &depth_mask_view,
        &depth_bind_group,
        0,
    );
    if color_key.is_some() {
        let bind_group = stream::create_focus_area_bind_group(device, &pipelines, &uniform_buffer);
        stream::encode_focus_area(&mut encoder, &pipelines, &focus_area_view, &bind_group, 0);
    }

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
        pass.draw(0..4, 0..1);
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

    Output {
        pixels: data
            .chunks_exact(4)
            .map(|c| [c[0], c[1], c[2], c[3]])
            .collect(),
    }
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
                    },
                )),
            ],
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
    solid[HAND_SEGMENTS - 1] = 0.5;
    let half = render_zones(HandZones { solid, ..zones });
    assert!((half.alpha(0.5, 0.75) - 0.25).abs() < 0.03);

    // A fast hand is keyed, not solid, also further away
    let fast = render_zones(zones.for_speed(10.0, 0.02, 0.004, 0.05));
    assert!(fast.alpha(0.5, 0.75) > out.alpha(0.5, 0.75) + 0.1);
    assert_eq!(out.alpha(0.5, 0.9), 1.0);
    // Near the end of the rim the key fades out
    let faded = fast.alpha(0.5, 0.9);
    assert!((0.55..0.95).contains(&faded), "{faded}");
    assert!(fast.alpha(0.5, 0.85) < faded);
}

#[test]
fn hand_zones_follow_the_speed() {
    let zones = HandZones {
        core: -0.004,
        falloff: 0.008,
        rim: 0.01,
        solid: [1.0; HAND_SEGMENTS],
    };

    let still = zones.for_speed(0.0, 0.01, 0.04, 0.05);
    assert!((still.core - zones.core).abs() < 1e-6 && (still.rim - zones.rim).abs() < 1e-6);

    // 1 m/s: 1 cm and 4 cm
    let moving = zones.for_speed(1.0, 0.01, 0.04, 0.05);
    assert!((moving.core - -0.014).abs() < 1e-6 && (moving.rim - 0.05).abs() < 1e-6);

    // Limited
    let fast = zones.for_speed(100.0, 0.01, 0.04, 0.05);
    assert!((fast.core - -0.044).abs() < 1e-6 && (fast.rim - 0.06).abs() < 1e-6);
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
    let render_arm = |displacement: Vec3| {
        render_full(
            &device,
            &queue,
            &config,
            &scene,
            Vec3::ZERO,
            Vec3::ZERO,
            true,
            None,
            [
                None,
                Some(ArmMotion {
                    wrist: Vec3::new(0.05, 0.05, -0.5),
                    direction: Vec3::NEG_Y,
                    displacement,
                }),
            ],
        )
    };

    let still = render_arm(Vec3::ZERO);
    assert_eq!(still.alpha(0.45, 0.48), 0.0);
    assert_eq!(still.alpha(0.55, 0.48), 1.0);

    // Near the wrist: moved
    let moved = render_arm(Vec3::new(0.1, 0.0, 0.0));
    assert!(moved.alpha(0.55, 0.48) < 0.1, "{}", moved.alpha(0.55, 0.48));
    assert!(moved.alpha(0.43, 0.48) > 0.9, "{}", moved.alpha(0.43, 0.48));
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
