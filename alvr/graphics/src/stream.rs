use super::{GraphicsContext, MAX_IMMEDIATE_SIZE, staging::StagingRenderer};
use crate::{
    depth_burst::BurstReadbacks,
    depth_dump::{self, DEPTH_DUMP_TIMEOUT, DepthDumpFiles, DepthDumpRequest, DumpReadback},
    depth_mask::{self, DEPTH_UNIFORMS_SIZE, DepthRenderInput},
    key_dump::{array, object, string},
    overlay::{self, OVERLAY_UNIFORMS_SIZE, OverlayEye, OverlaySegment, SEGMENT_FLOAT_COUNT},
    problem_view::{self, ProblemViewData, ProblemViewGpu},
    room_memory::MemoryStatus,
    room_memory_gpu::{MemoryPasses, MemoryPipelines, RoomMemory},
};
use alvr_common::{
    AlvrFoveatedEncodingParams, ViewParams,
    glam::{Mat4, UVec2, Vec2, Vec3, Vec4},
};
use alvr_session::{DepthPassthroughConfig, PassthroughMode, UpscalingConfig};
use std::{
    borrow::Cow,
    cell::{Cell, OnceCell, RefCell},
    ffi::c_void,
    iter, mem,
    rc::Rc,
    time::{Duration, Instant},
};
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
const VIEW_IDX_IMMEDIATE_SIZE: u32 = U32_SIZE;
// overlay.wgsl: view_proj, viewport, pixels_per_radian, first
const OVERLAY_VIEWPORT_OFFSET: u32 = TRANSFORM_SIZE;
const OVERLAY_PIXELS_PER_RADIAN_OFFSET: u32 = OVERLAY_VIEWPORT_OFFSET + 2 * FLOAT_SIZE;
const OVERLAY_FIRST_OFFSET: u32 = OVERLAY_PIXELS_PER_RADIAN_OFFSET + FLOAT_SIZE;
const OVERLAY_IMMEDIATE_SIZE: u32 = OVERLAY_FIRST_OFFSET + U32_SIZE;
const FOVEATION_EYE_UNIFORM_VEC4_COUNT: usize = 5;
const FOVEATION_UNIFORM_VEC4_COUNT: usize = 1 + FOVEATION_EYE_UNIFORM_VEC4_COUNT * 2;
pub(crate) const FOVEATION_UNIFORM_SIZE: usize =
    FOVEATION_UNIFORM_VEC4_COUNT * mem::size_of::<Vec4>();
const _: () = assert!(
    IMMEDIATE_SIZE <= MAX_IMMEDIATE_SIZE,
    "Immediate size exceeds the maximum size"
);
const _: () = assert!(
    OVERLAY_IMMEDIATE_SIZE <= MAX_IMMEDIATE_SIZE,
    "Immediate size exceeds the maximum size"
);

const PASSTHROUGH_MODE_DEPTH: u32 = 3;

pub const LIGHT_PROBE_SIZE: u32 = 16;
pub const LIGHT_PROBE_FORMAT: TextureFormat = TextureFormat::Rgba16Float;

/// The depth passthrough mask is computed at 1 / DEPTH_MASK_DOWNSCALE of the view resolution. This
/// still has more texels than the environment depth map (320x320 on Quest 3).
const DEPTH_MASK_DOWNSCALE: u32 = 5;
/// The outline of the hands is visible, keep it sharp
const FOCUS_AREA_DOWNSCALE: u32 = 5;
// Distances in meters
const PLAYZONE_FORMAT: TextureFormat = LIGHT_PROBE_FORMAT;
const DEPTH_MASK_FORMAT: TextureFormat = TextureFormat::Rgba8Unorm;

const FOVEATION_COMMON_WGSL: &str = include_str!("../resources/foveation_common.wgsl");
const DEPTH_COMMON_WGSL: &str = include_str!("../resources/depth_common.wgsl");
const DEPTH_MASK_WGSL: &str = include_str!("../resources/depth_mask.wgsl");
const FOCUS_AREA_WGSL: &str = include_str!("../resources/focus_area.wgsl");
const PLAYZONE_COPY_WGSL: &str = include_str!("../resources/playzone_copy.wgsl");
const ROOM_MEMORY_WGSL: &str = include_str!("../resources/room_memory.wgsl");
const DEPTH_DUMP_WGSL: &str = include_str!("../resources/depth_dump.wgsl");
const STREAM_WGSL: &str = include_str!("../resources/stream.wgsl");
const LIGHT_PROBE_LOCAL_WGSL: &str = include_str!("../resources/light_probe_local.wgsl");
const LIGHT_PROBE_GLOBAL_WGSL: &str = include_str!("../resources/light_probe_global.wgsl");
pub(crate) const OVERLAY_WGSL: &str = include_str!("../resources/overlay.wgsl");

// The common code must come last: wgpu-hal (GLES) resolves the types of the immediates using indices
// that are only correct if those types are the first ones in the shader.
pub(crate) fn stream_shader_source() -> String {
    format!("{STREAM_WGSL}\n{FOVEATION_COMMON_WGSL}\n{DEPTH_COMMON_WGSL}")
}

pub(crate) fn depth_mask_shader_source() -> String {
    format!("{DEPTH_MASK_WGSL}\n{DEPTH_COMMON_WGSL}")
}

pub(crate) fn playzone_copy_shader_source() -> String {
    format!("{PLAYZONE_COPY_WGSL}\n{DEPTH_COMMON_WGSL}")
}

pub(crate) fn room_memory_shader_source() -> String {
    format!("{ROOM_MEMORY_WGSL}\n{DEPTH_COMMON_WGSL}")
}

pub(crate) fn depth_dump_shader_source() -> String {
    format!("{DEPTH_DUMP_WGSL}\n{DEPTH_COMMON_WGSL}")
}

pub(crate) fn focus_area_shader_source() -> String {
    format!("{FOCUS_AREA_WGSL}\n{DEPTH_COMMON_WGSL}")
}

pub(crate) fn light_probe_local_shader_source() -> String {
    format!("{LIGHT_PROBE_LOCAL_WGSL}\n{FOVEATION_COMMON_WGSL}")
}

pub(crate) fn light_probe_global_shader_source() -> String {
    LIGHT_PROBE_GLOBAL_WGSL.to_owned()
}

pub(crate) fn overlay_shader_source() -> String {
    OVERLAY_WGSL.to_owned()
}

pub(crate) fn shader_module(device: &Device, label: &str, source: String) -> ShaderModule {
    device.create_shader_module(ShaderModuleDescriptor {
        label: Some(label),
        source: ShaderSource::Wgsl(Cow::Owned(source)),
    })
}

pub(crate) fn texture_entry(
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

pub(crate) fn uniform_entry(binding: u32) -> BindGroupLayoutEntry {
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
    /// stream texture, sampler, foveation uniforms, local light probe, global light probe, depth mask,
    /// focus area
    pub view_layout: BindGroupLayout,
    /// depth uniforms, depth texture array
    pub depth_layout: BindGroupLayout,
    /// Group 2 of `main`: `debug.alvr.problem_view`, see problem_view.rs
    pub problem_layout: BindGroupLayout,
    pub main: RenderPipeline,
    /// Uses `depth_layout` and `playzone_layout`
    pub depth_mask: RenderPipeline,
    /// playzone depth, focus area, sampler
    pub playzone_layout: BindGroupLayout,
    /// Uses `depth_layout`
    pub playzone_copy: RenderPipeline,
    /// depth uniforms
    pub focus_area_layout: BindGroupLayout,
    pub focus_area: RenderPipeline,
    /// stream texture, sampler, foveation uniforms
    pub probe_local_layout: BindGroupLayout,
    pub probe_local: RenderPipeline,
    /// local light probe
    pub probe_global_layout: BindGroupLayout,
    pub probe_global: RenderPipeline,
    /// overlay segments
    pub overlay_layout: BindGroupLayout,
    /// Drawn in the main pass, after the stream
    pub overlay: RenderPipeline,
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
                texture_entry(5, true, TextureViewDimension::D2),
                texture_entry(6, true, TextureViewDimension::D2),
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

        let problem_layout = problem_view::problem_view_layout(device);

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
                bind_group_layouts: &[
                    Some(&view_layout),
                    Some(&depth_layout),
                    Some(&problem_layout),
                ],
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

        let playzone_layout = device.create_bind_group_layout(&BindGroupLayoutDescriptor {
            label: Some("playzone"),
            entries: &[
                texture_entry(0, false, TextureViewDimension::D2),
                texture_entry(1, true, TextureViewDimension::D2),
                sampler_entry(2),
            ],
        });
        let playzone_copy_module =
            shader_module(device, "playzone copy", playzone_copy_shader_source());
        let playzone_copy = device.create_render_pipeline(&RenderPipelineDescriptor {
            label: Some("playzone copy"),
            layout: Some(&device.create_pipeline_layout(&PipelineLayoutDescriptor {
                label: None,
                bind_group_layouts: &[Some(&depth_layout)],
                immediate_size: VIEW_IDX_IMMEDIATE_SIZE,
            })),
            vertex: VertexState {
                module: &playzone_copy_module,
                entry_point: None,
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(FragmentState {
                module: &playzone_copy_module,
                entry_point: None,
                compilation_options: Default::default(),
                targets: &[Some(ColorTargetState {
                    format: PLAYZONE_FORMAT,
                    blend: None,
                    write_mask: ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });

        let depth_mask_module = shader_module(device, "depth mask", depth_mask_shader_source());
        let depth_mask = device.create_render_pipeline(&RenderPipelineDescriptor {
            label: Some("depth mask"),
            layout: Some(&device.create_pipeline_layout(&PipelineLayoutDescriptor {
                label: None,
                bind_group_layouts: &[Some(&depth_layout), Some(&playzone_layout)],
                immediate_size: VIEW_IDX_IMMEDIATE_SIZE,
            })),
            vertex: VertexState {
                module: &depth_mask_module,
                entry_point: None,
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(FragmentState {
                module: &depth_mask_module,
                entry_point: None,
                compilation_options: Default::default(),
                targets: &[Some(ColorTargetState {
                    format: DEPTH_MASK_FORMAT,
                    blend: None,
                    write_mask: ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });

        let focus_area_layout = device.create_bind_group_layout(&BindGroupLayoutDescriptor {
            label: Some("focus area"),
            entries: &[uniform_entry(0)],
        });
        let focus_area_module = shader_module(device, "focus area", focus_area_shader_source());
        let focus_area = device.create_render_pipeline(&RenderPipelineDescriptor {
            label: Some("focus area"),
            layout: Some(&device.create_pipeline_layout(&PipelineLayoutDescriptor {
                label: None,
                bind_group_layouts: &[Some(&focus_area_layout)],
                immediate_size: VIEW_IDX_IMMEDIATE_SIZE,
            })),
            vertex: VertexState {
                module: &focus_area_module,
                entry_point: None,
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(FragmentState {
                module: &focus_area_module,
                entry_point: None,
                compilation_options: Default::default(),
                targets: &[Some(ColorTargetState {
                    format: DEPTH_MASK_FORMAT,
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
                immediate_size: VIEW_IDX_IMMEDIATE_SIZE,
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

        let overlay_layout = device.create_bind_group_layout(&BindGroupLayoutDescriptor {
            label: Some("overlay"),
            entries: &[BindGroupLayoutEntry {
                binding: 0,
                visibility: ShaderStages::VERTEX,
                ty: BindingType::Buffer {
                    ty: BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let overlay_module = shader_module(device, "overlay", overlay_shader_source());
        // The colors are corrected like the stream
        let overlay_constants = [(
            "ENABLE_SRGB_CORRECTION",
            constants
                .iter()
                .find(|(name, _)| *name == "ENABLE_SRGB_CORRECTION")
                .map_or(0.0, |(_, value)| *value),
        )];
        let overlay_compilation_options = PipelineCompilationOptions {
            constants: &overlay_constants,
            zero_initialize_workgroup_memory: false,
        };
        let overlay = device.create_render_pipeline(&RenderPipelineDescriptor {
            label: Some("overlay"),
            layout: Some(&device.create_pipeline_layout(&PipelineLayoutDescriptor {
                label: None,
                bind_group_layouts: &[Some(&overlay_layout)],
                immediate_size: OVERLAY_IMMEDIATE_SIZE,
            })),
            vertex: VertexState {
                module: &overlay_module,
                entry_point: None,
                compilation_options: overlay_compilation_options.clone(),
                buffers: &[],
            },
            primitive: PrimitiveState {
                topology: PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(FragmentState {
                module: &overlay_module,
                entry_point: None,
                compilation_options: overlay_compilation_options,
                // Opaque, see overlay.wgsl
                targets: &[Some(ColorTargetState {
                    format: target_format,
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
            problem_layout,
            main,
            depth_mask,
            playzone_layout,
            playzone_copy,
            focus_area_layout,
            focus_area,
            probe_local_layout,
            probe_local,
            probe_global_layout,
            probe_global,
            overlay_layout,
            overlay,
        }
    }
}

/// For `debug.alvr.key_dump`, created at the first dump. Uses `depth_layout` and `playzone_layout`
pub(crate) fn create_depth_dump_pipeline(
    device: &Device,
    pipelines: &StreamPipelines,
) -> RenderPipeline {
    let module = shader_module(device, "depth dump", depth_dump_shader_source());
    device.create_render_pipeline(&RenderPipelineDescriptor {
        label: Some("depth dump"),
        layout: Some(&device.create_pipeline_layout(&PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[
                Some(&pipelines.depth_layout),
                Some(&pipelines.playzone_layout),
            ],
            immediate_size: VIEW_IDX_IMMEDIATE_SIZE,
        })),
        vertex: VertexState {
            module: &module,
            entry_point: None,
            compilation_options: Default::default(),
            buffers: &[],
        },
        primitive: Default::default(),
        depth_stencil: None,
        multisample: Default::default(),
        fragment: Some(FragmentState {
            module: &module,
            entry_point: None,
            compilation_options: Default::default(),
            targets: &[Some(ColorTargetState {
                format: TextureFormat::Rgba8Unorm,
                blend: None,
                write_mask: ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    })
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

pub(crate) fn create_depth_mask_texture(device: &Device, view_resolution: UVec2) -> Texture {
    create_mask_texture(device, "depth mask", view_resolution / DEPTH_MASK_DOWNSCALE)
}

pub(crate) fn create_focus_area_texture(device: &Device, view_resolution: UVec2) -> Texture {
    create_mask_texture(device, "focus area", view_resolution / FOCUS_AREA_DOWNSCALE)
}

pub(crate) fn focus_area_resolution(view_resolution: UVec2) -> UVec2 {
    (view_resolution / FOCUS_AREA_DOWNSCALE).max(UVec2::ONE)
}

fn create_mask_texture(device: &Device, label: &str, size: UVec2) -> Texture {
    let size = size.max(UVec2::ONE);

    device.create_texture(&TextureDescriptor {
        label: Some(label),
        size: Extent3d {
            width: size.x,
            height: size.y,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: TextureDimension::D2,
        format: DEPTH_MASK_FORMAT,
        // Copied by `debug.alvr.key_dump`
        usage: TextureUsages::RENDER_ATTACHMENT
            | TextureUsages::TEXTURE_BINDING
            | TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}

/// Depth map without hands, for one eye
pub(crate) fn create_playzone_texture(device: &Device, depth_resolution: UVec2) -> Texture {
    device.create_texture(&TextureDescriptor {
        label: Some("playzone"),
        size: Extent3d {
            width: depth_resolution.x,
            height: depth_resolution.y,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: TextureDimension::D2,
        format: PLAYZONE_FORMAT,
        usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    })
}

pub(crate) fn create_playzone_bind_group(
    device: &Device,
    pipelines: &StreamPipelines,
    playzone: &TextureView,
    focus_area: &TextureView,
    sampler: &Sampler,
) -> BindGroup {
    device.create_bind_group(&BindGroupDescriptor {
        label: Some("playzone"),
        layout: &pipelines.playzone_layout,
        entries: &[
            BindGroupEntry {
                binding: 0,
                resource: BindingResource::TextureView(playzone),
            },
            BindGroupEntry {
                binding: 1,
                resource: BindingResource::TextureView(focus_area),
            },
            BindGroupEntry {
                binding: 2,
                resource: BindingResource::Sampler(sampler),
            },
        ],
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

pub(crate) fn create_focus_area_bind_group(
    device: &Device,
    pipelines: &StreamPipelines,
    uniforms: &Buffer,
) -> BindGroup {
    device.create_bind_group(&BindGroupDescriptor {
        label: Some("focus area"),
        layout: &pipelines.focus_area_layout,
        entries: &[BindGroupEntry {
            binding: 0,
            resource: uniforms.as_entire_binding(),
        }],
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

/// The segments of both eyes, see overlay.wgsl
pub(crate) fn create_overlay_buffer(device: &Device) -> Buffer {
    device.create_buffer(&BufferDescriptor {
        label: Some("overlay segments"),
        size: OVERLAY_UNIFORMS_SIZE as u64,
        usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

pub(crate) fn create_overlay_bind_group(
    device: &Device,
    pipelines: &StreamPipelines,
    buffer: &Buffer,
) -> BindGroup {
    device.create_bind_group(&BindGroupDescriptor {
        label: Some("overlay"),
        layout: &pipelines.overlay_layout,
        entries: &[BindGroupEntry {
            binding: 0,
            resource: buffer.as_entire_binding(),
        }],
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
    depth_mask: TextureView,
    focus_area: TextureView,
    focus_area_bind_group: BindGroup,
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
    focus_area_resolution: UVec2,
    // The focus area of each view is empty, its pass can be skipped
    focus_area_cleared: [Cell<bool>; 2],
    sampler: Sampler,
    playzone_textures: [TextureView; 2],
    playzone_bind_groups: [BindGroup; 2],
    // Depth cameras of the remembered playzone, and when it was remembered
    playzone: Cell<Option<([ViewParams; 2], Instant)>>,
    depth_resolution: UVec2,
    depth_mask_resolution: UVec2,
    // `debug.alvr.key_dump`: waiting for a depth frame, then for the GPU
    dump_request: Cell<Option<(DepthDumpRequest, Instant)>>,
    dump_pipeline: OnceCell<RenderPipeline>,
    dump_readback: RefCell<Option<DumpReadback>>,
    // `debug.alvr.burst`: the depth of the next frame to read back (its token), and the readbacks
    burst_request: Cell<Option<u32>>,
    burst_readbacks: RefCell<BurstReadbacks>,
    // Outline overlay: segments of both eyes
    overlay_buffer: Buffer,
    overlay_bind_group: BindGroup,
    target_view_resolution: UVec2,
    // `debug.alvr.mem_roll` (room_memory.rs): the pipelines from its first use, the textures while it is
    // on (dropped when it is off)
    memory_pipelines: OnceCell<MemoryPipelines>,
    memory: RefCell<Option<RoomMemory>>,
    problem_view: ProblemViewGpu,
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

        let depth_uniform_buffer = create_depth_uniform_buffer(device);

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

            let depth_mask = create_depth_mask_texture(device, target_view_resolution)
                .create_view(&TextureViewDescriptor::default());
            let focus_area = create_focus_area_texture(device, target_view_resolution)
                .create_view(&TextureViewDescriptor::default());
            let focus_area_bind_group =
                create_focus_area_bind_group(device, &pipelines, &depth_uniform_buffer);

            let bind_group = create_view_bind_group(
                device,
                &pipelines,
                &staging_view,
                &sampler,
                &foveation_buffer,
                &light_probe,
                &depth_mask,
                &focus_area,
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
                depth_mask,
                focus_area,
                focus_area_bind_group,
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

        let dummy_depth_texture = create_dummy_depth_texture(device);
        let dummy_depth_bind_group = create_depth_bind_group(
            device,
            &pipelines,
            &depth_uniform_buffer,
            &depth_array_view(&dummy_depth_texture),
        );

        let views_objects: [ViewObjects; 2] =
            view_objects.try_into().unwrap_or_else(|_| unreachable!());
        let (playzone_textures, playzone_bind_groups) =
            create_playzone(device, &pipelines, &views_objects, &sampler, UVec2::ONE);
        let overlay_buffer = create_overlay_buffer(device);
        let overlay_bind_group = create_overlay_bind_group(device, &pipelines, &overlay_buffer);
        let problem_view = ProblemViewGpu::new(device, &pipelines.problem_layout);

        Self {
            context,
            staging_renderer,
            pipelines,
            views_objects,
            foveated_encoding,
            foveation_buffer,
            last_foveation_center_shifts: Cell::new(None),
            depth_uniform_buffer,
            dummy_depth_bind_group,
            depth_bind_groups: vec![],
            light_probe_initialized: Cell::new(false),
            focus_area_resolution: focus_area_resolution(target_view_resolution),
            focus_area_cleared: [const { Cell::new(false) }; 2],
            sampler,
            playzone_textures,
            playzone_bind_groups,
            playzone: Cell::new(None),
            depth_resolution: UVec2::ONE,
            depth_mask_resolution: (target_view_resolution / DEPTH_MASK_DOWNSCALE).max(UVec2::ONE),
            dump_request: Cell::new(None),
            dump_pipeline: OnceCell::new(),
            dump_readback: RefCell::new(None),
            burst_request: Cell::new(None),
            burst_readbacks: RefCell::new(BurstReadbacks::default()),
            overlay_buffer,
            overlay_bind_group,
            target_view_resolution,
            memory_pipelines: OnceCell::new(),
            memory: RefCell::new(None),
            problem_view,
        }
    }

    /// `debug.alvr.problem_view`: the leak map as a tint over the room (depth passthrough only), or off
    /// (None, the default). Writes nothing while it stays off.
    pub fn set_problem_view(&self, data: Option<ProblemViewData>) {
        self.problem_view.write(&self.context.queue, data);
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

        (self.playzone_textures, self.playzone_bind_groups) = create_playzone(
            &self.context.device,
            &self.pipelines,
            &self.views_objects,
            &self.sampler,
            resolution,
        );
        self.depth_resolution = resolution;
        self.clear_playzone();
    }

    pub fn clear_depth_swapchain(&mut self) {
        self.depth_bind_groups.clear();
        self.clear_playzone();
    }

    /// Forget the playzone, e.g. if the reference space changed. The room memory too: the next depth
    /// image anchors it cleanly
    pub fn clear_playzone(&self) {
        self.playzone.set(None);
        if let Some(memory) = self.memory.borrow_mut().as_mut() {
            memory.forget();
        }
    }

    /// The room memory (`debug.alvr.mem_roll`) as the last frame left it. None: off
    pub fn memory_status(&self) -> Option<MemoryStatus> {
        self.memory.borrow().as_ref().map(RoomMemory::status)
    }

    // The room memory of this frame: created (or created again for another `mem_size`) while it is on,
    // dropped when it is off. Its passes, None: off
    fn plan_memory(&self, input: &DepthRenderInput) -> Option<MemoryPasses> {
        let mut memory = self.memory.borrow_mut();
        let Some(memory_input) = &input.memory else {
            *memory = None;
            return None;
        };
        let size = memory_input.params.size;
        if memory.as_ref().is_none_or(|memory| memory.size != size) {
            let device = &self.context.device;
            let memory_pipelines = self
                .memory_pipelines
                .get_or_init(|| MemoryPipelines::new(device, &self.pipelines));
            *memory = Some(RoomMemory::new(
                device,
                &self.pipelines,
                memory_pipelines,
                [
                    &self.views_objects[0].focus_area,
                    &self.views_objects[1].focus_area,
                ],
                &self.sampler,
                size,
            ));
        }

        memory
            .as_mut()
            .map(|memory| memory.plan(&self.context.queue, memory_input, input.frame.as_ref()))
    }

    /// How long ago the playzone was remembered. None: there is none
    pub fn playzone_age(&self) -> Option<Duration> {
        self.playzone.get().map(|(_, time)| time.elapsed())
    }

    /// Read back the depth, the playzone, the depth mask and the focus area of the next frame with a
    /// depth image, see `poll_depth_dump`. Returns the n of a request that was still waiting and is
    /// replaced
    pub fn request_depth_dump(&self, request: DepthDumpRequest) -> Option<i64> {
        self.dump_request
            .replace(Some((request, Instant::now())))
            .map(|(request, _)| request.n)
    }

    /// Call every frame. Some when a dump is finished or failed, with its n
    pub fn poll_depth_dump(&self) -> Option<(i64, Result<DepthDumpFiles, String>)> {
        if let Some((request, time)) = self.dump_request.get()
            && time.elapsed() > DEPTH_DUMP_TIMEOUT
        {
            self.dump_request.set(None);
            return Some((request.n, Err("no depth frame".into())));
        }

        let result = {
            let readback = self.dump_readback.borrow();
            let readback = readback.as_ref()?;
            let result = match self.context.device.poll(wgpu::PollType::Poll) {
                Ok(_) => readback.poll()?,
                Err(e) => Err(format!("poll: {e}")),
            };
            (readback.n, result)
        };
        self.dump_readback.replace(None);

        Some(result)
    }

    /// `debug.alvr.burst`: read back both eyes of the depth image of the next `render` (16 bit values,
    /// through the pass of the depth dump), answered by `poll_burst_depth` with `token`. A request that
    /// is still waiting is answered with an error
    pub fn request_burst_depth(&self, token: u32) {
        if let Some(replaced) = self.burst_request.replace(Some(token)) {
            self.burst_readbacks
                .borrow_mut()
                .fail(replaced, "replaced by the next request");
        }
    }

    /// Call every frame: the burst readbacks that are done or failed, with their token. Values of the
    /// left eye then of the right one, rows in the order of the depth texture
    pub fn poll_burst_depth(&self) -> Vec<(u32, Result<Vec<u16>, String>)> {
        let mut readbacks = self.burst_readbacks.borrow_mut();
        if readbacks.is_waiting() {
            // A failed poll leaves them waiting until they time out
            self.context.device.poll(wgpu::PollType::Poll).ok();
        }

        readbacks.poll()
    }

    /// Runtime foveation centers must already be encoder-aligned. `None` uses the negotiated centers.
    /// `overlay`: lines in stage space drawn over the stream, only with Passthrough = Depth (empty:
    /// nothing is uploaded or drawn). Returns the part of each view with the hands, if the color key is
    /// used (see `hands_view_rect`).
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
        overlay: &[OverlaySegment],
    ) -> Option<[Option<(Vec2, Vec2)>; 2]> {
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
        let mut capture_playzone = false;
        // Only the part of the view with the hands is rendered
        let reach = depth
            .and_then(|d| d.memory)
            .map_or([0.0; 2], |memory| memory.reach);
        let hand_rects = depth.and_then(|d| d.color_key).map(|key| {
            [0, 1].map(|i| {
                depth_mask::hands_view_rect_reach(
                    &view_params[i].input_view_params,
                    depth.map_or(Vec3::ZERO, |d| d.eye_positions[i]),
                    &key,
                    reach,
                )
            })
        });
        let mut lighting_response = None;
        // `debug.alvr.key_dump`: the request, the depth frame and the JSON
        let mut dump = None;
        // `debug.alvr.burst`: the depth of this frame is read back if there is one
        let burst_token = self.burst_request.take();
        let mut has_depth_frame = false;
        // `debug.alvr.mem_roll`: the passes of the room memory, None: off
        let mut memory_passes = None;
        if let Some(config) = depth_config {
            let input = depth.copied().unwrap_or(DepthRenderInput {
                frame: None,
                eye_positions: [Vec3::ZERO; 2],
                color_key: None,
                arms: [None; 2],
                arm_gaps: [None; 2],
                edge_cut: 0.0,
                lever_windows: [None; depth_mask::MAX_LEVER_WINDOWS],
                capture_playzone: false,
                playzone_view: false,
                memory: None,
                key_lower_rects: None,
                region_override: None,
                game_dim: 0.0,
                game_see: 0.0,
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
                has_depth_frame = true;
            }

            memory_passes = self.plan_memory(&input);
            // The room memory replaces today's copy while it is on
            if input.capture_playzone
                && input.memory.is_none()
                && let Some(frame) = &input.frame
            {
                self.playzone.set(Some((frame.depth_views, Instant::now())));
                capture_playzone = true;
            }

            lighting_response = config.lighting.as_option().map(|l| l.response_speed);

            let stream_views = [
                view_params[0].input_view_params,
                view_params[1].input_view_params,
            ];
            // With the room memory its camera (both eyes, each with its own texture), none before it is
            // anchored
            let playzone_views = if input.memory.is_some() {
                self.memory
                    .borrow()
                    .as_ref()
                    .and_then(RoomMemory::camera)
                    .map(|camera| [camera.view_params(); 2])
            } else {
                self.playzone.get().map(|(views, _)| views)
            };
            let uniforms = depth_mask::depth_uniforms(
                config,
                &stream_views,
                &input,
                playzone_views.as_ref(),
                lighting_response.is_some(),
            );
            self.context.queue.write_buffer(
                &self.depth_uniform_buffer,
                0,
                &floats_to_bytes(&uniforms),
            );

            if let Some(frame) = &input.frame
                && self.dump_readback.borrow().is_none()
                && let Some((request, _)) = self.dump_request.take()
            {
                let json = depth_dump::DumpFrame {
                    request,
                    config,
                    stream_views: &stream_views,
                    input: &input,
                    frame,
                    playzone: self
                        .playzone
                        .get()
                        .map(|(views, time)| (views, time.elapsed().as_secs_f32())),
                    depth_size: self.depth_resolution,
                    mask_size: self.depth_mask_resolution,
                    focus_size: self.focus_area_resolution,
                    memory: self.memory_dump_json(&input, request.n),
                }
                .to_json();
                dump = Some((request.n, *frame, json));
            }
        }

        let mut encoder = self
            .context
            .device
            .create_command_encoder(&Default::default());

        // Before the mask, which reads the memory
        if let Some(passes) = &memory_passes
            && let Some(memory) = self.memory.borrow_mut().as_mut()
            && let Some(pipelines) = self.memory_pipelines.get()
        {
            memory.encode(&mut encoder, pipelines, depth_bind_group, passes);
        }
        let memory = self.memory.borrow();
        // The memory is read once it has a camera; while it is on today's playzone is not read
        let memory_read = memory
            .as_ref()
            .filter(|memory| memory_passes.is_some() && memory.camera().is_some());
        let stats_encoded = memory.as_ref().is_some_and(RoomMemory::has_encoded_stats);

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

        if depth_config.is_some() {
            for (view_idx, objects) in self.views_objects.iter().enumerate() {
                if capture_playzone {
                    encode_playzone_copy(
                        &mut encoder,
                        &self.pipelines,
                        &self.playzone_textures[view_idx],
                        depth_bind_group,
                        view_idx as u32,
                    );
                }
                // The depth mask uses the focus area. Without hands it only needs to be cleared once.
                if let Some(rects) = &hand_rects
                    && !(rects[view_idx].is_none() && self.focus_area_cleared[view_idx].get())
                {
                    self.focus_area_cleared[view_idx].set(rects[view_idx].is_none());
                    encode_focus_area(
                        &mut encoder,
                        &self.pipelines,
                        &objects.focus_area,
                        &objects.focus_area_bind_group,
                        view_idx as u32,
                        self.focus_area_resolution,
                        rects[view_idx],
                    );
                }
                encode_depth_mask(
                    &mut encoder,
                    &self.pipelines,
                    &objects.depth_mask,
                    depth_bind_group,
                    memory_read.map_or(&self.playzone_bind_groups[view_idx], |memory| {
                        memory.read_bind_group(view_idx)
                    }),
                    view_idx as u32,
                );
            }
        }
        let dump = dump.map(|(n, frame, json)| {
            self.encode_depth_dump(&mut encoder, depth_bind_group, memory_read, n, &frame, json)
        });
        let burst_encoded = match burst_token {
            Some(token) if has_depth_frame => {
                let device = &self.context.device;
                let pipeline = self
                    .dump_pipeline
                    .get_or_init(|| create_depth_dump_pipeline(device, &self.pipelines));
                self.burst_readbacks.borrow_mut().encode(
                    device,
                    &mut encoder,
                    pipeline,
                    depth_bind_group,
                    &self.playzone_bind_groups,
                    self.depth_resolution,
                    token,
                )
            }
            Some(token) => {
                self.burst_readbacks
                    .borrow_mut()
                    .fail(token, "no depth image in the renderer");
                false
            }
            None => false,
        };

        let overlay_draws = (depth_config.is_some() && !overlay.is_empty())
            .then(|| self.write_overlay(&view_params, depth, overlay));

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
            render_pass.set_bind_group(2, &self.problem_view.bind_group, &[]);
            set_passthrough_immediates(&mut render_pass, passthrough);
            render_pass.draw(0..4, 0..1);

            if let Some(draws) = &overlay_draws
                && let (eye, first, count) = &draws[view_idx]
                && *count > 0
            {
                encode_overlay(
                    &mut render_pass,
                    &self.pipelines,
                    &self.overlay_bind_group,
                    eye,
                    *first,
                    *count,
                );
            }
        }

        self.context.queue.submit(iter::once(encoder.finish()));

        // wgpu doesn't look at the GL errors (of an unsupported copy), and the next `check_error` would
        // abort
        drop(memory);
        let gl_error = if dump.is_some() || burst_encoded || stats_encoded {
            self.context.take_gl_error()
        } else {
            None
        };
        if let Some(memory) = self.memory.borrow_mut().as_mut() {
            memory.submitted(gl_error.map(|error| format!("{error:#x}")).as_deref());
            memory.poll(&self.context.device);
        }
        if let Some(dump) = dump {
            let dump = match gl_error {
                Some(error) => dump.fail(format!("GL error {error:#x}")),
                None => dump.start(),
            };
            self.dump_readback.replace(Some(dump));
        }
        if burst_encoded {
            self.burst_readbacks
                .borrow_mut()
                .submitted(gl_error.map(|error| format!("{error:#x}")).as_deref());
        }

        hand_rects
    }

    // Clips the segments for each eye and uploads them. Per eye: the camera, the first segment and
    // their number. The camera must be the one of the depth mask, or the outline is offset from the
    // hole it describes. The mask casts its rays from the eye position located at display time
    // (`DepthRenderInput::eye_positions`) in the directions of the pixels of the stream view, and that
    // image is drawn into the output view turned only by the rotation between the two (the quad of
    // `reprojection_transform` is 1 km away). So a point hit by a ray shows in the output view in its
    // direction from that eye position: the camera is that position with the orientation and the fov of
    // the output view (on Quest the same as the stream view)
    fn write_overlay(
        &self,
        view_params: &[StreamViewParams; 2],
        depth: Option<&DepthRenderInput>,
        overlay: &[OverlaySegment],
    ) -> [(OverlayEye, u32, u32); 2] {
        // No eye positions in the first frame of the environment depth: the position of the view
        let eye_positions = depth
            .map(|depth| depth.eye_positions)
            .filter(|positions| *positions != [Vec3::ZERO; 2]);
        let mut data = Vec::with_capacity(2 * overlay.len() * SEGMENT_FLOAT_COUNT);
        let draws = [0, 1].map(|i| {
            let output = &view_params[i].output_view_params;
            let eye = OverlayEye::new(
                eye_positions.map_or(output.pose.position, |positions| positions[i]),
                output.pose.orientation,
                output.fov,
                self.target_view_resolution.as_vec2(),
            );
            let first = (data.len() / SEGMENT_FLOAT_COUNT) as u32;
            let count = overlay::pack_segments(&eye, overlay, &mut data);
            (eye, first, count)
        });
        if !data.is_empty() {
            self.context
                .queue
                .write_buffer(&self.overlay_buffer, 0, &floats_to_bytes(&data));
        }

        draws
    }

    // `debug.alvr.mem_roll`: the memory in the JSON of a depth dump, with its files. None: off or not
    // anchored yet (then it has no files either)
    fn memory_dump_json(&self, input: &DepthRenderInput, n: i64) -> Option<String> {
        let memory_input = input.memory?;
        let memory = self.memory.borrow();
        let memory = memory.as_ref().filter(|memory| memory.camera().is_some())?;
        let status = memory
            .status()
            .to_json(Some(&memory_input.params), Some(memory_input.hand_sources));

        Some(object([
            ("status", status),
            (
                "files",
                array([0, 1].map(|eye| string(&depth_dump::file_name(n, "memory", eye)))),
            ),
        ]))
    }

    fn encode_depth_dump(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        depth_bind_group: &BindGroup,
        memory: Option<&RoomMemory>,
        n: i64,
        frame: &depth_mask::DepthFrame,
        json: String,
    ) -> DumpReadback {
        let device = &self.context.device;
        let pipeline = self
            .dump_pipeline
            .get_or_init(|| create_depth_dump_pipeline(device, &self.pipelines));
        let has_playzone = self.playzone.get().is_some();

        let mut sources = vec![];
        for (eye, objects) in self.views_objects.iter().enumerate() {
            sources.extend([
                depth_dump::DumpSource::Depth {
                    eye: eye as u32,
                    depth_bind_group,
                    playzone_bind_group: &self.playzone_bind_groups[eye],
                    size: self.depth_resolution,
                    file: depth_dump::file_name(n, "depth", eye),
                    playzone_file: has_playzone.then(|| depth_dump::file_name(n, "playzone", eye)),
                },
                depth_dump::DumpSource::Rgba {
                    texture: objects.depth_mask.texture().clone(),
                    file: depth_dump::file_name(n, "mask", eye),
                },
                depth_dump::DumpSource::Rgba {
                    texture: objects.focus_area.texture().clone(),
                    file: depth_dump::file_name(n, "focus", eye),
                },
            ]);
            if let Some(memory) = memory
                && let Some(pipelines) = self.memory_pipelines.get()
            {
                sources.push(memory.dump_source(
                    pipelines,
                    depth_bind_group,
                    eye,
                    depth_dump::file_name(n, "memory", eye),
                ));
            }
        }

        DumpReadback::encode(
            device,
            encoder,
            pipeline,
            n,
            sources,
            frame,
            (depth_dump::json_name(n), json),
        )
    }
}

fn create_playzone(
    device: &Device,
    pipelines: &StreamPipelines,
    views_objects: &[ViewObjects; 2],
    sampler: &Sampler,
    depth_resolution: UVec2,
) -> ([TextureView; 2], [BindGroup; 2]) {
    let textures = [0, 1].map(|_| {
        create_playzone_texture(device, depth_resolution)
            .create_view(&TextureViewDescriptor::default())
    });
    let bind_groups = [0, 1].map(|i| {
        create_playzone_bind_group(
            device,
            pipelines,
            &textures[i],
            &views_objects[i].focus_area,
            sampler,
        )
    });

    (textures, bind_groups)
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
    depth_mask: &TextureView,
    focus_area: &TextureView,
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
            BindGroupEntry {
                binding: 5,
                resource: BindingResource::TextureView(depth_mask),
            },
            BindGroupEntry {
                binding: 6,
                resource: BindingResource::TextureView(focus_area),
            },
        ],
    })
}

pub(crate) fn encode_depth_mask(
    encoder: &mut wgpu::CommandEncoder,
    pipelines: &StreamPipelines,
    target: &TextureView,
    depth_bind_group: &BindGroup,
    playzone_bind_group: &BindGroup,
    view_idx: u32,
) {
    let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
        label: Some("depth mask"),
        color_attachments: &[Some(RenderPassColorAttachment {
            view: target,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: LoadOp::Clear(Color::TRANSPARENT),
                store: StoreOp::Store,
            },
        })],
        ..Default::default()
    });
    pass.set_pipeline(&pipelines.depth_mask);
    pass.set_bind_group(0, depth_bind_group, &[]);
    pass.set_bind_group(1, playzone_bind_group, &[]);
    pass.set_immediates(0, &view_idx.to_le_bytes());
    pass.draw(0..3, 0..1);
}

/// The segments `first..first + count` of the overlay buffer, in the main pass of an eye after the
/// stream
pub(crate) fn encode_overlay(
    render_pass: &mut RenderPass,
    pipelines: &StreamPipelines,
    bind_group: &BindGroup,
    eye: &OverlayEye,
    first: u32,
    count: u32,
) {
    render_pass.set_pipeline(&pipelines.overlay);
    render_pass.set_bind_group(0, bind_group, &[]);
    render_pass.set_immediates(0, &floats_to_bytes(&eye.view_proj.to_cols_array()));
    render_pass.set_immediates(
        OVERLAY_VIEWPORT_OFFSET,
        &floats_to_bytes(&eye.viewport.to_array()),
    );
    render_pass.set_immediates(
        OVERLAY_PIXELS_PER_RADIAN_OFFSET,
        &eye.pixels_per_radian.to_le_bytes(),
    );
    render_pass.set_immediates(OVERLAY_FIRST_OFFSET, &first.to_le_bytes());
    render_pass.draw(0..4, 0..count);
}

pub(crate) fn encode_playzone_copy(
    encoder: &mut wgpu::CommandEncoder,
    pipelines: &StreamPipelines,
    target: &TextureView,
    depth_bind_group: &BindGroup,
    view_idx: u32,
) {
    let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
        label: Some("playzone copy"),
        color_attachments: &[Some(RenderPassColorAttachment {
            view: target,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: LoadOp::Clear(Color::TRANSPARENT),
                store: StoreOp::Store,
            },
        })],
        ..Default::default()
    });
    pass.set_pipeline(&pipelines.playzone_copy);
    pass.set_bind_group(0, depth_bind_group, &[]);
    pass.set_immediates(0, &view_idx.to_le_bytes());
    pass.draw(0..3, 0..1);
}

/// One eye of the environment depth and the playzone, see depth_dump.wgsl. `target`: RGBA8, the size of
/// the depth map
pub(crate) fn encode_depth_dump(
    encoder: &mut wgpu::CommandEncoder,
    pipeline: &RenderPipeline,
    target: &TextureView,
    depth_bind_group: &BindGroup,
    playzone_bind_group: &BindGroup,
    view_idx: u32,
) {
    let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
        label: Some("depth dump"),
        color_attachments: &[Some(RenderPassColorAttachment {
            view: target,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: LoadOp::Clear(Color::TRANSPARENT),
                store: StoreOp::Store,
            },
        })],
        ..Default::default()
    });
    pass.set_pipeline(pipeline);
    pass.set_bind_group(0, depth_bind_group, &[]);
    pass.set_bind_group(1, playzone_bind_group, &[]);
    pass.set_immediates(0, &view_idx.to_le_bytes());
    pass.draw(0..3, 0..1);
}

pub(crate) fn encode_focus_area(
    encoder: &mut wgpu::CommandEncoder,
    pipelines: &StreamPipelines,
    target: &TextureView,
    bind_group: &BindGroup,
    view_idx: u32,
    resolution: UVec2,
    rect: Option<(Vec2, Vec2)>,
) {
    let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
        label: Some("focus area"),
        color_attachments: &[Some(RenderPassColorAttachment {
            view: target,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: LoadOp::Clear(Color::TRANSPARENT),
                store: StoreOp::Store,
            },
        })],
        ..Default::default()
    });
    // Without hands the target is only cleared
    let Some((min, max)) = rect else {
        return;
    };
    let min = (min * resolution.as_vec2()).floor().as_uvec2();
    let max = (max * resolution.as_vec2())
        .ceil()
        .as_uvec2()
        .min(resolution);
    if !min.cmplt(max).all() {
        return;
    }
    pass.set_scissor_rect(min.x, min.y, max.x - min.x, max.y - min.y);
    pass.set_pipeline(&pipelines.focus_area);
    pass.set_bind_group(0, bind_group, &[]);
    pass.set_immediates(0, &view_idx.to_le_bytes());
    pass.draw(0..3, 0..1);
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

pub(crate) fn reprojection_transform(view_params: &StreamViewParams) -> Mat4 {
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
