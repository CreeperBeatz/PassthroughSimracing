//! The GPU side of the depth cloud (`debug.alvr.cloud`; the maths in motion_cloud.rs, the passes in
//! motion_cloud.wgsl): the pipelines, created at the first use, and the textures, created when the cloud
//! is switched on and dropped when it is switched off. While it is off none of this exists and no pass
//! runs.
//!
//! Per eye: two state textures in the grid of the depth (ping-pong: `mark_main` reads the current one,
//! the state of the image before, and writes the other), one rows texture, two field textures of 2 x 2
//! texel cells (ping-pong: `field_main` reads the current one for the history). The state and the field
//! of an eye flip together, so one slot index tells both. The mask reads the current field after this
//! frame's passes, through `read_bind_group`. Eye 0 is done on the frame of a new depth image, eye 1 on
//! the next one (see `plan_eyes`).

use crate::{
    DepthFrame,
    motion_cloud::{
        CLOUD_UNIFORMS_SIZE, CloudEyeUniform, CloudImage, CloudInput, CloudPasses, CloudSchedule,
        CloudStatus, cloud_uniforms, field_open, field_stale, lead_ms, plan_eyes, reach_seconds,
        region_signature,
    },
    room_memory_gpu::{DepthKey, depth_key},
    stream::{self, StreamPipelines, floats_to_bytes, shader_module, texture_entry, uniform_entry},
};
use alvr_common::glam::UVec2;
use wgpu::{
    AddressMode, BindGroup, BindGroupDescriptor, BindGroupEntry, BindGroupLayout,
    BindGroupLayoutDescriptor, BindingResource, Buffer, BufferDescriptor, BufferUsages, Color,
    ColorTargetState, ColorWrites, CommandEncoder, Device, Extent3d, FilterMode, FragmentState,
    LoadOp, Origin3d, PipelineLayoutDescriptor, Queue, RenderPassColorAttachment,
    RenderPassDescriptor, RenderPipeline, RenderPipelineDescriptor, SamplerDescriptor, StoreOp,
    TexelCopyBufferLayout, TexelCopyTextureInfo, Texture, TextureAspect, TextureDescriptor,
    TextureDimension, TextureFormat, TextureUsages, TextureView, TextureViewDescriptor,
    TextureViewDimension, VertexState,
};

/// `mark_main`: depth (m), M, Mp, Mp before the flip test
pub(crate) const CLOUD_STATE_FORMAT: TextureFormat = TextureFormat::Rgba16Float;
/// `rows_main` and `field_main`
pub(crate) const CLOUD_FIELD_FORMAT: TextureFormat = TextureFormat::Rgba8Unorm;
/// A field with nothing in it (the dummy, a field before the first image): far from any edge, no
/// speed. As RGBA8 bytes
pub(crate) const CLOUD_CLEARED_FIELD: [u8; 4] = [255, 0, 0, 128];
// motion_cloud.wgsl: view_idx, mode
const CLOUD_IMMEDIATE_SIZE: u32 = 2 * size_of::<u32>() as u32;

/// Pipelines of motion_cloud.wgsl. Each uses `depth_layout` and `layout`
pub(crate) struct CloudPipelines {
    /// state texture, rows texture, previous field texture, linear sampler, cloud uniforms
    pub layout: BindGroupLayout,
    /// `mark_main`, into a state texture
    pub mark: RenderPipeline,
    /// `rows_main`, into the rows texture
    pub rows: RenderPipeline,
    /// `field_main`, into a field texture
    pub field: RenderPipeline,
}

impl CloudPipelines {
    pub fn new(device: &Device, pipelines: &StreamPipelines) -> Self {
        let layout = device.create_bind_group_layout(&BindGroupLayoutDescriptor {
            label: Some("motion cloud"),
            entries: &[
                // Sampled by rows_main (Rgba16Float is filterable)
                texture_entry(0, true, TextureViewDimension::D2),
                texture_entry(1, false, TextureViewDimension::D2),
                texture_entry(2, true, TextureViewDimension::D2),
                stream::sampler_entry(3),
                uniform_entry(4),
            ],
        });
        let module = shader_module(device, "motion cloud", stream::motion_cloud_shader_source());
        let pipeline_layout = device.create_pipeline_layout(&PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&pipelines.depth_layout), Some(&layout)],
            immediate_size: CLOUD_IMMEDIATE_SIZE,
        });
        let pipeline = |label: &str, entry_point: &str, format: TextureFormat| {
            device.create_render_pipeline(&RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&pipeline_layout),
                vertex: VertexState {
                    module: &module,
                    entry_point: Some("vertex_main"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                primitive: Default::default(),
                depth_stencil: None,
                multisample: Default::default(),
                fragment: Some(FragmentState {
                    module: &module,
                    entry_point: Some(entry_point),
                    compilation_options: Default::default(),
                    targets: &[Some(ColorTargetState {
                        format,
                        blend: None,
                        write_mask: ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };

        Self {
            mark: pipeline("motion cloud mark", "mark_main", CLOUD_STATE_FORMAT),
            rows: pipeline("motion cloud rows", "rows_main", CLOUD_FIELD_FORMAT),
            field: pipeline("motion cloud field", "field_main", CLOUD_FIELD_FORMAT),
            layout,
        }
    }
}

/// Group 2 of the depth mask: the field of the eye, the cloud uniforms
pub(crate) fn create_cloud_read_layout(device: &Device) -> BindGroupLayout {
    device.create_bind_group_layout(&BindGroupLayoutDescriptor {
        label: Some("motion cloud read"),
        entries: &[
            texture_entry(0, true, TextureViewDimension::D2),
            uniform_entry(1),
        ],
    })
}

pub(crate) fn create_cloud_read_bind_group(
    device: &Device,
    layout: &BindGroupLayout,
    field: &TextureView,
    uniforms: &Buffer,
) -> BindGroup {
    device.create_bind_group(&BindGroupDescriptor {
        label: Some("motion cloud read"),
        layout,
        entries: &[
            BindGroupEntry {
                binding: 0,
                resource: BindingResource::TextureView(field),
            },
            BindGroupEntry {
                binding: 1,
                resource: uniforms.as_entire_binding(),
            },
        ],
    })
}

/// Zeros until written: every camera of an eye without a field projects nowhere
pub(crate) fn create_cloud_uniform_buffer(device: &Device) -> Buffer {
    device.create_buffer(&BufferDescriptor {
        label: Some("motion cloud uniforms"),
        size: CLOUD_UNIFORMS_SIZE as u64,
        usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn create_texture(device: &Device, label: &str, size: UVec2, format: TextureFormat) -> Texture {
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
        format,
        // The fields are RGBA8: they can be read back (tests, a later stats pass)
        usage: TextureUsages::RENDER_ATTACHMENT
            | TextureUsages::TEXTURE_BINDING
            | if format == CLOUD_FIELD_FORMAT {
                TextureUsages::COPY_SRC
            } else {
                TextureUsages::empty()
            },
        view_formats: &[],
    })
}

/// A 1 x 1 field with nothing in it: the mask's field while the cloud is off, and what rows_main binds
/// in place of the rows it writes
pub(crate) fn create_cloud_dummy_field(device: &Device, queue: &Queue) -> Texture {
    let texture = device.create_texture(&TextureDescriptor {
        label: Some("motion cloud dummy"),
        size: Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: TextureDimension::D2,
        format: CLOUD_FIELD_FORMAT,
        usage: TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: Origin3d::ZERO,
            aspect: TextureAspect::All,
        },
        &CLOUD_CLEARED_FIELD,
        TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(4),
            rows_per_image: Some(1),
        },
        Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
    );
    texture
}

/// One fullscreen pass of motion_cloud.wgsl into `target`
pub(crate) fn encode_cloud_pass(
    encoder: &mut CommandEncoder,
    pipeline: &RenderPipeline,
    target: &TextureView,
    depth_bind_group: &BindGroup,
    cloud_bind_group: &BindGroup,
    view_idx: u32,
) {
    let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
        label: Some("motion cloud"),
        color_attachments: &[Some(RenderPassColorAttachment {
            view: target,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                // Every texel is written
                load: LoadOp::Clear(Color::TRANSPARENT),
                store: StoreOp::Store,
            },
        })],
        ..Default::default()
    });
    pass.set_pipeline(pipeline);
    pass.set_bind_group(0, depth_bind_group, &[]);
    pass.set_bind_group(1, cloud_bind_group, &[]);
    pass.set_immediates(0, &view_idx.to_le_bytes());
    pass.set_immediates(4, &0_u32.to_le_bytes());
    pass.draw(0..3, 0..1);
}

// Clears a field to "nothing in it"
fn encode_field_clear(encoder: &mut CommandEncoder, target: &TextureView) {
    let [r, g, b, a] = CLOUD_CLEARED_FIELD.map(|byte| f64::from(byte) / 255.0);
    encoder.begin_render_pass(&RenderPassDescriptor {
        label: Some("motion cloud clear"),
        color_attachments: &[Some(RenderPassColorAttachment {
            view: target,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: LoadOp::Clear(Color { r, g, b, a }),
                store: StoreOp::Store,
            },
        })],
        ..Default::default()
    });
}

/// The textures of the cloud, its uniforms and the schedule of the eyes
pub(crate) struct MotionCloud {
    /// Texels of the depth image (the state), and cells of the field
    pub depth_size: UVec2,
    #[cfg_attr(not(test), expect(dead_code, reason = "read back by the tests only"))]
    pub field_size: UVec2,
    // [eye][slot]
    states: [[TextureView; 2]; 2],
    rows: [TextureView; 2],
    #[cfg_attr(not(test), expect(dead_code, reason = "read back by the tests only"))]
    fields: [[Texture; 2]; 2],
    field_views: [[TextureView; 2]; 2],
    // [eye][slot of the state and field before the passes]. A pass never binds its target: rows_main
    // binds the dummy in place of the rows
    mark_groups: [[BindGroup; 2]; 2],
    rows_groups: [[BindGroup; 2]; 2],
    field_groups: [[BindGroup; 2]; 2],
    // The mask reads the field of slot `current[eye]` (group 2: field, uniforms)
    read_groups: [[BindGroup; 2]; 2],
    current: [usize; 2],
    uniform_buffer: Buffer,
    // The fields still hold whatever the texture was created with
    cleared: bool,
    last_depth: Option<DepthKey>,
    schedule: CloudSchedule,
    // The cameras and pair of the field of each eye, None: no field yet
    eyes: [Option<CloudEyeUniform>; 2],
    status: CloudStatus,
}

impl MotionCloud {
    /// `read_layout`: group 2 of the mask (`create_cloud_read_layout`). `depth_size`: of the depth images
    pub fn new(
        device: &Device,
        queue: &Queue,
        pipelines: &CloudPipelines,
        read_layout: &BindGroupLayout,
        depth_size: UVec2,
    ) -> Self {
        let depth_size = depth_size.max(UVec2::ONE);
        let field_size = (depth_size + 1) / 2;
        let view = |texture: &Texture| texture.create_view(&TextureViewDescriptor::default());
        let states = [0, 1].map(|_| {
            [0, 1].map(|_| {
                view(&create_texture(
                    device,
                    "motion cloud state",
                    depth_size,
                    CLOUD_STATE_FORMAT,
                ))
            })
        });
        let rows = [0, 1].map(|_| {
            view(&create_texture(
                device,
                "motion cloud rows",
                field_size,
                CLOUD_FIELD_FORMAT,
            ))
        });
        let fields = [0, 1].map(|_| {
            [0, 1].map(|_| {
                create_texture(device, "motion cloud field", field_size, CLOUD_FIELD_FORMAT)
            })
        });
        let field_views = [0, 1].map(|eye| [0, 1].map(|slot| view(&fields[eye][slot])));
        let dummy = view(&create_cloud_dummy_field(device, queue));
        let uniform_buffer = create_cloud_uniform_buffer(device);
        let sampler = device.create_sampler(&SamplerDescriptor {
            label: Some("motion cloud"),
            address_mode_u: AddressMode::ClampToEdge,
            address_mode_v: AddressMode::ClampToEdge,
            mag_filter: FilterMode::Linear,
            min_filter: FilterMode::Linear,
            ..Default::default()
        });
        let group = |state: &TextureView, rows: &TextureView, field: &TextureView| {
            device.create_bind_group(&BindGroupDescriptor {
                label: Some("motion cloud"),
                layout: &pipelines.layout,
                entries: &[
                    BindGroupEntry {
                        binding: 0,
                        resource: BindingResource::TextureView(state),
                    },
                    BindGroupEntry {
                        binding: 1,
                        resource: BindingResource::TextureView(rows),
                    },
                    BindGroupEntry {
                        binding: 2,
                        resource: BindingResource::TextureView(field),
                    },
                    BindGroupEntry {
                        binding: 3,
                        resource: BindingResource::Sampler(&sampler),
                    },
                    BindGroupEntry {
                        binding: 4,
                        resource: uniform_buffer.as_entire_binding(),
                    },
                ],
            })
        };
        // mark_main writes state 1 - slot; rows_main the rows; field_main field 1 - slot
        let mark_groups = [0, 1].map(|eye| {
            [0, 1].map(|slot| group(&states[eye][slot], &rows[eye], &field_views[eye][slot]))
        });
        let rows_groups = [0, 1].map(|eye| {
            [0, 1].map(|slot| group(&states[eye][1 - slot], &dummy, &field_views[eye][slot]))
        });
        let field_groups = [0, 1].map(|eye| {
            [0, 1].map(|slot| group(&states[eye][1 - slot], &rows[eye], &field_views[eye][slot]))
        });
        let read_groups = [0, 1].map(|eye| {
            [0, 1].map(|slot| {
                create_cloud_read_bind_group(
                    device,
                    read_layout,
                    &field_views[eye][slot],
                    &uniform_buffer,
                )
            })
        });

        Self {
            depth_size,
            field_size,
            states,
            rows,
            fields,
            field_views,
            mark_groups,
            rows_groups,
            field_groups,
            read_groups,
            current: [0; 2],
            uniform_buffer,
            cleared: false,
            last_depth: None,
            schedule: CloudSchedule::default(),
            eyes: [None; 2],
            status: CloudStatus::default(),
        }
    }

    /// The passes of this frame, and the uniforms (written on every frame: the cameras of the eyes whose
    /// passes run now, the params, the reach time of both eyes). `depth_uniforms`: of this frame, to
    /// tell a changed region
    pub fn plan(
        &mut self,
        queue: &Queue,
        input: &CloudInput,
        frame: Option<&DepthFrame>,
        depth_uniforms: &[f32],
    ) -> CloudPasses {
        let mut passes = CloudPasses::default();
        if let Some(frame) = frame {
            let key = depth_key(input.capture_ns, frame);
            let new_image = self.last_depth != Some(key);
            self.last_depth = Some(key);
            let image = CloudImage {
                capture_ns: input.capture_ns,
                views: frame.depth_views,
                region: region_signature(depth_uniforms),
            };
            let before = self.schedule.sources;
            passes = plan_eyes(&mut self.schedule, &input.params, new_image, &image);
            for (eye, pass) in passes.eyes.iter().enumerate() {
                let Some(pass) = pass else {
                    continue;
                };
                self.eyes[eye] = Some(CloudEyeUniform {
                    field: image.views[eye],
                    prev: before[eye].map_or(image.views[eye], |source| source.view),
                    dt_s: pass.dt_s,
                    decay: pass.decay(&input.params),
                    reset: pass.reset,
                });
                self.status.count(eye, pass, image.capture_ns);
            }
        }

        // Also without a new image (the depth stalled, or no frame at all): a field older than
        // CLOUD_STALE_MS opens no zone
        let field_capture = self
            .schedule
            .sources
            .map(|source| source.and_then(|source| source.capture_ns));
        let reach =
            [0, 1].map(|eye| reach_seconds(&input.params, input.display_ns, field_capture[eye]));
        let open = [0, 1].map(|eye| field_open(input.display_ns, field_capture[eye]));
        for eye in 0..2 {
            let record = &mut self.status.eyes[eye];
            record.a_ms = lead_ms(&input.params, input.display_ns, field_capture[eye]);
            record.reach_ms = reach[eye] * 1000.0;
            record.stale = field_stale(input.display_ns, field_capture[eye]);
        }
        queue.write_buffer(
            &self.uniform_buffer,
            0,
            &floats_to_bytes(&cloud_uniforms(&self.eyes, &input.params, reach, open)),
        );

        passes
    }

    /// Encodes the passes of `plan`, before the mask that reads the field. `depth_bind_group`: the
    /// depth image of this frame with the depth uniforms
    pub fn encode(
        &mut self,
        encoder: &mut CommandEncoder,
        pipelines: &CloudPipelines,
        depth_bind_group: &BindGroup,
        passes: &CloudPasses,
    ) {
        if !self.cleared {
            for view in self.field_views.iter().flatten() {
                encode_field_clear(encoder, view);
            }
            self.cleared = true;
        }
        for eye in 0..2 {
            if passes.eyes[eye].is_none() {
                continue;
            }
            let from = self.current[eye];
            let to = 1 - from;
            let view_idx = eye as u32;
            encode_cloud_pass(
                encoder,
                &pipelines.mark,
                &self.states[eye][to],
                depth_bind_group,
                &self.mark_groups[eye][from],
                view_idx,
            );
            encode_cloud_pass(
                encoder,
                &pipelines.rows,
                &self.rows[eye],
                depth_bind_group,
                &self.rows_groups[eye][from],
                view_idx,
            );
            encode_cloud_pass(
                encoder,
                &pipelines.field,
                &self.field_views[eye][to],
                depth_bind_group,
                &self.field_groups[eye][from],
                view_idx,
            );
            self.current[eye] = to;
        }
    }

    /// Forgets the fields (the reference space changed): the next pair of each eye starts over, as after
    /// a switch-on, and until then the mask finds no zone. The counts of the status are kept, its
    /// records of the eyes (the fields they described) are emptied
    pub fn forget(&mut self) {
        self.schedule = CloudSchedule::default();
        self.eyes = [None; 2];
        self.last_depth = None;
        self.cleared = false;
        self.status.eyes = Default::default();
    }

    /// The mask's group 2 for this eye: the current field, after this frame's passes
    pub fn read_bind_group(&self, eye: usize) -> &BindGroup {
        &self.read_groups[eye][self.current[eye]]
    }

    /// The current field of an eye (RGBA8, `field_size`)
    #[cfg(test)]
    pub fn field_texture(&self, eye: usize) -> &Texture {
        &self.fields[eye][self.current[eye]]
    }

    pub fn status(&self) -> CloudStatus {
        self.status
    }
}
