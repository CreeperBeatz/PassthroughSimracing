//! The GPU side of the room memory (`debug.alvr.mem_roll`; the maths in room_memory.rs, the passes in
//! room_memory.wgsl): the pipelines, created at the first use, and the textures, created when the memory
//! is switched on and dropped when it is switched off. While it is off none of this exists and no pass
//! runs.
//!
//! Per eye two textures (ping-pong): an update reads the current one and writes the other, which then is
//! the current one; the mask reads the current one after this frame's passes. The two eyes are updated on
//! different display frames (eye 0 on the frame a new depth image arrives, eye 1 on the next one), so the
//! work of a depth image is spread over two frames (mask-v2-check.md, section 4: the pass is a spike on
//! depth frames). A re-anchor's clear or re-projection does both eyes at once (rare).

use crate::{
    DepthFrame,
    depth_dump::DumpSource,
    room_memory::{
        AnchorAction, MEMORY_FORMAT, MEMORY_STATS_CELLS, MEMORY_UNIFORMS_SIZE, MemoryAnchor,
        MemoryCamera, MemoryInput, MemoryStats, MemoryStatus, memory_uniforms,
    },
    stream::{self, StreamPipelines, floats_to_bytes, shader_module, texture_entry, uniform_entry},
};
use std::{
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};
use wgpu::{
    BindGroup, BindGroupDescriptor, BindGroupEntry, BindGroupLayout, BindGroupLayoutDescriptor,
    BindingResource, Buffer, BufferDescriptor, BufferUsages, Color, ColorTargetState, ColorWrites,
    CommandEncoder, Device, Extent3d, FragmentState, LoadOp, MapMode, PipelineLayoutDescriptor,
    Queue, RenderPassColorAttachment, RenderPassDescriptor, RenderPipeline,
    RenderPipelineDescriptor, Sampler, StoreOp, TexelCopyBufferInfo, TexelCopyBufferLayout,
    Texture, TextureDescriptor, TextureDimension, TextureFormat, TextureUsages, TextureView,
    TextureViewDescriptor, TextureViewDimension, VertexState,
};

// room_memory.wgsl: view_idx, mode
const MEMORY_IMMEDIATE_SIZE: u32 = 2 * size_of::<u32>() as u32;
/// `update_main` modes
pub(crate) const MODE_UPDATE: u32 = 0;
pub(crate) const MODE_REPROJECT: u32 = 1;
/// The stats of the left eye are read back this often
pub const MEMORY_STATS_INTERVAL: Duration = Duration::from_secs(2);
// The stats target: 2 texels per cell, rows padded for the copy
const STATS_WIDTH: u32 = 2 * MEMORY_STATS_CELLS;
const STATS_BYTES_PER_ROW: u32 = 256;

/// Pipelines of room_memory.wgsl. Each uses `depth_layout` and `layout`
pub(crate) struct MemoryPipelines {
    /// memory texture, memory uniforms
    pub layout: BindGroupLayout,
    /// `update_main`, into a memory texture
    pub update: RenderPipeline,
    /// `stats_main`, into the RGBA8 stats target
    pub stats: RenderPipeline,
    /// `dump_main`, into an RGBA8 target twice as wide as the memory
    pub dump: RenderPipeline,
}

impl MemoryPipelines {
    pub fn new(device: &Device, pipelines: &StreamPipelines) -> Self {
        let layout = device.create_bind_group_layout(&BindGroupLayoutDescriptor {
            label: Some("room memory"),
            entries: &[
                texture_entry(0, false, TextureViewDimension::D2),
                uniform_entry(1),
            ],
        });
        let module = shader_module(device, "room memory", stream::room_memory_shader_source());
        let pipeline_layout = device.create_pipeline_layout(&PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&pipelines.depth_layout), Some(&layout)],
            immediate_size: MEMORY_IMMEDIATE_SIZE,
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
            update: pipeline("room memory update", "update_main", MEMORY_FORMAT),
            stats: pipeline("room memory stats", "stats_main", TextureFormat::Rgba8Unorm),
            dump: pipeline("room memory dump", "dump_main", TextureFormat::Rgba8Unorm),
            layout,
        }
    }
}

/// One memory texture of `size` x `size`
pub(crate) fn create_memory_texture(device: &Device, size: u32) -> Texture {
    device.create_texture(&TextureDescriptor {
        label: Some("room memory"),
        size: Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: TextureDimension::D2,
        format: MEMORY_FORMAT,
        usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    })
}

pub(crate) fn create_memory_uniform_buffer(device: &Device) -> Buffer {
    device.create_buffer(&BufferDescriptor {
        label: Some("room memory uniforms"),
        size: MEMORY_UNIFORMS_SIZE as u64,
        usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

/// The memory a pass reads (group 1 of room_memory.wgsl)
pub(crate) fn create_memory_bind_group(
    device: &Device,
    pipelines: &MemoryPipelines,
    memory: &TextureView,
    uniforms: &Buffer,
) -> BindGroup {
    device.create_bind_group(&BindGroupDescriptor {
        label: Some("room memory"),
        layout: &pipelines.layout,
        entries: &[
            BindGroupEntry {
                binding: 0,
                resource: BindingResource::TextureView(memory),
            },
            BindGroupEntry {
                binding: 1,
                resource: uniforms.as_entire_binding(),
            },
        ],
    })
}

/// One fullscreen pass of room_memory.wgsl into `target`
pub(crate) fn encode_memory_pass(
    encoder: &mut CommandEncoder,
    pipeline: &RenderPipeline,
    target: &TextureView,
    depth_bind_group: &BindGroup,
    memory_bind_group: &BindGroup,
    view_idx: u32,
    mode: u32,
) {
    let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
        label: Some("room memory"),
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
    pass.set_bind_group(1, memory_bind_group, &[]);
    pass.set_immediates(0, &view_idx.to_le_bytes());
    pass.set_immediates(4, &mode.to_le_bytes());
    pass.draw(0..3, 0..1);
}

/// Clears a memory texture: nothing known
pub(crate) fn encode_memory_clear(encoder: &mut CommandEncoder, target: &TextureView) {
    encoder.begin_render_pass(&RenderPassDescriptor {
        label: Some("room memory clear"),
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
}

/// The passes of a frame, see `RoomMemory::plan`
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct MemoryPasses {
    /// Both eyes cleared (anchored cleanly)
    pub clear: bool,
    /// Both eyes re-projected into the new camera (anchored with hands)
    pub reproject: bool,
    /// Eyes updated with this frame's depth image
    pub update: [bool; 2],
    /// The stats of the left eye are read back
    pub stats: bool,
}

// A depth image: its capture time, swapchain index and the positions of its two views (bits). As the
// burst tells a new one. Also how the depth cloud tells one (motion_cloud_gpu.rs)
pub(crate) type DepthKey = (Option<i64>, u32, [u32; 6]);

pub(crate) fn depth_key(capture_ns: Option<i64>, frame: &DepthFrame) -> DepthKey {
    let [a, b] = frame
        .depth_views
        .map(|view| view.pose.position.to_array().map(f32::to_bits));
    (
        capture_ns,
        frame.swapchain_index,
        [a[0], a[1], a[2], b[0], b[1], b[2]],
    )
}

enum StatsState {
    Idle,
    /// Copied in this frame's encoder, to be mapped after the submission
    Encoded,
    Mapping(Arc<OnceLock<Result<(), String>>>),
}

/// The stats of the left eye on their way back: a 32 x 16 RGBA8 target and its buffer
struct StatsReadback {
    target: Texture,
    buffer: Buffer,
    state: StatsState,
}

/// The memory textures, their camera and state
pub(crate) struct RoomMemory {
    pub size: u32,
    // [eye][slot]
    textures: [[TextureView; 2]; 2],
    // The mask reads slot `current[eye]` (`playzone_layout`: memory, focus area, sampler)
    read_bind_groups: [[BindGroup; 2]; 2],
    // The passes read slot k (`MemoryPipelines::layout`: memory, uniforms)
    source_bind_groups: [[BindGroup; 2]; 2],
    current: [usize; 2],
    uniform_buffer: Buffer,
    anchor: MemoryAnchor,
    last_depth: Option<DepthKey>,
    last_update: Option<Instant>,
    // Eye 1 of the last new depth image is still to be updated
    eye_pending: bool,
    updates: [u32; 2],
    stats: Option<(MemoryStats, Instant)>,
    stats_readback: StatsReadback,
    last_stats_request: Instant,
}

impl RoomMemory {
    /// `focus_areas`: of each eye, read by the mask with the memory
    pub fn new(
        device: &Device,
        pipelines: &StreamPipelines,
        memory_pipelines: &MemoryPipelines,
        focus_areas: [&TextureView; 2],
        sampler: &Sampler,
        size: u32,
    ) -> Self {
        let size = size.max(1);
        let textures = [0, 1].map(|_| {
            [0, 1].map(|_| {
                create_memory_texture(device, size).create_view(&TextureViewDescriptor::default())
            })
        });
        let uniform_buffer = create_memory_uniform_buffer(device);
        let read_bind_groups = [0, 1].map(|eye| {
            [0, 1].map(|slot| {
                stream::create_playzone_bind_group(
                    device,
                    pipelines,
                    &textures[eye][slot],
                    focus_areas[eye],
                    sampler,
                )
            })
        });
        let source_bind_groups = [0, 1].map(|eye| {
            [0, 1].map(|slot| {
                create_memory_bind_group(
                    device,
                    memory_pipelines,
                    &textures[eye][slot],
                    &uniform_buffer,
                )
            })
        });
        let stats_readback = StatsReadback {
            target: device.create_texture(&TextureDescriptor {
                label: Some("room memory stats"),
                size: Extent3d {
                    width: STATS_WIDTH,
                    height: MEMORY_STATS_CELLS,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: TextureDimension::D2,
                format: TextureFormat::Rgba8Unorm,
                usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::COPY_SRC,
                view_formats: &[],
            }),
            buffer: device.create_buffer(&BufferDescriptor {
                label: Some("room memory stats"),
                size: u64::from(STATS_BYTES_PER_ROW * MEMORY_STATS_CELLS),
                usage: BufferUsages::COPY_DST | BufferUsages::MAP_READ,
                mapped_at_creation: false,
            }),
            state: StatsState::Idle,
        };

        Self {
            size,
            textures,
            read_bind_groups,
            source_bind_groups,
            current: [0; 2],
            uniform_buffer,
            anchor: MemoryAnchor::default(),
            last_depth: None,
            last_update: None,
            eye_pending: false,
            updates: [0; 2],
            stats: None,
            stats_readback,
            last_stats_request: Instant::now(),
        }
    }

    /// The camera the mask reads the memory with. None: not anchored yet (nothing is read)
    pub fn camera(&self) -> Option<MemoryCamera> {
        self.anchor.camera
    }

    /// No camera: the next depth image anchors it cleanly (the reference space changed, a forget)
    pub fn forget(&mut self) {
        self.anchor.forget();
        self.last_depth = None;
        self.eye_pending = false;
    }

    /// The passes of this frame, and the uniforms they need (written at a new depth image only: eye 1 of
    /// a depth image is updated on the next frame with the same ones). The anchor is decided here.
    /// `stats_interval`: of the statistics readback, `MEMORY_STATS_INTERVAL` today (`debug.alvr.dbg_readback_ms`,
    /// None: never)
    pub fn plan(
        &mut self,
        queue: &Queue,
        input: &MemoryInput,
        frame: Option<&DepthFrame>,
        stats_interval: Option<Duration>,
    ) -> MemoryPasses {
        let mut passes = MemoryPasses::default();
        if let Some(frame) = frame {
            let key = depth_key(input.capture_ns, frame);
            if self.last_depth != Some(key) {
                self.last_depth = Some(key);
                let now = Instant::now();
                let dt = self
                    .last_update
                    .map_or(0.0, |last| now.duration_since(last).as_secs_f32());
                self.last_update = Some(now);

                let action = self.anchor.update(
                    &frame.depth_views,
                    input.params.pitch_deg,
                    input.hands_away,
                );
                if let Some(camera) = self.anchor.camera {
                    let source = match action {
                        AnchorAction::Reproject(old) => Some(old),
                        _ => None,
                    };
                    queue.write_buffer(
                        &self.uniform_buffer,
                        0,
                        &floats_to_bytes(&memory_uniforms(
                            &camera,
                            source.as_ref(),
                            &input.params,
                            &input.hands,
                            dt,
                        )),
                    );
                    passes.clear = action == AnchorAction::Clean;
                    passes.reproject = source.is_some();
                    // Eye 1 of the image before is done now if it is still waiting (two new images in a
                    // row), else on the next frame
                    passes.update = [true, self.eye_pending];
                    self.eye_pending = !self.eye_pending;
                }
            } else if self.eye_pending {
                passes.update[1] = true;
                self.eye_pending = false;
            }
        }

        if self.anchor.camera.is_some()
            && matches!(self.stats_readback.state, StatsState::Idle)
            && stats_interval.is_some_and(|interval| self.last_stats_request.elapsed() >= interval)
        {
            self.last_stats_request = Instant::now();
            passes.stats = true;
        }

        passes
    }

    /// Encodes the passes of `plan`, before the mask that reads the memory
    pub fn encode(
        &mut self,
        encoder: &mut CommandEncoder,
        pipelines: &MemoryPipelines,
        depth_bind_group: &BindGroup,
        passes: &MemoryPasses,
    ) {
        if passes.clear {
            for eye in 0..2 {
                encode_memory_clear(encoder, &self.textures[eye][self.current[eye]]);
            }
        }
        if passes.reproject {
            for eye in 0..2 {
                self.encode_step(encoder, pipelines, depth_bind_group, eye, MODE_REPROJECT);
            }
        }
        for eye in 0..2 {
            if passes.update[eye] {
                self.encode_step(encoder, pipelines, depth_bind_group, eye, MODE_UPDATE);
                self.updates[eye] += 1;
            }
        }
        if passes.stats {
            encode_memory_pass(
                encoder,
                &pipelines.stats,
                &self
                    .stats_readback
                    .target
                    .create_view(&TextureViewDescriptor::default()),
                depth_bind_group,
                &self.source_bind_groups[0][self.current[0]],
                0,
                MODE_UPDATE,
            );
            encoder.copy_texture_to_buffer(
                self.stats_readback.target.as_image_copy(),
                TexelCopyBufferInfo {
                    buffer: &self.stats_readback.buffer,
                    layout: TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(STATS_BYTES_PER_ROW),
                        rows_per_image: Some(MEMORY_STATS_CELLS),
                    },
                },
                Extent3d {
                    width: STATS_WIDTH,
                    height: MEMORY_STATS_CELLS,
                    depth_or_array_layers: 1,
                },
            );
            self.stats_readback.state = StatsState::Encoded;
        }
    }

    // The current texture of the eye in, the other one out, which is then the current one
    fn encode_step(
        &mut self,
        encoder: &mut CommandEncoder,
        pipelines: &MemoryPipelines,
        depth_bind_group: &BindGroup,
        eye: usize,
        mode: u32,
    ) {
        let from = self.current[eye];
        let to = 1 - from;
        encode_memory_pass(
            encoder,
            &pipelines.update,
            &self.textures[eye][to],
            depth_bind_group,
            &self.source_bind_groups[eye][from],
            eye as u32,
            mode,
        );
        self.current[eye] = to;
    }

    /// Whether this frame's encoder holds the stats copy (its GL error must be taken after the submission)
    pub fn has_encoded_stats(&self) -> bool {
        matches!(self.stats_readback.state, StatsState::Encoded)
    }

    /// After the submission: maps the stats buffer if it was copied in this frame. `gl_error`: of the
    /// submission, the stats are then dropped
    pub fn submitted(&mut self, gl_error: Option<&str>) {
        if !matches!(self.stats_readback.state, StatsState::Encoded) {
            return;
        }
        if gl_error.is_some() {
            self.stats_readback.state = StatsState::Idle;
            return;
        }
        let status = Arc::new(OnceLock::new());
        let answer = Arc::clone(&status);
        self.stats_readback
            .buffer
            .map_async(MapMode::Read, .., move |result| {
                answer.set(result.map_err(|e| e.to_string())).ok();
            });
        self.stats_readback.state = StatsState::Mapping(status);
    }

    /// Call every frame: takes the stats when their buffer is mapped
    pub fn poll(&mut self, device: &Device) {
        let StatsState::Mapping(status) = &self.stats_readback.state else {
            return;
        };
        if status.get().is_none() {
            device.poll(wgpu::PollType::Poll).ok();
        }
        match status.get() {
            None => (),
            Some(Ok(())) => {
                let stats = {
                    let data = self.stats_readback.buffer.slice(..).get_mapped_range();
                    MemoryStats::decode(&data, STATS_BYTES_PER_ROW as usize)
                };
                self.stats_readback.buffer.unmap();
                if let Some(stats) = stats {
                    self.stats = Some((stats, Instant::now()));
                }
                self.stats_readback.state = StatsState::Idle;
            }
            Some(Err(_)) => self.stats_readback.state = StatsState::Idle,
        }
    }

    /// The mask's group 1 for this eye: the current memory, after this frame's passes
    pub fn read_bind_group(&self, eye: usize) -> &BindGroup {
        &self.read_bind_groups[eye][self.current[eye]]
    }

    pub fn status(&self) -> MemoryStatus {
        MemoryStatus {
            anchor: self.anchor,
            size: self.size,
            updates: self.updates,
            stats: self
                .stats
                .map(|(stats, time)| (stats, time.elapsed().as_secs_f32())),
        }
    }

    /// The current memory of an eye for the depth dump (all four channels)
    pub fn dump_source<'a>(
        &'a self,
        pipelines: &'a MemoryPipelines,
        depth_bind_group: &'a BindGroup,
        eye: usize,
        file: String,
    ) -> DumpSource<'a> {
        DumpSource::Memory {
            eye: eye as u32,
            pipeline: &pipelines.dump,
            depth_bind_group,
            memory_bind_group: &self.source_bind_groups[eye][self.current[eye]],
            size: self.size,
            file,
        }
    }
}
