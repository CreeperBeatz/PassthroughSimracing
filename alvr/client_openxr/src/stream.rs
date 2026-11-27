#[cfg(target_os = "android")]
use crate::auto_color_key::AutoColorKey;
use crate::{
    arm_history::{ArmEstimator, HandHistory, HandSample},
    burst::{self, BurstRecorder},
    extra_extensions::EnvironmentDepthMETA,
    graphics::{self, ProjectionLayerAlphaConfig, ProjectionLayerBuilder},
    green_screen::{GreenConditions, GreenFrame, GreenLayers, GreenScreen},
    interaction::{self, InteractionContext, InteractionSourcesConfig},
    lever::{LeverHand, LeverParams, LeverWindows},
    motion_cloud::{self, CloudLog, CloudOff},
    passthrough::{self, ColorKeyParams, ProblemViewParams, debug_property},
    perf::{self, FfrOutcome, FfrParams, FrameLatencies, PerfLog, StreamFacts},
    room_memory::{self, MemoryLog},
};
use alvr_client_core::{
    ClientCoreContext, VideoFrameMetadata,
    video_decoder::{self, VideoDecoderConfig, VideoDecoderSource},
};
use alvr_common::{
    AlvrFoveatedEncodingParams, DETACHED_CONTROLLER_LEFT_ID, DETACHED_CONTROLLER_RIGHT_ID,
    HAND_LEFT_ID, HAND_RIGHT_ID, HEAD_ID, Pose, RelaxedAtomic, ViewParams,
    anyhow::Result,
    error,
    glam::{Quat, UVec2, Vec2, Vec3, Vec4},
    info,
    parking_lot::RwLock,
};
use alvr_graphics::{
    ArmGap, ArmMotion, BridgeChoice, BurstArmSample, BurstArmSamples, BurstHand, BurstLut,
    CloudInput, CloudParams, CloudRecord, ColorKeyInput, DepthDumpFiles, DepthDumpRequest,
    DepthFrame, DepthRenderInput, DumpJoint, EditorHand, EditorInput, GpuRectParams,
    GpuRectRequest, GraphicsContext, GreenKeyParams, HAND_SEGMENTS, HandRay, HandShape, HandZones,
    LeakGains, LeverWindow, MAX_GREEN_WINDOWS, MAX_LEVER_WINDOWS, MemoryInput, MemoryParams,
    MemoryRecord, OverlaySegment, PlacedArea, RectFallback, RectStats, RenderAblation,
    SEGMENT_JOINTS, StreamRenderer, StreamViewParams, WristBridge,
};
use alvr_packets::{ClientStreamConfig, RealTimeConfig, TrackingData};
use alvr_session::{
    ClientsideFoveationConfig, ClientsideFoveationMode, ClientsidePostProcessingConfig, CodecType,
    DepthMaskRegion, MediacodecProperty, PassthroughMode, UpscalingConfig,
};
use alvr_system_info::Platform;
use openxr as xr;
use std::{
    cell::Cell,
    fs,
    path::PathBuf,
    ptr,
    rc::Rc,
    sync::Arc,
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const DECODER_MAX_TIMEOUT_MULTIPLIER: f32 = 0.8;

// Joints that move with the arm, not with the fingers: palm, wrist and the knuckles of the four
// fingers (indices of `xr::HandJointEXT`). The wrist alone barely moves when the hand turns in place
pub(crate) const PALM_JOINTS: [usize; 6] = [0, 1, 7, 12, 17, 22];
const WRIST_JOINT: usize = 1;

// The solid fill of the hands fades out between these speeds (m/s)
const MIN_FILL_SPEED: f32 = 0.08;
const MAX_FILL_SPEED: f32 = 0.25;

// How often `debug.alvr.arm_*`, `debug.alvr.edge_cut`, `debug.alvr.playzone_auto` and
// `debug.alvr.key_dump` are read
const PROPERTY_POLL_INTERVAL: Duration = Duration::from_millis(500);

// The playzone is remembered when no hand was near the view of the depth camera for a while
const PLAYZONE_HANDS_TIMEOUT: Duration = Duration::from_millis(1500);
const PLAYZONE_INTERVAL: Duration = Duration::from_millis(500);
// Around the bounding sphere of a hand: the runtime removes it from the depth map, and what it holds
const PLAYZONE_HAND_MARGIN: f32 = 0.1;
// Why no playzone is remembered, at most this often
const PLAYZONE_LOG_INTERVAL: Duration = Duration::from_secs(2);

// The layers of the color key stay a moment after the hands left the view, so they don't flicker
const KEY_LAYERS_HOLD: Duration = Duration::from_millis(300);
// Of the view, around the hands, so that the edge of the cropped layer is not on a hand
const KEY_CROP_MARGIN: f32 = 0.03;

// Per hand for the log: ring, map, known share of the map, leak, rim
type LeakLog = Option<(f32, Option<f32>, f32, f32, f32)>;

// Longest step of the placed wrist from the newest hand sample (m)
const MAX_ARM_EXTRAPOLATION: f32 = 0.06;
// Without XR_META_hand_tracking_unextrapolated_poses the newest hand sample is read with a lookup this
// long before now: the runtime returns its newest sample for any time before it (the sample is 35-65 ms
// old), never the hand at that time. Only the sample is used, the time asked for is not
const ARM_SAMPLE_LOOKUP_AGE: Duration = Duration::from_millis(100);
// The `arm:` line is logged this often while an arm uses `arm_shift 2`, and on each change of the
// properties
const ARM_LOG_INTERVAL: Duration = Duration::from_secs(2);
// The `lever:` line is logged this often while a box is a lever (`debug.alvr.lever_boxes`)
const LEVER_LOG_INTERVAL: Duration = Duration::from_secs(2);
// The `memory:` line is logged this often while the room memory is on (`debug.alvr.mem_roll`)
const MEMORY_LOG_INTERVAL: Duration = Duration::from_secs(2);
// The `cloud:` line is logged this often while the depth cloud is set (`debug.alvr.cloud`)
const CLOUD_LOG_INTERVAL: Duration = Duration::from_secs(2);

/// `debug.alvr.arm_shift` and the properties of its mode 2 (steps 1 and 1b of
/// `docs/arm-mask-build-plan.md`)
#[derive(Clone, Copy, PartialEq, Eq)]
struct ArmParams {
    /// 0: plain depth. 1: the depth around each arm moves with the display-time wrist, weighted by a
    /// smoothstep to 45 cm. 2: moved from the wrist of the arm's own sample history at the depth's
    /// capture time to the wrist of `estimator`, weighted linearly
    shift: i64,
    /// Arms that use mode 2 when `shift` is 2 (1 left, 2 right, 3 both). The others use mode 1, to
    /// compare them side by side
    hands: i64,
    /// The arm is placed for the display time minus this (the passthrough latency), with the line or
    /// the parabola. The held sample stands for its own time
    target_ms: i64,
    /// Length of the linear weight
    weight_mm: i64,
    /// `debug.alvr.arm_estimator`: how the placed wrist comes from the history (0 hold, the default)
    estimator: ArmEstimator,
    /// `debug.alvr.arm_sample_age_ms`: how old a hand sample is when the app first gets it, for its time
    /// when the runtime gives no capture time (or `capture_time` is off)
    sample_age_ms: i64,
    /// `debug.alvr.arm_capture_time`: the times of the hand samples are the runtime's capture times if it
    /// gives them (1, default), or always the time seen minus `sample_age_ms` (0)
    capture_time: bool,
}

impl ArmParams {
    fn read() -> Self {
        Self {
            shift: arm_shift_mode(debug_property("debug.alvr.arm_shift").unwrap_or(1)),
            hands: debug_property("debug.alvr.arm_hands")
                .unwrap_or(3)
                .clamp(0, 3),
            target_ms: debug_property("debug.alvr.arm_target_ms")
                .unwrap_or(40)
                .clamp(0, 150),
            weight_mm: debug_property("debug.alvr.arm_weight_mm")
                .unwrap_or(300)
                .clamp(100, 600),
            estimator: ArmEstimator::from_value(
                debug_property("debug.alvr.arm_estimator").unwrap_or(0),
            ),
            sample_age_ms: debug_property("debug.alvr.arm_sample_age_ms")
                .unwrap_or(35)
                .clamp(0, 150),
            capture_time: debug_property("debug.alvr.arm_capture_time").unwrap_or(1) != 0,
        }
    }

    // Whether arm `hand` (0 left, 1 right) uses mode 2
    fn is_new(&self, hand: usize) -> bool {
        self.shift == 2 && self.hands & (1 << hand) != 0
    }

    // Mode 2 at the display time `display_ns`: the target time T of the line and the parabola
    fn target_ns(&self, display_ns: i64) -> i64 {
        display_ns - self.target_ms * 1_000_000
    }

    // The samples kept so far have times of another rule: the histories start again
    fn changes_sample_times(&self, other: &Self) -> bool {
        self.sample_age_ms != other.sample_age_ms || self.capture_time != other.capture_time
    }
}

/// `debug.alvr.edge_cut`, `debug.alvr.arm_gap_mm` and `debug.alvr.arm_gap_hands`: steps 1 and 2 of
/// `docs/mask-v2-design.md`. Unset, the mask is exactly today's
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct MaskParams {
    /// Centre of the cut of the mask edge, percent of the coverage: 20 today's (also for 0 or less),
    /// higher draws the edge further on the object. 17..84, so that the cut stays inside 0..1
    edge_cut: i64,
    /// Length of the wrist gap (0: off), at most 80 mm
    gap_mm: i64,
    /// Arms with a wrist gap (1 left, 2 right, 3 both), to compare them side by side
    gap_hands: i64,
}

impl MaskParams {
    fn read() -> Self {
        Self::from_values(
            debug_property("debug.alvr.edge_cut"),
            debug_property("debug.alvr.arm_gap_mm"),
            debug_property("debug.alvr.arm_gap_hands"),
        )
    }

    fn from_values(edge_cut: Option<i64>, gap_mm: Option<i64>, gap_hands: Option<i64>) -> Self {
        Self {
            edge_cut: match edge_cut.unwrap_or(20) {
                ..=0 => 20,
                value => value.clamp(17, 84),
            },
            gap_mm: gap_mm.unwrap_or(0).clamp(0, 80),
            gap_hands: gap_hands.unwrap_or(3).clamp(0, 3),
        }
    }

    // The wrist gap of arm `hand` (0 left, 1 right) in meters, None: off
    fn gap(&self, hand: usize) -> Option<f32> {
        (self.gap_mm > 0 && self.gap_hands & (1 << hand) != 0).then(|| self.gap_mm as f32 / 1000.0)
    }

    // The `mask:` line on `ALVR_CAM`, at the start and on each change
    fn text(&self) -> String {
        let cut = if self.edge_cut == 20 {
            "edge cut 20 % (today's)".to_string()
        } else {
            format!("edge cut {} %", self.edge_cut)
        };
        let gap = match (self.gap_mm, self.gap_hands) {
            (0, _) => "wrist gap off".to_string(),
            (_, 0) => format!("wrist gap {} mm on no arm", self.gap_mm),
            (mm, hands) => format!(
                "wrist gap {mm} mm on {}",
                match hands {
                    1 => "the left arm",
                    2 => "the right arm",
                    _ => "both arms",
                }
            ),
        };

        format!("mask: {cut}, {gap}")
    }
}

// `debug.alvr.arm_shift`: 0 off, 2 the new mode, any other value today's shift (mode 1), as before
// mode 2 existed, when every value but 0 meant on
fn arm_shift_mode(value: i64) -> i64 {
    match value {
        0 => 0,
        2 => 2,
        _ => 1,
    }
}

// The joint lookups of an arm in mode 1, counted apart for the `arm:` line
#[derive(Clone, Copy)]
enum Lookup {
    // The wrist the arm is placed from: at the display time minus `key_delay`
    Place,
    // At the capture time of the depth image
    Capture,
}

// What the `arm:` line reports of one arm since the last one
#[derive(Default)]
struct ArmHandLog {
    // Mode 2 with the line or the parabola: how far the wrist was placed from the newest sample (mm:
    // ahead, or back for a target inside the history), the speed it was extrapolated with (m/s, 0 back).
    // The distance between the wrist of the hand capsules and the wrist the arm
    // is placed with (mm)
    extrapolated_mm: Vec<f32>,
    speeds: Vec<f32>,
    gaps_mm: Vec<f32>,
    // Frames with a hand tracker, and the failed lookups of mode 1 per `Lookup`. A failed `Place` lookup
    // skips the `Capture` one
    frames: u32,
    failed: [u32; 2],
    // The arm's own sample history: new samples, the time between them (ms), the age of those with a
    // capture time when first seen (ms), how often it was emptied (the hand lost), and mode 2's frames
    // without a sample at the depth's capture time (plain depth for the arm)
    samples: u32,
    intervals_ms: Vec<f32>,
    ages_ms: Vec<f32>,
    resets: u32,
    misses: u32,
}

impl ArmHandLog {
    // Joint lookup of the arm, counted
    fn count<T>(&mut self, lookup: Lookup, result: Option<T>) -> Option<T> {
        if result.is_none() {
            self.failed[lookup as usize] += 1;
        }

        result
    }

    // For the `arm:` line, mode 1
    fn lookups_text(&self) -> String {
        let [place, capture] = self.failed;
        format!(
            "lookups failed display {place}, capture {capture} of {} frames",
            self.frames
        )
    }

    // For the `arm:` line, mode 2: the sample history. `sample_age_ms` is the age used for samples
    // without a capture time
    fn samples_text(&mut self, sample_age_ms: i64) -> String {
        let interval = median_p90(&mut self.intervals_ms)
            .map_or(format!("{} samples", self.samples), |(median, p90)| {
                format!("{} samples every {median:.1} ms p90 {p90:.1}", self.samples)
            });
        let timed = self.ages_ms.len();
        let age = match median_p90(&mut self.ages_ms) {
            Some((median, p90)) if timed as u32 == self.samples => {
                format!("age {median:.0} ms p90 {p90:.0} (captureTime)")
            }
            Some((median, p90)) => format!(
                "age {median:.0} ms p90 {p90:.0} (captureTime, {timed} samples), else {sample_age_ms} ms"
            ),
            None => format!("age {sample_age_ms} ms (arm_sample_age_ms)"),
        };

        format!(
            "{interval}, {age}, buffer misses {} of {} frames, resets {}",
            self.misses, self.frames, self.resets
        )
    }
}

// A sample of the arm's history as bursts record it
fn burst_arm_sample(sample: &HandSample) -> BurstArmSample {
    BurstArmSample {
        time_ns: sample.time_ns,
        seen_ns: sample.seen_ns,
        wrist: sample.wrist,
        from_capture_time: sample.from_capture_time,
    }
}

// Median and 90th percentile, None without values
fn median_p90(values: &mut [f32]) -> Option<(f32, f32)> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f32::total_cmp);
    let at = |q: f32| values[((values.len() - 1) as f32 * q).round() as usize];

    Some((at(0.5), at(0.9)))
}

// Spheres of `debug.alvr.key_leak_view 2`, meters
const LEAK_PROBE_RADIUS: f32 = 0.0125;
// The leak map values are logged this often while it is used
const LEAK_LOG_INTERVAL: Duration = Duration::from_secs(2);

/// What the color key needs in this frame
#[derive(Clone, Copy, Default)]
pub struct ColorKeyFrame {
    /// Hands with a pose
    pub tracked_hands: [bool; 2],
    /// Age of the remembered playzone, None: there is none
    pub playzone_age: Option<Duration>,
    /// Part of each view with the hands, see `hands_view_rect`
    pub hand_rects: [Option<(Vec2, Vec2)>; 2],
    /// The hands are in view, or were a moment ago: the layers of the color key are needed
    pub hands_in_view: bool,
    /// Applied zones of each hand, for the log
    pub hand_zones: [Option<HandZones>; 2],
    /// `debug.alvr.green_boxes` (green_screen.rs): the rule of the green-keyed base passthrough while the
    /// zone is on. None: the plain base layer (the default)
    pub green_key: Option<GreenKeyParams>,
}

pub struct ParsedStreamConfig {
    pub view_resolution: UVec2,
    pub refresh_rate_hint: f32,
    pub encoding_gamma: f32,
    pub enable_hdr: bool,
    pub passthrough: Option<PassthroughMode>,
    pub foveated_encoding_config: Option<AlvrFoveatedEncodingParams>,
    pub clientside_foveation_config: Option<ClientsideFoveationConfig>,
    pub clientside_post_processing: Option<ClientsidePostProcessingConfig>,
    pub upscaling: Option<UpscalingConfig>,
    pub force_software_decoder: bool,
    pub max_buffering_frames: f32,
    pub buffering_history_weight: f32,
    pub decoder_options: Vec<(String, MediacodecProperty)>,
    pub interaction_sources: InteractionSourcesConfig,
}

impl ParsedStreamConfig {
    pub fn new(config: &ClientStreamConfig) -> Self {
        Self {
            view_resolution: config.negotiated_config.view_resolution,
            refresh_rate_hint: config.negotiated_config.refresh_rate_hint,
            encoding_gamma: config.negotiated_config.encoding_gamma,
            enable_hdr: config.negotiated_config.enable_hdr,
            passthrough: config.settings.video.passthrough.as_option().cloned(),
            foveated_encoding_config: config.negotiated_config.foveated_encoding,
            clientside_foveation_config: config
                .settings
                .video
                .clientside_foveation
                .as_option()
                .cloned(),
            clientside_post_processing: config
                .settings
                .video
                .clientside_post_processing
                .as_option()
                .cloned(),
            upscaling: config.settings.video.upscaling.as_option().cloned(),
            force_software_decoder: config.settings.video.force_software_decoder,
            max_buffering_frames: config.settings.video.max_buffering_frames,
            buffering_history_weight: config.settings.video.buffering_history_weight,
            decoder_options: config.settings.video.mediacodec_extra_options.clone(),
            interaction_sources: InteractionSourcesConfig::new(config),
        }
    }
}

pub struct StreamContext {
    core_context: Arc<ClientCoreContext>,
    xr_session: xr::Session<xr::OpenGlEs>,
    gfx_ctx: Rc<GraphicsContext>,
    interaction_context: Arc<RwLock<InteractionContext>>,
    stage_reference_space: Arc<xr::Space>,
    view_reference_space: Arc<xr::Space>,
    swapchains: [xr::Swapchain<xr::OpenGlEs>; 2],
    last_good_video_frame_metadata: VideoFrameMetadata,
    input_thread: Option<JoinHandle<()>>,
    input_thread_running: Arc<RelaxedAtomic>,
    // The hands are not sent to the PC (while the play area is edited), see `withhold_hands`
    hands_withheld: Arc<RelaxedAtomic>,
    config: ParsedStreamConfig,
    target_view_resolution: UVec2,
    renderer: StreamRenderer,
    decoder: Option<(VideoDecoderConfig, VideoDecoderSource)>,
    use_custom_reprojection: bool,
    // Created on demand when the depth passthrough mode is selected
    environment_depth: Option<EnvironmentDepthMETA>,
    environment_depth_failed: bool,
    // Set if the passthrough is color keyed around the hands
    color_key: Option<ColorKeyParams>,
    // Learns the colors of the key, if enabled
    #[cfg(target_os = "android")]
    auto_color_key: Option<AutoColorKey>,
    // The depth of the arms is shifted by the motion of the wrist since the depth image was captured.
    // `setprop debug.alvr.arm_shift 0`: plain depth, 2: from the arm's own sample history, the linear
    // weight
    arm_params: ArmParams,
    // The hand samples of each arm (`arm_history.rs`), filled every frame with a depth mask. With
    // XR_META_hand_tracking_unextrapolated_poses enabled the samples come with their capture time
    arm_histories: [HandHistory; 2],
    unextrapolated_poses: bool,
    // Of this frame, for bursts: per hand the new samples, whether the history was emptied, the
    // capture-time wrist of mode 2. Filled only while a burst records; its first record gets every sample
    // kept (`arm_samples_recording`: the last frame was recorded)
    arm_samples: [BurstArmSamples; 2],
    arm_samples_recording: bool,
    // For the `arm:` line: since when, whether the properties changed, per arm
    last_arm_log: Instant,
    arm_params_changed: bool,
    arm_log: [ArmHandLog; 2],
    // `debug.alvr.edge_cut` and the wrist gap. None until the first poll, which logs them
    mask_params: Option<MaskParams>,
    // `debug.alvr.lever_*` (U2 of design/universal/final.md, lever.rs): the play-area boxes that are levers,
    // their windows, the `lever:` line. Off by default: no hand lookup, no window, no line
    lever_params: LeverParams,
    levers: LeverWindows,
    last_lever_log: Instant,
    // `debug.alvr.mem_*` (U3 of design/universal/final.md, room_memory.rs): the room memory's properties
    // (None: off, the default: nothing is kept or drawn, today's playzone copy), the last known joints of
    // each hand (time, joints), the `memory:` line
    memory_params: Option<MemoryParams>,
    last_known_joints: [Option<(i64, [Vec4; 26])>; 2],
    memory_log: MemoryLog,
    last_memory_log: Instant,
    // `debug.alvr.cloud*` (design/depthcloud/final.md, motion_cloud.rs): the depth cloud's properties (None:
    // off, the default: no pass, no line, today's picture), since when `cloud_ab` alternates, the `cloud:`
    // line
    cloud_params: Option<CloudParams>,
    cloud_ab_start: Instant,
    cloud_log: CloudLog,
    last_cloud_log: Instant,
    // `debug.alvr.cloud_leak` 1: the band's gain and cap are scaled by the key's measured leak
    // (`motion_cloud::leak_scale`). 0, the default: as set
    cloud_leak: bool,
    // The wrists of the hand capsules of the key in this frame (display time minus `key_delay`), for
    // the distance to the wrist of the arm
    key_wrists: Cell<[Option<Vec3>; 2]>,
    // The learned colour table last uploaded to the compositor, for bursts, and how many were
    // uploaded since the stream started
    published_lut: Option<BurstLut>,
    lut_uploads: u32,
    // Of the key last given to `set_color_key`, 0: none
    color_key_version: u32,
    // `debug.alvr.playzone_auto`
    playzone_auto: bool,
    last_property_poll: Instant,
    // `debug.alvr.key_dump`: last value, a new one waits for a depth image (since when)
    last_dump_value: Option<i64>,
    pending_dump: Option<(i64, Instant)>,
    // `debug.alvr.burst`, and the n of the depth dump it asked for (written into its folder)
    burst: BurstRecorder,
    burst_dump: Option<i64>,
    // When the depth image of this frame was captured
    depth_capture_time: Option<xr::Time>,
    // Bounding spheres of the hands with a pose (xyz, radius), before `key_hands` and `key_leak_view`
    hand_bounds: Cell<[Option<Vec4>; 2]>,
    // Last part of each view with the hands, and when there was one
    last_hand_rects: [Option<(Vec2, Vec2)>; 2],
    last_hand_rect_time: Option<Instant>,
    last_hands_seen: Instant,
    last_playzone_capture: Instant,
    last_playzone_request: i64,
    // Automatic captures since the hands left, only the first is logged
    playzone_auto_series: bool,
    last_playzone_log: Instant,
    // Smoothed, meters per second, stage space
    hand_velocities: [Cell<Vec3>; 2],
    hand_solid: [Cell<[f32; HAND_SEGMENTS]>; 2],
    // How solid the bridge from each wrist to the forearm is (0..1), smoothed
    bridge_solid: [Cell<f32>; 2],
    // How much the rim of each hand is narrowed over backgrounds that look like it (0..1), smoothed
    hand_leak: [Cell<f32>; 2],
    // Log of the leak map: last time, and the lookup time since then
    last_leak_log: Cell<Instant>,
    leak_lookup_time: Cell<(Duration, u32)>,
    // Play area placed from a marker (its boxes), used instead of the region of the settings, and
    // whether the hands of the color key show only inside it
    region_override: Option<PlacedArea>,
    hands_in_region: bool,
    // Outline of the anchor and the box, drawn over the stream (stage space). Empty: none
    area_overlay: Vec<OverlaySegment>,
    // How much the game is dimmed (0..1), while the play area is edited
    game_dim: f32,
    // How much the room shows through the game outside the mask (0..1), while the play area is edited
    game_see: f32,
    // `debug.alvr.problem_view` (U4): false while the play area is edited, the parameters (polled every
    // 500 ms, None: off) and the leak map that was uploaded last
    problem_view_allowed: bool,
    problem_view: (Instant, Option<ProblemViewParams>),
    #[cfg(target_os = "android")]
    problem_view_shown: Option<(
        std::sync::Arc<crate::auto_color_key::LeakMapState>,
        ProblemViewParams,
    )>,
    problem_view_on: bool,
    // Stage S0 (perf.rs): the swapchain format, `debug.alvr.ffr` at the start, whether the `stream:` line
    // was written (and the frames it waited for located views), the `perf:` line's window, the ablation
    // properties (`debug.alvr.dbg_*`) and when they were polled
    swapchain_format: u32,
    ffr: FfrOutcome,
    stream_logged: bool,
    stream_log_waits: u32,
    perf_log: Box<PerfLog>,
    last_perf_log: Instant,
    ablation: RenderAblation,
    last_ablation_poll: Instant,
    // Stage S2, `debug.alvr.edge_fill` (alvr_graphics edge_fill.rs, docs/edge-fill.md): the margin the game
    // layers' fov is widened by, tenths of a degree, polled with the ablation properties. 0 (the default):
    // the layers have the stream's fov, today's
    edge_fill: u32,
    // Stage S3 (alvr_graphics gpu_rect.rs, docs/gpu-optimization.md): `debug.alvr.gpu_rect` with its pad
    // (None: off) and `debug.alvr.fast_srgb`, polled with the ablation properties; the `rect:` line's window
    // and when it was last written. Unset (the default): today's renderer, no line
    gpu_rect: Option<GpuRectParams>,
    fast_srgb: bool,
    rect_stats: RectStats,
    last_rect_log: Instant,
    // `debug.alvr.green_boxes` (green_screen.rs): the green-screen key's properties, the camera learner's
    // share, the `green:` line. Unset (the default): nothing per frame
    green: GreenScreen,
}

impl StreamContext {
    pub fn new(
        core_ctx: Arc<ClientCoreContext>,
        xr_session: xr::Session<xr::OpenGlEs>,
        gfx_ctx: Rc<GraphicsContext>,
        interaction_ctx: Arc<RwLock<InteractionContext>>,
        config: ParsedStreamConfig,
    ) -> StreamContext {
        interaction_ctx
            .write()
            .select_sources(&config.interaction_sources);

        let xr_exts = xr_session.instance().exts();

        if xr_exts.fb_display_refresh_rate.is_some() {
            xr_session
                .request_display_refresh_rate(config.refresh_rate_hint)
                .unwrap();
        }

        let foveation_profile = if let Some(config) = &config.clientside_foveation_config
            && xr_exts.fb_swapchain_update_state.is_some()
            && xr_exts.fb_foveation.is_some()
            && xr_exts.fb_foveation_configuration.is_some()
        {
            let level;
            let dynamic;
            match config.mode {
                ClientsideFoveationMode::Static { level: lvl } => {
                    level = lvl;
                    dynamic = false;
                }
                ClientsideFoveationMode::Dynamic { max_level } => {
                    level = max_level;
                    dynamic = true;
                }
            };

            xr_session
                .create_foveation_profile(Some(xr::FoveationLevelProfile {
                    level: xr::FoveationLevelFB::from_raw(level as i32),
                    vertical_offset: config.vertical_offset_deg,
                    dynamic: xr::FoveationDynamicFB::from_raw(dynamic as i32),
                }))
                .ok()
        } else {
            None
        };

        let target_view_resolution = alvr_graphics::compute_target_view_resolution(
            config.view_resolution,
            &config.upscaling,
        );
        let format = graphics::swapchain_format(&gfx_ctx, &xr_session, config.enable_hdr);

        // `debug.alvr.ffr` (perf.rs): the fixed foveation of the eye swapchains, an experiment. Unset: the
        // swapchains as today (the profile of the server setting above stays unused, as it is since
        // upstream moved to the `openxr` crate without foveated swapchains, see docs/gpu-optimization.md)
        let ffr_params = FfrParams::read();
        let ffr_profile =
            ffr_params.map(|params| graphics::create_foveation_profile(&xr_session, &params));
        let mut ffr_errors = Vec::new();
        let swapchains = [0, 1].map(|_| match &ffr_profile {
            Some(Ok(profile)) => {
                let (swapchain, applied) = graphics::create_foveated_swapchain(
                    &xr_session,
                    &gfx_ctx,
                    target_view_resolution,
                    format,
                    profile,
                    ffr_params.is_some_and(|params| params.scaled_bin),
                );
                if let Err(e) = applied {
                    ffr_errors.push(e);
                }
                swapchain
            }
            _ => graphics::create_swapchain(
                &xr_session,
                &gfx_ctx,
                target_view_resolution,
                format,
                foveation_profile.as_ref(),
            ),
        });
        let ffr = match (ffr_params, ffr_profile) {
            (None, _) | (_, None) => FfrOutcome::Off,
            (Some(params), Some(Err(e))) => FfrOutcome::NotApplied(params, e),
            (Some(params), Some(Ok(_))) if !ffr_errors.is_empty() => {
                FfrOutcome::NotApplied(params, ffr_errors.join("; "))
            }
            (Some(params), Some(Ok(_))) => FfrOutcome::Applied(params),
        };

        let renderer = StreamRenderer::new(
            Rc::clone(&gfx_ctx),
            config.view_resolution,
            target_view_resolution,
            [
                swapchains[0]
                    .enumerate_images()
                    .unwrap()
                    .iter()
                    .map(|i| *i as _)
                    .collect(),
                swapchains[1]
                    .enumerate_images()
                    .unwrap()
                    .iter()
                    .map(|i| *i as _)
                    .collect(),
            ],
            format,
            config.foveated_encoding_config,
            !((core_ctx.platform().is_pico()
                || (core_ctx.platform() == Platform::SamsungGalaxyXR))
                && config.enable_hdr),
            // TODO: Find a driver heuristic for the limited range bug instead?
            core_ctx.platform() != Platform::SamsungGalaxyXR && !config.enable_hdr,
            config.encoding_gamma,
            config.upscaling.clone(),
        );

        let unextrapolated_poses = {
            let int_ctx = interaction_ctx.read();
            core_ctx.send_active_interaction_profile(
                *HAND_LEFT_ID,
                int_ctx.hands_interaction[0].controllers_profile_id,
                int_ctx.hands_interaction[0].input_ids.clone(),
            );
            core_ctx.send_active_interaction_profile(
                *HAND_RIGHT_ID,
                int_ctx.hands_interaction[1].controllers_profile_id,
                int_ctx.hands_interaction[1].input_ids.clone(),
            );

            int_ctx.has_extra_extension(
                crate::extra_extensions::META_HAND_TRACKING_UNEXTRAPOLATED_POSES_EXTENSION_NAME,
            )
        };

        let input_thread_running = Arc::new(RelaxedAtomic::new(false));
        let hands_withheld = Arc::new(RelaxedAtomic::new(false));

        let stage_reference_space = Arc::new(interaction::get_reference_space(
            &xr_session,
            xr::ReferenceSpaceType::STAGE,
        ));
        let view_reference_space = Arc::new(interaction::get_reference_space(
            &xr_session,
            xr::ReferenceSpaceType::VIEW,
        ));

        let mut this = StreamContext {
            use_custom_reprojection: core_ctx.platform().is_yvr(),
            core_context: core_ctx,
            xr_session,
            gfx_ctx,
            interaction_context: interaction_ctx,
            stage_reference_space,
            view_reference_space,
            swapchains,
            last_good_video_frame_metadata: VideoFrameMetadata {
                view_params: [ViewParams::DUMMY; 2],
                foveation_center_shifts: None,
            },
            input_thread: None,
            input_thread_running,
            hands_withheld,
            config,
            target_view_resolution,
            renderer,
            decoder: None,
            environment_depth: None,
            environment_depth_failed: false,
            color_key: None,
            #[cfg(target_os = "android")]
            auto_color_key: None,
            arm_params: ArmParams::read(),
            last_arm_log: Instant::now(),
            // The settings at the start are logged once
            arm_params_changed: true,
            arm_log: Default::default(),
            arm_histories: Default::default(),
            unextrapolated_poses,
            arm_samples: Default::default(),
            arm_samples_recording: false,
            mask_params: None,
            lever_params: LeverParams::default(),
            levers: LeverWindows::default(),
            last_lever_log: Instant::now(),
            memory_params: None,
            last_known_joints: [None; 2],
            memory_log: MemoryLog::default(),
            last_memory_log: Instant::now(),
            cloud_params: None,
            cloud_ab_start: Instant::now(),
            cloud_log: CloudLog::default(),
            last_cloud_log: Instant::now(),
            cloud_leak: false,
            key_wrists: Cell::new([None; 2]),
            published_lut: None,
            lut_uploads: 0,
            color_key_version: 0,
            playzone_auto: true,
            last_property_poll: Instant::now(),
            last_dump_value: None,
            pending_dump: None,
            burst: BurstRecorder::new(),
            burst_dump: None,
            depth_capture_time: None,
            hand_bounds: Cell::new([None; 2]),
            last_hand_rects: [None; 2],
            last_hand_rect_time: None,
            last_hands_seen: Instant::now(),
            last_playzone_capture: Instant::now(),
            // A value left from before is not a request
            last_playzone_request: debug_property("debug.alvr.playzone").unwrap_or(0),
            playzone_auto_series: false,
            last_playzone_log: Instant::now(),
            hand_velocities: [const { Cell::new(Vec3::ZERO) }; 2],
            hand_solid: [const { Cell::new([0.0; HAND_SEGMENTS]) }; 2],
            bridge_solid: [const { Cell::new(0.0) }; 2],
            hand_leak: [const { Cell::new(0.0) }; 2],
            last_leak_log: Cell::new(Instant::now()),
            leak_lookup_time: Cell::new((Duration::ZERO, 0)),
            region_override: None,
            hands_in_region: false,
            area_overlay: Vec::new(),
            game_dim: 0.0,
            game_see: 0.0,
            problem_view_allowed: true,
            problem_view: (Instant::now(), None),
            #[cfg(target_os = "android")]
            problem_view_shown: None,
            problem_view_on: false,
            swapchain_format: format,
            ffr,
            stream_logged: false,
            stream_log_waits: 0,
            perf_log: Box::default(),
            last_perf_log: Instant::now(),
            ablation: RenderAblation::default(),
            last_ablation_poll: Instant::now(),
            edge_fill: 0,
            gpu_rect: None,
            fast_srgb: false,
            rect_stats: RectStats::default(),
            last_rect_log: Instant::now(),
            green: GreenScreen::default(),
        };
        // From the first frame
        this.poll_ablation(true);

        this.update_reference_space(None);

        this
    }

    pub fn uses_depth_passthrough(&self) -> bool {
        alvr_graphics::depth_passthrough_config(self.config.passthrough.as_ref()).is_some()
    }

    /// `version`: of the loaded key (`debug.alvr.color_key`), a new one shows the table of its file
    pub fn set_color_key(&mut self, params: Option<ColorKeyParams>, version: u32) {
        // The depth map shows the hands where they were a while ago. With the color key they are not
        // needed there. `debug.alvr.hand_removal 0` asks the runtime to keep them, but on 2026-11-24 it
        // still removed them (captures 626 / 627): a probe, the answer of the runtime is logged
        let removal = params.is_some_and(|params| params.hand_removal);
        if self.color_key.is_some_and(|params| params.hand_removal) != removal
            && let Some(depth) = &self.environment_depth
        {
            let answer = depth.set_hand_removal(removal);
            crate::stream_log(&format!(
                "hand removal from the depth map: {}: the runtime answered {answer:?}",
                if removal { "on" } else { "off" }
            ));
        }
        self.color_key = params;
        // Without the key no learned table is shown. With it again, or after a reload (a new version),
        // the table of the file is shown until the learner publishes one
        if self.color_key.is_none() || version != self.color_key_version {
            self.published_lut = None;
        }
        self.color_key_version = version;

        #[cfg(target_os = "android")]
        if self.color_key.is_none() {
            self.auto_color_key = None;
        } else if self.auto_color_key.is_none() && crate::auto_color_key::enabled() {
            self.auto_color_key = Some(AutoColorKey::new(
                Arc::clone(&self.stage_reference_space),
                Arc::clone(&self.view_reference_space),
                Arc::clone(&self.interaction_context),
                self.burst.camera_sink(),
                self.green.camera(),
            ));
        }
    }

    /// `debug.alvr.green_boxes`: what lib.rs submitted in the frame before (for the `green:` line)
    pub fn report_green_layers(&mut self, layers: GreenLayers) {
        self.green.report_layers(layers);
    }

    // `debug.alvr.green_boxes` (green_screen.rs): the zone of this frame and the `green:` line, None while
    // the property is unset. `views`: the pose and fov the game layers are submitted with
    fn green_frame(&mut self, views: &[ViewParams; 2]) -> Option<GreenFrame> {
        self.green.poll();
        let conditions = GreenConditions {
            depth: self.uses_depth_passthrough(),
            color_key: self.color_key.is_some(),
            custom_reprojection: self.use_custom_reprojection,
        };
        let frame = self
            .green
            .frame(conditions, self.region_override.as_ref(), views)?;
        #[cfg(target_os = "android")]
        let learner = self.auto_color_key.is_some();
        #[cfg(not(target_os = "android"))]
        let learner = false;
        self.green
            .log(&frame, learner, self.region_override.as_ref());

        Some(frame)
    }

    /// The play area placed from a marker on the rig (its boxes), used for the depth mask instead of
    /// the region of the settings. None: the settings. `hands_in_region`: the hands of the color key
    /// show only inside the area (`debug.alvr.marker_hands`); the region of the settings never limits
    /// them
    pub fn set_region_override(&mut self, region: Option<PlacedArea>, hands_in_region: bool) {
        self.region_override = region;
        self.hands_in_region = hands_in_region;
    }

    /// Lines drawn over the stream in this and the next frames, stage space. Empty: none (nothing is
    /// uploaded or drawn). Only with Passthrough = Depth
    pub fn set_area_overlay(&mut self, segments: &[OverlaySegment]) {
        self.area_overlay.clear();
        self.area_overlay.extend_from_slice(segments);
    }

    /// How much the game is dimmed (0: not, 1: black), while the play area is edited. Only with
    /// Passthrough = Depth
    pub fn set_game_dim(&mut self, dim: f32) {
        self.game_dim = dim;
    }

    /// How much the real room shows through the game where the mask does not show it (0: not, 1: no
    /// game), while the play area is edited. Only with Passthrough = Depth
    pub fn set_game_see(&mut self, see: f32) {
        self.game_see = see;
    }

    /// `debug.alvr.problem_view` is never shown while the play area is edited
    pub fn allow_problem_view(&mut self, allowed: bool) {
        self.problem_view_allowed = allowed;
    }

    // `debug.alvr.problem_view`: upload the leak map when it changes (all unknown until it has one),
    // turn the view off once when it stops. Nothing is written while it stays off
    fn update_problem_view(&mut self) {
        if self.problem_view.0.elapsed() > Duration::from_millis(500) {
            self.problem_view = (Instant::now(), ProblemViewParams::load());
        }
        let Some(params) = self.problem_view.1.filter(|_| self.problem_view_allowed) else {
            if self.problem_view_on {
                self.problem_view_on = false;
                #[cfg(target_os = "android")]
                {
                    self.problem_view_shown = None;
                }
                self.renderer.set_problem_view(None);
            }
            return;
        };

        #[cfg(target_os = "android")]
        let state = self.auto_color_key.as_ref().and_then(|key| key.leak_map());
        #[cfg(target_os = "android")]
        let is_new = match (&state, &self.problem_view_shown) {
            (Some(state), Some((shown, shown_params))) => {
                !std::sync::Arc::ptr_eq(shown, state) || *shown_params != params
            }
            (None, None) => !self.problem_view_on,
            _ => true,
        };
        #[cfg(not(target_os = "android"))]
        let is_new = !self.problem_view_on;
        if !is_new {
            return;
        }

        #[cfg(target_os = "android")]
        let (anchor, cells) = match &state {
            Some(state) => (
                state.snapshot.anchor,
                alvr_graphics::problem_cells(&state.snapshot, params.lo, params.hi),
            ),
            None => (
                Vec3::ZERO,
                vec![alvr_graphics::LEAK_UNKNOWN; alvr_graphics::LEAK_MAP_CELLS],
            ),
        };
        #[cfg(not(target_os = "android"))]
        let (anchor, cells) = (
            Vec3::ZERO,
            vec![alvr_graphics::LEAK_UNKNOWN; alvr_graphics::LEAK_MAP_CELLS],
        );
        self.renderer
            .set_problem_view(Some(alvr_graphics::ProblemViewData {
                anchor,
                cells: &cells,
                room: params.room,
                dim: params.dim,
            }));
        #[cfg(target_os = "android")]
        {
            self.problem_view_shown = state.map(|state| (state, params));
        }
        self.problem_view_on = true;
    }

    /// While true the tracked hands are not sent to the PC: no hand skeletons, and no hand pose of a
    /// hand that is tracked as a hand (controllers are sent as before). The PC then sees the hands as
    /// not tracked, as when they are out of view: its hand gestures (hand_tracking_interaction) and the
    /// SteamVR hands see none of the pinches of the play area editor
    pub fn withhold_hands(&self, withheld: bool) {
        self.hands_withheld.set(withheld);
    }

    /// The eyes and the hands at `time` (stage space) for the play area editor. None: not Passthrough =
    /// Depth, or no head pose. A hand without a valid pose of its thumb tip, index tip and palm is None.
    /// The aim pose of a hand (XR_FB_hand_tracking_aim) when the runtime gives a valid one
    pub fn editor_input(&self, time: xr::Time) -> Option<EditorInput> {
        if !self.uses_depth_passthrough() {
            return None;
        }
        let head = self
            .view_reference_space
            .locate(&self.stage_reference_space, time)
            .ok()?;
        let valid =
            xr::SpaceLocationFlags::POSITION_VALID | xr::SpaceLocationFlags::ORIENTATION_VALID;
        if !head.location_flags.contains(valid) {
            return None;
        }
        let head = crate::from_xr_pose(head.pose);
        let int_ctx = self.interaction_context.read();

        let hands = [0, 1].map(|i| {
            let tracker = int_ctx.hands_interaction[i].skeleton_tracker.as_ref()?;
            // With XR_FB_hand_tracking_aim also the ray of the system pointer
            let (joints, aim) = crate::extra_extensions::locate_hand_joints_with_aim(
                &self.xr_session,
                tracker,
                &self.stage_reference_space,
                time,
            )
            .ok()
            .flatten()?;
            let thumb = joints[xr::HandJointEXT::THUMB_TIP];
            let index = joints[xr::HandJointEXT::INDEX_TIP];
            let palm = joints[xr::HandJointEXT::PALM];
            let has = |joint: &xr::HandJointLocation, flags| joint.location_flags.contains(flags);
            if ![&thumb, &index, &palm]
                .iter()
                .all(|&joint| has(joint, xr::SpaceLocationFlags::POSITION_VALID))
            {
                return None;
            }
            // The palm faces its -Y (XR_EXT_hand_tracking: +Y out of the back of the hand)
            // ... and its fingers point along its -Z
            let (palm_normal, finger_direction) =
                if has(&palm, xr::SpaceLocationFlags::ORIENTATION_VALID) {
                    let orientation = crate::from_xr_quat(palm.pose.orientation);
                    (orientation * Vec3::NEG_Y, orientation * Vec3::NEG_Z)
                } else {
                    (Vec3::ZERO, Vec3::ZERO)
                };

            Some(EditorHand {
                thumb_tip: crate::from_xr_vec3(thumb.pose.position),
                index_tip: crate::from_xr_vec3(index.pose.position),
                tip_radii: thumb.radius + index.radius,
                palm: crate::from_xr_vec3(palm.pose.position),
                palm_normal,
                tracked: [&thumb, &index]
                    .iter()
                    .all(|&joint| has(joint, xr::SpaceLocationFlags::POSITION_TRACKED)),
                finger_direction,
                // The pointer looks along -Z of the aim pose
                aim: aim.filter(|aim| aim.valid).and_then(|aim| {
                    HandRay::new(
                        crate::from_xr_vec3(aim.pose.position),
                        crate::from_xr_quat(aim.pose.orientation) * Vec3::NEG_Z,
                    )
                }),
            })
        });

        Some(EditorInput {
            eye: head.position,
            forward: head.orientation * Vec3::NEG_Z,
            hands,
        })
    }

    /// New colors for the key, learned from the camera. The caller uploads them: a copy is kept for
    /// the next burst
    pub fn take_color_key_lut(&mut self) -> Option<Vec<u8>> {
        #[cfg(target_os = "android")]
        let lut = self.auto_color_key.as_ref()?.take_lut();
        #[cfg(not(target_os = "android"))]
        let lut: Option<Vec<u8>> = None;

        if let Some(lut) = &lut {
            self.lut_uploads += 1;
            self.published_lut = Some(BurstLut {
                version: self.lut_uploads,
                published_ns: runtime_now_ns(&self.xr_session),
                data: lut.clone(),
            });
        }

        lut
    }

    // Objects that are held are partly missing in the depth image, so remember how the playzone looks
    // without hands: automatically when no hand was near the view of the depth camera for a while (the
    // runtime reports hands as tracked also when they are out of view), or on request. Whether to
    // remember the depth image of this frame
    fn should_capture_playzone(&mut self, frame: Option<&DepthFrame>) -> bool {
        let request = debug_property("debug.alvr.playzone").unwrap_or(0);
        let is_new_request = request != self.last_playzone_request;
        // A positive request waits for a depth image
        if is_new_request && (request <= 0 || frame.is_some()) {
            self.last_playzone_request = request;
            // A negative value forgets the playzone
            if request < 0 {
                self.renderer.clear_playzone();
                self.playzone_auto_series = false;
                crate::stream_log("playzone: forgotten (requested)");
            }
        }
        // The room memory (`debug.alvr.mem_roll`) replaces the copy while it is on: no copy is asked for and
        // no `playzone:` line written (the hands near the depth view are still followed: it anchors by them)
        let memory_on = self.memory_params.is_some();
        let is_requested = is_new_request && request > 0 && !memory_on;

        let Some(frame) = frame else {
            if is_requested {
                self.log_playzone_wait("requested, no depth image");
            }
            return false;
        };

        let hands_in_view = self.hand_bounds.get().iter().flatten().any(|bounds| {
            frame.depth_views.iter().any(|view| {
                alvr_graphics::sphere_in_view(
                    view,
                    bounds.truncate(),
                    bounds.w + PLAYZONE_HAND_MARGIN,
                )
            })
        });
        if hands_in_view {
            self.last_hands_seen = Instant::now();
            self.playzone_auto_series = false;
        }

        let is_auto_on = self.color_key.is_some() && self.playzone_auto && !memory_on;
        let is_automatic = is_auto_on
            && self.last_hands_seen.elapsed() > PLAYZONE_HANDS_TIMEOUT
            && self.last_playzone_capture.elapsed() > PLAYZONE_INTERVAL;

        if is_requested {
            crate::stream_log(&format!(
                "playzone: remembered (requested {request}), a hand was near the depth view {:.1} s ago",
                self.last_hands_seen.elapsed().as_secs_f32()
            ));
        } else if is_automatic && !self.playzone_auto_series {
            crate::stream_log(&format!(
                "playzone: remembered (automatic, no hand near the depth view for {:.1} s), again \
                 every {} ms while none is",
                PLAYZONE_HANDS_TIMEOUT.as_secs_f32(),
                PLAYZONE_INTERVAL.as_millis()
            ));
        } else if is_auto_on && self.renderer.playzone_age().is_none() {
            let reason = if hands_in_view {
                "a hand is near the depth view".into()
            } else {
                format!(
                    "no hand near the depth view for {:.1} s",
                    self.last_hands_seen.elapsed().as_secs_f32()
                )
            };
            self.log_playzone_wait(&reason);
        }

        let capture = is_requested || is_automatic;
        if capture {
            self.last_playzone_capture = Instant::now();
            self.playzone_auto_series |= is_automatic;
        }

        capture
    }

    fn log_playzone_wait(&mut self, reason: &str) {
        if self.last_playzone_log.elapsed() > PLAYZONE_LOG_INTERVAL {
            self.last_playzone_log = Instant::now();
            crate::stream_log(&format!("playzone: none remembered, {reason}"));
        }
    }

    // `debug.alvr.arm_shift` (with the other `arm_*`, also `arm_gap_*`), `debug.alvr.edge_cut` (logged on each
    // change), `debug.alvr.playzone_auto`, and a new `debug.alvr.key_dump` (like the learner, which writes the camera
    // image: the depth of the next frame with a depth image)
    fn poll_properties(&mut self) {
        if self.last_property_poll.elapsed() < PROPERTY_POLL_INTERVAL {
            return;
        }
        self.last_property_poll = Instant::now();

        let arm_params = ArmParams::read();
        if arm_params != self.arm_params {
            if arm_params.changes_sample_times(&self.arm_params) {
                for (history, samples) in self.arm_histories.iter_mut().zip(&mut self.arm_samples) {
                    samples.reset |= history.clear();
                }
            }
            self.arm_params = arm_params;
            self.arm_params_changed = true;
        }
        let mask_params = MaskParams::read();
        if self.mask_params != Some(mask_params) {
            self.mask_params = Some(mask_params);
            crate::stream_log(&mask_params.text());
        }
        // Logged on each change (unset: never, as before). A change shuts every window: one open when
        // switched off would otherwise show again, with no hand, when switched on
        let lever_params = LeverParams::read();
        if lever_params != self.lever_params {
            crate::stream_log(&lever_params.text());
            self.lever_params = lever_params;
            self.levers.shut_all();
        }
        // Logged on each change (unset: never, as before). Off, the samples keep no joints
        let memory_params = room_memory::read_params();
        if memory_params != self.memory_params {
            crate::stream_log(&room_memory::params_text(memory_params.as_ref()));
            self.memory_params = memory_params;
            self.last_memory_log = Instant::now();
        }
        // Logged on each change (unset: never, as before). `cloud_ab` starts with the cloud when it is
        // switched on or the alternation is
        let cloud_params = motion_cloud::read_params();
        if cloud_params != self.cloud_params {
            crate::stream_log(&motion_cloud::params_text(cloud_params.as_ref()));
            let was_ab = self.cloud_params.is_some_and(|params| params.ab);
            if self.cloud_params.is_none() {
                self.cloud_log = CloudLog::default();
                self.last_cloud_log = Instant::now();
            }
            if self.cloud_params.is_none()
                || cloud_params.is_some_and(|params| params.ab && !was_ab)
            {
                self.cloud_ab_start = Instant::now();
            }
            self.cloud_params = cloud_params;
        }
        self.cloud_leak = debug_property("debug.alvr.cloud_leak").unwrap_or(0) > 0;
        self.playzone_auto = debug_property("debug.alvr.playzone_auto").unwrap_or(1) > 0;

        let dump = debug_property("debug.alvr.key_dump").unwrap_or(0);
        if self.last_dump_value.is_some_and(|last| last != dump)
            && dump != 0
            && let Some((replaced, _)) = self.pending_dump.replace((dump, Instant::now()))
        {
            crate::stream_log(&format!("depth dump {replaced}: replaced by {dump}"));
        }
        self.last_dump_value = Some(dump);

        // A new burst also asks for a depth dump, written into its folder
        let depth_size = self
            .environment_depth
            .as_ref()
            .map_or(UVec2::ZERO, |depth| depth.resolution());
        let flip_y = alvr_graphics::depth_passthrough_config(self.config.passthrough.as_ref())
            .is_some_and(|config| config.flip_depth_y);
        let xr_session = &self.xr_session;
        if let Some(n) = self.burst.poll(
            || crate::xr_runtime_now(xr_session.instance()).map(|time| time.as_nanos()),
            depth_size,
            flip_y,
        ) {
            if let Some((replaced, _)) = self.pending_dump.replace((n, Instant::now())) {
                crate::stream_log(&format!("depth dump {replaced}: replaced by burst {n}"));
            }
            self.burst_dump = Some(n);
            self.burst.set_dump(format!("alvr_dump_{n}_depth.json"));
            self.burst.set_key_lut(self.published_lut.clone());
        }
    }

    // `debug.alvr.dbg_*` (perf.rs, alvr_graphics::RenderAblation): every 0.5 s, in any passthrough mode, given
    // to the renderer at once. A change is logged (`ablation:`); the values at the start are in the
    // `stream:` line
    fn poll_ablation(&mut self, start: bool) {
        if !start && self.last_ablation_poll.elapsed() < PROPERTY_POLL_INTERVAL {
            return;
        }
        self.last_ablation_poll = Instant::now();
        let ablation = perf::read_ablation();
        if ablation != self.ablation {
            if !start {
                crate::stream_log(&format!("ablation: {} (new settings)", ablation.text()));
            }
            self.ablation = ablation;
            self.renderer.set_ablation(ablation);
        }

        // `debug.alvr.edge_fill`: live, each frame's layers carry their own fov. The value at the start is
        // on the `stream:` line, a change is logged. Not on the custom reprojection path (YVR): it draws
        // into the display-time view, and the layer crops are off there anyway
        let edge_fill = alvr_graphics::edge_fill_tenths(debug_property("debug.alvr.edge_fill"));
        if edge_fill != self.edge_fill {
            if !start {
                let fov = self.last_good_video_frame_metadata.view_params[0].fov;
                let known = fov.right > fov.left && fov.up > fov.down;
                crate::stream_log(&alvr_graphics::edge_fill_text(
                    edge_fill,
                    self.use_custom_reprojection,
                    known.then_some(fov),
                ));
            }
            self.edge_fill = edge_fill;
            self.renderer
                .set_edge_fill(edge_fill > 0 && !self.use_custom_reprojection);
        }

        // Stage S3, `debug.alvr.gpu_rect` and `debug.alvr.fast_srgb`: live. The values at the start are on the
        // `stream:` line, a change is logged (`gpu work:`)
        let (gpu_rect, fast_srgb) = perf::read_gpu_work();
        if gpu_rect != self.gpu_rect || fast_srgb != self.fast_srgb {
            if !start {
                crate::stream_log(&format!(
                    "gpu work: {} (new settings)",
                    alvr_graphics::gpu_work_text(gpu_rect.as_ref(), fast_srgb)
                ));
            }
            if gpu_rect.is_none() {
                self.renderer.set_gpu_rect(None);
            }
            self.gpu_rect = gpu_rect;
            self.fast_srgb = fast_srgb;
            self.renderer.set_fast_srgb(fast_srgb);
            self.rect_stats = RectStats::default();
            self.last_rect_log = Instant::now();
        }
    }

    // `debug.alvr.gpu_rect`: what the renderer is asked for in this frame. The lower key layer is cropped to
    // the active rectangle only where both game layers are submitted with `key_crop 0` (the crop of
    // `key_crop` stays as it is), not on the custom reprojection path. The full view while the area editor
    // is open or a burst runs
    fn gpu_rect_request(&self, hands_in_view: bool) -> Option<GpuRectRequest> {
        let params = self.gpu_rect?;
        let crop_lower = self.color_key.is_some_and(|key| {
            key.crop == 0 && key.layers != 30 && key.extra_layers(hands_in_view)
        }) && !self.use_custom_reprojection;
        let hold = if !self.problem_view_allowed {
            Some(RectFallback::Editor)
        } else if self.burst.is_active() {
            Some(RectFallback::Burst)
        } else {
            None
        };
        Some(GpuRectRequest {
            params,
            crop_lower,
            hold,
        })
    }

    // `debug.alvr.gpu_rect`: the `rect:` line every `PERF_LOG_INTERVAL` while it is on and the stream is Depth
    fn record_gpu_rect(&mut self) {
        if self.gpu_rect.is_none() {
            return;
        }
        if let Some(frame) = self.renderer.gpu_rect_frame() {
            self.rect_stats.add(&frame);
        }
        if self.last_rect_log.elapsed() >= perf::PERF_LOG_INTERVAL {
            self.last_rect_log = Instant::now();
            if let Some(text) = self.rect_stats.text() {
                crate::stream_log(&text);
            }
            self.rect_stats = RectStats::default();
        }
    }

    // `debug.alvr.edge_fill` as applied: the margin (radians), None: off
    fn edge_fill_margin(&self) -> Option<f32> {
        (self.edge_fill > 0 && !self.use_custom_reprojection)
            .then(|| alvr_graphics::edge_fill_radians(self.edge_fill))
    }

    // The `stream:` line, once per stream at its first frame with located views (after 2 s without, with
    // the fov "not located")
    fn log_stream_facts(&mut self, views: Option<&[xr::View]>) {
        if self.stream_logged {
            return;
        }
        self.stream_log_waits += 1;
        if views.is_none() && self.stream_log_waits < 144 {
            return;
        }
        self.stream_logged = true;

        let exts = self.xr_session.instance().exts();
        let refresh_now = exts
            .fb_display_refresh_rate
            .and_then(|_| self.xr_session.get_display_refresh_rate().ok());
        let depth = alvr_graphics::depth_passthrough_config(self.config.passthrough.as_ref());
        let text = perf::stream_text(&StreamFacts {
            view_resolution: self.config.view_resolution,
            swapchain_resolution: self.target_view_resolution,
            swapchain_format: self.swapchain_format,
            upscale_factor: self
                .config
                .upscaling
                .as_ref()
                .map(|upscaling| upscaling.upscale_factor),
            foveated_encoding: self.config.foveated_encoding_config.as_ref(),
            refresh_hint: self.config.refresh_rate_hint,
            refresh_now,
            fovs: views.map(|views| [0, 1].map(|i| crate::from_xr_fov(views[i].fov))),
            mask_size: self.renderer.depth_mask_resolution(),
            passthrough: self.config.passthrough.as_ref().map(|mode| match mode {
                PassthroughMode::Blend { .. } => "Blend",
                PassthroughMode::RgbChromaKey(_) => "RgbChromaKey",
                PassthroughMode::HsvChromaKey(_) => "HsvChromaKey",
                PassthroughMode::Depth(_) => "Depth",
            }),
            lighting: depth.map(|config| config.lighting.as_option().is_some()),
            key: self.color_key.map(|params| (params.layers, params.crop)),
            ablation: &self.ablation,
            ffr: &self.ffr,
            edge_fill: self.edge_fill,
            custom_reprojection: self.use_custom_reprojection,
            gpu_rect: self.gpu_rect,
            fast_srgb: self.fast_srgb,
        });
        crate::stream_log(&text);
    }

    // The `perf:` line: this display frame (`new_frame`: the timestamp of a new decoded frame), the
    // client's statistics of a new frame, the line every `PERF_LOG_INTERVAL`
    fn record_perf(
        &mut self,
        vsync_time: Duration,
        new_frame: Option<Duration>,
        rendered: &[ViewParams; 2],
        views: Option<&[xr::View]>,
    ) {
        // The drawn frame's pose is known once a frame was drawn (before: the dummy views)
        let has_frame = new_frame.is_some() || self.perf_log.has_frame();
        let turn = views.filter(|_| has_frame).map(|views| {
            perf::turn_deg(
                rendered[0].pose.orientation,
                crate::from_xr_quat(views[0].pose.orientation),
            )
        });
        self.perf_log.display_frame(
            vsync_time.as_nanos() as i64,
            new_frame.map(|timestamp| timestamp.as_nanos() as i64),
            turn,
        );
        if let Some(timestamp) = new_frame
            && let Some(stats) = self.core_context.frame_statistics(timestamp)
        {
            self.perf_log.latencies(&FrameLatencies {
                decode: stats.video_decode,
                decoder_queue: stats.video_decoder_queue,
                total: stats.total_pipeline_latency,
            });
        }

        let elapsed = self.last_perf_log.elapsed();
        if elapsed >= perf::PERF_LOG_INTERVAL {
            self.last_perf_log = Instant::now();
            let text = self.perf_log.text(
                elapsed.as_secs_f32(),
                self.core_context.get_total_prediction_offset(),
            );
            crate::stream_log(&text);
        }
    }

    // `debug.alvr.burst`: the joints of both hands at `time` (stage space), as the key locates them
    fn burst_hands(&self, time: xr::Time) -> [Option<BurstHand>; 2] {
        let int_ctx = self.interaction_context.read();

        [0, 1].map(|i| {
            let tracker = int_ctx.hands_interaction[i].skeleton_tracker.as_ref()?;
            let (joints, velocities) = self
                .stage_reference_space
                .relate_hand_joints(tracker, time)
                .ok()
                .flatten()?;

            Some(std::array::from_fn(|j| DumpJoint {
                position: crate::from_xr_vec3(joints[j].pose.position),
                radius: joints[j].radius,
                tracked: joints[j]
                    .location_flags
                    .contains(xr::SpaceLocationFlags::POSITION_TRACKED),
                velocity: crate::from_xr_vec3(velocities[j].linear_velocity),
                velocity_valid: velocities[j]
                    .velocity_flags
                    .contains(xr::SpaceVelocityFlags::LINEAR_VALID),
            }))
        })
    }

    // `debug.alvr.burst`: the depth frame (if new: its readback is asked for) and the display frame
    fn record_burst_frame(
        &mut self,
        display_time: xr::Time,
        depth_input: &DepthRenderInput,
        headset_views: &[xr::View],
        stream_views: [ViewParams; 2],
    ) {
        let now_ns = runtime_now_ns(&self.xr_session);
        if let Some(frame) = &depth_input.frame
            && let Some(token) = self.burst.depth_frame(
                frame,
                self.depth_capture_time.map(|time| time.as_nanos()),
                display_time.as_nanos(),
                now_ns,
            )
        {
            self.renderer.request_burst_depth(token);
        }

        let delay = self
            .color_key
            .map_or(0, |params| params.passthrough_delay_ms * 1_000_000);
        let hand_ns = display_time.as_nanos() - delay;
        let hands = self.burst_hands(xr::Time::from_nanos(hand_ns));
        let region = alvr_graphics::depth_passthrough_config(self.config.passthrough.as_ref())
            .map_or("null".into(), |config| {
                alvr_graphics::region_json(depth_input.region_override.as_ref(), config)
            });
        let views = [0, 1].map(|i| {
            headset_views
                .get(i)
                .map_or(ViewParams::DUMMY, |view| ViewParams {
                    pose: crate::from_xr_pose(view.pose),
                    fov: crate::from_xr_fov(view.fov),
                })
        });
        self.burst.display_frame(burst::DisplayInput {
            display_ns: display_time.as_nanos(),
            now_ns,
            views,
            stream_views,
            eye_positions: depth_input.eye_positions,
            has_depth: depth_input.frame.is_some(),
            hand_ns,
            hands,
            color_key: depth_input.color_key.as_ref(),
            arms: depth_input.arms,
            arm_gaps: depth_input.arm_gaps,
            arm_gap_set_mm: [0, 1].map(|i| {
                self.mask_params
                    .filter(|params| params.gap(i).is_some())
                    .map_or(0.0, |params| params.gap_mm as f32)
            }),
            edge_cut: depth_input.edge_cut,
            arm_samples: std::mem::take(&mut self.arm_samples),
            region,
        });
    }

    // The arm's own sample history (`arm_history.rs`): the newest hand sample of each hand, kept if it is
    // new. With XR_META_hand_tracking_unextrapolated_poses a lookup at now asks for the sample as it is and
    // its capture time. Without it a lookup `ARM_SAMPLE_LOOKUP_AGE` before now returns the sample (the
    // runtime gives its newest sample for any time before it), timed by `arm_sample_age_ms`. The runtime is
    // never asked for the hand at a past time. A hand that is not located empties its history: where it
    // comes back the arm starts again, with no velocity from the old place
    fn update_arm_histories(&mut self) {
        let params = self.arm_params;
        let now_ns = runtime_now_ns(&self.xr_session);
        let unextrapolated = self.unextrapolated_poses && params.capture_time;
        let lookup_ns = if unextrapolated {
            now_ns
        } else {
            now_ns - ARM_SAMPLE_LOOKUP_AGE.as_nanos() as i64
        };
        let age_ns = params.sample_age_ms * 1_000_000;
        // Samples for bursts only while one records (no allocation otherwise). Its first record gets every
        // sample kept, so that the capture-time wrists of its first frames can be rebuilt
        let recording = self.burst.is_recording();
        let records_history = recording && !self.arm_samples_recording;
        self.arm_samples_recording = recording;
        // The room memory needs the capsules at the depth's capture time
        let keeps_joints = self.memory_params.is_some();
        let int_ctx = self.interaction_context.read();

        for i in 0..2 {
            let history = &mut self.arm_histories[i];
            let samples = &mut self.arm_samples[i];
            samples.estimator = if params.is_new(i) {
                params.estimator.value()
            } else {
                -1
            };
            // Counted only while the `arm:` line reports this arm, which empties the counts
            let log = params.is_new(i).then_some(&mut self.arm_log[i]);

            let located = int_ctx.hands_interaction[i]
                .skeleton_tracker
                .as_ref()
                .and_then(|tracker| {
                    crate::extra_extensions::locate_hand_sample(
                        &self.xr_session,
                        tracker,
                        &self.stage_reference_space,
                        xr::Time::from_nanos(lookup_ns),
                        unextrapolated,
                    )
                    .ok()
                    .flatten()
                });
            let sample = located.and_then(|(joints, capture_time)| {
                let position = |joint: xr::HandJointEXT| {
                    let joint = joints[joint];
                    joint
                        .location_flags
                        .contains(xr::SpaceLocationFlags::POSITION_VALID)
                        .then(|| crate::from_xr_vec3(joint.pose.position))
                };

                // All of them or none
                let all_joints = keeps_joints
                    .then(|| {
                        let mut all = [Vec4::ZERO; 26];
                        for (joint, location) in all.iter_mut().zip(joints.iter()) {
                            if !location
                                .location_flags
                                .contains(xr::SpaceLocationFlags::POSITION_VALID)
                            {
                                return None;
                            }
                            *joint =
                                crate::from_xr_vec3(location.pose.position).extend(location.radius);
                        }
                        Some(all)
                    })
                    .flatten();

                Some((
                    position(xr::HandJointEXT::WRIST)?,
                    position(xr::HandJointEXT::MIDDLE_PROXIMAL)?,
                    capture_time.map(|time| time.as_nanos()),
                    all_joints,
                ))
            });
            let Some((wrist, knuckle, capture_ns, all_joints)) = sample else {
                if history.clear() {
                    samples.reset = true;
                    if let Some(log) = log {
                        log.resets += 1;
                    }
                }
                continue;
            };

            let previous_ns = history.newest().map(|sample| sample.time_ns);
            let Some(sample) =
                history.push_joints(now_ns, wrist, knuckle, capture_ns, age_ns, all_joints)
            else {
                continue;
            };
            if recording {
                samples.new.push(burst_arm_sample(&sample));
            }
            if let Some(log) = log {
                log.samples += 1;
                if let Some(previous_ns) = previous_ns {
                    log.intervals_ms
                        .push((sample.time_ns - previous_ns) as f32 * 1e-6);
                }
                if sample.from_capture_time {
                    log.ages_ms
                        .push((sample.seen_ns - sample.time_ns) as f32 * 1e-6);
                }
            }
        }

        if records_history {
            for (history, samples) in self.arm_histories.iter().zip(&mut self.arm_samples) {
                samples.new = history.samples().map(burst_arm_sample).collect();
            }
        }
    }

    // How the arms moved since the depth image was captured. None if `debug.alvr.arm_shift` is 0, or
    // while the zone of `debug.alvr.cloud 1` shows (`cloud`: of this frame, motion_cloud.rs)
    fn arm_motions(
        &mut self,
        display_time: xr::Time,
        capture_time: xr::Time,
        views: &[xr::View],
        cloud: Option<&CloudInput>,
    ) -> [Option<ArmMotion>; 2] {
        // Bigger displacements are tracking errors: plain depth for this arm
        const MAX_DISPLACEMENT: f32 = 0.25;
        // Rough position of the elbows, relative to the head
        const ELBOW_OFFSET: Vec3 = Vec3::new(0.2, -0.5, 0.05);

        let params = self.arm_params;
        if params.shift == 0 || !motion_cloud::arm_shift_on(cloud, self.cloud_zone_shown(cloud)) {
            return [None; 2];
        }

        let display_ns = display_time.as_nanos();
        let capture_ns = capture_time.as_nanos();
        // Mode 1: the wrist for the moment the passthrough image shows, the newest estimate of the
        // runtime (late while the arm moves)
        let delay = self
            .color_key
            .map_or(0, |params| params.passthrough_delay_ms * 1_000_000);
        let now_ns = display_ns - delay;
        // Mode 2: the line and the parabola place the arm for the target time T
        let target_ns = params.target_ns(display_ns);
        let weight_len = params.weight_mm as f32 / 1000.0;
        let int_ctx = self.interaction_context.read();

        let head_position = (crate::from_xr_vec3(views[0].pose.position)
            + crate::from_xr_vec3(views[1].pose.position))
            / 2.0;
        let forward = crate::from_xr_quat(views[0].pose.orientation) * Vec3::NEG_Z;
        let yaw = Quat::from_rotation_y(f32::atan2(-forward.x, -forward.z));

        [0, 1].map(|i| {
            let tracker = int_ctx.hands_interaction[i].skeleton_tracker.as_ref()?;
            let log = &mut self.arm_log[i];
            let is_new = params.is_new(i);
            log.frames += 1;

            // The wrist the arm is placed with, the one in the depth, and for bursts the time placed for,
            // the newest sample's time and wrist, how far the wrist was extrapolated
            let (wrist_now, wrist_capture, arm_target_ns, sample_ns, wrist_sample, extrapolated) =
                if is_new {
                    // Mode 2, from the arm's own history: the depth's arm is where the history was at
                    // the capture time. No sample that early (the hand came back, or mode 2 just
                    // started): plain depth for this arm, counted as a miss
                    let history = &self.arm_histories[i];
                    let newest = history.newest().copied();
                    let placed = history.place(params.estimator, target_ns, MAX_ARM_EXTRAPOLATION);
                    let capture = history.at(capture_ns);
                    let (Some(newest), Some(placed), Some((wrist_capture, _))) =
                        (newest, placed, capture)
                    else {
                        log.misses += 1;
                        return None;
                    };
                    if params.estimator != ArmEstimator::Hold {
                        log.extrapolated_mm.push(placed.extrapolated * 1000.0);
                        log.speeds.push(placed.speed);
                    }

                    (
                        placed.wrist,
                        wrist_capture,
                        placed.time_ns,
                        newest.time_ns,
                        newest.wrist,
                        placed.extrapolated,
                    )
                } else {
                    // Mode 1, as before step 1: two lookups of the runtime. The one at the capture time
                    // returns the runtime's newest sample, not the wrist at that time (the runtime
                    // doesn't answer past lookups, `arm_history.rs`): kept as it was, as the comparison
                    // for mode 2
                    let wrist = |time_ns: i64| {
                        let joints = self
                            .stage_reference_space
                            .locate_hand_joints(tracker, xr::Time::from_nanos(time_ns))
                            .ok()
                            .flatten()?;
                        let wrist = joints[xr::HandJointEXT::WRIST];

                        wrist
                            .location_flags
                            .contains(xr::SpaceLocationFlags::POSITION_VALID)
                            .then(|| crate::from_xr_vec3(wrist.pose.position))
                    };
                    let wrist_now = log.count(Lookup::Place, wrist(now_ns))?;
                    let wrist_capture = log.count(Lookup::Capture, wrist(capture_ns))?;

                    (wrist_now, wrist_capture, now_ns, now_ns, wrist_now, 0.0)
                };
            let displacement = wrist_now - wrist_capture;
            if displacement.length() > MAX_DISPLACEMENT {
                return None;
            }
            if is_new {
                self.arm_samples[i].capture_wrist = Some(wrist_capture);
            }

            let side = if i == 0 { -1.0 } else { 1.0 };
            let elbow = head_position + yaw * (ELBOW_OFFSET * Vec3::new(side, 1.0, 1.0));

            Some(ArmMotion {
                wrist: wrist_now,
                direction: (elbow - wrist_now).normalize_or(Vec3::NEG_Y),
                displacement,
                weight_len: if is_new { weight_len } else { 0.0 },
                mode: if is_new { 2 } else { 1 },
                target_ns: arm_target_ns,
                history_ns: sample_ns,
                wrist_history: wrist_sample,
                extrapolated,
            })
        })
    }

    // The wrist gap of each arm (`debug.alvr.arm_gap_mm`), with the joints of the time the arm is placed
    // for. `arm_shift 2`: the gap starts at the arm's wrist, along the newest sample of the arm's history.
    // `arm_shift 1`: the joints of the display time minus `key_delay`, the gap at the arm's wrist. Without
    // a motion (`arm_shift 0`, or no motion for this arm) the depth's own arm is drawn, so the joints of the
    // depth's capture time, from the history (its oldest sample if the capture time is earlier, none in a
    // gap of the history; the joints
    // of the display time minus `key_delay` while there is no depth image). None: off, or the wrist or the
    // middle knuckle not located
    fn arm_gaps(
        &self,
        display_time: xr::Time,
        arms: &[Option<ArmMotion>; 2],
    ) -> [Option<ArmGap>; 2] {
        let Some(params) = self.mask_params else {
            return [None; 2];
        };
        let delay = self
            .color_key
            .map_or(0, |params| params.passthrough_delay_ms * 1_000_000);
        let int_ctx = self.interaction_context.read();

        [0, 1].map(|i| {
            let length = params.gap(i)?;
            let tracker = int_ctx.hands_interaction[i].skeleton_tracker.as_ref()?;
            let history = &self.arm_histories[i];
            let arm = arms[i];
            let (wrist, knuckle) = match (arm, self.depth_capture_time) {
                (Some(arm), _) if arm.mode == 2 => {
                    let newest = history.newest()?;
                    (newest.wrist, newest.knuckle)
                }
                (None, Some(capture_time)) => {
                    let capture_ns = capture_time.as_nanos();
                    history.at(capture_ns).or_else(|| {
                        history
                            .oldest()
                            .filter(|sample| sample.time_ns > capture_ns)
                            .map(|sample| (sample.wrist, sample.knuckle))
                    })?
                }
                _ => {
                    let time_ns = arm.map_or(display_time.as_nanos() - delay, |arm| arm.target_ns);
                    let joints = self
                        .stage_reference_space
                        .locate_hand_joints(tracker, xr::Time::from_nanos(time_ns))
                        .ok()
                        .flatten()?;
                    let position = |joint: xr::HandJointEXT| {
                        let joint = joints[joint];
                        joint
                            .location_flags
                            .contains(xr::SpaceLocationFlags::POSITION_VALID)
                            .then(|| crate::from_xr_vec3(joint.pose.position))
                    };
                    (
                        position(xr::HandJointEXT::WRIST)?,
                        position(xr::HandJointEXT::MIDDLE_PROXIMAL)?,
                    )
                }
            };

            Some(ArmGap {
                wrist: arm.map_or(wrist, |arm| arm.wrist),
                axis: (wrist - knuckle).try_normalize()?,
                length,
            })
        })
    }

    // The `arm:` line on `ALVR_CAM`: on each change of the `arm_*` properties (the settings only), and
    // every `ARM_LOG_INTERVAL` while an arm uses mode 2. `key_wrists` and `arms` are of this frame: the
    // hand-to-arm wrist gap tells whether the hand capsules (display-time joints) and the arm (the placed
    // wrist) part during fast moves
    fn log_arms(&mut self, arms: &[Option<ArmMotion>; 2]) {
        let key_wrists = self.key_wrists.get();
        for ((log, arm), key_wrist) in self.arm_log.iter_mut().zip(arms).zip(key_wrists) {
            if let (Some(arm), Some(key_wrist)) = (arm, key_wrist)
                && arm.mode == 2
            {
                log.gaps_mm.push(arm.wrist.distance(key_wrist) * 1000.0);
            }
        }

        let params = self.arm_params;
        let is_due = params.is_new(0) || params.is_new(1);
        let is_due = is_due && self.last_arm_log.elapsed() >= ARM_LOG_INTERVAL;
        if !self.arm_params_changed && !is_due {
            return;
        }
        self.last_arm_log = Instant::now();
        let logs = std::mem::take(&mut self.arm_log);

        let mut text = match (params.shift, params.hands) {
            (0, _) => "mode 0 (plain depth)".to_string(),
            (1, _) => "mode 1 (display-time wrist, smoothstep to 45 cm)".to_string(),
            (_, hands) => {
                let target = if params.estimator == ArmEstimator::Hold {
                    String::new()
                } else {
                    format!(" to display-{} ms", params.target_ms)
                };
                format!(
                    "mode 2 ({}), estimator {} ({}{target}), weight {} mm, sample times {}",
                    match hands {
                        3 => "both",
                        1 => "left; right 1",
                        2 => "right; left 1",
                        _ => "no arm; both 1",
                    },
                    params.estimator.value(),
                    params.estimator.name(),
                    params.weight_mm,
                    match (params.capture_time, self.unextrapolated_poses) {
                        (true, true) =>
                            format!("from captureTime, else seen-{} ms", params.sample_age_ms),
                        (false, _) => format!(
                            "seen-{} ms (captureTime off by arm_capture_time 0)",
                            params.sample_age_ms
                        ),
                        (true, false) => format!(
                            "seen-{} ms (the runtime has no captureTime)",
                            params.sample_age_ms
                        ),
                    },
                )
            }
        };
        // The counts since the last line belong to the old settings
        if self.arm_params_changed {
            self.arm_params_changed = false;
            crate::stream_log(&format!("arm: {text} (new settings)"));
            return;
        }

        for (i, mut log) in logs.into_iter().enumerate() {
            let name = if i == 0 { "L" } else { "R" };
            if !params.is_new(i) {
                text += &format!("; {name} mode 1, {}", log.lookups_text());
                continue;
            }
            let samples = log.samples_text(params.sample_age_ms);
            let extrapolated = if params.estimator == ArmEstimator::Hold {
                String::new()
            } else {
                median_p90(&mut log.extrapolated_mm).map_or(
                    ", not placed".to_string(),
                    |(median, p90)| {
                        let speed = median_p90(&mut log.speeds)
                            .map_or("-".to_string(), |(median, _)| format!("{median:.2}"));
                        format!(", placed {median:.0} mm p90 {p90:.0} from the newest sample at {speed} m/s")
                    },
                )
            };
            let gap = median_p90(&mut log.gaps_mm)
                .map_or("- (no hand capsules)".to_string(), |(median, p90)| {
                    format!("{median:.0} mm p90 {p90:.0}")
                });
            text += &format!("; {name} {samples}{extrapolated}, hand-to-arm wrist gap {gap}");
        }
        crate::stream_log(&format!("arm: {text}"));
    }

    // The hands, at the moment shown by the passthrough image. `eye_positions` (stage space) are needed
    // for the leak map.
    fn color_key_input(
        &self,
        display_time: xr::Time,
        eye_positions: Option<[Vec3; 2]>,
    ) -> Option<ColorKeyInput> {
        self.key_wrists.set([None; 2]);
        let params = self.color_key?;
        let delay = params.passthrough_delay_ms * 1_000_000;
        let time = xr::Time::from_nanos(display_time.as_nanos() - delay);
        let int_ctx = self.interaction_context.read();

        #[cfg(target_os = "android")]
        let verified = self
            .auto_color_key
            .as_ref()
            .map_or([[0.0; HAND_SEGMENTS]; 2], |key| key.verified_hands());
        #[cfg(not(target_os = "android"))]
        let verified = [[0.0; HAND_SEGMENTS]; 2];
        // Direction of the forearm from each wrist, seen by the camera
        #[cfg(target_os = "android")]
        let learned_bridges = self
            .auto_color_key
            .as_ref()
            .filter(|_| params.wrist.is_some())
            .map_or([BridgeChoice::default(); 2], |key| key.wrist_bridges());
        #[cfg(not(target_os = "android"))]
        let learned_bridges = [BridgeChoice::default(); 2];

        let uses_leak = params.leak_gain > 0.0 || params.leak_force > 0.0 || params.leak_map > 0.0;
        #[cfg(target_os = "android")]
        let ring_leak = self
            .auto_color_key
            .as_ref()
            .filter(|_| uses_leak)
            .map_or([0.0; 2], |key| key.ring_leak());
        #[cfg(not(target_os = "android"))]
        let ring_leak = [0.0; 2];

        // Where the room leaks through the key, learned only while it is used
        let shows_probes = params.leak_view == 2;
        #[cfg(target_os = "android")]
        let leak_map_state = self
            .auto_color_key
            .as_ref()
            .filter(|_| params.leak_map > 0.0 || shows_probes)
            .and_then(|key| key.leak_map());
        #[cfg(target_os = "android")]
        let leak_map = leak_map_state
            .as_deref()
            .map(|state| (&state.snapshot, state.probes.as_slice()));
        #[cfg(not(target_os = "android"))]
        let leak_map: Option<(&alvr_graphics::LeakSnapshot, &[Vec3])> = None;
        let map_eyes = eye_positions.filter(|_| params.leak_map > 0.0);
        let gains = LeakGains {
            force: params.leak_force,
            ring_gain: params.leak_gain,
            ring_lo: params.leak_lo,
            ring_hi: params.leak_hi,
            map_weight: params.leak_map,
            map_lo: params.leak_map_lo,
            map_hi: params.leak_map_hi,
        };
        let mut lookup_time = Duration::ZERO;
        let mut leak_log: [LeakLog; 2] = [None; 2];
        let mut bridges = [None; 2];
        // The solid fill fades out with the speed
        let stillness = |speed: f32| {
            1.0 - ((speed - MIN_FILL_SPEED) / (MAX_FILL_SPEED - MIN_FILL_SPEED)).clamp(0.0, 1.0)
        };

        let mut key_wrists = [None; 2];
        let hands = [0, 1].map(|i| {
            let tracker = int_ctx.hands_interaction[i].skeleton_tracker.as_ref()?;
            let (joints, velocities) = self
                .stage_reference_space
                .relate_hand_joints(tracker, time)
                .ok()
                .flatten()?;

            let wrist = joints[xr::HandJointEXT::WRIST];
            key_wrists[i] = wrist
                .location_flags
                .contains(xr::SpaceLocationFlags::POSITION_VALID)
                .then(|| crate::from_xr_vec3(wrist.pose.position));

            let shape = HandShape::from_joints(&joints.map(|joint| {
                crate::from_xr_vec3(joint.pose.position).extend(joint.radius * params.radius_scale)
            }));

            // The fastest joint, in the stage space like the shape. Some joints may have no valid
            // velocity. Of the wrist only: fingers that bend and a hand that turns while the arm
            // rests don't grow the rim
            let velocity = velocities
                .iter()
                .enumerate()
                .filter(|(joint, v)| {
                    (match params.speed_joints {
                        0 => true,
                        1 => PALM_JOINTS.contains(joint),
                        _ => *joint == WRIST_JOINT,
                    }) && v
                        .velocity_flags
                        .contains(xr::SpaceVelocityFlags::LINEAR_VALID)
                })
                .map(|(_, v)| crate::from_xr_vec3(v.linear_velocity))
                .fold(Vec3::ZERO, |fastest, v| {
                    if v.length_squared() > fastest.length_squared() {
                        v
                    } else {
                        fastest
                    }
                });
            // Grow immediately, shrink slowly: velocities are noisy
            let last = self.hand_velocities[i].get() * 0.9;
            let velocity = if velocity.length_squared() >= last.length_squared() {
                velocity
            } else {
                last
            };
            self.hand_velocities[i].set(velocity);

            let tracked_count = joints
                .iter()
                .filter(|joint| {
                    joint
                        .location_flags
                        .contains(xr::SpaceLocationFlags::POSITION_TRACKED)
                })
                .count();
            // Verified segments are solid, as long as they don't move: the tracking is late
            let mut solid = self.hand_solid[i].get();
            for (segment, weight) in solid.iter_mut().enumerate() {
                let (start, end) = SEGMENT_JOINTS[segment];
                let is_tracked = [start, end].iter().all(|joint| {
                    joints[*joint]
                        .location_flags
                        .contains(xr::SpaceLocationFlags::POSITION_TRACKED)
                });
                let segment_speed = [start, end]
                    .map(|joint| crate::from_xr_vec3(velocities[joint].linear_velocity).length())
                    .into_iter()
                    .fold(0.0, f32::max);
                let target = if is_tracked {
                    verified[i][segment] * stillness(segment_speed)
                } else {
                    0.0
                };
                // No popping, but let go quickly
                *weight += (target - *weight) * if target < *weight { 0.35 } else { 0.1 };
            }
            self.hand_solid[i].set(solid);

            let mut zones = params.zones;
            for (zone, solid) in zones.solid.iter_mut().zip(solid) {
                // Some segments may always be solid
                *zone = f32::max(*zone, solid);
            }

            let zones = zones
                .for_confidence(tracked_count as f32 / joints.len() as f32)
                .for_speed(
                    velocity,
                    params.core_error_s,
                    params.rim_error_s,
                    params.max_rim_growth_m,
                    params.sweep,
                );

            // Where the background around the hand looks like it, the rim would show it
            let zones = if uses_leak {
                // The map: the cells behind the segments, seen from both eyes
                let (map_leak, known) = match (leak_map, map_eyes) {
                    (Some((snapshot, _)), Some(eyes)) => {
                        let lookup_start = Instant::now();
                        let points = shape
                            .segments
                            .map(|[start, end]| ((start + end) * 0.5).truncate());
                        let result = snapshot.hand_leak_known(&eyes, &points);
                        lookup_time += lookup_start.elapsed();
                        result
                    }
                    _ => (None, 0.0),
                };
                let target = gains.target(ring_leak[i], map_leak);
                let leak = self.hand_leak[i].get();
                // Narrow quickly, widen slowly
                let leak = leak + (target - leak) * if target > leak { 0.2 } else { 0.05 };
                self.hand_leak[i].set(leak);

                let zones = zones.for_leak(leak, params.leak_inset_m);
                leak_log[i] = Some((ring_leak[i], map_leak, known, leak, zones.rim));

                zones
            } else {
                self.hand_leak[i].set(0.0);
                zones
            };

            // From the wrist towards the forearm, at the display time. The camera only chose the
            // direction in the space of the wrist joint.
            bridges[i] = params.wrist.and_then(|wrist| {
                let joint = joints[xr::HandJointEXT::WRIST];
                if !joint.location_flags.contains(
                    xr::SpaceLocationFlags::POSITION_VALID
                        | xr::SpaceLocationFlags::ORIENTATION_VALID,
                ) {
                    self.bridge_solid[i].set(0.0);
                    return None;
                }
                let position = crate::from_xr_vec3(joint.pose.position);
                let learned = learned_bridges[i];
                let direction = match learned.direction {
                    Some(local) => crate::from_xr_quat(joint.pose.orientation) * local,
                    // Opposite to the fingers
                    None => (position
                        - crate::from_xr_vec3(
                            joints[xr::HandJointEXT::MIDDLE_METACARPAL].pose.position,
                        ))
                    .normalize_or_zero(),
                };

                // Never solid below the confidence limit, else like the verified segments. The
                // direction turns with the wrist joint, which also turns when only the hand bends
                // (the forearm stays): the speed of its end counts too, until the camera follows.
                // A turn about the forearm doesn't move it.
                let wrist_velocity = velocities[xr::HandJointEXT::WRIST];
                let mut speed = crate::from_xr_vec3(wrist_velocity.linear_velocity).length();
                if wrist_velocity
                    .velocity_flags
                    .contains(xr::SpaceVelocityFlags::ANGULAR_VALID)
                {
                    let turn = crate::from_xr_vec3(wrist_velocity.angular_velocity)
                        .cross(direction)
                        .length()
                        * wrist.length_m;
                    speed = speed.max(turn);
                }
                let target = if joint
                    .location_flags
                    .contains(xr::SpaceLocationFlags::POSITION_TRACKED)
                {
                    WristBridge::solid_target(
                        learned.confidence,
                        wrist.min_confidence,
                        stillness(speed),
                    )
                } else {
                    0.0
                };
                let solid = self.bridge_solid[i].get();
                let solid = if target > 0.0 {
                    solid + (target - solid) * if target < solid { 0.35 } else { 0.1 }
                } else {
                    0.0
                };
                self.bridge_solid[i].set(solid);

                WristBridge::new(
                    position,
                    direction,
                    wrist.length_m,
                    joint.radius * wrist.radius_scale,
                    solid,
                )
            });

            Some((shape, zones))
        });

        self.hand_bounds
            .set(hands.map(|hand| hand.map(|(shape, _)| shape.bounds())));
        self.key_wrists.set(key_wrists);

        // With a play area placed from a marker only the part of the hands inside it shows, faded at
        // its surface. After the bounds: the playzone still knows about the whole hands
        let mut hands = hands;
        if let Some(region) = self.region_override.filter(|_| self.hands_in_region) {
            for (hand, bridge) in hands.iter_mut().zip(&mut bridges) {
                if let Some((shape, zones)) = hand {
                    (*shape, *bridge) = alvr_graphics::limit_hand_to_region(
                        shape,
                        zones,
                        *bridge,
                        &region,
                        alvr_graphics::HAND_REGION_FEATHER,
                    );
                }
            }
        }

        // To measure the cost of the hands on the GPU. The playzone still knows about them.
        let (mut hands, mut bridges) = if params.hands {
            (hands, bridges)
        } else {
            ([None; 2], [None; 2])
        };
        let mut leak = if params.leak_view != 0 {
            self.hand_leak.each_ref().map(Cell::get)
        } else {
            [0.0; 2]
        };

        // Debug view of the map: the right hand is replaced by spheres where the room leaks. They are
        // in the hand zones, so they also keep the layers of the key.
        if shows_probes {
            hands[1] = leak_map.and_then(|(snapshot, probes)| {
                let shape = alvr_graphics::leak_probes(
                    snapshot.anchor,
                    probes,
                    params.leak_probe_m,
                    LEAK_PROBE_RADIUS,
                )?;
                let mut zones = params.zones;
                zones.solid = [0.0; HAND_SEGMENTS];

                Some((shape, zones))
            });
            leak[1] = 0.0;
            bridges[1] = None;
        }

        if params.leak_map > 0.0 {
            self.log_leak_map(leak_log, lookup_time);
        }

        Some(ColorKeyInput {
            hands,
            leak,
            bridges,
        })
    }

    // Every 2 s, to tune `debug.alvr.key_leak_map_lo` / `key_leak_map_hi`
    #[allow(unused_variables)]
    fn log_leak_map(&self, hands: [LeakLog; 2], lookup_time: Duration) {
        let (total, count) = self.leak_lookup_time.get();
        let (total, count) = (total + lookup_time, count + 1);
        if self.last_leak_log.get().elapsed() < LEAK_LOG_INTERVAL {
            self.leak_lookup_time.set((total, count));
            return;
        }
        self.last_leak_log.set(Instant::now());
        self.leak_lookup_time.set((Duration::ZERO, 0));

        let text = ["left", "right"]
            .iter()
            .zip(hands)
            .map(|(name, hand)| match hand {
                Some((ring, map, known, leak, rim)) => format!(
                    "{name} ring {ring:.2} map {} known {:.0}% L {leak:.2} rim {:.1} mm",
                    map.map_or("-".into(), |map| format!("{map:.2}")),
                    known * 100.0,
                    rim * 1000.0,
                ),
                None => format!("{name} -"),
            })
            .collect::<Vec<_>>()
            .join(", ");
        #[cfg(target_os = "android")]
        crate::camera::log(&format!(
            "leak map: {text}, lookup {:.0} us per frame",
            total.as_secs_f32() * 1e6 / count as f32
        ));
    }

    pub fn uses_passthrough(&self) -> bool {
        self.config.passthrough.is_some()
    }

    /// `change_time`: when the change takes effect, None: now
    pub fn update_reference_space(&mut self, change_time: Option<xr::Time>) {
        self.renderer.clear_playzone();
        self.playzone_auto_series = false;
        #[cfg(target_os = "android")]
        if let Some(key) = &self.auto_color_key {
            key.clear_leak_map(change_time.unwrap_or(xr::Time::from_nanos(0)));
        }
        #[cfg(not(target_os = "android"))]
        let _ = change_time;
        self.input_thread_running.set(false);

        self.stage_reference_space = Arc::new(interaction::get_reference_space(
            &self.xr_session,
            xr::ReferenceSpaceType::STAGE,
        ));
        self.view_reference_space = Arc::new(interaction::get_reference_space(
            &self.xr_session,
            xr::ReferenceSpaceType::VIEW,
        ));

        self.core_context.send_playspace(
            self.xr_session
                .reference_space_bounds_rect(xr::ReferenceSpaceType::STAGE)
                .unwrap()
                .map(|a| Vec2::new(a.width, a.height)),
        );

        if let Some(running) = self.input_thread.take() {
            running.join().ok();
        }

        self.input_thread_running.set(true);

        self.input_thread = Some(thread::spawn({
            let core_ctx = Arc::clone(&self.core_context);
            let xr_session = self.xr_session.clone();
            let interaction_ctx = Arc::clone(&self.interaction_context);
            let stage_reference_space = Arc::clone(&self.stage_reference_space);
            let view_reference_space = Arc::clone(&self.view_reference_space);
            let refresh_rate = self.config.refresh_rate_hint;
            let running = Arc::clone(&self.input_thread_running);
            let hands_withheld = Arc::clone(&self.hands_withheld);
            move || {
                stream_input_loop(
                    &core_ctx,
                    xr_session,
                    &interaction_ctx,
                    &stage_reference_space,
                    &view_reference_space,
                    refresh_rate,
                    running,
                    &hands_withheld,
                )
            }
        }));
    }

    pub fn maybe_initialize_decoder(&mut self, codec: CodecType, config_nal: Vec<u8>) {
        let new_config = VideoDecoderConfig {
            codec,
            force_software_decoder: self.config.force_software_decoder,
            max_buffering_frames: self.config.max_buffering_frames,
            buffering_history_weight: self.config.buffering_history_weight,
            options: self.config.decoder_options.clone(),
            config_buffer: config_nal,
        };

        let maybe_config = if let Some((config, _)) = &self.decoder {
            (new_config != *config).then_some(new_config)
        } else {
            Some(new_config)
        };

        if let Some(config) = maybe_config {
            let (mut sink, source) = video_decoder::create_decoder(config.clone(), {
                let ctx = Arc::clone(&self.core_context);
                move |maybe_timestamp: Result<Duration>| match maybe_timestamp {
                    Ok(timestamp) => ctx.report_frame_decoded(timestamp),
                    Err(e) => ctx.report_fatal_decoder_error(&e.to_string()),
                }
            });
            self.decoder = Some((config, source));

            self.core_context.set_decoder_input_callback(Box::new(
                move |timestamp, buffer| -> bool { sink.push_nal(timestamp, buffer) },
            ));
        }
    }

    pub fn update_real_time_config(&mut self, config: &RealTimeConfig) {
        self.config.passthrough = config.passthrough.clone();
        self.config.clientside_post_processing = config.clientside_post_processing.clone();
    }

    // The room memory of this frame (`debug.alvr.mem_roll`, room_memory.rs). None: off, or without the color
    // key (the mask reads the playzone only with it, so the memory would be work for nothing). The hand
    // capsules at the depth's capture time come from the arm's sample history, else the last known ones
    // grown by 3 cm. The zone near each hand reaches farther by the wrist's travel since the capture
    // (`arms`)
    fn memory_input(&mut self, arms: &[Option<ArmMotion>; 2]) -> Option<MemoryInput> {
        let params = self.memory_params.filter(|_| self.color_key.is_some())?;
        let capture_ns = self.depth_capture_time.map(|time| time.as_nanos());
        let mut hands = [None; 2];
        let mut sources = [alvr_graphics::CaptureHand::None; 2];
        for i in 0..2 {
            let history = &self.arm_histories[i];
            if let Some(newest) = history.newest()
                && let Some(joints) = newest.joints
            {
                self.last_known_joints[i] = Some((newest.time_ns, joints));
            }
            (hands[i], sources[i]) =
                room_memory::capture_hand(history, self.last_known_joints[i], capture_ns);
        }
        self.memory_log.count(sources);

        Some(MemoryInput {
            params,
            capture_ns,
            hands,
            hand_sources: sources,
            reach: [0, 1].map(|i| {
                let travel = arms[i].map_or(0.0, |arm| arm.displacement.length());
                room_memory::near_reach(&params, travel)
            }),
            hands_away: self.last_hands_seen.elapsed() > PLAYZONE_HANDS_TIMEOUT,
        })
    }

    // The `memory:` line every 2 s while the room memory is on, and its part of the display record of a
    // burst, both after the renderer ran this frame
    fn report_memory(&mut self, display_time: xr::Time, input: Option<&DepthRenderInput>) {
        let Some(params) = self.memory_params else {
            return;
        };
        let status = self.renderer.memory_status();
        if self.burst.is_recording()
            && let Some(memory) = input.and_then(|input| input.memory)
            && let Some(status) = status
        {
            self.burst.set_display_memory(
                display_time.as_nanos(),
                MemoryRecord {
                    status,
                    params,
                    hands: memory.hand_sources,
                },
            );
        }
        let elapsed = self.last_memory_log.elapsed();
        if elapsed >= MEMORY_LOG_INTERVAL {
            self.last_memory_log = Instant::now();
            let text = self.memory_log.text(status.as_ref(), elapsed.as_secs_f32());
            crate::stream_log(&text);
        }
    }

    // The depth cloud of this frame (`debug.alvr.cloud`, motion_cloud.rs). None: off. With `cloud_ab 1` it
    // shows for 2 s, then today's picture for 2 s (its passes keep running). With `cloud_own` the gate of
    // each hand: its newest wrist against its wrist at the depth's capture time (0 without either)
    fn cloud_input(&self, display_time: xr::Time) -> Option<CloudInput> {
        let params = self.cloud_params?;
        let capture_ns = self.depth_capture_time.map(|time| time.as_nanos());
        let own_gate = [0, 1].map(|i| {
            if params.own <= 0.0 {
                return 0.0;
            }
            let history = &self.arm_histories[i];
            let travel = capture_ns.and_then(|time| {
                let (then, _) = history.at(time)?;
                Some(history.newest()?.wrist.distance(then))
            });
            alvr_graphics::own_gate(travel)
        });

        Some(CloudInput {
            params: motion_cloud::frame_params(params, self.cloud_leak_follow().as_ref()),
            capture_ns,
            display_ns: display_time.as_nanos(),
            shown: !params.ab || motion_cloud::ab_shown(self.cloud_ab_start.elapsed()),
            own_gate,
        })
    }

    // `debug.alvr.cloud_leak` 1: the key's fresh leak and the scale of the band's gain and cap (1 without a
    // fresh leak). None: off
    fn cloud_leak_follow(&self) -> Option<motion_cloud::LeakFollow> {
        if !self.cloud_leak {
            return None;
        }
        #[cfg(target_os = "android")]
        let leak = self.auto_color_key.as_ref().and_then(|key| key.key_leak());
        #[cfg(not(target_os = "android"))]
        let leak = None;

        Some(motion_cloud::LeakFollow::new(leak))
    }

    // Whether the region of the mask is made of boxes: the play area placed from the marker, or the volume
    // of the settings (as `depth_mask::cloud_runs` reads it)
    fn region_is_volume(&self) -> bool {
        self.region_override.is_some()
            || alvr_graphics::depth_passthrough_config(self.config.passthrough.as_ref())
                .is_some_and(|config| matches!(config.region, DepthMaskRegion::Volume(_)))
    }

    // Whether the depth cloud can run in this frame: with the colour key and a region of boxes (the renderer
    // also needs a depth image)
    fn cloud_can_run(&self) -> bool {
        self.color_key.is_some() && self.region_is_volume()
    }

    // Whether the mask shows the zone of the depth cloud in this frame, as the renderer decides it
    // (`alvr_graphics::cloud_shown`: it can run, not today's phase of `cloud_ab`, not the Show depth view)
    fn cloud_zone_shown(&self, cloud: Option<&CloudInput>) -> bool {
        cloud.is_some()
            && alvr_graphics::depth_passthrough_config(self.config.passthrough.as_ref())
                .is_some_and(|config| {
                    alvr_graphics::cloud_shown(config, cloud, self.cloud_can_run())
                })
    }

    // The `cloud:` line every 2 s while the depth cloud is set, and its part of the display record of a burst
    // (null while it does not run), both after the renderer ran this frame
    fn report_cloud(&mut self, display_time: xr::Time, input: Option<&DepthRenderInput>) {
        let Some(params) = self.cloud_params else {
            return;
        };
        let status = self.renderer.cloud_status();
        if let Some(status) = &status {
            self.cloud_log.count(status, display_time.as_nanos());
            if self.burst.is_recording()
                && let Some(cloud) = input.and_then(|input| input.cloud)
            {
                // The parameters of this frame (with `cloud_leak` the scaled gain and cap)
                self.burst.set_display_cloud(
                    display_time.as_nanos(),
                    CloudRecord {
                        params: cloud.params,
                        shown: cloud.shown,
                        eyes: status.eyes,
                    },
                );
            }
        }
        let elapsed = self.last_cloud_log.elapsed();
        if elapsed >= CLOUD_LOG_INTERVAL {
            self.last_cloud_log = Instant::now();
            let state = status.as_ref().ok_or_else(|| {
                CloudOff::reason(
                    input.is_some(),
                    self.color_key.is_some(),
                    self.region_is_volume(),
                )
            });
            let layers = self
                .color_key
                .map_or((0, 0), |params| (params.layers, params.crop));
            let follow = self.cloud_leak_follow();
            let text = self.cloud_log.text(
                &params,
                state,
                elapsed.as_secs_f32(),
                layers,
                follow.as_ref(),
            );
            crate::stream_log(&text);
        }
    }

    // The lever windows of this frame (`debug.alvr.lever_boxes`, lever.rs) and the `lever:` line every 2 s.
    // Off (the default): nothing is located, no window. The hands at the display time (the key's are
    // earlier, by `passthrough_delay_ms`: the gate opens a little sooner, the safe side), in the frame of
    // the play area placed from the marker (without one: no window). `edge_cut`: the centre of the cut
    // of the mask edge in use, for the start of the fade
    fn lever_windows(
        &mut self,
        display_time: xr::Time,
        edge_cut: f32,
    ) -> [Option<LeverWindow>; MAX_LEVER_WINDOWS] {
        if !self.lever_params.is_on() {
            return [None; MAX_LEVER_WINDOWS];
        }
        let area = self.region_override;
        let hands = area.map_or([None; 2], |area| {
            let world_to_area = area.area_to_world.inverse();
            let int_ctx = self.interaction_context.read();
            [0, 1].map(|i| {
                let tracker = int_ctx.hands_interaction[i].skeleton_tracker.as_ref()?;
                let (joints, _) = self
                    .stage_reference_space
                    .relate_hand_joints(tracker, display_time)
                    .ok()
                    .flatten()?;

                Some(LeverHand {
                    joints: std::array::from_fn(|j| {
                        joints[j]
                            .location_flags
                            .contains(xr::SpaceLocationFlags::POSITION_VALID)
                            .then(|| {
                                world_to_area
                                    .transform_point3(crate::from_xr_vec3(joints[j].pose.position))
                            })
                    }),
                })
            })
        });
        let windows = self.levers.update(
            &self.lever_params,
            area.as_ref(),
            &hands,
            display_time.as_nanos(),
            self.depth_capture_time.map(|time| time.as_nanos()),
            alvr_graphics::mask_edge_top(edge_cut),
        );
        if self.last_lever_log.elapsed() >= LEVER_LOG_INTERVAL {
            self.last_lever_log = Instant::now();
            crate::stream_log(&self.levers.log_text(&self.lever_params, area.as_ref()));
        }

        windows
    }

    // Start or stop the environment depth provider depending on the passthrough mode, and get the
    // depth image for this frame
    fn update_environment_depth(
        &mut self,
        display_time: xr::Time,
        current_headset_views: &[xr::View],
    ) -> Option<DepthRenderInput> {
        if alvr_graphics::depth_passthrough_config(self.config.passthrough.as_ref()).is_none() {
            if let Some(depth) = self.environment_depth.take() {
                self.renderer.clear_depth_swapchain();
                self.gfx_ctx.make_current();
                drop(depth);
                info!("Environment depth stopped");
            }
            // Retry next time the mode is selected (e.g. after granting the permission)
            self.environment_depth_failed = false;

            return None;
        }

        // The runtime imports the depth textures into the current GL context, and wgpu releases the
        // context after each call. Without this the texture names are 0.
        self.gfx_ctx.make_current();

        if self.environment_depth.is_none() && !self.environment_depth_failed {
            match EnvironmentDepthMETA::new(
                &self.xr_session,
                self.color_key.is_some_and(|params| params.hand_removal),
            ) {
                Ok(depth) => {
                    info!(
                        "Environment depth started: {} images, {}x{}",
                        depth.images().len(),
                        depth.resolution().x,
                        depth.resolution().y
                    );
                    self.renderer
                        .set_depth_swapchain(depth.images(), depth.resolution());
                    self.environment_depth = Some(depth);

                    // The runtime refuses to acquire depth images until the next xrBeginFrame
                    return Some(DepthRenderInput {
                        frame: None,
                        eye_positions: [Vec3::ZERO; 2],
                        color_key: self.color_key_input(display_time, None),
                        arms: [None; 2],
                        arm_gaps: [None; 2],
                        edge_cut: 0.0,
                        lever_windows: [None; MAX_LEVER_WINDOWS],
                        green_windows: [None; MAX_GREEN_WINDOWS],
                        capture_playzone: false,
                        playzone_view: false,
                        memory: None,
                        cloud: None,
                        key_lower_rects: None,
                        region_override: self.region_override,
                        game_dim: self.game_dim,
                        game_see: self.game_see,
                    });
                }
                Err(e) => {
                    error!(
                        "Depth passthrough unavailable (requires Quest 3/3S and the spatial data permission): {e}"
                    );
                    self.environment_depth_failed = true;
                }
            }
        }

        let frame = self.environment_depth.as_ref().and_then(|depth| {
            depth
                .acquire(&self.stage_reference_space, display_time)
                .inspect_err(|e| error!("Acquire environment depth: {e}"))
                .ok()
                .flatten()
        });

        let (frame, capture_time) = frame.unzip();
        self.depth_capture_time = capture_time.flatten();
        self.arm_samples = Default::default();
        self.poll_properties();
        self.update_arm_histories();
        let cloud = self.cloud_input(display_time);
        let arms = self.depth_capture_time.map_or([None; 2], |time| {
            self.arm_motions(display_time, time, current_headset_views, cloud.as_ref())
        });

        let eye_positions = [
            crate::from_xr_vec3(current_headset_views[0].pose.position),
            crate::from_xr_vec3(current_headset_views[1].pose.position),
        ];
        let color_key = self.color_key_input(display_time, Some(eye_positions));
        self.log_arms(&arms);
        let arm_gaps = self.arm_gaps(display_time, &arms);
        let edge_cut = self.mask_params.map_or(0.0, |params| {
            alvr_graphics::edge_cut_centre(params.edge_cut)
        });
        let capture_playzone = self.should_capture_playzone(frame.as_ref());
        let lever_windows = self.lever_windows(display_time, edge_cut);
        let memory = self.memory_input(&arms);
        // `mem_view` shows what only the memory draws with the playzone's debug view
        let playzone_view = self.color_key.is_some_and(|params| params.playzone_view)
            || memory.is_some_and(|memory| memory.params.view > 0);

        Some(DepthRenderInput {
            frame,
            eye_positions,
            color_key,
            arms,
            arm_gaps,
            edge_cut,
            lever_windows,
            // Set by `render` while the green-screen zone is on
            green_windows: [None; MAX_GREEN_WINDOWS],
            capture_playzone,
            playzone_view,
            memory,
            cloud,
            key_lower_rects: None,
            region_override: self.region_override,
            game_dim: self.game_dim,
            game_see: self.game_see,
        })
    }

    pub fn render(
        &mut self,
        frame_interval: Duration,
        vsync_time: Duration,
    ) -> (ProjectionLayerBuilder<'_>, Duration, ColorKeyFrame) {
        let xr_vsync_time = xr::Time::from_nanos(vsync_time.as_nanos() as _);
        self.poll_ablation(false);
        let frame_poll_deadline = Instant::now()
            + Duration::from_secs_f32(
                frame_interval.as_secs_f32() * DECODER_MAX_TIMEOUT_MULTIPLIER,
            );
        let mut frame_result = None;
        if let Some((_, source)) = &mut self.decoder {
            while frame_result.is_none() && Instant::now() < frame_poll_deadline {
                frame_result = source.get_frame();
                thread::sleep(Duration::from_micros(500));
            }
        }

        let (timestamp, frame_metadata, buffer_ptr) =
            if let Some((timestamp, buffer_ptr)) = frame_result {
                if let Some(metadata) = self.core_context.report_compositor_start(timestamp) {
                    self.last_good_video_frame_metadata = VideoFrameMetadata {
                        foveation_center_shifts: metadata
                            .foveation_center_shifts
                            .or(self.last_good_video_frame_metadata.foveation_center_shifts),
                        ..metadata
                    };
                }

                // Keep displaying new images even when their metadata is unavailable.
                (timestamp, self.last_good_video_frame_metadata, buffer_ptr)
            } else {
                (
                    vsync_time,
                    self.last_good_video_frame_metadata,
                    ptr::null_mut(),
                )
            };
        let view_params = frame_metadata.view_params;

        let left_swapchain_idx = self.swapchains[0].acquire_image().unwrap();
        let right_swapchain_idx = self.swapchains[1].acquire_image().unwrap();

        self.swapchains[0]
            .wait_image(xr::Duration::INFINITE)
            .unwrap();
        self.swapchains[1]
            .wait_image(xr::Duration::INFINITE)
            .unwrap();

        let (flags, maybe_views) = self
            .xr_session
            .locate_views(
                xr::ViewConfigurationType::PRIMARY_STEREO,
                xr_vsync_time,
                &self.stage_reference_space,
            )
            .unwrap();

        let views_located = flags.contains(xr::ViewStateFlags::ORIENTATION_VALID);
        let current_headset_views = if views_located {
            maybe_views
        } else {
            vec![crate::default_view(), crate::default_view()]
        };
        let located_views = views_located.then_some(current_headset_views.as_slice());
        self.log_stream_facts(located_views);

        // The poses and FoVs we received from the PC runtime, which may differ and/or include
        // altered FoVs based on settings and view conversions done for canting.
        let input_view_params = view_params;
        let mut output_view_params = input_view_params;
        // Avoid passing invalid timestamp to runtime.
        // `timestamp` is generally a current vsync time, but may be repeated if frames are
        // dropped. Some runtimes dislike it if the timestamp is repeated for too long, so after
        // one second we begin presenting a lagged vsync time instead.
        let mut openxr_display_time =
            Duration::max(timestamp, vsync_time.saturating_sub(Duration::from_secs(1)));

        // (shinyquagsire23) I don't entirely trust runtimes to implement CompositionLayerProjectionView
        // correctly, but if we do trust them, avoid doing rotation ourselves. Otherwise, rerender.
        // Ex: YVR/PFDMR has issues with aspect ratio mismatches and passthrough compositing.
        if self.use_custom_reprojection {
            output_view_params = [
                ViewParams {
                    pose: crate::from_xr_pose(current_headset_views[0].pose),
                    fov: crate::from_xr_fov(current_headset_views[0].fov),
                },
                ViewParams {
                    pose: crate::from_xr_pose(current_headset_views[1].pose),
                    fov: crate::from_xr_fov(current_headset_views[1].fov),
                },
            ];

            openxr_display_time = vsync_time;
        }

        // `debug.alvr.edge_fill`: the game layers cover a wider fov (same pose, swapchains and sizes); the
        // renderer draws the stream view inside it and fills the margin. Everything in stream-view uv
        // that sets a layer's image rect or fov is converted to this view below
        let edge_fill = self.edge_fill_margin();
        if let Some(margin) = edge_fill {
            for view in &mut output_view_params {
                view.fov = alvr_graphics::widen_fov(view.fov, margin);
            }
        }

        let mut depth_input = self.update_environment_depth(xr_vsync_time, &current_headset_views);

        // Part of each view with the hands, the same as in the renderer
        let hand_rects = depth_input
            .as_ref()
            .and_then(|input| Some((input, input.color_key?)))
            .map_or([None; 2], |(input, key)| {
                let reach = input.memory.map_or([0.0; 2], |memory| memory.reach);
                [0, 1].map(|i| {
                    alvr_graphics::hands_view_rect_reach(
                        &input_view_params[i],
                        input.eye_positions[i],
                        &key,
                        reach,
                    )
                })
            });
        if hand_rects.iter().any(Option::is_some) {
            self.last_hand_rect_time = Some(Instant::now());
        }
        for (last, rect) in self.last_hand_rects.iter_mut().zip(hand_rects) {
            if rect.is_some() {
                *last = rect;
            }
        }
        // `debug.alvr.green_boxes`: the zone's boxes go to the depth mask while it is on. Its rects are in
        // the view the layers are submitted with (with the edge fill the wider one), as is its under
        // layer's crop
        let green = self.green_frame(&output_view_params);
        if let Some(green) = &green
            && let Ok(windows) = green.state
            && let Some(input) = &mut depth_input
        {
            input.green_windows = windows;
        }
        let key_frame = ColorKeyFrame {
            tracked_hands: self.hand_bounds.get().map(|bounds| bounds.is_some()),
            playzone_age: self.renderer.playzone_age(),
            hand_rects,
            hands_in_view: self
                .last_hand_rect_time
                .is_some_and(|time| time.elapsed() < KEY_LAYERS_HOLD),
            hand_zones: depth_input
                .as_ref()
                .and_then(|input| input.color_key)
                .map_or([None; 2], |key| {
                    key.hands.map(|hand| hand.map(|(_, zones)| zones))
                }),
            green_key: green.as_ref().and_then(|green| green.key),
        };

        // Outside the hands the lower color key layer is covered by the upper one, or shows the same
        // passthrough. For a view without hands the last rect is used, the content there is the same.
        let crop = self.color_key.map_or(0, |params| params.crop);
        let resolution = self.target_view_resolution;
        let lower_crops = (crop > 0 && !self.use_custom_reprojection).then(|| {
            [0, 1].map(|i| {
                self.last_hand_rects[i].map(|rect| {
                    let fov = output_view_params[i].fov;
                    // The hands' rect is in the stream view: in the output view with the edge fill
                    let rect = if edge_fill.is_some() {
                        alvr_graphics::stream_rect_to_output(input_view_params[i].fov, fov, rect)
                    } else {
                        rect
                    };
                    alvr_graphics::crop_view(fov, resolution, rect, KEY_CROP_MARGIN, crop == 2)
                })
            })
        });
        // Where there is no lower layer the shader makes the upper one look like both
        if let Some(params) = self.color_key
            && params.crop > 0
            && let Some(input) = &mut depth_input
        {
            let extra_layers = params.extra_layers(key_frame.hands_in_view);
            input.key_lower_rects = Some([0, 1].map(|i| match lower_crops.map(|crops| crops[i]) {
                _ if !extra_layers => (Vec2::ZERO, Vec2::ZERO),
                Some(Some((offset, size, _))) => {
                    let rect = alvr_graphics::crop_view_uv(resolution, offset, size, crop == 2);
                    // The shader compares it with the stream uv
                    if edge_fill.is_some() {
                        alvr_graphics::output_rect_to_stream(
                            input_view_params[i].fov,
                            output_view_params[i].fov,
                            rect,
                        )
                    } else {
                        rect
                    }
                }
                _ => (Vec2::ZERO, Vec2::ONE),
            }));
        }

        // `debug.alvr.key_dump`: the depth of a frame that has a depth image
        if let Some((n, time)) = self.pending_dump
            && time.elapsed() > alvr_graphics::DEPTH_DUMP_TIMEOUT
        {
            self.pending_dump = None;
            crate::stream_log(&format!("depth dump {n}: no depth image"));
        }
        if depth_input
            .as_ref()
            .is_some_and(|input| input.frame.is_some())
            && let Some((n, _)) = self.pending_dump.take()
        {
            if let Some(replaced) = self.renderer.request_depth_dump(DepthDumpRequest {
                n,
                display_ns: xr_vsync_time.as_nanos(),
                depth_capture_ns: self.depth_capture_time.map(|time| time.as_nanos()),
                now_ns: crate::xr_runtime_now(self.xr_session.instance())
                    .map(|time| time.as_nanos()),
            }) {
                crate::stream_log(&format!("depth dump {replaced}: replaced by {n}"));
            }
        }

        if self.burst.is_recording()
            && let Some(input) = &depth_input
        {
            self.record_burst_frame(
                xr_vsync_time,
                input,
                &current_headset_views,
                input_view_params,
            );
        }

        self.update_problem_view();
        self.renderer
            .set_gpu_rect(self.gpu_rect_request(key_frame.hands_in_view));
        self.renderer.render(
            buffer_ptr,
            [
                StreamViewParams {
                    swapchain_index: left_swapchain_idx,
                    input_view_params: input_view_params[0],
                    output_view_params: output_view_params[0],
                },
                StreamViewParams {
                    swapchain_index: right_swapchain_idx,
                    input_view_params: input_view_params[1],
                    output_view_params: output_view_params[1],
                },
            ],
            self.config.passthrough.as_ref(),
            frame_metadata
                .foveation_center_shifts
                .map(|centers| centers.map(Vec2::from_array)),
            depth_input.as_ref(),
            &self.area_overlay,
        );
        self.report_memory(xr_vsync_time, depth_input.as_ref());
        self.report_cloud(xr_vsync_time, depth_input.as_ref());
        self.record_gpu_rect();
        // `debug.alvr.gpu_rect`: the lower key layer cropped to the active rectangle (only asked for
        // without the crop of `key_crop`)
        let gpu_rect_crops = self
            .renderer
            .gpu_rect_frame()
            .filter(|_| lower_crops.is_none() && self.gpu_rect.is_some())
            .and_then(|frame| frame.lower_crops);

        if let Some((n, result)) = self.renderer.poll_depth_dump() {
            // The dump of the start of a burst goes into its folder
            let dir = if self.burst_dump == Some(n) {
                self.burst_dump = None;
                burst::burst_dir(n)
            } else {
                PathBuf::from(passthrough::FILES_DIR)
            };
            write_depth_dump(n, result, dir);
        }
        let burst_images = self.renderer.poll_burst_depth();
        if !burst_images.is_empty() {
            self.burst.depth_images(burst_images);
        }
        if self.burst.is_active() {
            let xr_session = &self.xr_session;
            self.burst.update(|| {
                crate::xr_runtime_now(xr_session.instance()).map(|time| time.as_nanos())
            });
        }

        self.swapchains[0].release_image().unwrap();
        self.swapchains[1].release_image().unwrap();

        if !buffer_ptr.is_null()
            && let Some(xr_now) = crate::xr_runtime_now(self.xr_session.instance())
        {
            self.core_context.report_submit(
                timestamp,
                vsync_time.saturating_sub(Duration::from_nanos(xr_now.as_nanos() as u64)),
            );
        }
        self.record_perf(
            vsync_time,
            (!buffer_ptr.is_null()).then_some(timestamp),
            &input_view_params,
            located_views,
        );

        let rect = xr::Rect2Di {
            offset: xr::Offset2Di { x: 0, y: 0 },
            extent: xr::Extent2Di {
                width: self.target_view_resolution.x as _,
                height: self.target_view_resolution.y as _,
            },
        };

        let clientside_post_processing = self
            .xr_session
            .instance()
            .exts()
            .fb_composition_layer_settings
            .and(self.config.clientside_post_processing.clone());

        let layer = ProjectionLayerBuilder::new(
            &self.stage_reference_space,
            [
                xr::CompositionLayerProjectionView::new()
                    .pose(crate::to_xr_pose(output_view_params[0].pose))
                    .fov(crate::to_xr_fov(output_view_params[0].fov))
                    .sub_image(
                        xr::SwapchainSubImage::new()
                            .swapchain(&self.swapchains[0])
                            .image_array_index(0)
                            .image_rect(rect),
                    ),
                xr::CompositionLayerProjectionView::new()
                    .pose(crate::to_xr_pose(output_view_params[1].pose))
                    .fov(crate::to_xr_fov(output_view_params[1].fov))
                    .sub_image(
                        xr::SwapchainSubImage::new()
                            .swapchain(&self.swapchains[1])
                            .image_array_index(0)
                            .image_rect(rect),
                    ),
            ],
            self.config
                .passthrough
                .clone()
                .map(|mode| ProjectionLayerAlphaConfig {
                    premultiplied: self.color_key.is_none()
                        && alvr_graphics::passthrough_requires_premultiplied_alpha(&mode),
                }),
            clientside_post_processing,
        );

        let layer = if let Some(crops) = lower_crops {
            let swapchains = &self.swapchains;
            layer.crop_lower_key_layer([0, 1].map(|i| {
                let (offset, size, fov) =
                    crops[i].unwrap_or((UVec2::ZERO, resolution, output_view_params[i].fov));

                xr::CompositionLayerProjectionView::new()
                    .pose(crate::to_xr_pose(output_view_params[i].pose))
                    .fov(crate::to_xr_fov(fov))
                    .sub_image(
                        xr::SwapchainSubImage::new()
                            .swapchain(&swapchains[i])
                            .image_array_index(0)
                            .image_rect(xr::Rect2Di {
                                offset: xr::Offset2Di {
                                    x: offset.x as _,
                                    y: offset.y as _,
                                },
                                extent: xr::Extent2Di {
                                    width: size.x as _,
                                    height: size.y as _,
                                },
                            }),
                    )
            }))
        } else if let Some(crops) = gpu_rect_crops {
            let swapchains = &self.swapchains;
            layer.crop_lower_key_layer([0, 1].map(|i| {
                let (offset, size, fov) = crops[i];

                xr::CompositionLayerProjectionView::new()
                    .pose(crate::to_xr_pose(output_view_params[i].pose))
                    .fov(crate::to_xr_fov(fov))
                    .sub_image(
                        xr::SwapchainSubImage::new()
                            .swapchain(&swapchains[i])
                            .image_array_index(0)
                            .image_rect(xr::Rect2Di {
                                offset: xr::Offset2Di {
                                    x: offset.x as _,
                                    y: offset.y as _,
                                },
                                extent: xr::Extent2Di {
                                    width: size.x as _,
                                    height: size.y as _,
                                },
                            }),
                    )
            }))
        } else {
            layer
        };

        // `debug.alvr.green_boxes`: the layer under the green-keyed passthrough, the zone's part of each
        // view (padded) with the pose of the other game layers. The y origin of the image rect as the
        // lower layer's crop (`key_crop 2`: at the top)
        let top_origin = self.color_key.is_some_and(|params| params.crop == 2);
        let green_under = green.and_then(|green| Some((green.under?, green.pad)));
        let layer = if let Some((rects, pad)) = green_under {
            let swapchains = &self.swapchains;
            layer.with_green_under([0, 1].map(|i| {
                let (offset, size, fov) = alvr_graphics::green_under_crop(
                    output_view_params[i].fov,
                    resolution,
                    rects[i],
                    pad,
                    top_origin,
                );

                xr::CompositionLayerProjectionView::new()
                    .pose(crate::to_xr_pose(output_view_params[i].pose))
                    .fov(crate::to_xr_fov(fov))
                    .sub_image(
                        xr::SwapchainSubImage::new()
                            .swapchain(&swapchains[i])
                            .image_array_index(0)
                            .image_rect(xr::Rect2Di {
                                offset: xr::Offset2Di {
                                    x: offset.x as _,
                                    y: offset.y as _,
                                },
                                extent: xr::Extent2Di {
                                    width: size.x as _,
                                    height: size.y as _,
                                },
                            }),
                    )
            }))
        } else {
            layer
        };

        (layer, openxr_display_time, key_frame)
    }
}

// Not on the render thread: a few MB
fn write_depth_dump(n: i64, result: Result<DepthDumpFiles, String>, dir: PathBuf) {
    let dump = match result {
        Ok(dump) => dump,
        Err(e) => {
            crate::stream_log(&format!("depth dump {n}: {e}"));
            return;
        }
    };
    thread::spawn(move || {
        let files = dump.into_files();
        let result = fs::create_dir_all(&dir).and_then(|()| {
            for (name, data) in &files {
                fs::write(dir.join(name), data)?;
            }
            Ok(())
        });
        match result {
            Ok(()) => crate::stream_log(&format!(
                "depth dump {n}: {} files in {}",
                files.len(),
                dir.display(),
            )),
            Err(e) => crate::stream_log(&format!("depth dump {n}: {e}")),
        }
    });
}

// XrTime now (CLOCK_MONOTONIC on Quest), 0 if the runtime doesn't say
fn runtime_now_ns(session: &xr::Session<xr::OpenGlEs>) -> i64 {
    crate::xr_runtime_now(session.instance()).map_or(0, |time| time.as_nanos())
}

impl Drop for StreamContext {
    fn drop(&mut self) {
        // Closes the camera
        #[cfg(target_os = "android")]
        {
            self.auto_color_key = None;
        }

        self.input_thread_running.set(false);
        self.input_thread.take().unwrap().join().ok();
    }
}

#[expect(clippy::too_many_arguments)]
fn stream_input_loop(
    core_ctx: &ClientCoreContext,
    xr_session: xr::Session<xr::OpenGlEs>,
    interaction_ctx: &RwLock<InteractionContext>,
    stage_reference_space: &xr::Space,
    view_reference_space: &xr::Space,
    refresh_rate: f32,
    running: Arc<RelaxedAtomic>,
    hands_withheld: &RelaxedAtomic,
) {
    let mut last_controller_poses = [Pose::IDENTITY; 2];
    let mut last_palm_poses = [Pose::IDENTITY; 2];
    let mut last_view_params = [ViewParams::DUMMY; 2];

    let mut deadline = Instant::now();
    let frame_interval = Duration::from_secs_f32(1.0 / refresh_rate);
    while running.value() {
        let int_ctx_lock = interaction_ctx.read();
        let int_ctx = &*int_ctx_lock;
        // Streaming related inputs are updated here. Make sure every input poll is done in this
        // thread
        if let Err(e) = xr_session.sync_actions(&[(&int_ctx.action_set).into()]) {
            error!("{e}");
            return;
        }

        let Some(now) = crate::xr_runtime_now(xr_session.instance()).map(crate::from_xr_time)
        else {
            error!("Cannot poll tracking: invalid time");
            return;
        };

        let target_time = now + core_ctx.get_total_prediction_offset();

        let Some((head_motion, local_views)) = interaction::get_head_data(
            &xr_session,
            core_ctx.platform(),
            stage_reference_space,
            view_reference_space,
            now,
            target_time,
            &last_view_params,
        ) else {
            continue;
        };

        if let Some(views) = local_views {
            core_ctx.send_view_params(views);
            last_view_params = views;
        }

        let mut device_motions = Vec::with_capacity(3);

        device_motions.push((*HEAD_ID, head_motion));

        let mut left_hand_data = crate::interaction::get_hand_data(
            &xr_session,
            core_ctx.platform(),
            stage_reference_space,
            now,
            target_time,
            &int_ctx.hands_interaction[0],
            &mut last_controller_poses[0],
            &mut last_palm_poses[0],
        );
        let mut right_hand_data = crate::interaction::get_hand_data(
            &xr_session,
            core_ctx.platform(),
            stage_reference_space,
            now,
            target_time,
            &int_ctx.hands_interaction[1],
            &mut last_controller_poses[1],
            &mut last_palm_poses[1],
        );
        // While the play area is edited: the hands that are tracked as hands (with a skeleton) are not
        // sent, see `withhold_hands`. Without multimodal input the grip pose of such a hand is the hand
        // (it would be sent below once its skeleton is gone); with it, a held controller stays
        if hands_withheld.value() {
            for data in [&mut left_hand_data, &mut right_hand_data] {
                if data.skeleton_joints.take().is_some() && !int_ctx.multimodal_hands_enabled {
                    data.grip_motion = None;
                }
            }
        }

        // Note: When multimodal input is enabled, we are sure that when free hands are used
        // (not holding controllers) the controller data is None.
        if (int_ctx.multimodal_hands_enabled || left_hand_data.skeleton_joints.is_none())
            && let Some(motion) = left_hand_data.grip_motion
        {
            device_motions.push((*HAND_LEFT_ID, motion));
        }
        if (int_ctx.multimodal_hands_enabled || right_hand_data.skeleton_joints.is_none())
            && let Some(motion) = right_hand_data.grip_motion
        {
            device_motions.push((*HAND_RIGHT_ID, motion));
        }

        if int_ctx.multimodal_hands_enabled
            && let Some(detached_controller) = left_hand_data.detached_grip_motion
        {
            device_motions.push((*DETACHED_CONTROLLER_LEFT_ID, detached_controller));
        }
        if int_ctx.multimodal_hands_enabled
            && let Some(detached_controller) = right_hand_data.detached_grip_motion
        {
            device_motions.push((*DETACHED_CONTROLLER_RIGHT_ID, detached_controller));
        }

        let face = interaction::get_face_data(
            &xr_session,
            core_ctx.platform(),
            &int_ctx.face_sources,
            stage_reference_space,
            view_reference_space,
            head_motion.pose.orientation,
            now,
        );

        let body = int_ctx
            .body_source
            .as_ref()
            .and_then(|source| interaction::get_body_skeleton(source, stage_reference_space, now));

        if let Some(source) = &int_ctx.body_source {
            device_motions.append(&mut interaction::get_bd_motion_trackers(source, now));
        }

        drop(int_ctx_lock);

        // Even though the server is already adding the motion-to-photon latency, here we use
        // target_time as the poll_timestamp to compensate for the fact that video frames are sent
        // with the poll timestamp instead of the vsync time. This is to ensure correctness when
        // submitting frames to OpenXR. This won't cause any desync with the server because no time
        // sync step is performed between client and server.
        core_ctx.send_tracking(TrackingData {
            poll_timestamp: target_time,
            device_motions,
            hand_skeletons: [
                left_hand_data.skeleton_joints,
                right_hand_data.skeleton_joints,
            ],
            face,
            body,
        });

        let button_entries =
            interaction::update_buttons(&xr_session, &interaction_ctx.read().button_actions);
        if !button_entries.is_empty() {
            core_ctx.send_buttons(button_entries);
        }

        deadline += frame_interval / 3;
        thread::sleep(deadline.saturating_duration_since(Instant::now()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(shift: i64, hands: i64, target_ms: i64) -> ArmParams {
        ArmParams {
            shift,
            hands,
            target_ms,
            weight_mm: 300,
            estimator: ArmEstimator::Hold,
            sample_age_ms: 35,
            capture_time: true,
        }
    }

    #[test]
    fn mask_params_unset_are_today() {
        // Nothing set: today's edge (the uniform gets 0) and no wrist gap
        let unset = MaskParams::from_values(None, None, None);
        assert_eq!(unset.edge_cut, 20);
        assert_eq!(alvr_graphics::edge_cut_centre(unset.edge_cut), 0.0);
        assert_eq!(unset.gap(0), None);
        assert_eq!(unset.gap(1), None);
        assert_eq!(unset.text(), "mask: edge cut 20 % (today's), wrist gap off");
        // 0 also means today's edge
        assert_eq!(MaskParams::from_values(Some(0), None, None), unset);

        // The values the user will try, and the clamps (the cut's lower end stays in 0.02..0.69)
        for (value, expected) in [(50, 50), (65, 65), (80, 80), (5, 17), (100, 84)] {
            let params = MaskParams::from_values(Some(value), None, None);
            assert_eq!(params.edge_cut, expected, "{value}");
        }
        assert_eq!(alvr_graphics::edge_cut_centre(50), 0.5);
        assert_eq!(MaskParams::from_values(None, Some(200), None).gap_mm, 80);

        // Bit 1: left (hand 0), bit 2: right (hand 1)
        let left = MaskParams::from_values(Some(65), Some(30), Some(1));
        assert_eq!(left.gap(0), Some(0.03));
        assert_eq!(left.gap(1), None);
        assert_eq!(
            left.text(),
            "mask: edge cut 65 %, wrist gap 30 mm on the left arm"
        );
        let both = MaskParams::from_values(None, Some(50), None);
        assert_eq!(both.gap(0), Some(0.05));
        assert_eq!(both.gap(1), Some(0.05));
        assert_eq!(
            MaskParams::from_values(None, Some(30), Some(0)).gap(0),
            None
        );
    }

    #[test]
    fn arm_shift_keeps_old_values() {
        // Before mode 2 every value but 0 meant today's shift
        assert_eq!(arm_shift_mode(0), 0);
        assert_eq!(arm_shift_mode(1), 1);
        assert_eq!(arm_shift_mode(2), 2);
        for value in [-1, 3, 5, 100] {
            assert_eq!(arm_shift_mode(value), 1, "{value}");
        }
    }

    #[test]
    fn arm_hands_select_mode_2() {
        // Bit 1: left (hand 0), bit 2: right (hand 1)
        let both = params(2, 3, 40);
        assert!(both.is_new(0) && both.is_new(1));
        let left = params(2, 1, 40);
        assert!(left.is_new(0) && !left.is_new(1));
        let right = params(2, 2, 40);
        assert!(!right.is_new(0) && right.is_new(1));
        assert!(!params(2, 0, 40).is_new(0));
        // Only with `arm_shift 2`
        assert!(!params(1, 3, 40).is_new(0));
    }

    #[test]
    fn arm_target_is_before_the_display() {
        let display_ns = 10_000_000_000;
        assert_eq!(
            params(2, 3, 40).target_ns(display_ns),
            display_ns - 40_000_000
        );
        assert_eq!(params(2, 3, 0).target_ns(display_ns), display_ns);
    }

    #[test]
    fn sample_times_change_with_the_age_or_the_capture_time() {
        let base = params(2, 3, 40);
        // The estimator, the target, the arms: the samples stay
        for other in [
            ArmParams {
                estimator: ArmEstimator::Parabola,
                ..base
            },
            params(2, 1, 60),
            params(1, 3, 40),
        ] {
            assert!(!base.changes_sample_times(&other));
        }
        assert!(base.changes_sample_times(&ArmParams {
            sample_age_ms: 40,
            ..base
        }));
        assert!(base.changes_sample_times(&ArmParams {
            capture_time: false,
            ..base
        }));
    }

    #[test]
    fn arm_lookups_are_counted_apart() {
        let mut log = ArmHandLog {
            frames: 3,
            ..Default::default()
        };
        assert_eq!(log.count(Lookup::Place, Some(1)), Some(1));
        assert_eq!(log.count::<i32>(Lookup::Place, None), None);
        log.count::<i32>(Lookup::Capture, None);
        log.count::<i32>(Lookup::Capture, None);
        assert_eq!(log.failed, [1, 2]);
        assert_eq!(
            log.lookups_text(),
            "lookups failed display 1, capture 2 of 3 frames"
        );
    }

    #[test]
    fn arm_samples_are_reported() {
        // No sample yet: the age set
        let mut log = ArmHandLog {
            frames: 144,
            misses: 3,
            resets: 1,
            ..Default::default()
        };
        assert_eq!(
            log.samples_text(35),
            "0 samples, age 35 ms (arm_sample_age_ms), buffer misses 3 of 144 frames, resets 1"
        );

        // Every sample with a capture time
        let mut log = ArmHandLog {
            frames: 144,
            samples: 3,
            intervals_ms: vec![14.6, 14.4, 29.0],
            ages_ms: vec![36.0, 34.0, 35.0],
            ..Default::default()
        };
        assert_eq!(
            log.samples_text(35),
            "3 samples every 14.6 ms p90 29.0, age 35 ms p90 36 (captureTime), buffer misses 0 of 144 frames, resets 0"
        );

        // Some without
        let mut log = ArmHandLog {
            samples: 4,
            ages_ms: vec![36.0],
            ..Default::default()
        };
        assert!(
            log.samples_text(40)
                .starts_with("4 samples, age 36 ms p90 36 (captureTime, 1 samples), else 40 ms,")
        );
    }

    #[test]
    fn median_and_p90() {
        assert_eq!(median_p90(&mut []), None);
        let mut values = [5.0, 1.0, 3.0, 2.0, 4.0];
        assert_eq!(median_p90(&mut values), Some((3.0, 5.0)));
    }
}
