//! `debug.alvr.key_dump` on the render thread: the environment depth, the remembered playzone, the depth
//! mask and the focus area of one frame, read back from the GPU, with what is needed to map their texels
//! to the world and to the eyes. The file format is in the knowledge base.

use crate::{
    PlacedArea,
    depth_mask::{self, ArmGap, ArmMotion, DepthFrame, DepthRenderInput},
    key_dump::{array, number, numbers, object, pose, string},
    stream,
};
use alvr_common::{ViewParams, glam::UVec2};
use alvr_session::{DepthMaskRegion, DepthPassthroughConfig, DepthPassthroughDebugView};
use std::{
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};
use wgpu::{
    BindGroup, Buffer, BufferDescriptor, BufferUsages, COPY_BYTES_PER_ROW_ALIGNMENT,
    CommandEncoder, Device, Extent3d, MapMode, RenderPipeline, TexelCopyBufferInfo,
    TexelCopyBufferLayout, Texture, TextureDescriptor, TextureDimension, TextureFormat,
    TextureUsages, TextureViewDescriptor,
};

/// A dump is given up if it has no data after this long
pub const DEPTH_DUMP_TIMEOUT: Duration = Duration::from_secs(1);

const EYES: [&str; 2] = ["left", "right"];

/// What the client knows about the frame of a dump
#[derive(Clone, Copy, Debug)]
pub struct DepthDumpRequest {
    /// `debug.alvr.key_dump`, names the files
    pub n: i64,
    /// Predicted display time of the frame: XrTime, CLOCK_MONOTONIC on Quest like the camera images
    pub display_ns: i64,
    /// When the depth image was captured, same clock. None: not reported
    pub depth_capture_ns: Option<i64>,
    /// When the frame was rendered, same clock
    pub now_ns: Option<i64>,
}

/// A finished dump, still to be decoded into files (a few MB: not on the render thread)
pub struct DepthDumpFiles {
    data: Vec<u8>,
    sections: Vec<Section>,
    near: f32,
    far: f32,
    json: (String, String),
}

impl DepthDumpFiles {
    /// Name and content of each file, the JSON last
    pub fn into_files(self) -> Vec<(String, Vec<u8>)> {
        let mut files = decode(&self.data, &self.sections, self.near, self.far);
        files.push((self.json.0, self.json.1.into_bytes()));

        files
    }
}

/// `kind`: depth, playzone (little-endian f32), memory (4 little-endian f32 per texel), mask, focus
/// (RGBA8)
pub(crate) fn file_name(n: i64, kind: &str, eye: usize) -> String {
    let extension = if matches!(kind, "depth" | "playzone" | "memory") {
        "f32"
    } else {
        "rgba"
    };
    format!("alvr_dump_{n}_{kind}_{}.{extension}", EYES[eye])
}

pub(crate) fn json_name(n: i64) -> String {
    format!("alvr_dump_{n}_depth.json")
}

/// Distance from the camera plane in meters of a depth value (16 bits), 0: no data. Same as
/// `load_depth` and `linearize_depth` in depth_mask.wgsl. `far` may be infinite
pub(crate) fn depth_meters(raw: u16, near: f32, far: f32) -> f32 {
    let depth = f32::from(raw) / 65535.0;
    if raw == 0 || depth >= 0.9999 {
        return 0.0;
    }
    let ndc = depth * 2.0 - 1.0;
    if far.is_finite() && far > near {
        2.0 * near * far / ((far + near) - ndc * (far - near))
    } else {
        2.0 * near / (1.0 - ndc).max(1e-6)
    }
}

pub(crate) fn f16_to_f32(bits: u16) -> f32 {
    let sign = if bits & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exponent = i32::from((bits >> 10) & 0x1f);
    let mantissa = f32::from(bits & 0x3ff);
    sign * match exponent {
        0 => mantissa * 2_f32.powi(-24),
        31 if mantissa == 0.0 => f32::INFINITY,
        31 => f32::NAN,
        _ => (1.0 + mantissa / 1024.0) * 2_f32.powi(exponent - 15),
    }
}

/// One image to read back
pub(crate) enum DumpSource<'a> {
    /// One eye of the environment depth and of the remembered playzone, through the dump pass
    Depth {
        eye: u32,
        depth_bind_group: &'a BindGroup,
        playzone_bind_group: &'a BindGroup,
        size: UVec2,
        file: String,
        /// None: there is no playzone
        playzone_file: Option<String>,
    },
    /// A Rgba8Unorm texture with COPY_SRC, as it is
    Rgba { texture: Texture, file: String },
    /// One eye of the room memory (`debug.alvr.mem_roll`), through `dump_main` of room_memory.wgsl
    Memory {
        eye: u32,
        pipeline: &'a RenderPipeline,
        depth_bind_group: &'a BindGroup,
        memory_bind_group: &'a BindGroup,
        /// Texels per side of the memory
        size: u32,
        file: String,
    },
}

#[derive(Clone)]
enum Content {
    Depth {
        file: String,
        playzone_file: Option<String>,
    },
    Rgba {
        file: String,
    },
    /// Two RGBA8 texels per memory texel: its four channels as 16 bit floats
    Memory {
        file: String,
    },
}

#[derive(Clone)]
struct Section {
    content: Content,
    size: UVec2,
    offset: u64,
    bytes_per_row: u32,
}

/// The images of a dump on their way from the GPU
pub(crate) struct DumpReadback {
    /// `debug.alvr.key_dump`
    pub n: i64,
    buffer: Buffer,
    sections: Vec<Section>,
    near: f32,
    far: f32,
    /// Name and content
    json: (String, String),
    status: Arc<OnceLock<Result<(), String>>>,
    start: Instant,
}

impl DumpReadback {
    /// Copies the images into one buffer. `start` must be called after the submission
    pub fn encode(
        device: &Device,
        encoder: &mut CommandEncoder,
        dump_pipeline: &RenderPipeline,
        n: i64,
        sources: Vec<DumpSource>,
        frame: &DepthFrame,
        json: (String, String),
    ) -> Self {
        let mut size = 0;
        let mut copies = vec![];
        for source in sources {
            let (texture, content) = match source {
                DumpSource::Depth {
                    eye,
                    depth_bind_group,
                    playzone_bind_group,
                    size,
                    file,
                    playzone_file,
                } => {
                    let target = device.create_texture(&TextureDescriptor {
                        label: Some("depth dump"),
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
                    stream::encode_depth_dump(
                        encoder,
                        dump_pipeline,
                        &target.create_view(&TextureViewDescriptor::default()),
                        depth_bind_group,
                        playzone_bind_group,
                        eye,
                    );
                    (
                        target,
                        Content::Depth {
                            file,
                            playzone_file,
                        },
                    )
                }
                DumpSource::Rgba { texture, file } => (texture, Content::Rgba { file }),
                DumpSource::Memory {
                    eye,
                    pipeline,
                    depth_bind_group,
                    memory_bind_group,
                    size,
                    file,
                } => {
                    let target = device.create_texture(&TextureDescriptor {
                        label: Some("room memory dump"),
                        size: Extent3d {
                            width: 2 * size,
                            height: size,
                            depth_or_array_layers: 1,
                        },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: TextureDimension::D2,
                        format: TextureFormat::Rgba8Unorm,
                        usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::COPY_SRC,
                        view_formats: &[],
                    });
                    crate::room_memory_gpu::encode_memory_pass(
                        encoder,
                        pipeline,
                        &target.create_view(&TextureViewDescriptor::default()),
                        depth_bind_group,
                        memory_bind_group,
                        eye,
                        crate::room_memory_gpu::MODE_UPDATE,
                    );
                    (target, Content::Memory { file })
                }
            };
            let extent = texture.size();
            let bytes_per_row = (extent.width * 4).next_multiple_of(COPY_BYTES_PER_ROW_ALIGNMENT);
            copies.push((
                texture,
                Section {
                    content,
                    size: UVec2::new(extent.width, extent.height),
                    offset: size,
                    bytes_per_row,
                },
            ));
            size += u64::from(bytes_per_row) * u64::from(extent.height);
        }

        let buffer = device.create_buffer(&BufferDescriptor {
            label: Some("depth dump"),
            size: size.max(4),
            usage: BufferUsages::COPY_DST | BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut sections = vec![];
        for (texture, section) in copies {
            encoder.copy_texture_to_buffer(
                texture.as_image_copy(),
                TexelCopyBufferInfo {
                    buffer: &buffer,
                    layout: TexelCopyBufferLayout {
                        offset: section.offset,
                        bytes_per_row: Some(section.bytes_per_row),
                        rows_per_image: Some(section.size.y),
                    },
                },
                Extent3d {
                    width: section.size.x,
                    height: section.size.y,
                    depth_or_array_layers: 1,
                },
            );
            sections.push(section);
        }

        Self {
            n,
            buffer,
            sections,
            near: frame.near,
            far: frame.far,
            json,
            status: Arc::new(OnceLock::new()),
            start: Instant::now(),
        }
    }

    /// Maps the buffer, after the submission of the copies
    pub fn start(self) -> Self {
        let status = Arc::clone(&self.status);
        self.buffer.map_async(MapMode::Read, .., move |result| {
            status.set(result.map_err(|e| e.to_string())).ok();
        });

        self
    }

    /// Ends the dump without reading the buffer
    pub fn fail(self, error: String) -> Self {
        self.status.set(Err(error)).ok();

        self
    }

    /// None while the buffer is not mapped. The device must be polled
    pub fn poll(&self) -> Option<Result<DepthDumpFiles, String>> {
        match self.status.get() {
            None if self.start.elapsed() > DEPTH_DUMP_TIMEOUT => {
                Some(Err("the GPU readback timed out".into()))
            }
            None => None,
            Some(Err(e)) => Some(Err(e.clone())),
            Some(Ok(())) => {
                // Only a copy here, the decoding is done by `into_files`
                let data = self.buffer.slice(..).get_mapped_range().to_vec();
                self.buffer.unmap();

                Some(Ok(DepthDumpFiles {
                    data,
                    sections: self.sections.clone(),
                    near: self.near,
                    far: self.far,
                    json: self.json.clone(),
                }))
            }
        }
    }
}

fn decode(data: &[u8], sections: &[Section], near: f32, far: f32) -> Vec<(String, Vec<u8>)> {
    let mut files = vec![];
    for section in sections {
        let texel_count = (section.size.x * section.size.y) as usize;
        let rows = (0..section.size.y).map(|y| {
            let start = (section.offset + u64::from(y) * u64::from(section.bytes_per_row)) as usize;
            &data[start..start + section.size.x as usize * 4]
        });
        match &section.content {
            Content::Depth {
                file,
                playzone_file,
            } => {
                let mut depth = Vec::with_capacity(texel_count * 4);
                let mut playzone = Vec::with_capacity(texel_count * 4);
                for texel in rows.flat_map(|row| row.chunks_exact(4)) {
                    let raw = u16::from_le_bytes([texel[0], texel[1]]);
                    depth.extend(depth_meters(raw, near, far).to_le_bytes());
                    let remembered = f16_to_f32(u16::from_le_bytes([texel[2], texel[3]]));
                    playzone.extend(remembered.to_le_bytes());
                }
                files.push((file.clone(), depth));
                if let Some(file) = playzone_file {
                    files.push((file.clone(), playzone));
                }
            }
            Content::Rgba { file } => files.push((file.clone(), rows.flatten().copied().collect())),
            Content::Memory { file } => files.push((file.clone(), decode_memory(rows))),
        }
    }

    files
}

/// The rows of the memory dump target as little-endian f32, 4 per memory texel (r, g, b, a of
/// room_memory.wgsl), rows in the order of the memory texture
fn decode_memory<'a>(rows: impl Iterator<Item = &'a [u8]>) -> Vec<u8> {
    let mut values = vec![];
    for half in rows.flat_map(|row| row.chunks_exact(2)) {
        let value = f16_to_f32(u16::from_le_bytes([half[0], half[1]]));
        values.extend(value.to_le_bytes());
    }

    values
}

/// Everything about the frame of a dump, for its JSON
pub(crate) struct DumpFrame<'a> {
    pub request: DepthDumpRequest,
    pub config: &'a DepthPassthroughConfig,
    /// Of the streamed image, see `depth_uniforms`
    pub stream_views: &'a [ViewParams; 2],
    pub input: &'a DepthRenderInput,
    pub frame: &'a DepthFrame,
    /// Depth cameras of the remembered playzone, and its age in seconds
    pub playzone: Option<([ViewParams; 2], f32)>,
    pub depth_size: UVec2,
    pub mask_size: UVec2,
    pub focus_size: UVec2,
    /// The room memory (`debug.alvr.mem_roll`) as JSON (`MemoryRecord::to_json`), with its files. None:
    /// off, or not anchored yet
    pub memory: Option<String>,
}

/// A depth camera: `pose`, `tangents` (left, right, down, up), `world_to_view`, `view_to_world`
pub(crate) fn camera(view: &ViewParams) -> String {
    let view_to_world = depth_mask::pose_to_mat4(view.pose);
    object([
        ("pose", pose(&view.pose)),
        (
            "tangents",
            numbers(&depth_mask::fov_tangents(view.fov).to_array()),
        ),
        (
            "world_to_view",
            numbers(&view_to_world.inverse().to_cols_array()),
        ),
        ("view_to_world", numbers(&view_to_world.to_cols_array())),
    ])
}

/// The shift of the depth around an arm, null: plain depth. Also in the display records of bursts
pub(crate) fn arm(arm: Option<&ArmMotion>) -> String {
    arm.map_or("null".into(), |arm| {
        object([
            ("wrist", numbers(&arm.wrist.to_array())),
            ("direction", numbers(&arm.direction.to_array())),
            ("displacement", numbers(&arm.displacement.to_array())),
            ("mode", arm.mode.to_string()),
            ("weight_len", number(arm.weight_len)),
            ("target_ns", arm.target_ns.to_string()),
            ("history_ns", arm.history_ns.to_string()),
            ("wrist_history", numbers(&arm.wrist_history.to_array())),
            ("extrapolated_mm", number(arm.extrapolated * 1000.0)),
        ])
    })
}

/// The wrist gap of an arm (`debug.alvr.arm_gap_mm`), null: off. Also in the display records of bursts
pub(crate) fn arm_gap(gap: Option<&ArmGap>) -> String {
    gap.map_or("null".into(), |gap| {
        object([
            ("wrist", numbers(&gap.wrist.to_array())),
            ("axis", numbers(&gap.axis.to_array())),
            ("length_mm", number(gap.length * 1000.0)),
        ])
    })
}

/// `debug.alvr.edge_cut` in percent as the mask used it (`DepthRenderInput::edge_cut`, 0: today's 20)
pub(crate) fn edge_cut_percent(centre: f32) -> f32 {
    if centre > 0.0 {
        (centre * 100.0).round()
    } else {
        20.0
    }
}

fn optional(value: Option<i64>) -> String {
    value.map_or("null".into(), |value| value.to_string())
}

fn size(size: UVec2) -> [(&'static str, String); 2] {
    [
        ("width", size.x.to_string()),
        ("height", size.y.to_string()),
    ]
}

/// The region the mask uses as JSON (`region` of the depth dump, also in the burst): the play area
/// placed from a marker replaces the one of the settings. center, yaw_deg, size and volume_to_world are
/// of its first box, "boxes" has all of them in the area frame (center, half extents in the frame of the
/// box, its yaw about +y, the 6 dome heights)
pub fn region_json(
    region_override: Option<&PlacedArea>,
    config: &DepthPassthroughConfig,
) -> String {
    let mut region = match (region_override, &config.region) {
        (Some(placed), _) => {
            let volume = placed.box_region(0);
            vec![
                ("mode", string("volume")),
                ("source", string("marker")),
                ("center", numbers(&volume.center().to_array())),
                ("yaw_deg", number(volume.yaw().to_degrees())),
                ("size", numbers(&volume.size().to_array())),
                (
                    "volume_to_world",
                    numbers(&volume.volume_to_world.to_cols_array()),
                ),
                (
                    "area_to_world",
                    numbers(&placed.area_to_world.to_cols_array()),
                ),
                (
                    "boxes",
                    array(placed.area.boxes().iter().map(|area_box| {
                        object([
                            ("center", numbers(&area_box.center.to_array())),
                            ("half", numbers(&area_box.half.to_array())),
                            ("yaw_deg", number(area_box.yaw.to_degrees())),
                            (
                                "domes",
                                numbers(&crate::Side::ALL.map(|side| area_box.dome_height(side))),
                            ),
                        ])
                    })),
                ),
            ]
        }
        (None, DepthMaskRegion::Distance { max_distance }) => vec![
            ("mode", string("distance")),
            ("max_distance", number(*max_distance)),
        ],
        (None, DepthMaskRegion::Volume(volume)) => vec![
            ("mode", string("volume")),
            (
                "center",
                numbers(&[volume.center_x, volume.center_y, volume.center_z]),
            ),
            ("yaw_deg", number(volume.yaw_deg)),
            (
                "size",
                numbers(&[volume.width, volume.height, volume.depth]),
            ),
        ],
    };
    region.push(("feathering", number(config.edge_feathering)));

    object(region)
}

impl DumpFrame<'_> {
    pub fn to_json(&self) -> String {
        let n = self.request.n;
        let config = self.config;
        let region = region_json(self.input.region_override.as_ref(), config);
        let debug_view = match config.debug_view {
            DepthPassthroughDebugView::Off => "off",
            DepthPassthroughDebugView::TintMask => "tint_mask",
            DepthPassthroughDebugView::ShowDepth => "show_depth",
        };

        let eyes = [0, 1].map(|eye| {
            let view = &self.stream_views[eye];
            object([
                (
                    "eye_position",
                    numbers(&self.input.eye_positions[eye].to_array()),
                ),
                (
                    "view",
                    object([
                        ("pose", pose(&view.pose)),
                        (
                            "tangents",
                            numbers(&depth_mask::fov_tangents(view.fov).to_array()),
                        ),
                    ]),
                ),
                (
                    "uv_to_world_dir",
                    numbers(&depth_mask::uv_to_world_dir(view).to_cols_array()),
                ),
                ("depth_camera", camera(&self.frame.depth_views[eye])),
                (
                    "playzone_camera",
                    self.playzone
                        .map_or("null".into(), |(views, _)| camera(&views[eye])),
                ),
                (
                    "files",
                    object([
                        ("depth", string(&file_name(n, "depth", eye))),
                        (
                            "playzone",
                            if self.playzone.is_some() {
                                string(&file_name(n, "playzone", eye))
                            } else {
                                "null".into()
                            },
                        ),
                        ("mask", string(&file_name(n, "mask", eye))),
                        ("focus", string(&file_name(n, "focus", eye))),
                    ]),
                ),
            ])
        });

        let hands = [0, 1].map(|i| {
            self.input.color_key.and_then(|key| key.hands[i]).map_or(
                "null".into(),
                |(shape, zones)| {
                    object([
                        ("bounds", numbers(&shape.bounds().to_array())),
                        (
                            "capsules",
                            array(shape.segments.map(|[start, end]| {
                                numbers(&[start.to_array(), end.to_array()].concat())
                            })),
                        ),
                        ("rim", number(zones.rim)),
                    ])
                },
            )
        });
        let arms = self
            .input
            .arms
            .each_ref()
            .map(|motion| arm(motion.as_ref()));
        let arm_gaps = self
            .input
            .arm_gaps
            .each_ref()
            .map(|gap| arm_gap(gap.as_ref()));

        let mut depth = size(self.depth_size).to_vec();
        depth.extend([
            ("near", number(self.frame.near)),
            // null: infinite
            (
                "far",
                number(if self.frame.far > self.frame.near {
                    self.frame.far
                } else {
                    f32::INFINITY
                }),
            ),
            ("flip_y", config.flip_depth_y.to_string()),
        ]);

        let mut fields = vec![
            ("n", n.to_string()),
            ("display_ns", self.request.display_ns.to_string()),
            ("depth_capture_ns", optional(self.request.depth_capture_ns)),
            ("now_ns", optional(self.request.now_ns)),
            ("depth", object(depth)),
            ("mask", object(size(self.mask_size))),
            ("focus", object(size(self.focus_size))),
            (
                "playzone",
                self.playzone
                    .map_or("null".into(), |(_, age)| object([("age_s", number(age))])),
            ),
            ("region", region),
            ("debug_view", string(debug_view)),
            ("color_key", self.input.color_key.is_some().to_string()),
            ("eyes", array(eyes)),
            ("hands", array(hands)),
            ("arms", array(arms)),
            // Steps 1 and 2 of docs/mask-v2-design.md
            ("edge_cut", number(edge_cut_percent(self.input.edge_cut))),
            ("arm_gaps", array(arm_gaps)),
        ];
        // U2 (`debug.alvr.lever_boxes`): only when a window is drawn, so that other dumps stay as they
        // were
        if self.input.lever_windows.iter().any(Option::is_some) {
            fields.push((
                "lever_windows",
                array(self.input.lever_windows.each_ref().map(lever_window)),
            ));
        }
        // U3 (`debug.alvr.mem_roll`): only with the memory, so that other dumps stay as they were
        if let Some(memory) = &self.memory {
            fields.push(("memory", memory.clone()));
        }

        object(fields)
    }
}

/// A lever window of the mask (stage space: the centre, the half extents in the frame of the box, its
/// yaw about +y, how open it is), null: none
fn lever_window(window: &Option<depth_mask::LeverWindow>) -> String {
    window.map_or("null".into(), |window| {
        object([
            ("center", numbers(&window.region.center().to_array())),
            ("half", numbers(&window.region.half_extents.to_array())),
            // + 0: an unturned box gives -0
            ("yaw_deg", number(window.region.yaw().to_degrees() + 0.0)),
            ("open", number(window.open)),
        ])
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::key_dump::tests::is_valid_json;
    use alvr_common::{
        Fov, Pose,
        glam::{Vec3, Vec4},
    };
    use alvr_session::settings_schema::Switch;

    #[test]
    fn half_floats_are_decoded() {
        for (bits, value) in [
            (0x0000, 0.0),
            (0x3c00, 1.0),
            (0x3800, 0.5),
            (0xc000, -2.0),
            (0x7bff, 65504.0),
            (0x0001, 2_f32.powi(-24)),
            (0x3e00, 1.5),
        ] {
            assert_eq!(f16_to_f32(bits), value, "{bits:#x}");
        }
        assert_eq!(f16_to_f32(0x7c00), f32::INFINITY);
        assert!(f16_to_f32(0x7e00).is_nan());
    }

    #[test]
    fn memory_dump_is_decoded() {
        // Two texels in a row: (0.5, 0, 1, 2.5) and (1.25, 1, 0, 0), as dump_main packs them: r, g in
        // the first output texel, b, a in the second
        let halves: [u16; 8] = [
            0x3800, 0x0000, 0x3c00, 0x4100, 0x3d00, 0x3c00, 0x0000, 0x0000,
        ];
        let row = halves
            .iter()
            .flat_map(|half| half.to_le_bytes())
            .collect::<Vec<_>>();
        let values = decode_memory([row.as_slice()].into_iter())
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect::<Vec<_>>();
        assert_eq!(values, [0.5, 0.0, 1.0, 2.5, 1.25, 1.0, 0.0, 0.0]);
        assert_eq!(file_name(7, "memory", 1), "alvr_dump_7_memory_right.f32");
    }

    #[test]
    fn depth_values_are_converted_to_meters() {
        for (near, far) in [(0.1, f32::INFINITY), (0.1, 20.0)] {
            for distance in [0.15, 0.6, 3.0] {
                let raw = (depth_mask::encode_depth(distance, near, far) * 65535.0).round() as u16;
                let meters = depth_meters(raw, near, far);
                assert!(
                    (meters - distance).abs() < 1e-3 * distance,
                    "{meters} {distance}"
                );
            }
            // No data
            assert_eq!(depth_meters(0, near, far), 0.0);
            assert_eq!(depth_meters(u16::MAX, near, far), 0.0);
        }
    }

    #[test]
    fn json_is_valid() {
        let config = DepthPassthroughConfig {
            region: DepthMaskRegion::Distance { max_distance: 0.9 },
            edge_feathering: 0.02,
            lighting: Switch::Disabled,
            debug_view: DepthPassthroughDebugView::TintMask,
            flip_depth_y: false,
        };
        let view = ViewParams {
            pose: Pose {
                orientation: Default::default(),
                position: Vec3::new(0.0, 1.6, 0.0),
            },
            fov: Fov {
                left: -0.8,
                right: 0.7,
                up: 0.75,
                down: -0.85,
            },
        };
        let frame = DepthFrame {
            swapchain_index: 1,
            near: 0.1,
            far: f32::INFINITY,
            depth_views: [view; 2],
        };
        let input = DepthRenderInput {
            frame: Some(frame),
            eye_positions: [Vec3::new(-0.03, 1.6, 0.0), Vec3::new(0.03, 1.6, 0.0)],
            color_key: Some(depth_mask::ColorKeyInput {
                hands: [
                    None,
                    Some((
                        depth_mask::HandShape::from_joints(&[Vec4::new(0.1, 1.2, -0.3, 0.01); 26]),
                        depth_mask::HandZones {
                            core: -0.004,
                            falloff: 0.004,
                            rim: 0.0,
                            solid: [0.0; depth_mask::HAND_SEGMENTS],
                            sweep: depth_mask::HandSweep::NONE,
                        },
                    )),
                ],
                leak: [0.0; 2],
                bridges: [None; 2],
            }),
            arms: [
                None,
                Some(ArmMotion {
                    wrist: Vec3::new(0.1, 1.2, -0.3),
                    direction: Vec3::NEG_Y,
                    displacement: Vec3::new(0.02, 0.0, 0.0),
                    weight_len: 0.25,
                    mode: 2,
                    target_ns: 1_234_527_890_123,
                    history_ns: 1_234_487_890_123,
                    wrist_history: Vec3::new(0.08, 1.2, -0.3),
                    extrapolated: 0.015625,
                }),
            ],
            arm_gaps: [
                None,
                Some(ArmGap {
                    wrist: Vec3::new(0.1, 1.2, -0.3),
                    axis: Vec3::NEG_Y,
                    length: 0.03,
                }),
            ],
            edge_cut: 0.65,
            lever_windows: [None; depth_mask::MAX_LEVER_WINDOWS],
            capture_playzone: false,
            playzone_view: false,
            memory: None,
            key_lower_rects: None,
            region_override: None,
            game_dim: 0.0,
            game_see: 0.0,
        };

        for playzone in [None, Some(([view; 2], 2.5))] {
            let json = DumpFrame {
                request: DepthDumpRequest {
                    n: 7,
                    display_ns: 1_234_567_890_123,
                    depth_capture_ns: None,
                    now_ns: Some(1_234_500_000_000),
                },
                config: &config,
                stream_views: &[view; 2],
                input: &input,
                frame: &frame,
                playzone,
                depth_size: UVec2::new(320, 320),
                mask_size: UVec2::new(412, 441),
                focus_size: UVec2::new(412, 441),
                memory: None,
            }
            .to_json();
            assert!(is_valid_json(&json), "{json}");
            assert!(json.contains(r#""depth_capture_ns":null"#), "{json}");
            assert!(json.contains(r#""far":null"#), "{json}");
            assert!(json.contains(r#""depth":"alvr_dump_7_depth_left.f32""#));
            assert!(json.contains(r#""focus":"alvr_dump_7_focus_right.rgba""#));
            assert_eq!(
                json.contains(r#""playzone":"alvr_dump_7_playzone_right.f32""#),
                playzone.is_some()
            );
            assert_eq!(json.contains(r#""playzone":null"#), playzone.is_none());
            assert!(json.contains(r#""hands":[null,{"bounds":"#), "{json}");
            assert!(
                json.contains(
                    r#""displacement":[0.02,0,0],"mode":2,"weight_len":0.25,"target_ns":1234527890123,"history_ns":1234487890123,"wrist_history":[0.08,1.2,-0.3],"extrapolated_mm":15.625}]"#
                ),
                "{json}"
            );
            assert!(json.contains(r#""arms":[null,{"wrist":"#), "{json}");
            assert!(
                json.contains(
                    r#""edge_cut":65,"arm_gaps":[null,{"wrist":[0.1,1.2,-0.3],"axis":[0,-1,0],"length_mm":30}]"#
                ),
                "{json}"
            );
        }
        // Today's edge reads as 20
        assert_eq!(edge_cut_percent(0.0), 20.0);

        // Without a lever window nothing is written; with one its box and how open it is
        let dump = |input: &DepthRenderInput| {
            DumpFrame {
                request: DepthDumpRequest {
                    n: 9,
                    display_ns: 1,
                    depth_capture_ns: None,
                    now_ns: None,
                },
                config: &config,
                stream_views: &[view; 2],
                input,
                frame: &frame,
                playzone: None,
                depth_size: UVec2::new(320, 320),
                mask_size: UVec2::new(412, 441),
                focus_size: UVec2::new(412, 441),
                memory: None,
            }
            .to_json()
        };
        assert!(!dump(&input).contains("lever_windows"));
        // Without the room memory neither
        assert!(!dump(&input).contains(r#""memory""#));
        let mut lever_input = input;
        lever_input.lever_windows[1] = Some(depth_mask::LeverWindow {
            region: depth_mask::RegionOverride {
                volume_to_world: alvr_common::glam::Mat4::from_translation(Vec3::new(
                    0.25, 0.5, -0.5,
                )),
                half_extents: Vec3::new(0.1, 0.15, 0.1),
            },
            open: 0.5,
        });
        let json = dump(&lever_input);
        assert!(is_valid_json(&json), "{json}");
        assert!(
            json.contains(
                r#""lever_windows":[null,{"center":[0.25,0.5,-0.5],"half":[0.1,0.15,0.1],"yaw_deg":0,"open":0.5},null,null]"#
            ),
            "{json}"
        );

        // A box placed from a marker is written instead of the region of the settings
        let input = DepthRenderInput {
            region_override: Some(crate::PlacedArea::from(depth_mask::RegionOverride {
                volume_to_world: alvr_common::glam::Mat4::from_rotation_translation(
                    alvr_common::glam::Quat::from_rotation_y(90_f32.to_radians()),
                    Vec3::new(0.25, 0.9, -0.5),
                ),
                half_extents: Vec3::new(0.6, 0.45, 0.5),
            })),
            ..input
        };
        let json = DumpFrame {
            request: DepthDumpRequest {
                n: 8,
                display_ns: 1,
                depth_capture_ns: None,
                now_ns: None,
            },
            config: &config,
            stream_views: &[view; 2],
            input: &input,
            frame: &frame,
            playzone: None,
            depth_size: UVec2::new(320, 320),
            mask_size: UVec2::new(412, 441),
            focus_size: UVec2::new(412, 441),
            memory: None,
        }
        .to_json();
        assert!(is_valid_json(&json), "{json}");
        assert!(
            json.contains(
                r#""region":{"mode":"volume","source":"marker","center":[0.25,0.9,-0.5]"#
            ),
            "{json}"
        );
        assert!(!json.contains("max_distance"), "{json}");
        assert!(json.contains(r#""size":[1.2,0.9,1]"#), "{json}");
        assert!(
            json.contains(
                r#""boxes":[{"center":[0,0,0],"half":[0.6,0.45,0.5],"yaw_deg":0,"domes":[0,0,0,0,0,0]}]"#
            ),
            "{json}"
        );
    }
}
