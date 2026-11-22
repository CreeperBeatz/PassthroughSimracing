//! Shader tests that don't need a headset:
//! - the WGSL shaders must translate to GLSL ES 3.2 (the backend used on Quest)
//! - the depth passthrough shader is run on the desktop GPU against a synthetic depth scene

use crate::{
    depth_mask::{
        self, ArmMotion, ColorKeyInput, DepthFrame, DepthRenderInput, HAND_SEGMENTS, HandShape,
        HandSweep, HandZones, SweepParams, WristBridge,
    },
    stream::{self, FOVEATION_UNIFORM_SIZE, LIGHT_PROBE_SIZE, StreamPipelines, floats_to_bytes},
};
use alvr_common::{
    Fov, Pose, ViewParams,
    glam::{Mat4, UVec2, Vec2, Vec3, Vec4},
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
fn playzone_copy_shader_translates_to_gles() {
    translate_to_gles("playzone copy", &stream::playzone_copy_shader_source(), &[]);
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
        None,
        None,
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

    let depth_mask_view = stream::create_depth_mask_texture(device, UVec2::splat(TARGET_SIZE * 5))
        .create_view(&TextureViewDescriptor::default());

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
            depth_views: [view(depth_camera_position); 2],
        }),
        eye_positions: [eye_position; 2],
        color_key,
        arms,
        capture_playzone: false,
        key_lower_rects,
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
    stream::encode_depth_mask(
        &mut encoder,
        &pipelines,
        &depth_mask_view,
        &depth_bind_group,
        &playzone_bind_group,
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
        )
    };
    let motion = |displacement| ArmMotion {
        wrist: Vec3::new(0.05, 0.05, -0.5),
        direction: Vec3::NEG_Y,
        displacement,
    };

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
    );
    assert_eq!(keyed.alpha(0.45, 0.37), 0.0);
    assert!(keyed.alpha(0.55, 0.48) < 0.1, "{}", keyed.alpha(0.55, 0.48));
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
    let render_playzone = |playzone| {
        render_full(
            &device,
            &queue,
            &config,
            &current,
            Vec3::ZERO,
            Vec3::ZERO,
            true,
            Some(key),
            [None; 2],
            playzone,
            None,
        )
    };

    // Beside the hand, 4 cm from its surface
    let without = render_playzone(None);
    assert_eq!(without.alpha(0.555, 0.5), 1.0);

    let with = render_playzone(Some(&remembered));
    assert!(with.alpha(0.555, 0.5) < 0.1, "{}", with.alpha(0.555, 0.5));
    // Above the wheel there is nothing
    assert_eq!(with.alpha(0.5, 0.36), 1.0);
    // Far from the hand the current depth map is right: somebody moved the wheel
    assert_eq!(with.alpha(0.8, 0.5), 1.0);
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
