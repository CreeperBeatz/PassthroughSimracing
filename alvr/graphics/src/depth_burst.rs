//! `debug.alvr.burst` of the client: every new environment depth frame of a few seconds (read back
//! from the GPU like the depth dump), one small record per display frame (poses, hands, which depth
//! frame the mask used) and the camera images of the key learner. Kept in RAM during the burst and
//! written by a thread after it: one folder `alvr_burst_<n>` with an index JSON that lists every record
//! in time order, and binary files for the images. The format is in the knowledge base.

use crate::{
    ArmGap, ArmMotion, DumpJoint, HandShape, HandZones,
    depth_dump::{self, DEPTH_DUMP_TIMEOUT},
    depth_mask::{self, DepthFrame},
    key_dump::{array, number, numbers, object, pose, string},
    motion_cloud::CloudRecord,
    room_memory::MemoryRecord,
    stream,
};
use alvr_common::{
    Pose, ViewParams,
    glam::{UVec2, Vec3},
    parking_lot::Mutex,
};
use std::{
    collections::VecDeque,
    fs, io,
    path::Path,
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};
use wgpu::{
    BindGroup, Buffer, BufferDescriptor, BufferUsages, COPY_BYTES_PER_ROW_ALIGNMENT,
    CommandEncoder, Device, Extent3d, MapMode, RenderPipeline, TexelCopyBufferInfo,
    TexelCopyBufferLayout, Texture, TextureDescriptor, TextureDimension, TextureFormat,
    TextureUsages, TextureViewDescriptor,
};

/// `debug.alvr.burst_ms`: default and limits
pub const BURST_DEFAULT_MS: i64 = 3000;
pub const BURST_MIN_MS: i64 = 100;
pub const BURST_MAX_MS: i64 = 6000;
/// Records of a burst at most (6 s: about 180 depth frames, 432 display frames at 72 Hz and 720 at
/// 120 Hz, 100 camera passes)
pub const BURST_MAX_DEPTH: usize = 240;
pub const BURST_MAX_DISPLAY: usize = 720;
pub const BURST_MAX_CAMERA: usize = 200;

/// Depth readbacks on their way from the GPU at most. One takes a frame or two, a depth frame comes
/// every 2 to 3 display frames
const MAX_IN_FLIGHT: usize = 6;

const EYES: [&str; 2] = ["left", "right"];

/// A new depth frame as the runtime delivered it
pub struct BurstDepth {
    pub frame: DepthFrame,
    /// When the image was captured (XrTime, CLOCK_MONOTONIC on Quest). None: not reported
    pub capture_ns: Option<i64>,
    /// Predicted display time of the frame it came with first (passed to the acquire). For the first
    /// depth frame of a burst, which may have come before the start: of the first display frame
    pub display_ns: i64,
    /// When the app got it, same clock (first depth frame of a burst: the start)
    pub now_ns: i64,
    /// The 16 bit depth values as in the texture, the rows of the left eye then of the right one.
    /// None: not read back (yet)
    pub raw: Option<Vec<u16>>,
    /// Why it was not read back
    pub error: Option<String>,
}

/// A hand as located for a record (stage space): its 26 joints
pub type BurstHand = [DumpJoint; 26];

/// One display frame
pub struct BurstDisplay {
    /// Predicted display time
    pub display_ns: i64,
    /// When it was rendered
    pub now_ns: i64,
    /// Index of the depth frame the mask used. None: no depth image, or one past the depth limit
    pub depth: Option<usize>,
    /// Index of the region record that applies
    pub region: Option<usize>,
    /// The eyes located at the display time (stage space): the mask casts its rays from their
    /// positions
    pub views: [ViewParams; 2],
    /// Of the streamed image: the directions of the pixels of the mask
    pub stream_views: [ViewParams; 2],
    pub eye_positions: [Vec3; 2],
    /// When the hands were located: the display time minus `debug.alvr.key_delay`
    pub hand_ns: i64,
    pub hands: [Option<BurstHand>; 2],
    /// The capsules the mask used, with the rim (None: no hand in the mask, or no color key)
    pub capsules: [Option<(HandShape, HandZones)>; 2],
    /// The shift of the depth around each arm. None: plain depth
    pub arms: [Option<ArmMotion>; 2],
    /// The wrist gap of each arm (`debug.alvr.arm_gap_mm`). None: off
    pub arm_gaps: [Option<ArmGap>; 2],
    /// The wrist gap each arm was set to (mm, `debug.alvr.arm_gap_mm` where `arm_gap_hands` selects the
    /// arm, 0: off). Set but `arm_gaps` None: the joints were not located in this frame
    pub arm_gap_set_mm: [f32; 2],
    /// `DepthRenderInput::edge_cut` (0: today's)
    pub edge_cut: f32,
    /// The arm's own sample history of each hand (step 1b of the arm plan)
    pub arm_samples: [BurstArmSamples; 2],
    /// The room memory (`debug.alvr.mem_roll`, U3 of design/universal/final.md): its camera, anchors and
    /// last shares as the renderer left them after this frame. None: off
    pub memory: Option<MemoryRecord>,
    /// The depth cloud (`debug.alvr.cloud`, design/depthcloud/final.md): its properties, whether it was
    /// shown, the field of each eye as the renderer left it after this frame. None: off
    pub cloud: Option<CloudRecord>,
}

/// A hand sample of the arm's own history (step 1b of the arm plan)
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BurstArmSample {
    /// When it shows the hand (XrTime)
    pub time_ns: i64,
    /// When the app got it first
    pub seen_ns: i64,
    pub wrist: Vec3,
    /// `time_ns` is the runtime's capture time (XR_META_hand_tracking_unextrapolated_poses), else the time
    /// seen minus `debug.alvr.arm_sample_age_ms`
    pub from_capture_time: bool,
}

/// The arm's own sample history of a hand in a display frame
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BurstArmSamples {
    /// The history was emptied in this frame (the hand was lost)
    pub reset: bool,
    /// The samples first seen in this frame (each sample is in one record only). The first record of a
    /// burst has every sample the history held then (the samples kept when the burst started)
    pub new: Vec<BurstArmSample>,
    /// The wrist the history gave for the depth's capture time, where the arm is in the depth. None: the
    /// arm doesn't use `arm_shift 2`, the history had no wrist at that time (a buffer miss), or the arm fell
    /// back to plain depth (a displacement over 25 cm)
    pub capture_wrist: Option<Vec3>,
    /// `debug.alvr.arm_estimator` (0 hold, 1 line, 2 parabola) of an arm in mode 2, else -1
    pub estimator: i64,
}

impl BurstArmSamples {
    fn to_json(&self) -> String {
        object([
            ("reset", self.reset.to_string()),
            (
                "new",
                array(self.new.iter().map(|sample| {
                    object([
                        ("time_ns", sample.time_ns.to_string()),
                        ("seen_ns", sample.seen_ns.to_string()),
                        ("wrist", numbers(&sample.wrist.to_array())),
                        ("capture_time", sample.from_capture_time.to_string()),
                    ])
                })),
            ),
            (
                "capture_wrist",
                self.capture_wrist
                    .map_or("null".into(), |wrist| numbers(&wrist.to_array())),
            ),
            ("estimator", self.estimator.to_string()),
        ])
    }
}

/// One camera image of the key learner
pub struct BurstCamera {
    /// When the pass started (CLOCK_MONOTONIC)
    pub now_ns: i64,
    pub image_ns: i64,
    pub width: u32,
    pub height: u32,
    /// fx, fy, cx, cy in pixels
    pub intrinsics: [f32; 4],
    /// Stage space at the time of the image, the camera looks at +z with +y down
    pub camera: Pose,
    pub head: Pose,
    /// width x height x 3 bytes, row major, as the learner converts it
    pub rgb: Vec<u8>,
    /// Located at the time of the image
    pub hands: [Option<BurstHand>; 2],
}

/// The colour table of the key as it was uploaded to the compositor last, at the start of a burst. The
/// bursts of 2026-11-24 had none, so offline tests had to invent one
#[derive(Clone)]
pub struct BurstLut {
    /// Counts the learned tables uploaded since the stream started (from 1)
    pub version: u32,
    /// When it was uploaded (XrTime)
    pub published_ns: i64,
    /// 32 x 32 x 32 RGBA bytes, indexed `[b][g][r]`, as uploaded
    pub data: Vec<u8>,
}

/// The region of the mask, each time it changes
pub struct BurstRegion {
    pub now_ns: i64,
    /// See `region_json`
    pub json: String,
}

/// Three clocks of the headset read back to back, at the start and at the end of a burst, so that logs
/// written on other machines (the laptop's logcat in wall time, a rig PC's input log on its own wall clock,
/// NTP: its error against the headset is not covered) can be put on the time of the burst records
/// (XrTime). Read in this order:
/// `CLOCK_MONOTONIC`, `CLOCK_REALTIME`, the runtime's XrTime now, `CLOCK_MONOTONIC` again; `read_ns` is
/// the time between the two monotonic reads, so every value lies within it of the others. The client's
/// XrTime "now" is the runtime's conversion of a `CLOCK_MONOTONIC` read (openxr crate,
/// `xrConvertTimespecTimeToTimeKHR`): on Quest XrTime is expected to equal `CLOCK_MONOTONIC` [assumed,
/// the pair shows it]. `CLOCK_REALTIME` is the headset's wall clock as NTP sets it, not the PC's
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BurstClocks {
    /// `CLOCK_REALTIME`: ns since 1970-01-01 UTC
    pub realtime_ns: i64,
    /// `CLOCK_MONOTONIC` (None: not readable on this platform, the PC builds of the tests)
    pub monotonic_ns: Option<i64>,
    /// XrTime now, the clock of every `*_ns` of the burst (None: the runtime gave no time, or the stream
    /// stopped before the end: no session to ask)
    pub xr_ns: Option<i64>,
    /// ns between the first and the last read (None without `CLOCK_MONOTONIC`)
    pub read_ns: Option<i64>,
}

impl BurstClocks {
    fn to_json(self) -> String {
        let optional = |value: Option<i64>| value.map_or("null".into(), |value| value.to_string());
        object([
            ("realtime_ns", self.realtime_ns.to_string()),
            ("monotonic_ns", optional(self.monotonic_ns)),
            ("xr_ns", optional(self.xr_ns)),
            ("read_ns", optional(self.read_ns)),
        ])
    }

    /// For log lines: `realtime_ns=<n> monotonic_ns=<n> xr_ns=<n> read_ns=<n>` (`null`: not read)
    pub fn text(&self) -> String {
        let optional = |value: Option<i64>| value.map_or("null".into(), |value| value.to_string());
        format!(
            "realtime_ns={} monotonic_ns={} xr_ns={} read_ns={}",
            self.realtime_ns,
            optional(self.monotonic_ns),
            optional(self.xr_ns),
            optional(self.read_ns)
        )
    }
}

/// The camera images of a burst, filled by the key learner on its own thread
pub struct CameraBurstSink {
    until: Instant,
    records: Vec<BurstCamera>,
}

impl CameraBurstSink {
    pub fn new(duration: Duration) -> Self {
        Self {
            until: Instant::now() + duration,
            records: Vec::new(),
        }
    }

    /// Whether the burst still takes images
    pub fn wants(&self) -> bool {
        Instant::now() < self.until && self.records.len() < BURST_MAX_CAMERA
    }

    pub fn push(&mut self, record: BurstCamera) {
        if self.wants() {
            self.records.push(record);
        }
    }

    pub fn into_records(self) -> Vec<BurstCamera> {
        self.records
    }
}

/// Shared between the render thread (starts and ends the burst) and the key learner
pub type SharedCameraBurst = Arc<Mutex<Option<CameraBurstSink>>>;

/// A whole burst, ready to be written
pub struct Burst {
    pub n: i64,
    pub duration_ms: i64,
    pub start_ns: i64,
    pub end_ns: i64,
    /// The clocks at the start (read with `start_ns`: its `xr_ns` is `start_ns`) and at the end (with
    /// `end_ns`), None: not read
    pub clocks: [Option<BurstClocks>; 2],
    pub depth_size: UVec2,
    pub flip_y: bool,
    /// The depth dump written at the start of the burst (its JSON), None: none
    pub dump: Option<String>,
    /// The learned colour table of the key at the start, None: none learned (the key off, no `auto_key`,
    /// or nothing learned yet: the table of the file is shown)
    pub key_lut: Option<BurstLut>,
    pub depth: Vec<BurstDepth>,
    /// New depth frames that were not recorded because the limit was reached. A frame that finds
    /// too many readbacks on their way is recorded, with that `error` and without images
    pub depth_skipped: u32,
    pub display: Vec<BurstDisplay>,
    pub camera: Vec<BurstCamera>,
    pub regions: Vec<BurstRegion>,
}

/// Name of the folder of burst `n`
pub fn burst_dir_name(n: i64) -> String {
    format!("alvr_burst_{n}")
}

fn index_name(n: i64) -> String {
    format!("alvr_burst_{n}.json")
}

fn depth_file_name(n: i64, index: usize, eye: usize) -> String {
    format!("alvr_burst_{n}_depth_{index:04}_{}.f32", EYES[eye])
}

fn camera_file_name(n: i64, index: usize) -> String {
    format!("alvr_burst_{n}_camera_{index:04}.rgb")
}

// Named like the files of the depth dump of the start, which it belongs to
fn key_lut_file_name(n: i64) -> String {
    format!("alvr_dump_{n}_key.lut")
}

fn optional(value: Option<impl ToString>) -> String {
    value.map_or("null".into(), |value| value.to_string())
}

fn view(view: &ViewParams) -> String {
    object([
        ("pose", pose(&view.pose)),
        (
            "tangents",
            numbers(&depth_mask::fov_tangents(view.fov).to_array()),
        ),
    ])
}

// `joints`: 26 x (x, y, z, radius), `tracked`: POSITION_TRACKED, `velocities`: 26 x (x, y, z), null
// without LINEAR_VALID
fn hand(hand: &Option<BurstHand>) -> String {
    hand.as_ref().map_or("null".into(), |joints| {
        object([
            (
                "joints",
                array(
                    joints
                        .iter()
                        .map(|joint| numbers(&joint.position.extend(joint.radius).to_array())),
                ),
            ),
            (
                "tracked",
                array(joints.iter().map(|joint| joint.tracked.to_string())),
            ),
            (
                "velocities",
                array(joints.iter().map(|joint| {
                    if joint.velocity_valid {
                        numbers(&joint.velocity.to_array())
                    } else {
                        "null".into()
                    }
                })),
            ),
        ])
    })
}

impl BurstDepth {
    fn to_json(&self, n: i64, index: usize) -> String {
        let frame = &self.frame;
        let files = if self.raw.is_some() {
            array([0, 1].map(|eye| string(&depth_file_name(n, index, eye))))
        } else {
            "null".into()
        };
        object([
            ("kind", string("depth")),
            ("t_ns", self.now_ns.to_string()),
            ("index", index.to_string()),
            ("capture_ns", optional(self.capture_ns)),
            ("display_ns", self.display_ns.to_string()),
            ("now_ns", self.now_ns.to_string()),
            ("swapchain_index", frame.swapchain_index.to_string()),
            ("near", number(frame.near)),
            // null: infinite
            (
                "far",
                number(if frame.far > frame.near {
                    frame.far
                } else {
                    f32::INFINITY
                }),
            ),
            (
                "eyes",
                array(frame.depth_views.iter().map(depth_dump::camera)),
            ),
            ("files", files),
            ("error", self.error.as_deref().map_or("null".into(), string)),
        ])
    }
}

impl BurstDisplay {
    fn to_json(&self, index: usize) -> String {
        let capsules = self.capsules.map(|hand| {
            hand.map_or("null".into(), |(shape, zones)| {
                object([
                    (
                        "capsules",
                        array(shape.segments.map(|[start, end]| {
                            numbers(&[start.to_array(), end.to_array()].concat())
                        })),
                    ),
                    ("rim", number(zones.rim)),
                ])
            })
        });
        let arms = self
            .arms
            .each_ref()
            .map(|arm| depth_dump::arm(arm.as_ref()));
        let arm_gaps = self
            .arm_gaps
            .each_ref()
            .map(|gap| depth_dump::arm_gap(gap.as_ref()));
        object([
            ("kind", string("display")),
            ("t_ns", self.now_ns.to_string()),
            ("index", index.to_string()),
            ("display_ns", self.display_ns.to_string()),
            ("now_ns", self.now_ns.to_string()),
            ("depth", optional(self.depth)),
            ("region", optional(self.region)),
            ("views", array(self.views.iter().map(view))),
            ("stream_views", array(self.stream_views.iter().map(view))),
            (
                "eye_positions",
                array(self.eye_positions.map(|p| numbers(&p.to_array()))),
            ),
            ("hand_ns", self.hand_ns.to_string()),
            ("hands", array(self.hands.iter().map(hand))),
            ("capsules", array(capsules)),
            ("arms", array(arms)),
            (
                "edge_cut",
                number(depth_dump::edge_cut_percent(self.edge_cut)),
            ),
            ("arm_gaps", array(arm_gaps)),
            ("arm_gap_set_mm", numbers(&self.arm_gap_set_mm)),
            (
                "arm_samples",
                array(self.arm_samples.iter().map(BurstArmSamples::to_json)),
            ),
            (
                "memory",
                self.memory
                    .as_ref()
                    .map_or("null".into(), MemoryRecord::to_json),
            ),
            (
                "cloud",
                self.cloud
                    .as_ref()
                    .map_or("null".into(), CloudRecord::to_json),
            ),
        ])
    }
}

impl BurstCamera {
    fn to_json(&self, n: i64, index: usize) -> String {
        let [fx, fy, cx, cy] = self.intrinsics;
        object([
            ("kind", string("camera")),
            ("t_ns", self.now_ns.to_string()),
            ("index", index.to_string()),
            ("image_ns", self.image_ns.to_string()),
            ("now_ns", self.now_ns.to_string()),
            ("width", self.width.to_string()),
            ("height", self.height.to_string()),
            ("fx", number(fx)),
            ("fy", number(fy)),
            ("cx", number(cx)),
            ("cy", number(cy)),
            ("pose", pose(&self.camera)),
            ("head", pose(&self.head)),
            ("file", string(&camera_file_name(n, index))),
            ("hands", array(self.hands.iter().map(hand))),
        ])
    }
}

impl Burst {
    pub fn depth_read_count(&self) -> usize {
        self.depth
            .iter()
            .filter(|depth| depth.raw.is_some())
            .count()
    }

    /// Bytes of the files, roughly (the index JSON is not counted)
    pub fn image_bytes(&self) -> usize {
        let depth =
            self.depth_read_count() * 2 * (self.depth_size.x * self.depth_size.y) as usize * 4;
        let camera = self.camera.iter().map(|c| c.rgb.len()).sum::<usize>();

        depth + camera
    }

    /// The index: the burst and every record in time order (`t_ns`, when the app got it)
    pub fn index_json(&self) -> String {
        let n = self.n;
        // Time, kind (a depth frame before the display frame that got it at the same time), JSON
        let mut records = Vec::with_capacity(
            self.depth.len() + self.display.len() + self.camera.len() + self.regions.len(),
        );
        for (index, region) in self.regions.iter().enumerate() {
            records.push((
                region.now_ns,
                0,
                object([
                    ("kind", string("region")),
                    ("t_ns", region.now_ns.to_string()),
                    ("index", index.to_string()),
                    ("region", region.json.clone()),
                ]),
            ));
        }
        for (index, depth) in self.depth.iter().enumerate() {
            records.push((depth.now_ns, 1, depth.to_json(n, index)));
        }
        for (index, display) in self.display.iter().enumerate() {
            records.push((display.now_ns, 2, display.to_json(index)));
        }
        for (index, camera) in self.camera.iter().enumerate() {
            records.push((camera.now_ns, 3, camera.to_json(n, index)));
        }
        records.sort_by_key(|(time, kind, _)| (*time, *kind));

        let camera = self.camera.first().map_or("null".into(), |camera| {
            object([
                ("width", camera.width.to_string()),
                ("height", camera.height.to_string()),
            ])
        });
        let key_lut = self.key_lut.as_ref().map_or("null".into(), |lut| {
            object([
                ("file", string(&key_lut_file_name(n))),
                ("version", lut.version.to_string()),
                ("published_ns", lut.published_ns.to_string()),
                ("bytes", lut.data.len().to_string()),
            ])
        });
        // Version 2: the arms of the display records have the fields of step 1 of the arm plan, and
        // `key_lut` is new. Version 3: the display records have `edge_cut`, `arm_gaps` and `arm_gap_set_mm` (steps 1 and 2
        // of docs/mask-v2-design.md). Version 4: the display records have `arm_samples` (step 1b of the arm
        // plan: the arm's own sample history; `history_ns` / `wrist_history` of an arm in mode 2 are its
        // newest sample). Version 5: `clocks`, the headset's wall clock, monotonic clock and XrTime read
        // back to back at the start and at the end (`BurstClocks`). Version 6: the display records have
        // `memory` (U3 of design/universal/final.md, `debug.alvr.mem_roll`; null when it is off). Version 7:
        // the display records have `cloud` (the depth cloud, `debug.alvr.cloud`; null when it is off)
        let clocks =
            |clocks: Option<BurstClocks>| clocks.map_or("null".into(), BurstClocks::to_json);
        object([
            ("format", string("alvr_burst")),
            ("version", "7".into()),
            ("n", n.to_string()),
            ("duration_ms", self.duration_ms.to_string()),
            ("start_ns", self.start_ns.to_string()),
            ("end_ns", self.end_ns.to_string()),
            (
                "clocks",
                object([
                    ("start", clocks(self.clocks[0])),
                    ("end", clocks(self.clocks[1])),
                ]),
            ),
            (
                "depth",
                object([
                    ("width", self.depth_size.x.to_string()),
                    ("height", self.depth_size.y.to_string()),
                    ("flip_y", self.flip_y.to_string()),
                ]),
            ),
            ("camera", camera),
            (
                "counts",
                object([
                    ("depth", self.depth.len().to_string()),
                    ("depth_read", self.depth_read_count().to_string()),
                    ("depth_skipped", self.depth_skipped.to_string()),
                    ("display", self.display.len().to_string()),
                    ("camera", self.camera.len().to_string()),
                    ("region", self.regions.len().to_string()),
                ]),
            ),
            ("dump", self.dump.as_deref().map_or("null".into(), string)),
            ("key_lut", key_lut),
            (
                "records",
                array(records.into_iter().map(|(_, _, json)| json)),
            ),
        ])
    }

    /// Writes the files into `dir` (created if needed), one at a time: the depth of each frame and eye
    /// in meters (little-endian f32, like the depth dump), the camera images, the index last. Files of
    /// an older burst with the same `n` are overwritten by name, the index lists the ones of this one.
    /// Returns the number of files and of bytes
    pub fn write_to(&self, dir: &Path) -> io::Result<(usize, usize)> {
        fs::create_dir_all(dir)?;
        let (mut files, mut bytes) = (0, 0);
        let texels = (self.depth_size.x * self.depth_size.y) as usize;
        for (index, depth) in self.depth.iter().enumerate() {
            let Some(raw) = &depth.raw else {
                continue;
            };
            for (eye, values) in raw.chunks_exact(texels).take(2).enumerate() {
                let mut data = Vec::with_capacity(texels * 4);
                for value in values {
                    data.extend(
                        depth_dump::depth_meters(*value, depth.frame.near, depth.frame.far)
                            .to_le_bytes(),
                    );
                }
                fs::write(dir.join(depth_file_name(self.n, index, eye)), &data)?;
                files += 1;
                bytes += data.len();
            }
        }
        for (index, camera) in self.camera.iter().enumerate() {
            fs::write(dir.join(camera_file_name(self.n, index)), &camera.rgb)?;
            files += 1;
            bytes += camera.rgb.len();
        }
        if let Some(lut) = &self.key_lut {
            fs::write(dir.join(key_lut_file_name(self.n)), &lut.data)?;
            files += 1;
            bytes += lut.data.len();
        }
        let index = self.index_json();
        fs::write(dir.join(index_name(self.n)), &index)?;

        Ok((files + 1, bytes + index.len()))
    }
}

struct InFlight {
    token: u32,
    buffer: Buffer,
    size: UVec2,
    bytes_per_row: u32,
    status: Arc<OnceLock<Result<(), String>>>,
    start: Instant,
}

/// The depth readbacks of a burst: both eyes of a depth frame through the pass of the depth dump into
/// two RGBA8 textures (kept), then into a mappable buffer (a few, reused). Per depth frame: two passes
/// of 320 x 320 texels, a copy of 800 KB on the GPU, and on the render thread 800 KB read from mapped
/// memory into 400 KB of 16 bit values once it is mapped (the conversion to meters is done by the
/// writer)
#[derive(Default)]
pub(crate) struct BurstReadbacks {
    targets: Vec<Texture>,
    free: Vec<(Buffer, u64)>,
    // Encoded in this frame, mapped after the submission
    encoded: Option<InFlight>,
    in_flight: VecDeque<InFlight>,
    // Answers that are known before the GPU
    failed: Vec<(u32, String)>,
}

impl BurstReadbacks {
    /// A request that could not be encoded (no depth frame in the renderer)
    pub fn fail(&mut self, token: u32, error: &str) {
        self.failed.push((token, error.into()));
    }

    /// Copies both eyes of the depth of this frame. Returns false if too many readbacks are on their
    /// way (the request is answered with an error)
    #[expect(clippy::too_many_arguments)]
    pub fn encode(
        &mut self,
        device: &Device,
        encoder: &mut CommandEncoder,
        pipeline: &RenderPipeline,
        depth_bind_group: &BindGroup,
        playzone_bind_groups: &[BindGroup; 2],
        size: UVec2,
        token: u32,
    ) -> bool {
        if self.in_flight.len() + usize::from(self.encoded.is_some()) >= MAX_IN_FLIGHT {
            self.fail(token, "too many readbacks on their way");
            return false;
        }
        if self
            .targets
            .first()
            .is_none_or(|target| target.size().width != size.x || target.size().height != size.y)
        {
            self.targets = (0..2)
                .map(|_| {
                    device.create_texture(&TextureDescriptor {
                        label: Some("depth burst"),
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
                    })
                })
                .collect();
            self.free.clear();
        }

        let bytes_per_row = (size.x * 4).next_multiple_of(COPY_BYTES_PER_ROW_ALIGNMENT);
        let eye_bytes = u64::from(bytes_per_row) * u64::from(size.y);
        let buffer = match self.free.pop() {
            Some((buffer, bytes)) if bytes == 2 * eye_bytes => buffer,
            _ => device.create_buffer(&BufferDescriptor {
                label: Some("depth burst"),
                size: 2 * eye_bytes,
                usage: BufferUsages::COPY_DST | BufferUsages::MAP_READ,
                mapped_at_creation: false,
            }),
        };
        for (eye, target) in self.targets.iter().enumerate() {
            stream::encode_depth_dump(
                encoder,
                pipeline,
                &target.create_view(&TextureViewDescriptor::default()),
                depth_bind_group,
                &playzone_bind_groups[eye],
                eye as u32,
            );
            encoder.copy_texture_to_buffer(
                target.as_image_copy(),
                TexelCopyBufferInfo {
                    buffer: &buffer,
                    layout: TexelCopyBufferLayout {
                        offset: eye as u64 * eye_bytes,
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
        }
        self.encoded = Some(InFlight {
            token,
            buffer,
            size,
            bytes_per_row,
            status: Arc::new(OnceLock::new()),
            start: Instant::now(),
        });

        true
    }

    /// After the submission: maps the buffer of this frame, or fails it (a GL error)
    pub fn submitted(&mut self, gl_error: Option<&str>) {
        let Some(readback) = self.encoded.take() else {
            return;
        };
        if let Some(error) = gl_error {
            self.failed
                .push((readback.token, format!("GL error {error}")));
            return;
        }
        let status = Arc::clone(&readback.status);
        readback.buffer.map_async(MapMode::Read, .., move |result| {
            status.set(result.map_err(|e| e.to_string())).ok();
        });
        self.in_flight.push_back(readback);
    }

    pub fn is_waiting(&self) -> bool {
        !self.in_flight.is_empty()
    }

    /// The readbacks that are done or failed, with their token. The device must be polled before
    pub fn poll(&mut self) -> Vec<(u32, Result<Vec<u16>, String>)> {
        let mut done: Vec<_> = self
            .failed
            .drain(..)
            .map(|(token, error)| (token, Err(error)))
            .collect();
        let mut waiting = VecDeque::with_capacity(self.in_flight.len());
        while let Some(readback) = self.in_flight.pop_front() {
            match readback.status.get() {
                None if readback.start.elapsed() > DEPTH_DUMP_TIMEOUT => {
                    // Dropped with its buffer
                    done.push((readback.token, Err("the GPU readback timed out".into())));
                }
                None => waiting.push_back(readback),
                Some(Err(e)) => done.push((readback.token, Err(e.clone()))),
                Some(Ok(())) => {
                    let raw = {
                        let mapped = readback.buffer.slice(..).get_mapped_range();
                        decode_raw(&mapped, readback.size, readback.bytes_per_row)
                    };
                    readback.buffer.unmap();
                    let bytes = readback.buffer.size();
                    if self.free.len() < MAX_IN_FLIGHT {
                        self.free.push((readback.buffer, bytes));
                    }
                    done.push((readback.token, Ok(raw)));
                }
            }
        }
        self.in_flight = waiting;

        done
    }
}

// The depth values (r: low byte, g: high byte of each texel, see depth_dump.wgsl) of both eyes
fn decode_raw(data: &[u8], size: UVec2, bytes_per_row: u32) -> Vec<u16> {
    let width = size.x as usize;
    let mut raw = Vec::with_capacity(2 * width * size.y as usize);
    for row in 0..2 * size.y as usize {
        let start = row * bytes_per_row as usize;
        raw.extend(
            data[start..start + width * 4]
                .chunks_exact(4)
                .map(|texel| u16::from_le_bytes([texel[0], texel[1]])),
        );
    }

    raw
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::key_dump::tests::is_valid_json;
    use alvr_common::{
        Fov,
        glam::{Quat, Vec4},
    };

    fn depth_view(x: f32) -> ViewParams {
        ViewParams {
            pose: Pose {
                orientation: Quat::from_rotation_x(-0.3),
                position: Vec3::new(x, 1.2, 0.0),
            },
            fov: Fov {
                left: -1.0,
                right: 1.0,
                up: 1.0,
                down: -1.0,
            },
        }
    }

    fn joints(offset: f32) -> BurstHand {
        std::array::from_fn(|i| DumpJoint {
            position: Vec3::new(offset + i as f32 * 0.01, 1.0, -0.4),
            radius: 0.01,
            tracked: i != 5,
            velocity: Vec3::new(0.1, 0.0, 0.0),
            velocity_valid: i != 7,
        })
    }

    /// A burst of `frames` depth frames at 30 Hz with display frames at 72 Hz and camera images at 16
    /// Hz, every depth texel at `distance(frame, eye, x, y)`
    pub(crate) fn synthetic_burst(n: i64, size: UVec2, frames: usize) -> Burst {
        let (near, far) = (0.1, f32::INFINITY);
        let start_ns = 1_000_000_000_000;
        let depth_period = 33_333_333;
        let display_period = 13_888_889;
        let depth = (0..frames)
            .map(|i| {
                let capture_ns = start_ns + i as i64 * depth_period;
                let raw = (0..2)
                    .flat_map(|eye| {
                        (0..size.y).flat_map(move |y| {
                            (0..size.x).map(move |x| {
                                let distance = synthetic_distance(i, eye, x, y);
                                (depth_mask::encode_depth(distance, near, far) * 65535.0).round()
                                    as u16
                            })
                        })
                    })
                    .collect();
                BurstDepth {
                    frame: DepthFrame {
                        swapchain_index: i as u32 % 3,
                        near,
                        far,
                        depth_views: [depth_view(-0.03), depth_view(0.03)],
                    },
                    capture_ns: Some(capture_ns),
                    // Delivered 20 ms after the capture, with the frame displayed 50 ms later
                    display_ns: capture_ns + 70_000_000,
                    now_ns: capture_ns + 20_000_000,
                    // The last one failed
                    raw: (i + 1 < frames).then_some(raw),
                    error: (i + 1 == frames).then(|| "the GPU readback timed out".into()),
                }
            })
            .collect::<Vec<_>>();
        let display_count = (frames as i64 * depth_period / display_period) as usize;
        let display = (0..display_count)
            .map(|k| {
                let now_ns = start_ns + 20_000_000 + k as i64 * display_period;
                // The newest depth frame the app has
                let depth_index = depth
                    .iter()
                    .rposition(|depth: &BurstDepth| depth.now_ns <= now_ns);
                let shape = HandShape::from_joints(&[Vec4::new(0.1, 1.0, -0.4, 0.01); 26]);
                BurstDisplay {
                    display_ns: now_ns + 50_000_000,
                    now_ns,
                    depth: depth_index,
                    region: Some(0),
                    views: [depth_view(-0.032), depth_view(0.032)],
                    stream_views: [depth_view(-0.032), depth_view(0.032)],
                    eye_positions: [Vec3::new(-0.032, 1.2, 0.0), Vec3::new(0.032, 1.2, 0.0)],
                    hand_ns: now_ns + 50_000_000,
                    hands: [None, Some(joints(k as f32 * 0.001))],
                    capsules: [
                        None,
                        Some((
                            shape,
                            HandZones {
                                core: -0.004,
                                falloff: 0.004,
                                rim: 0.008,
                                solid: [0.0; crate::HAND_SEGMENTS],
                                sweep: depth_mask::HandSweep::NONE,
                            },
                        )),
                    ],
                    // The left arm as `arm_shift 1`, the right one as 2 (`arm_hands 2`)
                    arms: [
                        Some(ArmMotion {
                            wrist: Vec3::new(-0.1, 1.0, -0.4),
                            direction: Vec3::NEG_Y,
                            displacement: Vec3::new(0.0, 0.01, 0.0),
                            weight_len: 0.0,
                            mode: 1,
                            target_ns: now_ns + 50_000_000,
                            history_ns: now_ns + 50_000_000,
                            wrist_history: Vec3::new(-0.1, 1.0, -0.4),
                            extrapolated: 0.0,
                        }),
                        Some(ArmMotion {
                            wrist: Vec3::new(0.1, 1.0, -0.4),
                            direction: Vec3::NEG_Y,
                            displacement: Vec3::new(0.01, 0.0, 0.0),
                            weight_len: 0.3,
                            mode: 2,
                            target_ns: now_ns + 10_000_000,
                            history_ns: now_ns - 30_000_000,
                            wrist_history: Vec3::new(0.09, 1.0, -0.4),
                            extrapolated: 0.01,
                        }),
                    ],
                    // A wrist gap on the right arm only (`arm_gap_hands 2`), the edge cut at 50
                    arm_gaps: [
                        None,
                        Some(ArmGap {
                            wrist: Vec3::new(0.1, 1.0, -0.4),
                            axis: Vec3::NEG_Y,
                            length: 0.03,
                        }),
                    ],
                    arm_gap_set_mm: [0.0, 30.0],
                    edge_cut: 0.5,
                    // The right arm in mode 2 with the parabola: one new sample per frame, the
                    // capture-time wrist; the left one was lost in the first frame
                    arm_samples: [
                        BurstArmSamples {
                            reset: k == 0,
                            ..Default::default()
                        },
                        BurstArmSamples {
                            reset: false,
                            new: vec![BurstArmSample {
                                time_ns: now_ns - 35_000_000,
                                seen_ns: now_ns,
                                wrist: Vec3::new(0.09, 1.0, -0.4),
                                from_capture_time: true,
                            }],
                            capture_wrist: Some(Vec3::new(0.08, 1.0, -0.4)),
                            estimator: 2,
                        },
                    ],
                    // The room memory in the second frame only
                    memory: (k == 1).then(|| MemoryRecord {
                        status: crate::MemoryStatus {
                            anchor: crate::MemoryAnchor {
                                camera: Some(crate::MemoryCamera {
                                    position: Vec3::new(0.0, 1.2, 0.0),
                                    yaw: 0.0,
                                    pitch: 0.5,
                                }),
                                clean: 1,
                                with_hands: 0,
                            },
                            size: 512,
                            updates: [3, 2],
                            stats: None,
                        },
                        params: Default::default(),
                        hands: [crate::CaptureHand::None, crate::CaptureHand::Exact],
                    }),
                    // The depth cloud in the third frame only, in today's phase of `cloud_ab`
                    cloud: (k == 2).then(|| CloudRecord {
                        params: crate::CloudParams {
                            ab: true,
                            ..Default::default()
                        },
                        shown: false,
                        eyes: [
                            crate::CloudEyeRecord {
                                field_capture_ns: Some(start_ns),
                                prev_capture_ns: None,
                                dt_ms: 0.0,
                                reset: Some(crate::ResetReason::Start),
                                head_deg_s: 0.0,
                                a_ms: 15.0,
                                reach_ms: 22.5,
                                stale: false,
                            },
                            crate::CloudEyeRecord::default(),
                        ],
                    }),
                }
            })
            .collect();
        let camera = (0..frames / 2)
            .map(|c| BurstCamera {
                now_ns: start_ns + 30_000_000 + c as i64 * 62_500_000,
                image_ns: start_ns + 5_000_000 + c as i64 * 62_500_000,
                width: 8,
                height: 6,
                intrinsics: [5.0, 5.0, 4.0, 3.0],
                camera: Pose {
                    orientation: Quat::IDENTITY,
                    position: Vec3::new(0.0, 1.2, 0.0),
                },
                head: Pose::IDENTITY,
                rgb: (0..8 * 6 * 3).map(|v| (v + c) as u8).collect(),
                hands: [Some(joints(0.0)), None],
            })
            .collect();

        Burst {
            n,
            duration_ms: 3000,
            start_ns,
            end_ns: start_ns + frames as i64 * depth_period,
            clocks: [
                Some(BurstClocks {
                    realtime_ns: 1_795_000_000_123_456_789,
                    monotonic_ns: Some(start_ns - 1_500),
                    xr_ns: Some(start_ns),
                    read_ns: Some(3_000),
                }),
                // The stream stopped before the end, on a PC build
                Some(BurstClocks {
                    realtime_ns: 1_795_000_003_123_456_789,
                    monotonic_ns: None,
                    xr_ns: None,
                    read_ns: None,
                }),
            ],
            depth_size: size,
            flip_y: false,
            dump: Some(format!("alvr_dump_{n}_depth.json")),
            key_lut: Some(BurstLut {
                version: 3,
                published_ns: start_ns - 400_000_000,
                data: (0..32 * 32 * 32 * 4).map(|i| (i % 251) as u8).collect(),
            }),
            depth,
            depth_skipped: 1,
            display,
            camera,
            regions: vec![BurstRegion {
                now_ns: start_ns,
                json: r#"{"mode":"distance","max_distance":0.9,"feathering":0.02}"#.into(),
            }],
        }
    }

    pub(crate) fn synthetic_distance(frame: usize, eye: usize, x: u32, y: u32) -> f32 {
        0.3 + 0.01 * frame as f32 + 0.1 * eye as f32 + 0.002 * x as f32 + 0.001 * y as f32
    }

    #[test]
    fn raw_values_are_decoded_from_padded_rows() {
        let size = UVec2::new(3, 2);
        let bytes_per_row = 256;
        let mut data = vec![0xee; 2 * 2 * bytes_per_row];
        for eye in 0..2 {
            for y in 0..2 {
                for x in 0..3 {
                    let value = (eye * 1000 + y * 100 + x) as u16;
                    let start = (eye * 2 + y) * bytes_per_row + x * 4;
                    data[start..start + 2].copy_from_slice(&value.to_le_bytes());
                }
            }
        }
        let raw = decode_raw(&data, size, bytes_per_row as u32);
        assert_eq!(
            raw,
            [0, 1, 2, 100, 101, 102, 1000, 1001, 1002, 1100, 1101, 1102]
        );
    }

    #[test]
    fn camera_sink_stops_at_its_time() {
        let record = || BurstCamera {
            now_ns: 0,
            image_ns: 0,
            width: 1,
            height: 1,
            intrinsics: [1.0; 4],
            camera: Pose::IDENTITY,
            head: Pose::IDENTITY,
            rgb: vec![0; 3],
            hands: [None; 2],
        };
        let mut sink = CameraBurstSink::new(Duration::from_secs(10));
        assert!(sink.wants());
        sink.push(record());
        let mut ended = CameraBurstSink::new(Duration::ZERO);
        assert!(!ended.wants());
        ended.push(record());
        assert_eq!(sink.into_records().len(), 1);
        assert!(ended.into_records().is_empty());
    }

    #[test]
    fn clocks_text_is_one_key_value_per_clock() {
        let clocks = BurstClocks {
            realtime_ns: 1_795_000_000_000_000_001,
            monotonic_ns: Some(42),
            xr_ns: None,
            read_ns: Some(700),
        };
        assert_eq!(
            clocks.text(),
            "realtime_ns=1795000000000000001 monotonic_ns=42 xr_ns=null read_ns=700"
        );
        assert!(is_valid_json(&clocks.to_json()));
    }

    #[test]
    fn index_is_valid_json_in_time_order() {
        let burst = synthetic_burst(7, UVec2::new(4, 3), 4);
        let json = burst.index_json();
        assert!(is_valid_json(&json), "{json}");
        assert!(json.starts_with(r#"{"format":"alvr_burst","version":7,"n":7,"#));
        // The clock pairs of version 5, right after the times they go with
        assert!(
            json.contains(
                r#""end_ns":1000133333332,"clocks":{"start":{"realtime_ns":1795000000123456789,"monotonic_ns":999999998500,"xr_ns":1000000000000,"read_ns":3000},"end":{"realtime_ns":1795000003123456789,"monotonic_ns":null,"xr_ns":null,"read_ns":null}},"depth":"#
            ),
            "{json}"
        );
        assert!(
            json.contains(
                r#""key_lut":{"file":"alvr_dump_7_key.lut","version":3,"published_ns":999600000000,"bytes":131072}"#
            ),
            "{json}"
        );
        // Both arm modes with the fields of step 1
        assert!(
            json.contains(r#""mode":1,"weight_len":0,"target_ns":"#),
            "{json}"
        );
        assert!(
            json.contains(r#""mode":2,"weight_len":0.3,"target_ns":"#),
            "{json}"
        );
        assert!(json.contains(r#""extrapolated_mm":10}],"#), "{json}");
        // The fields of steps 1 and 2 of the mask design
        assert!(
            json.contains(
                r#""edge_cut":50,"arm_gaps":[null,{"wrist":[0.1,1,-0.4],"axis":[0,-1,0],"length_mm":30}],"arm_gap_set_mm":[0,30],"#
            ),
            "{json}"
        );
        // The arm's own sample history (step 1b of the arm plan)
        assert!(
            json.contains(
                r#""arm_samples":[{"reset":true,"new":[],"capture_wrist":null,"estimator":0},{"reset":false,"new":[{"time_ns":"#
            ),
            "{json}"
        );
        assert!(
            json.contains(
                r#","wrist":[0.09,1,-0.4],"capture_time":true}],"capture_wrist":[0.08,1,-0.4],"estimator":2}],"memory":null,"cloud":null}"#
            ),
            "{json}"
        );
        // The room memory (version 6)
        assert!(
            json.contains(r#""memory":{"camera":{"position":[0,1.2,0],"yaw_deg":0,"#),
            "{json}"
        );
        assert!(
            json.contains(r#""hands":["none","exact"]},"cloud":null}"#),
            "{json}"
        );
        // The depth cloud (version 7)
        assert!(
            json.contains(
                r#""memory":null,"cloud":{"mode":1,"shown":false,"gain":150,"cap":14,"hold_ms":200,"dead":2.5,"lambda_ms":40,"fetch_mm":450,"fixed":0,"ab":true,"onesided":1,"trail":0,"eyes":[{"field_capture_ns":1000000000000,"prev_capture_ns":null,"dt_ms":0,"reset":"start","head_deg_s":0,"a_ms":15,"reach_ms":22.5,"stale":false},{"field_capture_ns":null,"#
            ),
            "{json}"
        );
        assert_eq!(json.matches(r#""cloud":{"#).count(), 1, "{json}");
        assert!(
            json.contains(
                r#""counts":{"depth":4,"depth_read":3,"depth_skipped":1,"display":9,"camera":2,"region":1}"#
            ),
            "{json}"
        );
        assert!(json.contains(
            r#""files":["alvr_burst_7_depth_0000_left.f32","alvr_burst_7_depth_0000_right.f32"]"#
        ));
        assert!(json.contains(r#""files":null,"error":"the GPU readback timed out""#));
        assert!(json.contains(r#""far":null"#));
        assert!(json.contains(r#""file":"alvr_burst_7_camera_0001.rgb""#));
        // 26 joints of 4 numbers, a joint without a valid velocity
        let hands = json.split(r#""hands":[null,{"joints":"#).nth(1).unwrap();
        assert!(
            hands.starts_with("[[0,1,-0.4,0.01],[0.01,1,-0.4,0.01],"),
            "{hands}"
        );
        assert!(json.contains(r#"[0.1,0,0],null,[0.1,0,0]"#));

        // The times of the records never go back
        let times = json
            .split(r#""t_ns":"#)
            .skip(1)
            .map(|rest| rest[..rest.find(',').unwrap()].parse::<i64>().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(times.len(), 4 + 9 + 2 + 1);
        assert!(times.windows(2).all(|pair| pair[0] <= pair[1]), "{times:?}");
    }

    // `ALVR_BURST_TEST_DIR=<folder>` keeps the files of a synthetic burst there, for
    // tools/read_burst.py
    #[test]
    fn files_hold_the_depth_in_meters() {
        let size = UVec2::new(32, 24);
        let frames = 30;
        let burst = synthetic_burst(9, size, frames);
        let kept = std::env::var_os("ALVR_BURST_TEST_DIR");
        let dir = kept.clone().map_or_else(
            || std::env::temp_dir().join(format!("alvr_burst_test_{}", std::process::id())),
            std::path::PathBuf::from,
        );
        let dir = dir.join(burst_dir_name(burst.n));
        let (files, bytes) = burst.write_to(&dir).unwrap();
        // Depth of each read frame and eye, the camera images, the key table, the index
        assert_eq!(files, (frames - 1) * 2 + frames / 2 + 1 + 1);
        assert_eq!(
            fs::read(dir.join(key_lut_file_name(9))).unwrap(),
            burst.key_lut.as_ref().unwrap().data
        );
        assert!(bytes >= burst.image_bytes());

        let data = fs::read(dir.join(depth_file_name(9, 2, 1))).unwrap();
        assert_eq!(data.len(), (size.x * size.y) as usize * 4);
        let value = |x: u32, y: u32| {
            let i = ((y * size.x + x) * 4) as usize;
            f32::from_le_bytes(data[i..i + 4].try_into().unwrap())
        };
        for (x, y) in [(0, 0), (31, 0), (5, 23)] {
            let expected = synthetic_distance(2, 1, x, y);
            assert!((value(x, y) - expected).abs() < 1e-3 * expected, "{x} {y}");
        }
        assert!(!dir.join(depth_file_name(9, frames - 1, 0)).exists());
        let index = fs::read_to_string(dir.join(index_name(9))).unwrap();
        assert!(is_valid_json(&index));

        if kept.is_none() {
            fs::remove_dir_all(dir.parent().unwrap()).ok();
        }
    }
}
