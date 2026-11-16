use super::{GraphicsContext, MAX_IMMEDIATE_SIZE, staging::StagingRenderer};
use crate::depth_mask::{self, DEPTH_UNIFORMS_SIZE, DepthRenderInput};
use alvr_common::{
    AlvrFoveatedEncodingParams, ViewParams,
    glam::{Mat4, UVec2, Vec2, Vec3, Vec4},
};
use alvr_session::{DepthPassthroughConfig, PassthroughMode, UpscalingConfig};
use std::{borrow::Cow, cell::Cell, ffi::c_void, iter, mem, rc::Rc};
use wgpu::{
    BindGroup, BindGroupDescriptor, BindGroupEntry, BindGroupLayout, BindGroupLayoutDescriptor,
    BindGroupLayoutEntry, BindingResource, BindingType, BlendComponent, BlendFactor,
    BlendOperation, BlendState, Buffer, BufferBindingType, BufferDescriptor, BufferUsages, Color,
    ColorTargetState, ColorWrites, Device, Extent3d, FragmentState, LoadOp,
    PipelineCompilationOptions, PipelineLayoutDescriptor, PrimitiveState, PrimitiveTopology,
    RenderPass, RenderPassColorAttachment, RenderPassDescriptor, RenderPipeline,
    RenderPipelineDescriptor, Sampler, SamplerBindingType, SamplerDescriptor, ShaderModule,
    ShaderModuleDescriptor, ShaderSource, ShaderStages, StoreOp, Texture, TextureDescriptor,
    TextureDimension, TextureFormat, TextureSampleType, TextureUsages, TextureView,
    TextureViewDescriptor, TextureViewDimension, VertexState,
};

const FLOAT_SIZE: u32 = mem::size_of::<f32>() as u32;
const U32_SIZE: u32 = mem::size_of::<u32>() as u32;
const VEC4_SIZE: u32 = mem::size_of::<Vec4>() as u32;
const TRANSFORM_SIZE: u32 = mem::size_of::<Mat4>() as u32;

const TRANSFORM_CONST_OFFSET: u32 = 0;
const VIEW_INDEX_CONST_OFFSET: u32 = TRANSFORM_SIZE;
const PASSTHROUGH_MODE_OFFSET: u32 = VIEW_INDEX_CONST_OFFSET + U32_SIZE;
const ALPHA_CONST_OFFSET: u32 = PASSTHROUGH_MODE_OFFSET + U32_SIZE;
const CK_CHANNEL0_CONST_OFFSET: u32 = ALPHA_CONST_OFFSET + FLOAT_SIZE + U32_SIZE;
const CK_CHANNEL1_CONST_OFFSET: u32 = CK_CHANNEL0_CONST_OFFSET + VEC4_SIZE;
const CK_CHANNEL2_CONST_OFFSET: u32 = CK_CHANNEL1_CONST_OFFSET + VEC4_SIZE;
const IMMEDIATE_SIZE: u32 = CK_CHANNEL2_CONST_OFFSET + VEC4_SIZE;
const PROBE_IMMEDIATE_SIZE: u32 = U32_SIZE;
const FOVEATION_EYE_UNIFORM_VEC4_COUNT: usize = 5;
const FOVEATION_UNIFORM_VEC4_COUNT: usize = 1 + FOVEATION_EYE_UNIFORM_VEC4_COUNT * 2;
pub(crate) const FOVEATION_UNIFORM_SIZE: usize =
    FOVEATION_UNIFORM_VEC4_COUNT * mem::size_of::<Vec4>();
const _: () = assert!(
    IMMEDIATE_SIZE <= MAX_IMMEDIATE_SIZE,
    "Immediate size exceeds the maximum size"
);

const PASSTHROUGH_MODE_DEPTH: u32 = 3;

pub const LIGHT_PROBE_SIZE: u32 = 16;
pub const LIGHT_PROBE_FORMAT: TextureFormat = TextureFormat::Rgba16Float;

const FOVEATION_COMMON_WGSL: &str = include_str!("../resources/foveation_common.wgsl");
const STREAM_WGSL: &str = include_str!("../resources/stream.wgsl");
const LIGHT_PROBE_LOCAL_WGSL: &str = include_str!("../resources/light_probe_local.wgsl");
const LIGHT_PROBE_GLOBAL_WGSL: &str = include_str!("../resources/light_probe_global.wgsl");

pub(crate) fn stream_shader_source() -> String {
    format!("{FOVEATION_COMMON_WGSL}\n{STREAM_WGSL}")
}

pub(crate) fn light_probe_local_shader_source() -> String {
    format!("{FOVEATION_COMMON_WGSL}\n{LIGHT_PROBE_LOCAL_WGSL}")
}

pub(crate) fn light_probe_global_shader_source() -> String {
    LIGHT_PROBE_GLOBAL_WGSL.to_owned()
}

fn shader_module(device: &Device, label: &str, source: String) -> ShaderModule {
    device.create_shader_module(ShaderModuleDescriptor {
        label: Some(label),
        source: ShaderSource::Wgsl(Cow::Owned(source)),
    })
}

fn texture_entry(
    binding: u32,
    filterable: bool,
    dimension: TextureViewDimension,
) -> BindGroupLayoutEntry {
    BindGroupLayoutEntry {
        binding,
        visibility: ShaderStages::FRAGMENT,
        ty: BindingType::Texture {
            sample_type: TextureSampleType::Float { filterable },
            view_dimension: dimension,
            multisampled: false,
        },
        count: None,
    }
}

fn sampler_entry(binding: u32) -> BindGroupLayoutEntry {
    BindGroupLayoutEntry {
        binding,
        visibility: ShaderStages::FRAGMENT,
        ty: BindingType::Sampler(SamplerBindingType::Filtering),
        count: None,
    }
}

fn uniform_entry(binding: u32) -> BindGroupLayoutEntry {
    BindGroupLayoutEntry {
        binding,
        visibility: ShaderStages::FRAGMENT,
        ty: BindingType::Buffer {
            ty: BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

/// Pipelines and layouts of the stream renderer. Separated from `StreamRenderer` so they can be
/// built on any wgpu backend (tests run them on the desktop GPU).
pub(crate) struct StreamPipelines {
    /// stream texture, sampler, foveation uniforms, local light probe, global light probe
    pub view_layout: BindGroupLayout,
    /// depth uniforms, depth texture array
    pub depth_layout: BindGroupLayout,
    pub main: RenderPipeline,
    /// stream texture, sampler, foveation uniforms
    pub probe_local_layout: BindGroupLayout,
    pub probe_local: RenderPipeline,
    /// local light probe
    pub probe_global_layout: BindGroupLayout,
    pub probe_global: RenderPipeline,
}

impl StreamPipelines {
    pub fn new(
        device: &Device,
        target_format: TextureFormat,
        constants: &[(&str, f64)],
        enable_ffe: bool,
    ) -> Self {
        let view_layout = device.create_bind_group_layout(&BindGroupLayoutDescriptor {
            label: Some("stream view"),
            entries: &[
                texture_entry(0, true, TextureViewDimension::D2),
                sampler_entry(1),
                uniform_entry(2),
                texture_entry(3, true, TextureViewDimension::D2),
                texture_entry(4, true, TextureViewDimension::D2),
            ],
        });

        let depth_layout = device.create_bind_group_layout(&BindGroupLayoutDescriptor {
            label: Some("stream depth"),
            entries: &[
                uniform_entry(0),
                // Depth16Unorm can be bound as unfilterable float
                texture_entry(1, false, TextureViewDimension::D2Array),
            ],
        });

        let main_module = shader_module(device, "stream", stream_shader_source());
        let compilation_options = PipelineCompilationOptions {
            constants,
            zero_initialize_workgroup_memory: false,
        };

        let main = device.create_render_pipeline(&RenderPipelineDescriptor {
            label: Some("stream"),
            // Note: Layout cannot be inferred because of a bug with push constants
            layout: Some(&device.create_pipeline_layout(&PipelineLayoutDescriptor {
                label: None,
                bind_group_layouts: &[Some(&view_layout), Some(&depth_layout)],
                immediate_size: IMMEDIATE_SIZE,
            })),
            vertex: VertexState {
                module: &main_module,
                entry_point: None,
                compilation_options: compilation_options.clone(),
                buffers: &[],
            },
            primitive: PrimitiveState {
                topology: PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(FragmentState {
                module: &main_module,
                entry_point: None,
                compilation_options,
                targets: &[Some(ColorTargetState {
                    format: target_format,
                    blend: None,
                    write_mask: ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });

        let probe_local_layout = device.create_bind_group_layout(&BindGroupLayoutDescriptor {
            label: Some("light probe local"),
            entries: &[
                texture_entry(0, true, TextureViewDimension::D2),
                sampler_entry(1),
                uniform_entry(2),
            ],
        });
        let probe_local_module = shader_module(
            device,
            "light probe local",
            light_probe_local_shader_source(),
        );
        let probe_constants = [
            ("ENABLE_FFE", enable_ffe.into()),
            ("PROBE_SIZE", f64::from(LIGHT_PROBE_SIZE)),
        ];
        let probe_compilation_options = PipelineCompilationOptions {
            constants: &probe_constants,
            zero_initialize_workgroup_memory: false,
        };
        // Temporal smoothing: new * k + old * (1 - k), k is the blend constant
        let smoothing_blend = BlendComponent {
            src_factor: BlendFactor::Constant,
            dst_factor: BlendFactor::OneMinusConstant,
            operation: BlendOperation::Add,
        };
        let probe_local = device.create_render_pipeline(&RenderPipelineDescriptor {
            label: Some("light probe local"),
            layout: Some(&device.create_pipeline_layout(&PipelineLayoutDescriptor {
                label: None,
                bind_group_layouts: &[Some(&probe_local_layout)],
                immediate_size: PROBE_IMMEDIATE_SIZE,
            })),
            vertex: VertexState {
                module: &probe_local_module,
                entry_point: None,
                compilation_options: probe_compilation_options.clone(),
                buffers: &[],
            },
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(FragmentState {
                module: &probe_local_module,
                entry_point: None,
                compilation_options: probe_compilation_options,
                targets: &[Some(ColorTargetState {
                    format: LIGHT_PROBE_FORMAT,
                    blend: Some(BlendState {
                        color: smoothing_blend,
                        alpha: smoothing_blend,
                    }),
                    write_mask: ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });

        let probe_global_layout = device.create_bind_group_layout(&BindGroupLayoutDescriptor {
            label: Some("light probe global"),
            entries: &[texture_entry(0, true, TextureViewDimension::D2)],
        });
        let probe_global_module = shader_module(
            device,
            "light probe global",
            light_probe_global_shader_source(),
        );
        let probe_global = device.create_render_pipeline(&RenderPipelineDescriptor {
            label: Some("light probe global"),
            layout: Some(&device.create_pipeline_layout(&PipelineLayoutDescriptor {
                label: None,
                bind_group_layouts: &[Some(&probe_global_layout)],
                immediate_size: 0,
            })),
            vertex: VertexState {
                module: &probe_global_module,
                entry_point: None,
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(FragmentState {
                module: &probe_global_module,
                entry_point: None,
                compilation_options: Default::default(),
                targets: &[Some(ColorTargetState {
                    format: LIGHT_PROBE_FORMAT,
                    blend: None,
                    write_mask: ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });

        Self {
            view_layout,
            depth_layout,
            main,
            probe_local_layout,
            probe_local,
            probe_global_layout,
            probe_global,
        }
    }
}

pub(crate) fn create_light_probe_texture(device: &Device, size: u32) -> Texture {
    device.create_texture(&TextureDescriptor {
        label: Some("light probe"),
        size: Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: TextureDimension::D2,
        format: LIGHT_PROBE_FORMAT,
        usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    })
}

/// Placeholder bound when no environment depth is available
pub(crate) fn create_dummy_depth_texture(device: &Device) -> Texture {
    device.create_texture(&TextureDescriptor {
        label: Some("dummy depth"),
        size: Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 2,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: TextureDimension::D2,
        format: TextureFormat::R32Float,
        usage: TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    })
}

pub(crate) fn depth_array_view(texture: &Texture) -> TextureView {
    texture.create_view(&TextureViewDescriptor {
        dimension: Some(TextureViewDimension::D2Array),
        ..Default::default()
    })
}

pub(crate) fn create_depth_bind_group(
    device: &Device,
    pipelines: &StreamPipelines,
    uniforms: &Buffer,
    depth_view: &TextureView,
) -> BindGroup {
    device.create_bind_group(&BindGroupDescriptor {
        label: Some("stream depth"),
        layout: &pipelines.depth_layout,
        entries: &[
            BindGroupEntry {
                binding: 0,
                resource: uniforms.as_entire_binding(),
            },
            BindGroupEntry {
                binding: 1,
                resource: BindingResource::TextureView(depth_view),
            },
        ],
    })
}

pub(crate) fn create_depth_uniform_buffer(device: &Device) -> Buffer {
    device.create_buffer(&BufferDescriptor {
        label: Some("stream depth uniforms"),
        size: DEPTH_UNIFORMS_SIZE as u64,
        usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

pub(crate) fn floats_to_bytes(values: &[f32]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

pub struct StreamViewParams {
    pub swapchain_index: u32,
    pub input_view_params: ViewParams,
    pub output_view_params: ViewParams,
}

struct LightProbe {
    local_view: TextureView,
    global_view: TextureView,
    local_bind_group: BindGroup,
    global_bind_group: BindGroup,
}

struct ViewObjects {
    bind_group: BindGroup,
    render_target: Vec<TextureView>,
    light_probe: LightProbe,
}

pub struct StreamRenderer {
    context: Rc<GraphicsContext>,
    staging_renderer: StagingRenderer,
    pipelines: StreamPipelines,
    views_objects: [ViewObjects; 2],
    foveated_encoding: Option<AlvrFoveatedEncodingParams>,
    foveation_buffer: Buffer,
    last_foveation_center_shifts: Cell<Option<[Vec2; 2]>>,
    depth_uniform_buffer: Buffer,
    dummy_depth_bind_group: BindGroup,
    // One per environment depth swapchain image
    depth_bind_groups: Vec<BindGroup>,
    light_probe_initialized: Cell<bool>,
}

impl StreamRenderer {
    #[expect(clippy::too_many_arguments)]
    #[cfg_attr(any(target_os = "macos", target_os = "ios"), expect(unused))]
    pub fn new(
        context: Rc<GraphicsContext>,
        base_view_resolution: UVec2,
        target_view_resolution: UVec2,
        swapchain_textures: [Vec<u32>; 2],
        target_format: u32,
        foveated_encoding: Option<AlvrFoveatedEncodingParams>,
        enable_srgb_correction: bool,
        fix_limited_range: bool,
        encoding_gamma: f32,
        upscaling: Option<UpscalingConfig>,
    ) -> Self {
        let device = &context.device;

        let target_format = super::gl_format_to_wgpu(target_format);

        let mut constants = vec![];

        constants.extend([
            ("ENABLE_SRGB_CORRECTION", enable_srgb_correction.into()),
            ("ENCODING_GAMMA", encoding_gamma.into()),
        ]);

        let (staging_resolution, initial_foveation_uniforms) =
            if let Some(config) = &foveated_encoding {
                (
                    UVec2::from_array(config.encoded_view_resolution),
                    foveated_encoding_data(config, config.center_shifts.map(Vec2::from_array)),
                )
            } else {
                (
                    base_view_resolution,
                    [Vec4::ZERO; FOVEATION_UNIFORM_VEC4_COUNT],
                )
            };

        constants.push(("ENABLE_FFE", foveated_encoding.is_some().into()));

        let foveation_buffer = device.create_buffer(&BufferDescriptor {
            label: Some("stream foveation uniforms"),
            size: FOVEATION_UNIFORM_SIZE as u64,
            usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        context.queue.write_buffer(
            &foveation_buffer,
            0,
            &foveation_uniform_bytes(&initial_foveation_uniforms),
        );

        if foveated_encoding.is_none() {
            debug_assert_eq!(staging_resolution, base_view_resolution);
        }

        if let Some(upscaling) = upscaling {
            constants.extend([
                ("ENABLE_UPSCALING", true.into()),
                (
                    "UPSCALE_USE_EDGE_DIRECTION",
                    upscaling.edge_direction.into(),
                ),
                (
                    "UPSCALE_EDGE_THRESHOLD",
                    (upscaling.edge_threshold / 255.0).into(),
                ),
                ("UPSCALE_EDGE_SHARPNESS", upscaling.edge_sharpness.into()),
            ]);
        };

        let pipelines = StreamPipelines::new(
            device,
            target_format,
            &constants,
            foveated_encoding.is_some(),
        );

        let sampler = device.create_sampler(&SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let mut view_objects = vec![];
        let mut staging_textures_gl = vec![];
        for target_swapchain in &swapchain_textures {
            let staging_texture = super::create_texture(device, staging_resolution, target_format);
            let staging_view = staging_texture.create_view(&TextureViewDescriptor::default());

            let light_probe = create_light_probe(
                device,
                &pipelines,
                &staging_view,
                &sampler,
                &foveation_buffer,
            );

            let bind_group = create_view_bind_group(
                device,
                &pipelines,
                &staging_view,
                &sampler,
                &foveation_buffer,
                &light_probe,
            );

            let render_target = super::create_gl_swapchain(
                device,
                target_swapchain,
                target_view_resolution,
                target_format,
            );

            view_objects.push(ViewObjects {
                bind_group,
                render_target,
                light_probe,
            });

            #[cfg(not(any(target_os = "macos", target_os = "ios")))]
            {
                let staging_texture_gl = unsafe {
                    let tex = staging_texture.as_hal::<wgpu::hal::api::Gles>();
                    let wgpu::hal::gles::TextureInner::Texture { raw, .. } = tex.unwrap().inner
                    else {
                        panic!("invalid texture type");
                    };
                    raw
                };
                staging_textures_gl.push(staging_texture_gl);
            }
        }

        let staging_renderer = StagingRenderer::new(
            Rc::clone(&context),
            staging_textures_gl.try_into().unwrap(),
            staging_resolution,
            fix_limited_range,
        );

        let depth_uniform_buffer = create_depth_uniform_buffer(device);
        let dummy_depth_texture = create_dummy_depth_texture(device);
        let dummy_depth_bind_group = create_depth_bind_group(
            device,
            &pipelines,
            &depth_uniform_buffer,
            &depth_array_view(&dummy_depth_texture),
        );

        Self {
            context,
            staging_renderer,
            pipelines,
            views_objects: view_objects.try_into().unwrap_or_else(|_| unreachable!()),
            foveated_encoding,
            foveation_buffer,
            last_foveation_center_shifts: Cell::new(None),
            depth_uniform_buffer,
            dummy_depth_bind_group,
            depth_bind_groups: vec![],
            light_probe_initialized: Cell::new(false),
        }
    }

    /// Register the images of the environment depth swapchain (GL texture arrays with one layer
    /// per eye, GL_DEPTH_COMPONENT16).
    pub fn set_depth_swapchain(&mut self, gl_textures: &[u32], resolution: UVec2) {
        self.context.make_current();

        self.depth_bind_groups = gl_textures
            .iter()
            .map(|&gl_texture| {
                self.context.prepare_external_depth_texture(gl_texture);

                let texture = super::create_depth_array_from_gles(
                    &self.context.device,
                    gl_texture,
                    resolution,
                    2,
                );
                create_depth_bind_group(
                    &self.context.device,
                    &self.pipelines,
                    &self.depth_uniform_buffer,
                    &depth_array_view(&texture),
                )
            })
            .collect();
    }

    pub fn clear_depth_swapchain(&mut self) {
        self.depth_bind_groups.clear();
    }

    /// Runtime foveation centers must already be encoder-aligned. `None` uses the negotiated centers.
    ///
    /// # Safety
    /// `hardware_buffer` must be a valid pointer to a ANativeWindowBuffer.
    pub fn render(
        &self,
        hardware_buffer: *mut c_void,
        view_params: [StreamViewParams; 2],
        passthrough: Option<&PassthroughMode>,
        foveation_center_shifts: Option<[Vec2; 2]>,
        depth: Option<&DepthRenderInput>,
    ) {
        let new_frame = !hardware_buffer.is_null();

        // if hardware_buffer is available copy stream to staging texture
        if new_frame {
            self.staging_renderer.render(hardware_buffer);
        }

        if let Some(config) = &self.foveated_encoding
            && self.last_foveation_center_shifts.get() != foveation_center_shifts
        {
            let center_shifts = foveation_center_shifts
                .unwrap_or_else(|| config.center_shifts.map(Vec2::from_array));
            let uniforms = foveated_encoding_data(config, center_shifts);
            self.context.queue.write_buffer(
                &self.foveation_buffer,
                0,
                &foveation_uniform_bytes(&uniforms),
            );
            self.last_foveation_center_shifts
                .set(foveation_center_shifts);
        }

        let depth_config = match passthrough {
            Some(PassthroughMode::Depth(config)) => Some(config),
            _ => None,
        };

        let mut depth_bind_group = &self.dummy_depth_bind_group;
        let mut lighting_response = None;
        if let Some(config) = depth_config {
            let input = depth.copied().unwrap_or(DepthRenderInput {
                frame: None,
                eye_positions: [Vec3::ZERO; 2],
            });

            // Fall back to "no depth" if the swapchain image is unknown
            let input = DepthRenderInput {
                frame: input
                    .frame
                    .filter(|f| (f.swapchain_index as usize) < self.depth_bind_groups.len()),
                ..input
            };
            if let Some(frame) = &input.frame {
                depth_bind_group = &self.depth_bind_groups[frame.swapchain_index as usize];
            }

            lighting_response = config.lighting.as_option().map(|l| l.response_speed);

            let uniforms = depth_mask::depth_uniforms(
                config,
                &[
                    view_params[0].input_view_params,
                    view_params[1].input_view_params,
                ],
                &input,
                lighting_response.is_some(),
            );
            self.context.queue.write_buffer(
                &self.depth_uniform_buffer,
                0,
                &floats_to_bytes(&uniforms),
            );
        }

        let mut encoder = self
            .context
            .device
            .create_command_encoder(&Default::default());

        if let Some(response_speed) = lighting_response
            && new_frame
        {
            // The first update replaces the (zero initialized) content
            let smoothing = if self.light_probe_initialized.replace(true) {
                response_speed.clamp(0.0, 1.0)
            } else {
                1.0
            };

            for (view_idx, objects) in self.views_objects.iter().enumerate() {
                encode_light_probe_update(
                    &mut encoder,
                    &self.pipelines,
                    &objects.light_probe,
                    view_idx as u32,
                    smoothing,
                );
            }
        }

        for (view_idx, view_params) in view_params.iter().enumerate() {
            let mut render_pass = encoder.begin_render_pass(&RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: &self.views_objects[view_idx].render_target
                        [view_params.swapchain_index as usize],
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: LoadOp::Clear(Color::BLACK),
                        store: StoreOp::Store,
                    },
                })],
                ..Default::default()
            });

            let transform = reprojection_transform(view_params);
            let transform_bytes = floats_to_bytes(&transform.to_cols_array());

            render_pass.set_pipeline(&self.pipelines.main);
            render_pass.set_immediates(TRANSFORM_CONST_OFFSET, &transform_bytes);
            render_pass.set_immediates(VIEW_INDEX_CONST_OFFSET, &(view_idx as u32).to_le_bytes());
            render_pass.set_bind_group(0, &self.views_objects[view_idx].bind_group, &[]);
            render_pass.set_bind_group(1, depth_bind_group, &[]);
            set_passthrough_immediates(&mut render_pass, passthrough);
            render_pass.draw(0..4, 0..1);
        }

        self.context.queue.submit(iter::once(encoder.finish()));
    }
}

fn create_light_probe(
    device: &Device,
    pipelines: &StreamPipelines,
    stream_view: &TextureView,
    sampler: &Sampler,
    foveation_buffer: &Buffer,
) -> LightProbe {
    let local_view = create_light_probe_texture(device, LIGHT_PROBE_SIZE)
        .create_view(&TextureViewDescriptor::default());
    let global_view =
        create_light_probe_texture(device, 1).create_view(&TextureViewDescriptor::default());

    let local_bind_group = device.create_bind_group(&BindGroupDescriptor {
        label: Some("light probe local"),
        layout: &pipelines.probe_local_layout,
        entries: &[
            BindGroupEntry {
                binding: 0,
                resource: BindingResource::TextureView(stream_view),
            },
            BindGroupEntry {
                binding: 1,
                resource: BindingResource::Sampler(sampler),
            },
            BindGroupEntry {
                binding: 2,
                resource: foveation_buffer.as_entire_binding(),
            },
        ],
    });

    let global_bind_group = device.create_bind_group(&BindGroupDescriptor {
        label: Some("light probe global"),
        layout: &pipelines.probe_global_layout,
        entries: &[BindGroupEntry {
            binding: 0,
            resource: BindingResource::TextureView(&local_view),
        }],
    });

    LightProbe {
        local_view,
        global_view,
        local_bind_group,
        global_bind_group,
    }
}

fn create_view_bind_group(
    device: &Device,
    pipelines: &StreamPipelines,
    stream_view: &TextureView,
    sampler: &Sampler,
    foveation_buffer: &Buffer,
    light_probe: &LightProbe,
) -> BindGroup {
    device.create_bind_group(&BindGroupDescriptor {
        label: Some("stream view"),
        layout: &pipelines.view_layout,
        entries: &[
            BindGroupEntry {
                binding: 0,
                resource: BindingResource::TextureView(stream_view),
            },
            BindGroupEntry {
                binding: 1,
                resource: BindingResource::Sampler(sampler),
            },
            BindGroupEntry {
                binding: 2,
                resource: foveation_buffer.as_entire_binding(),
            },
            BindGroupEntry {
                binding: 3,
                resource: BindingResource::TextureView(&light_probe.local_view),
            },
            BindGroupEntry {
                binding: 4,
                resource: BindingResource::TextureView(&light_probe.global_view),
            },
        ],
    })
}

fn encode_light_probe_update(
    encoder: &mut wgpu::CommandEncoder,
    pipelines: &StreamPipelines,
    light_probe: &LightProbe,
    view_idx: u32,
    smoothing: f32,
) {
    {
        let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
            label: Some("light probe local"),
            color_attachments: &[Some(RenderPassColorAttachment {
                view: &light_probe.local_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: LoadOp::Load,
                    store: StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        let k = f64::from(smoothing);
        pass.set_pipeline(&pipelines.probe_local);
        pass.set_bind_group(0, &light_probe.local_bind_group, &[]);
        pass.set_immediates(0, &view_idx.to_le_bytes());
        pass.set_blend_constant(Color {
            r: k,
            g: k,
            b: k,
            a: k,
        });
        pass.draw(0..3, 0..1);
    }

    let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
        label: Some("light probe global"),
        color_attachments: &[Some(RenderPassColorAttachment {
            view: &light_probe.global_view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: LoadOp::Clear(Color::BLACK),
                store: StoreOp::Store,
            },
        })],
        ..Default::default()
    });
    pass.set_pipeline(&pipelines.probe_global);
    pass.set_bind_group(0, &light_probe.global_bind_group, &[]);
    pass.draw(0..3, 0..1);
}

fn reprojection_transform(view_params: &StreamViewParams) -> Mat4 {
    let input_fov = view_params.input_view_params.fov;

    let tanl = f32::tan(input_fov.left);
    let tanr = f32::tan(input_fov.right);
    let tanu = f32::tan(input_fov.up);
    let tand = f32::tan(input_fov.down);

    let width = tanr - tanl;
    let height = tanu - tand;
    let quad_depth = 1000.0;

    let output_mat4 = Mat4::from_translation(view_params.output_view_params.pose.position)
        * Mat4::from_quat(view_params.output_view_params.pose.orientation);
    let input_mat4 = Mat4::from_translation(view_params.input_view_params.pose.position)
        * Mat4::from_quat(view_params.input_view_params.pose.orientation);

    // The image is at z = -1.0, so we use tangents for the size
    let model_mat = Mat4::from_translation(Vec3 {
        x: 0.0,
        y: 0.0,
        z: -quad_depth * 0.5,
    }) * Mat4::from_scale(Vec3::new(quad_depth, quad_depth, quad_depth * 0.5))
        * Mat4::from_translation(Vec3::new(width / 2.0 + tanl, height / 2.0 + tand, -1.0))
        * Mat4::from_scale(Vec3::new(width, height, 1.));
    let view_mat = output_mat4.inverse() * input_mat4;
    let proj_mat = super::projection_from_fov(view_params.output_view_params.fov);

    proj_mat * view_mat * model_mat
}

/// Whether the projection layer must be submitted with premultiplied alpha for this mode
pub fn passthrough_requires_premultiplied_alpha(mode: &PassthroughMode) -> bool {
    matches!(
        mode,
        PassthroughMode::Blend {
            premultiplied_alpha: true,
            ..
        } | PassthroughMode::RgbChromaKey(_)
            | PassthroughMode::HsvChromaKey(_)
            | PassthroughMode::Depth(_)
    )
}

/// Returns the depth passthrough configuration if that mode is active
pub fn depth_passthrough_config(mode: Option<&PassthroughMode>) -> Option<&DepthPassthroughConfig> {
    match mode {
        Some(PassthroughMode::Depth(config)) => Some(config),
        _ => None,
    }
}

fn set_passthrough_immediates(render_pass: &mut RenderPass, config: Option<&PassthroughMode>) {
    const DEG_TO_NORM: f32 = 1. / 360.;

    fn set_u32(render_pass: &mut RenderPass, offset: u32, value: u32) {
        render_pass.set_immediates(offset, &value.to_le_bytes());
    }

    fn set_float(render_pass: &mut RenderPass, offset: u32, value: f32) {
        render_pass.set_immediates(offset, &value.to_le_bytes());
    }

    fn set_vec4(render_pass: &mut RenderPass, offset: u32, value: Vec4) {
        render_pass.set_immediates(offset, &value.x.to_le_bytes());
        render_pass.set_immediates(offset + FLOAT_SIZE, &value.y.to_le_bytes());
        render_pass.set_immediates(offset + 2 * FLOAT_SIZE, &value.z.to_le_bytes());
        render_pass.set_immediates(offset + 3 * FLOAT_SIZE, &value.w.to_le_bytes());
    }

    match config {
        None => {
            set_u32(render_pass, PASSTHROUGH_MODE_OFFSET, 0);
            set_float(render_pass, ALPHA_CONST_OFFSET, 1.);
        }
        Some(PassthroughMode::Blend { threshold, .. }) => {
            set_u32(render_pass, PASSTHROUGH_MODE_OFFSET, 0);
            set_float(render_pass, ALPHA_CONST_OFFSET, 1. - threshold);
        }
        Some(PassthroughMode::Depth(_)) => {
            set_u32(render_pass, PASSTHROUGH_MODE_OFFSET, PASSTHROUGH_MODE_DEPTH);
            set_float(render_pass, ALPHA_CONST_OFFSET, 1.);
        }
        Some(PassthroughMode::RgbChromaKey(config)) => {
            set_u32(render_pass, PASSTHROUGH_MODE_OFFSET, 1);

            let norm = |v| v as f32 / 255.;

            let red = norm(config.red);
            let green = norm(config.green);
            let blue = norm(config.blue);

            let thresh = norm(config.distance_threshold);

            let up_feather = 1. + config.feathering;
            let down_feather = 1. - config.feathering;

            let range_vec =
                thresh * Vec4::new(-up_feather, -down_feather, down_feather, up_feather);

            set_vec4(render_pass, CK_CHANNEL0_CONST_OFFSET, red + range_vec);
            set_vec4(render_pass, CK_CHANNEL1_CONST_OFFSET, green + range_vec);
            set_vec4(render_pass, CK_CHANNEL2_CONST_OFFSET, blue + range_vec);
        }
        Some(PassthroughMode::HsvChromaKey(config)) => {
            set_u32(render_pass, PASSTHROUGH_MODE_OFFSET, 2);

            set_vec4(
                render_pass,
                CK_CHANNEL0_CONST_OFFSET,
                Vec4::new(
                    config.hue_start_max_deg,
                    config.hue_start_min_deg,
                    config.hue_end_min_deg,
                    config.hue_end_max_deg,
                ) * DEG_TO_NORM,
            );

            set_vec4(
                render_pass,
                CK_CHANNEL1_CONST_OFFSET,
                Vec4::new(
                    config.saturation_start_max,
                    config.saturation_start_min,
                    config.saturation_end_min,
                    config.saturation_end_max,
                ),
            );

            set_vec4(
                render_pass,
                CK_CHANNEL2_CONST_OFFSET,
                Vec4::new(
                    config.value_start_max,
                    config.value_start_min,
                    config.value_end_min,
                    config.value_end_max,
                ),
            );
        }
    }
}

fn foveated_encoding_data(
    config: &AlvrFoveatedEncodingParams,
    center_shifts: [Vec2; 2],
) -> [Vec4; FOVEATION_UNIFORM_VEC4_COUNT] {
    let center_size_aligned = Vec2::from_array(config.center_size);
    let edge_ratio = Vec2::from_array(config.edge_ratio);
    let view_ratio_aligned = Vec2::from_array(config.view_ratio);

    let c0 = (1. - center_size_aligned) * 0.5;
    let c2 = (edge_ratio - 1.) * center_size_aligned + 1.;

    let mut uniforms = [Vec4::ZERO; FOVEATION_UNIFORM_VEC4_COUNT];
    let [common_uniform, eye_uniforms @ ..] = &mut uniforms;
    *common_uniform = Vec4::new(
        view_ratio_aligned.x,
        view_ratio_aligned.y,
        edge_ratio.x,
        edge_ratio.y,
    );

    for (uniforms, center_shift) in eye_uniforms
        .chunks_exact_mut(FOVEATION_EYE_UNIFORM_VEC4_COUNT)
        .zip(center_shifts)
    {
        let c1 = (edge_ratio - 1.) * c0 * (center_shift + 1.) / edge_ratio;
        let lo_bound = c0 * (center_shift + 1.);
        let hi_bound = c0 * (center_shift - 1.) + 1.;
        let lo_bound_c = c0 * (center_shift + 1.) / c2;
        let hi_bound_c = c0 * (center_shift - 1.) / c2 + 1.;

        let a_left = c2 * (1. - edge_ratio) / (edge_ratio * lo_bound_c);
        let b_left = (c1 + c2 * lo_bound_c) / lo_bound_c;

        let a_right = c2 * (edge_ratio - 1.) / (edge_ratio * (1. - hi_bound_c));
        let b_right = (c2 - edge_ratio * c1 - 2. * edge_ratio * c2
            + c2 * edge_ratio * (1. - hi_bound_c)
            + edge_ratio)
            / (edge_ratio * (1. - hi_bound_c));
        let c_right = (c2 * edge_ratio - c2) * (c1 - hi_bound_c + c2 * hi_bound_c)
            / (edge_ratio * (1. - hi_bound_c) * (1. - hi_bound_c));

        let [center, bounds, left, right, right_curve] = uniforms else {
            unreachable!("foveation eye uniform chunk must contain five vectors");
        };
        *center = Vec4::new(c1.x, c1.y, c2.x, c2.y);
        *bounds = Vec4::new(lo_bound.x, lo_bound.y, hi_bound.x, hi_bound.y);
        *left = Vec4::new(a_left.x, a_left.y, b_left.x, b_left.y);
        *right = Vec4::new(a_right.x, a_right.y, b_right.x, b_right.y);
        *right_curve = Vec4::new(c_right.x, c_right.y, 0.0, 0.0);
    }

    uniforms
}

fn foveation_uniform_bytes(
    uniforms: &[Vec4; FOVEATION_UNIFORM_VEC4_COUNT],
) -> [u8; FOVEATION_UNIFORM_SIZE] {
    let mut bytes = [0; FOVEATION_UNIFORM_SIZE];

    for (chunk, value) in bytes
        .chunks_exact_mut(mem::size_of::<f32>())
        .zip(uniforms.iter().flat_map(|value| value.to_array()))
    {
        chunk.copy_from_slice(&value.to_ne_bytes());
    }

    bytes
}

pub fn compute_target_view_resolution(
    resolution: UVec2,
    upscaling: &Option<UpscalingConfig>,
) -> UVec2 {
    let mut target_resolution = resolution.as_vec2();
    if let Some(upscaling) = upscaling {
        target_resolution *= upscaling.upscale_factor;
    }
    target_resolution.as_uvec2()
}
