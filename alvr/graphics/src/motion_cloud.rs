//! The depth cloud (`debug.alvr.cloud`, design/depthcloud/final.md, parts B and C). CPU side: the
//! parameters, which eye's field is built on which display frame and when a pair of depth images claims
//! no motion (`plan_eyes`), the uniforms of motion_cloud.wgsl, the reach time of each eye, the status for
//! the log line and the record of bursts. The GPU side is motion_cloud_gpu.rs.
//!
//! On every new depth image the passes measure, per eye, how far each edge of "in the play region"
//! against "game" moved since the image before, along world rays (the head's own motion cancelled), and
//! keep the largest speed of the last about 200 ms per ray: the field. The mask opens a band of speed x
//! age around the edges that moved, in which the colour key decides. An edge that does not move gets no
//! band.
//!
//! - Eye schedule (as the room memory): eye 0 on the display frame of a new depth image, eye 1 on the
//!   next one; both at once if eye 1 is still waiting when the next image arrives.
//! - A new image is one whose `DepthKey` changed, unless its capture time is that of eye 0's field (the
//!   runtime located the same image again).
//! - A pair claims no motion (reset) when the eye has no field yet, when the two images are less than 25
//!   or more than 220 ms apart or a time is unknown, when the region changed between them (history
//!   cleared too), or when they are 110-220 ms apart (a skipped image, e.g. eye 1 after two images on
//!   consecutive display frames) or the head turned faster than `head_max` between their poses (history
//!   kept, decayed over the real time between them).
//! - A field older than 300 ms (display time minus its capture time) opens no zone (the depth stalled),
//!   also with `cloud_fixed`.
//!
//! Pure maths, so it can be tested on the PC.

use alvr_common::{ViewParams, glam::Quat};

/// Half of the window of the distances of the field (cells of 2 x 2 depth texels)
pub const CLOUD_WINDOW_CELLS: i32 = 12;
/// The signed distances are truncated at this (depth texels: the window)
pub const CLOUD_TRUNCATION: f32 = 24.0;
/// Speeds are stored as a share of this (depth texels per second: 2 per 8 bit step)
pub const CLOUD_SPEED_SCALE: f32 = 510.0;
/// Held speeds below this are 0 (depth texels per second: 1 texel per 67 ms)
pub const CLOUD_SPEED_FLOOR: f32 = 15.0;
/// A pair of depth images is measured only if they are this far apart (ms)
pub const CLOUD_MIN_DT_MS: f32 = 25.0;
pub const CLOUD_MAX_DT_MS: f32 = 110.0;
/// A pair further apart than `CLOUD_MAX_DT_MS` and up to this claims no motion but keeps the history
/// (one skipped image); further apart, the history is cleared too (ms)
pub const CLOUD_MAX_GAP_MS: f32 = 220.0;
/// How far the live picture is ahead of a field is taken as at most this (ms)
pub const CLOUD_MAX_LEAD_MS: f32 = 250.0;
/// A field older than this (display time minus its capture time, before the latency; ms) opens no zone:
/// the depth stalled
pub const CLOUD_STALE_MS: f32 = 300.0;

/// `debug.alvr.cloud*` while `cloud` is 1 or 2
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CloudParams {
    /// 1: the depth cloud, today's arm shift is off while it shows. 2: on top of today's arm shift
    pub mode: u8,
    /// Reach = this % of the measured speed x the age of the field
    pub gain: f32,
    /// Largest reach (depth texels)
    pub cap: f32,
    /// Passthrough latency taken off the age of the field (ms)
    pub lambda_ms: f32,
    /// Decay time of the held speed (ms, 0: the newest pair only)
    pub hold_ms: f32,
    /// Deadband: an edge that moved less than this per pair claims no motion (depth texels)
    pub dead: f32,
    /// Distance at which a display ray is looked up in the field (m)
    pub fetch_m: f32,
    /// A pair whose head turn is faster than this claims no motion (deg/s, 0: no guard)
    pub head_max: f32,
    /// Constant reach wherever something moved recently instead of speed x age (depth texels, 0: off)
    pub fixed: f32,
    /// Alternates every 2 s between the cloud and today's picture
    pub ab: bool,
    /// A cell whose nearest edge is inside the window in only one of the two masks of a pair (an edge
    /// that entered or left the window or the image, or moved farther than the window). 0: a cell
    /// claims motion only where both masks have an edge inside the window. 1: a cell with an edge in
    /// one mask only claims none in the border band of the image (cells whose window reaches outside
    /// it). 2: every cell is measured as the plain rule says (see `pair_speed`)
    pub onesided: u8,
    /// `cloud_trail`: inside the depth mask, a border of at most this much (and at most the reach) along
    /// edges that moved is handed to the colour key instead of showing the room by itself (depth
    /// texels, 0: off)
    pub trail: f32,
}

impl Default for CloudParams {
    fn default() -> Self {
        Self {
            mode: 1,
            gain: 150.0,
            cap: 14.0,
            lambda_ms: 40.0,
            hold_ms: 200.0,
            dead: 2.5,
            fetch_m: 0.45,
            head_max: 120.0,
            fixed: 0.0,
            ab: false,
            onesided: 1,
            trail: 0.0,
        }
    }
}

/// Per frame, from the client, while `debug.alvr.cloud` is 1 or 2 (None: off)
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CloudInput {
    pub params: CloudParams,
    /// When the depth image of this frame was captured (XrTime)
    pub capture_ns: Option<i64>,
    /// Predicted display time of this frame (XrTime)
    pub display_ns: i64,
    /// False in the "today" phase of `cloud_ab`: the passes run, the mask does not read the field
    pub shown: bool,
}

/// Why a pair claimed no motion
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResetReason {
    /// No field before (switched on, depth swapchain re-created)
    Start,
    /// The two images are not 25-110 ms apart (or a capture time is unknown). History kept for 110-220
    /// ms, else cleared
    Dt,
    /// The head turned faster than `head_max`
    Head,
    /// The region changed between the two images
    Region,
}

impl ResetReason {
    pub const ALL: [Self; 4] = [Self::Start, Self::Dt, Self::Head, Self::Region];

    pub fn name(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Dt => "dt",
            Self::Head => "head",
            Self::Region => "region",
        }
    }
}

/// A depth image as the schedule sees it
#[derive(Clone, Copy)]
pub struct CloudImage {
    pub capture_ns: Option<i64>,
    /// The depth camera of each eye
    pub views: [ViewParams; 2],
    /// `region_signature` of the depth uniforms of its frame
    pub region: u64,
}

/// What an eye's field was last built from
#[derive(Clone, Copy)]
pub struct EyeSource {
    pub capture_ns: Option<i64>,
    pub view: ViewParams,
    pub region: u64,
}

/// The state of the eye schedule
#[derive(Clone, Copy, Default)]
pub struct CloudSchedule {
    /// Eye 1 of the last new image is still to be done
    pub eye_pending: bool,
    pub sources: [Option<EyeSource>; 2],
}

/// The passes of one eye on this display frame
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EyePass {
    /// The pair claims no motion: mark_main keeps the previous mask equal to the new one
    pub reset: bool,
    /// The held speed of the previous field is carried on (decayed); false: cleared
    pub keep_history: bool,
    pub reason: Option<ResetReason>,
    /// Capture time of the image before (None: none or unknown)
    pub prev_capture_ns: Option<i64>,
    /// Seconds between the two images (0: unknown)
    pub dt_s: f32,
    /// Head turn between the two depth cameras of this eye (deg/s, 0: unknown)
    pub head_deg_s: f32,
}

impl EyePass {
    /// `keep_history`: the held speed is carried on (a head turn, a skipped image)
    fn reset(
        reason: ResetReason,
        keep_history: bool,
        prev_capture_ns: Option<i64>,
        dt_s: f32,
        head: f32,
    ) -> Self {
        Self {
            reset: true,
            keep_history,
            reason: Some(reason),
            prev_capture_ns,
            dt_s,
            head_deg_s: head,
        }
    }

    /// Decay of the held speed over this pair (0: the history is not carried)
    pub fn decay(&self, params: &CloudParams) -> f32 {
        if self.keep_history && params.hold_ms > 0.0 && self.dt_s > 0.0 {
            (-self.dt_s * 1000.0 / params.hold_ms).exp()
        } else {
            0.0
        }
    }
}

/// Which eyes get their passes on this display frame
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CloudPasses {
    pub eyes: [Option<EyePass>; 2],
}

/// Head turn between two orientations `dt_s` apart (deg/s, 0 without a time)
pub fn head_speed_deg_s(from: Quat, to: Quat, dt_s: f32) -> f32 {
    if dt_s.is_nan() || dt_s <= 0.0 {
        return 0.0;
    }
    let speed = from.angle_between(to).to_degrees() / dt_s;
    if speed.is_finite() { speed } else { 0.0 }
}

fn eye_pass(
    source: Option<&EyeSource>,
    params: &CloudParams,
    image: &CloudImage,
    eye: usize,
) -> EyePass {
    let Some(source) = source else {
        return EyePass::reset(ResetReason::Start, false, None, 0.0, 0.0);
    };
    let dt_s = match (image.capture_ns, source.capture_ns) {
        (Some(now), Some(before)) => ((now - before) as f64 / 1e9) as f32,
        _ => 0.0,
    };
    let head = head_speed_deg_s(
        source.view.pose.orientation,
        image.views[eye].pose.orientation,
        dt_s,
    );
    let dt_ms = dt_s * 1000.0;
    // (reason, history kept). Too short, too long or unknown and a changed region clear the history; a
    // skipped image (110-220 ms) and a head turn keep it
    let reason = if !(CLOUD_MIN_DT_MS..=CLOUD_MAX_GAP_MS).contains(&dt_ms) {
        Some((ResetReason::Dt, false))
    } else if source.region != image.region {
        Some((ResetReason::Region, false))
    } else if dt_ms > CLOUD_MAX_DT_MS {
        Some((ResetReason::Dt, true))
    } else if params.head_max > 0.0 && head > params.head_max {
        Some((ResetReason::Head, true))
    } else {
        None
    };
    match reason {
        Some((reason, keep)) => EyePass::reset(reason, keep, source.capture_ns, dt_s, head),
        None => EyePass {
            reset: false,
            keep_history: true,
            reason: None,
            prev_capture_ns: source.capture_ns,
            dt_s,
            head_deg_s: head,
        },
    }
}

/// The passes of this display frame. `new_image`: the depth image differs from the last frame's (its
/// `DepthKey`, which also hashes the camera poses); an image with the capture time of eye 0's field is
/// the same image located again, not a new one. Eye 0 is done on the frame of a new image, eye 1 on the
/// next one (both at once if eye 1 is still waiting). The sources of the eyes done are set to `image`
pub fn plan_eyes(
    schedule: &mut CloudSchedule,
    params: &CloudParams,
    new_image: bool,
    image: &CloudImage,
) -> CloudPasses {
    let located_again = image.capture_ns.is_some()
        && schedule.sources[0].is_some_and(|source| source.capture_ns == image.capture_ns);
    let new_image = new_image && !located_again;
    let mut due = [false; 2];
    if new_image {
        due = [true, schedule.eye_pending];
        schedule.eye_pending = !schedule.eye_pending;
    } else if schedule.eye_pending {
        due[1] = true;
        schedule.eye_pending = false;
    }

    let mut passes = CloudPasses::default();
    for (eye, due) in due.into_iter().enumerate() {
        if due {
            passes.eyes[eye] = Some(eye_pass(schedule.sources[eye].as_ref(), params, image, eye));
            schedule.sources[eye] = Some(EyeSource {
                capture_ns: image.capture_ns,
                view: image.views[eye],
                region: image.region,
            });
        }
    }
    passes
}

/// How far the live picture is ahead of a field built from the depth captured at `field_capture_ns`
/// (ms, 0..250): its age at `display_ns` minus the passthrough latency
pub fn lead_ms(params: &CloudParams, display_ns: i64, field_capture_ns: Option<i64>) -> f32 {
    let Some(capture_ns) = field_capture_ns else {
        return 0.0;
    };
    let lead = ((display_ns - capture_ns) as f64 / 1e6) as f32 - params.lambda_ms;
    if lead.is_finite() {
        lead.clamp(0.0, CLOUD_MAX_LEAD_MS)
    } else {
        0.0
    }
}

/// Whether a field built from the depth captured at `field_capture_ns` may open its zone at
/// `display_ns`: it is known and not older than `CLOUD_STALE_MS` (before the latency). False also
/// without a field
pub fn field_open(display_ns: i64, field_capture_ns: Option<i64>) -> bool {
    field_capture_ns.is_some_and(|capture_ns| {
        let age_ms = ((display_ns - capture_ns) as f64 / 1e6) as f32;
        age_ms <= CLOUD_STALE_MS
    })
}

/// A field exists but is too old to open its zone (the depth stalled)
pub fn field_stale(display_ns: i64, field_capture_ns: Option<i64>) -> bool {
    field_capture_ns.is_some() && !field_open(display_ns, field_capture_ns)
}

/// The reach time of an eye on this display frame (s): the band is speed x this wide. 0 for a stale
/// field (`field_open`)
pub fn reach_seconds(params: &CloudParams, display_ns: i64, field_capture_ns: Option<i64>) -> f32 {
    if !field_open(display_ns, field_capture_ns) {
        return 0.0;
    }
    let reach = params.gain / 100.0 * lead_ms(params, display_ns, field_capture_ns) / 1000.0;
    if reach.is_finite() {
        reach.max(0.0)
    } else {
        0.0
    }
}

/// One eye's block of the uniforms: the camera of its field and of the image before, the pair
#[derive(Clone, Copy)]
pub struct CloudEyeUniform {
    pub field: ViewParams,
    pub prev: ViewParams,
    pub dt_s: f32,
    pub decay: f32,
    pub reset: bool,
}

/// Floats of `CloudEye` in depth_common.wgsl: two cameras (`PlayzoneEye`) and the pair
pub const CLOUD_EYE_FLOAT_COUNT: usize = 2 * (16 + 16 + 4) + 4;
/// Floats of `CloudUniforms`: two eyes, params, reach
pub const CLOUD_UNIFORMS_FLOAT_COUNT: usize = 2 * CLOUD_EYE_FLOAT_COUNT + 4 + 4;
pub const CLOUD_UNIFORMS_SIZE: usize = CLOUD_UNIFORMS_FLOAT_COUNT * size_of::<f32>();

fn push_camera(data: &mut Vec<f32>, view: &ViewParams) {
    let view_to_world = crate::depth_mask::pose_to_mat4(view.pose);
    data.extend(view_to_world.inverse().to_cols_array());
    data.extend(view_to_world.to_cols_array());
    data.extend(crate::depth_mask::fov_tangents(view.fov).to_array());
}

/// The uniforms of motion_cloud.wgsl and of the mask's read of the field. An eye without a field is
/// zeros: a zero matrix projects nowhere, so the mask finds no zone there. `reach_s`: of each eye.
/// `open`: whether each eye's field may open its zone on this display frame (`field_open`; false: no zone,
/// also with `cloud_fixed`)
pub fn cloud_uniforms(
    eyes: &[Option<CloudEyeUniform>; 2],
    params: &CloudParams,
    reach_s: [f32; 2],
    open: [bool; 2],
) -> [f32; CLOUD_UNIFORMS_FLOAT_COUNT] {
    let finite = |value: f32| if value.is_finite() { value } else { 0.0 };
    let mut data = Vec::with_capacity(CLOUD_UNIFORMS_FLOAT_COUNT);
    for (eye, open) in eyes.iter().zip(open) {
        if let Some(eye) = eye {
            push_camera(&mut data, &eye.field);
            push_camera(&mut data, &eye.prev);
            data.extend([
                finite(eye.dt_s).max(0.0),
                finite(eye.decay).clamp(0.0, 1.0),
                f32::from(u8::from(eye.reset)),
                f32::from(u8::from(open)),
            ]);
        } else {
            data.extend([0.0; CLOUD_EYE_FLOAT_COUNT]);
        }
    }
    data.extend([
        finite(params.fetch_m).max(0.0),
        finite(params.dead).max(0.0),
        finite(params.cap).max(0.0),
        finite(params.fixed).max(0.0),
    ]);
    data.extend([
        finite(reach_s[0]).max(0.0),
        finite(reach_s[1]).max(0.0),
        f32::from(params.onesided.min(2)),
        0.0,
    ]);

    data.try_into().unwrap()
}

/// A signature of the region part of the depth uniforms (`depth_mask::depth_uniforms`): the transform
/// of the area, the number of boxes, the region mode, max distance and soft edge, the boxes. Another
/// value: the region changed
pub fn region_signature(depth_uniforms: &[f32]) -> u64 {
    use crate::{depth_mask::LEVER_FLOAT_COUNT, play_area::AREA_FLOAT_COUNT};
    use std::hash::{DefaultHasher, Hasher};

    // world_to_area right after the two eyes of `DepthEye`, then `area` and `region`
    const AREA_START: usize = 2 * (16 + 4 + 16 + 16 + 4);
    let mut hasher = DefaultHasher::new();
    let mut add = |values: &[f32]| {
        for value in values {
            hasher.write_u32(value.to_bits());
        }
    };
    let len = depth_uniforms.len();
    if len < AREA_START + 24 + AREA_FLOAT_COUNT + LEVER_FLOAT_COUNT {
        add(depth_uniforms);
        return hasher.finish();
    }
    add(&depth_uniforms[AREA_START..AREA_START + 17]);
    add(&depth_uniforms[AREA_START + 20..AREA_START + 23]);
    let boxes_end = len - LEVER_FLOAT_COUNT;
    add(&depth_uniforms[boxes_end - AREA_FLOAT_COUNT..boxes_end]);
    hasher.finish()
}

/// The cells of the field from a mask of depth texels (`width` x `height`, rows in any order): a cell
/// of 2 x 2 texels is in when at least two of them are (their mean is at least 0.375, as rows_main
/// reads it with one bilinear tap; past the last texel the edge repeats). Returns the cells and their
/// width and height
pub fn cells_inside(texels: &[bool], width: usize, height: usize) -> (Vec<bool>, usize, usize) {
    let cells_x = width.div_ceil(2);
    let cells_y = height.div_ceil(2);
    let mut cells = vec![false; cells_x * cells_y];
    for y in 0..cells_y {
        for x in 0..cells_x {
            let mut count = 0;
            for dy in 0..2 {
                for dx in 0..2 {
                    let tx = (2 * x + dx).min(width - 1);
                    let ty = (2 * y + dy).min(height - 1);
                    count += usize::from(texels[ty * width + tx]);
                }
            }
            cells[y * cells_x + x] = count >= 2;
        }
    }
    (cells, cells_x, cells_y)
}

/// The signed distance of field_main on a grid of cells (in: true), in depth texels, positive on the
/// game side: the exact Euclidean distance (cells) to the nearest cell of the other kind within the
/// window of +-12 cells, as 2 * (d - 0.5), truncated at +-24. Cells outside the grid are neither. The
/// reference of rows_main and field_main
pub fn window_distance(inside: &[bool], width: usize, height: usize) -> Vec<f32> {
    let window = CLOUD_WINDOW_CELLS as isize;
    let mut result = vec![0.0; width * height];
    for y in 0..height {
        for x in 0..width {
            let own = inside[y * width + x];
            let mut nearest = f32::INFINITY;
            for dy in -window..=window {
                for dx in -window..=window {
                    let (cx, cy) = (x as isize + dx, y as isize + dy);
                    if cx < 0 || cy < 0 || cx >= width as isize || cy >= height as isize {
                        continue;
                    }
                    if inside[cy as usize * width + cx as usize] != own {
                        nearest = nearest.min(((dx * dx + dy * dy) as f32).sqrt());
                    }
                }
            }
            let distance = 2.0 * (nearest - 0.5);
            let signed = if own { -distance } else { distance };
            result[y * width + x] = signed.clamp(-CLOUD_TRUNCATION, CLOUD_TRUNCATION);
        }
    }
    result
}

/// The speed one pair of depth images claims at a cell of the field (texels per second), as field_main
/// computes it: `phi_raw` / `phip_raw` the signed distances of the cell to the edge of M and of the
/// corrected Mp before the truncation (texels, `window_distance` without its clamp; beyond the
/// truncation where no edge is inside the window), `border`: the cell's window reaches outside the image.
/// The held speed is not part of it
pub fn pair_speed(
    phi_raw: f32,
    phip_raw: f32,
    border: bool,
    dt_s: f32,
    params: &CloudParams,
) -> f32 {
    let truncated = |phi: f32| phi.clamp(-CLOUD_TRUNCATION, CLOUD_TRUNCATION);
    let inside = phi_raw.abs() < CLOUD_TRUNCATION;
    let inside_p = phip_raw.abs() < CLOUD_TRUNCATION;
    let refused = match params.onesided {
        0 => !(inside && inside_p),
        1 => inside != inside_p && border,
        _ => false,
    };
    let delta = (truncated(phip_raw) - truncated(phi_raw)).abs();
    if refused || delta < params.dead {
        return 0.0;
    }
    (delta / dt_s.max(0.001)).min(CLOUD_SPEED_SCALE)
}

/// One eye's field on a display frame, for the log line and bursts
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CloudEyeRecord {
    /// Capture time of the depth image the field was built from, and of the one before it
    pub field_capture_ns: Option<i64>,
    pub prev_capture_ns: Option<i64>,
    /// Between the two (ms, 0: unknown)
    pub dt_ms: f32,
    /// Why the last pair claimed no motion (None: it was measured)
    pub reset: Option<ResetReason>,
    pub head_deg_s: f32,
    /// How far the live picture is ahead of the field, and the reach time, on this display frame (ms)
    pub a_ms: f32,
    pub reach_ms: f32,
    /// The field is older than `CLOUD_STALE_MS` on this display frame: it opens no zone
    pub stale: bool,
}

impl CloudEyeRecord {
    pub fn to_json(&self) -> String {
        use crate::key_dump::{number, object, string};

        let optional = |value: Option<i64>| value.map_or("null".into(), |value| value.to_string());
        object([
            ("field_capture_ns", optional(self.field_capture_ns)),
            ("prev_capture_ns", optional(self.prev_capture_ns)),
            ("dt_ms", number(self.dt_ms)),
            (
                "reset",
                self.reset
                    .map_or("null".into(), |reason| string(reason.name())),
            ),
            ("head_deg_s", number(self.head_deg_s)),
            ("a_ms", number(self.a_ms)),
            ("reach_ms", number(self.reach_ms)),
            ("stale", self.stale.to_string()),
        ])
    }
}

/// What the renderer knows of the cloud, for the `cloud:` line and bursts
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CloudStatus {
    /// Pairs (passes) of each eye since the cloud was created
    pub pairs: [u32; 2],
    /// Of these, the resets of each eye by reason, in the order of `ResetReason::ALL`
    pub resets: [[u32; 4]; 2],
    /// Eye 0's pairs with a valid time between the images: up to 50 ms, up to 83 ms, longer (33, 67,
    /// 100 ms on the 33.3 ms grid of the depth)
    pub intervals: [u32; 3],
    /// Of the last display frame
    pub eyes: [CloudEyeRecord; 2],
}

impl CloudStatus {
    /// Counts a pass of an eye, whose image was captured at `capture_ns`
    pub fn count(&mut self, eye: usize, pass: &EyePass, capture_ns: Option<i64>) {
        self.pairs[eye] += 1;
        if let Some(reason) = pass.reason {
            self.resets[eye][ResetReason::ALL.iter().position(|r| *r == reason).unwrap()] += 1;
        }
        let dt_ms = pass.dt_s * 1000.0;
        if eye == 0 && !matches!(pass.reason, Some(ResetReason::Start | ResetReason::Dt)) {
            self.intervals[if dt_ms < 50.0 {
                0
            } else if dt_ms < 83.0 {
                1
            } else {
                2
            }] += 1;
        }
        let record = &mut self.eyes[eye];
        record.field_capture_ns = capture_ns;
        record.prev_capture_ns = pass.prev_capture_ns;
        record.dt_ms = dt_ms;
        record.reset = pass.reason;
        record.head_deg_s = pass.head_deg_s;
    }
}

/// The cloud in a display record of a burst (index version 7): the properties and the fields of the
/// two eyes on that frame
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CloudRecord {
    pub params: CloudParams,
    pub shown: bool,
    pub eyes: [CloudEyeRecord; 2],
}

impl CloudRecord {
    pub fn to_json(&self) -> String {
        use crate::key_dump::{array, number, object};

        let params = &self.params;
        object([
            ("mode", params.mode.to_string()),
            ("shown", self.shown.to_string()),
            ("gain", number(params.gain)),
            ("cap", number(params.cap)),
            ("hold_ms", number(params.hold_ms)),
            ("dead", number(params.dead)),
            ("lambda_ms", number(params.lambda_ms)),
            ("fetch_mm", number(params.fetch_m * 1000.0)),
            ("fixed", number(params.fixed)),
            ("ab", params.ab.to_string()),
            ("onesided", params.onesided.to_string()),
            ("trail", number(params.trail)),
            ("eyes", array(self.eyes.map(|eye| eye.to_json()))),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alvr_common::{Fov, Pose, glam::Vec3};

    const FRAME_NS: i64 = 33_333_333;

    fn views(yaw_deg: f32) -> [ViewParams; 2] {
        let orientation = Quat::from_rotation_y(yaw_deg.to_radians());
        [-0.032, 0.032].map(|x| ViewParams {
            pose: Pose {
                orientation,
                position: Vec3::new(x, 1.2, 0.0),
            },
            fov: Fov {
                left: -0.8,
                right: 0.8,
                up: 0.8,
                down: -0.8,
            },
        })
    }

    fn image(capture_ns: i64, yaw_deg: f32, region: u64) -> CloudImage {
        CloudImage {
            capture_ns: Some(capture_ns),
            views: views(yaw_deg),
            region,
        }
    }

    fn due(passes: &CloudPasses) -> [bool; 2] {
        passes.eyes.map(|eye| eye.is_some())
    }

    #[test]
    fn plan_schedules_the_eyes() {
        let params = CloudParams::default();
        let mut schedule = CloudSchedule::default();
        // A new image: eye 0 now, eye 1 on the next frame, then nothing until the next image
        let first = image(0, 0.0, 1);
        assert_eq!(
            due(&plan_eyes(&mut schedule, &params, true, &first)),
            [true, false]
        );
        assert_eq!(
            due(&plan_eyes(&mut schedule, &params, false, &first)),
            [false, true]
        );
        assert_eq!(
            due(&plan_eyes(&mut schedule, &params, false, &first)),
            [false, false]
        );
        assert_eq!(
            schedule.sources.map(|s| s.unwrap().capture_ns),
            [Some(0); 2]
        );

        // Two new images in a row: eye 1 waits on the first and is done with the second, with eye 0
        let second = image(2 * FRAME_NS, 0.0, 1);
        let third = image(3 * FRAME_NS, 0.0, 1);
        let passes = plan_eyes(&mut schedule, &params, true, &second);
        assert_eq!(due(&passes), [true, false]);
        let passes = plan_eyes(&mut schedule, &params, true, &third);
        assert_eq!(due(&passes), [true, true]);
        // Eye 1 skipped the second image: its pair spans 100 ms
        let eye1 = passes.eyes[1].unwrap();
        assert_eq!(eye1.prev_capture_ns, Some(0));
        assert!((eye1.dt_s - 0.1).abs() < 1e-6, "{}", eye1.dt_s);
        assert!(!eye1.reset);
        assert_eq!(
            due(&plan_eyes(&mut schedule, &params, false, &third)),
            [false, false]
        );
        assert_eq!(
            schedule.sources.map(|s| s.unwrap().capture_ns),
            [Some(3 * FRAME_NS); 2]
        );
    }

    // The runtime locates the same image again (its views move a little, so its `DepthKey` changes):
    // with the capture time of eye 0's field it is not a new image, no pass and no Dt reset. Without a
    // capture time it cannot be told
    #[test]
    fn plan_same_image_located_again() {
        let params = CloudParams::default();
        let mut schedule = CloudSchedule::default();
        plan_eyes(&mut schedule, &params, true, &image(0, 0.0, 1));
        plan_eyes(&mut schedule, &params, false, &image(0, 0.0, 1));
        let pass = plan_eyes(&mut schedule, &params, true, &image(2 * FRAME_NS, 0.0, 1));
        assert_eq!(pass.eyes[0].unwrap().reason, None);

        // Located again on the next frame: eye 1 is done as scheduled, eye 0 not again
        let again = image(2 * FRAME_NS, 0.5, 1);
        let passes = plan_eyes(&mut schedule, &params, true, &again);
        assert_eq!(due(&passes), [false, true]);
        assert_eq!(passes.eyes[1].unwrap().reason, None);
        // And again: nothing
        let passes = plan_eyes(&mut schedule, &params, true, &image(2 * FRAME_NS, 1.0, 1));
        assert_eq!(due(&passes), [false, false]);
        assert!(!schedule.eye_pending);

        // The next image is new, its pair measured over the real 33 ms
        let pass =
            plan_eyes(&mut schedule, &params, true, &image(3 * FRAME_NS, 1.0, 1)).eyes[0].unwrap();
        assert_eq!(pass.reason, None);
        assert!((pass.dt_s - 0.0333).abs() < 1e-3, "{}", pass.dt_s);

        // Without capture times every new key is a new image
        let unknown = CloudImage {
            capture_ns: None,
            ..image(0, 0.0, 1)
        };
        let mut schedule = CloudSchedule::default();
        plan_eyes(&mut schedule, &params, true, &unknown);
        plan_eyes(&mut schedule, &params, false, &unknown);
        let passes = plan_eyes(&mut schedule, &params, true, &unknown);
        assert_eq!(due(&passes), [true, false]);
    }

    // Two images on consecutive display frames: eye 1 skips the first and its pair spans two intervals
    // (133 ms here). It claims no motion but keeps its history, as eye 0 does over its 33 ms pair: the
    // eyes stay alike
    #[test]
    fn plan_eye_1_after_a_skip_keeps_its_history() {
        let params = CloudParams::default();
        let mut schedule = CloudSchedule::default();
        plan_eyes(&mut schedule, &params, true, &image(0, 0.0, 1));
        plan_eyes(&mut schedule, &params, false, &image(0, 0.0, 1));
        // 100 ms later (eye 0 measured), eye 1 waits
        let passes = plan_eyes(&mut schedule, &params, true, &image(3 * FRAME_NS, 0.0, 1));
        assert_eq!(due(&passes), [true, false]);
        assert_eq!(passes.eyes[0].unwrap().reason, None);
        // The next image on the next display frame: both eyes
        let passes = plan_eyes(&mut schedule, &params, true, &image(4 * FRAME_NS, 0.0, 1));
        let [eye0, eye1] = passes.eyes.map(Option::unwrap);
        assert_eq!(eye0.reason, None);
        assert_eq!(eye1.reason, Some(ResetReason::Dt));
        assert!(eye1.reset && eye1.keep_history);
        assert!((eye1.dt_s - 0.1333).abs() < 1e-3, "{}", eye1.dt_s);
        assert!((eye1.decay(&params) - (-0.1333_f32 / 0.2).exp()).abs() < 1e-3);
    }

    #[test]
    fn plan_resets() {
        let params = CloudParams::default();
        let mut schedule = CloudSchedule::default();
        let eye0 = |schedule: &mut CloudSchedule, image: CloudImage| {
            let pass = plan_eyes(schedule, &params, true, &image).eyes[0].unwrap();
            // Eye 1 is done on the next frame
            plan_eyes(schedule, &params, false, &image);
            pass
        };

        // The first frame: no field before, history cleared
        let pass = eye0(&mut schedule, image(0, 0.0, 1));
        assert_eq!(pass.reason, Some(ResetReason::Start));
        assert!(pass.reset && !pass.keep_history);
        assert_eq!(pass.decay(&params), 0.0);

        // 67 ms later, still: measured, the history decays over the hold
        let pass = eye0(&mut schedule, image(2 * FRAME_NS, 0.0, 1));
        assert_eq!(pass.reason, None);
        assert!(!pass.reset && pass.keep_history);
        assert!((pass.decay(&params) - (-0.0667_f32 / 0.2).exp()).abs() < 1e-3);
        // `cloud_hold_ms 0`: the newest pair only
        let newest = CloudParams {
            hold_ms: 0.0,
            ..params
        };
        assert_eq!(pass.decay(&newest), 0.0);

        // 200 ms later (a skipped image): no motion claimed, history kept, decayed over the 200 ms
        let pass = eye0(&mut schedule, image(8 * FRAME_NS, 0.0, 1));
        assert_eq!(pass.reason, Some(ResetReason::Dt));
        assert!(pass.reset && pass.keep_history);
        assert!((pass.decay(&params) - (-1.0_f32).exp()).abs() < 1e-3);

        // 233 ms later: no motion claimed, history cleared
        let pass = eye0(&mut schedule, image(15 * FRAME_NS, 0.0, 1));
        assert_eq!(pass.reason, Some(ResetReason::Dt));
        assert!(pass.reset && !pass.keep_history);
        assert_eq!(pass.decay(&params), 0.0);

        // A head turn of 10 degrees in 67 ms (150 deg/s): no motion claimed, history kept
        let pass = eye0(&mut schedule, image(17 * FRAME_NS, 10.0, 1));
        assert_eq!(pass.reason, Some(ResetReason::Head));
        assert!(pass.reset && pass.keep_history);
        assert!((pass.head_deg_s - 150.0).abs() < 0.5, "{}", pass.head_deg_s);
        assert!(pass.decay(&params) > 0.0);
        // Without the guard it is measured
        let unguarded = CloudParams {
            head_max: 0.0,
            ..params
        };
        let pass = plan_eyes(
            &mut schedule,
            &unguarded,
            true,
            &image(19 * FRAME_NS, 20.0, 1),
        )
        .eyes[0]
            .unwrap();
        assert_eq!(pass.reason, None);
        plan_eyes(
            &mut schedule,
            &unguarded,
            false,
            &image(19 * FRAME_NS, 20.0, 1),
        );

        // The region changed: no motion claimed, history cleared
        let pass = eye0(&mut schedule, image(21 * FRAME_NS, 20.0, 2));
        assert_eq!(pass.reason, Some(ResetReason::Region));
        assert!(pass.reset && !pass.keep_history);
        // Also after a skipped image
        let pass = eye0(&mut schedule, image(25 * FRAME_NS, 20.0, 3));
        assert_eq!(pass.reason, Some(ResetReason::Region));
        assert!(pass.reset && !pass.keep_history);
        // Below 25 ms: no motion claimed, history cleared
        let pass = eye0(&mut schedule, image(25 * FRAME_NS + 20_000_000, 20.0, 3));
        assert_eq!(pass.reason, Some(ResetReason::Dt));
        assert!(pass.reset && !pass.keep_history);

        // An unknown capture time cannot be measured
        let pass = eye0(
            &mut schedule,
            CloudImage {
                capture_ns: None,
                ..image(0, 20.0, 2)
            },
        );
        assert_eq!(pass.reason, Some(ResetReason::Dt));

        // The counts of the status
        let mut status = CloudStatus::default();
        status.count(0, &pass, None);
        status.count(
            1,
            &EyePass::reset(ResetReason::Head, true, Some(1), 0.033, 200.0),
            Some(2),
        );
        assert_eq!(status.pairs, [1, 1]);
        // Per eye
        assert_eq!(status.resets, [[0, 1, 0, 0], [0, 0, 1, 0]]);
        assert_eq!(status.intervals, [0; 3]);
        assert_eq!(status.eyes[1].reset, Some(ResetReason::Head));
    }

    #[test]
    fn reach_time() {
        let params = CloudParams::default();
        let ms = 1_000_000;
        // 131 ms old, minus 40 ms of latency, x 1.5
        assert!((lead_ms(&params, 1000 * ms, Some(869 * ms)) - 91.0).abs() < 1e-3);
        assert!((reach_seconds(&params, 1000 * ms, Some(869 * ms)) - 0.1365).abs() < 1e-5);
        // Younger than the latency: 0. Older than 250 ms + latency: 250 ms
        assert_eq!(reach_seconds(&params, 1000 * ms, Some(980 * ms)), 0.0);
        assert!((lead_ms(&params, 1000 * ms, Some(0)) - 250.0).abs() < 1e-3);
        // No field: 0
        assert_eq!(reach_seconds(&params, 1000 * ms, None), 0.0);
        assert!(!field_open(1000 * ms, None) && !field_stale(1000 * ms, None));
        // A field 299 ms old still opens (250 ms of lead), one 301 ms old is stale: 0
        assert!(field_open(1000 * ms, Some(701 * ms)));
        assert!(!field_stale(1000 * ms, Some(701 * ms)));
        assert!((reach_seconds(&params, 1000 * ms, Some(701 * ms)) - 0.375).abs() < 1e-5);
        assert!(field_open(1000 * ms, Some(700 * ms)));
        assert!(!field_open(1000 * ms, Some(699 * ms)));
        assert!(field_stale(1000 * ms, Some(699 * ms)));
        assert_eq!(reach_seconds(&params, 1000 * ms, Some(699 * ms)), 0.0);
        // The age counts before the latency
        let late = CloudParams {
            lambda_ms: 120.0,
            ..params
        };
        assert!(reach_seconds(&late, 1000 * ms, Some(701 * ms)) > 0.0);
        assert_eq!(reach_seconds(&late, 1000 * ms, Some(699 * ms)), 0.0);
        // Without the latency the whole age counts
        let params = CloudParams {
            lambda_ms: 0.0,
            gain: 100.0,
            ..params
        };
        assert!((reach_seconds(&params, 1000 * ms, Some(900 * ms)) - 0.1).abs() < 1e-6);
    }

    #[test]
    fn uniforms_hold_each_eye_and_the_params() {
        let [left, right] = views(0.0);
        let eye = CloudEyeUniform {
            field: left,
            prev: right,
            dt_s: 0.067,
            decay: 0.7,
            reset: true,
        };
        let data = cloud_uniforms(
            &[Some(eye), None],
            &CloudParams::default(),
            [0.1, 0.2],
            [true, true],
        );
        assert_eq!(data.len() * 4, CLOUD_UNIFORMS_SIZE);
        assert_eq!(CLOUD_UNIFORMS_SIZE, 640);
        // world_to_view of the field moves its position to the origin
        let world_to_view = alvr_common::glam::Mat4::from_cols_slice(&data[..16]);
        assert!(world_to_view.transform_point3(left.pose.position).length() < 1e-5);
        let prev_view_to_world = alvr_common::glam::Mat4::from_cols_slice(&data[36 + 16..36 + 32]);
        assert!(
            prev_view_to_world
                .w_axis
                .truncate()
                .distance(right.pose.position)
                < 1e-6
        );
        // The pair, w: the field may open its zone
        assert_eq!(&data[72..76], &[0.067, 0.7, 1.0, 1.0]);
        // No right eye: zeros (also not open)
        assert!(data[76..152].iter().all(|value| *value == 0.0));
        assert_eq!(&data[152..160], &[0.45, 2.5, 14.0, 0.0, 0.1, 0.2, 1.0, 0.0]);
        // A stale field: closed
        let data = cloud_uniforms(
            &[Some(eye), Some(eye)],
            &CloudParams::default(),
            [0.0; 2],
            [false, true],
        );
        assert_eq!(&data[72..76], &[0.067, 0.7, 1.0, 0.0]);
        assert_eq!(&data[148..152], &[0.067, 0.7, 1.0, 1.0]);
    }

    #[test]
    fn pair_speed_follows_the_onesided_mode() {
        let dt = 0.0667;
        let mode = |onesided| CloudParams {
            onesided,
            ..Default::default()
        };
        // Both edges in the window: measured in every mode, also in the border band
        for onesided in 0..3 {
            let speed = pair_speed(3.0, 9.0, true, dt, &mode(onesided));
            assert!((speed - 6.0 / dt).abs() < 1e-3, "{speed}");
            // Under the deadband
            assert_eq!(pair_speed(3.0, 5.0, false, dt, &mode(onesided)), 0.0);
        }
        // An edge in M only (Mp has none in the window, 33 texels: the corner of the window)
        let one_sided = |onesided, border| pair_speed(5.0, 33.0, border, dt, &mode(onesided));
        assert_eq!(one_sided(0, false), 0.0);
        assert_eq!(one_sided(0, true), 0.0);
        assert!((one_sided(1, false) - 19.0 / dt).abs() < 1e-3);
        assert_eq!(one_sided(1, true), 0.0);
        assert!((one_sided(2, true) - 19.0 / dt).abs() < 1e-3);
        // Truncated distances equal to the truncation are not inside the window
        assert_eq!(pair_speed(-24.0, 1.0, false, dt, &mode(0)), 0.0);
        // No edge in either mask but the side changed (deep inside a thing that appeared): measured
        // unless mode 0 asks for both edges
        assert_eq!(pair_speed(-30.0, 30.0, false, dt, &mode(0)), 0.0);
        for onesided in 1..3 {
            let speed = pair_speed(-30.0, 30.0, true, 0.1, &mode(onesided));
            assert!((speed - 480.0).abs() < 1e-3, "{speed}");
        }
        // At most the speed scale
        assert_eq!(
            pair_speed(-24.0, 23.0, false, 0.03, &mode(2)),
            CLOUD_SPEED_SCALE
        );
    }

    #[test]
    fn region_signature_follows_the_region_only() {
        let count = crate::depth_mask::DEPTH_UNIFORMS_FLOAT_COUNT;
        let base = vec![0.5; count];
        let signature = region_signature(&base);
        // An eye pose, the hands: the same
        let mut other = base.clone();
        other[3] = 2.0;
        other[300] = 2.0;
        assert_eq!(region_signature(&other), signature);
        // The transform of the area, the soft edge, a box: another
        for index in [112, 134, count - crate::depth_mask::LEVER_FLOAT_COUNT - 1] {
            let mut other = base.clone();
            other[index] = 2.0;
            assert_ne!(region_signature(&other), signature, "{index}");
        }
    }

    #[test]
    fn window_distance_is_exact_inside_the_window() {
        // One cell in, at (10, 10) of 40 x 30
        let (width, height) = (40, 30);
        let mut inside = vec![false; width * height];
        inside[10 * width + 10] = true;
        let phi = window_distance(&inside, width, height);
        assert_eq!(phi[10 * width + 10], -1.0);
        assert_eq!(phi[10 * width + 11], 1.0);
        // 3 and 4 cells away: 5 cells, 9 texels
        assert_eq!(phi[14 * width + 13], 9.0);
        // Beyond the window: truncated
        assert_eq!(phi[10 * width + 30], CLOUD_TRUNCATION);
        // Nothing in: truncated everywhere
        assert!(
            window_distance(&[false; 12], 4, 3)
                .iter()
                .all(|phi| *phi == CLOUD_TRUNCATION)
        );

        // Cells of texels: in from two of four
        let texels = [true, false, true, true, false, false, false, true];
        let (cells, w, h) = cells_inside(&texels, 4, 2);
        assert_eq!((w, h), (2, 1));
        assert_eq!(cells, [false, true]);
    }

    #[test]
    fn record_json_is_valid() {
        let record = CloudRecord {
            params: CloudParams::default(),
            shown: true,
            eyes: [
                CloudEyeRecord {
                    field_capture_ns: Some(123),
                    prev_capture_ns: None,
                    dt_ms: 33.3,
                    reset: Some(ResetReason::Head),
                    head_deg_s: 150.0,
                    a_ms: 91.0,
                    reach_ms: 136.5,
                    stale: false,
                },
                CloudEyeRecord {
                    stale: true,
                    ..Default::default()
                },
            ],
        };
        let json = record.to_json();
        assert!(crate::key_dump::tests::is_valid_json(&json), "{json}");
        assert!(
            json.starts_with(r#"{"mode":1,"shown":true,"gain":150"#),
            "{json}"
        );
        assert!(json.contains(r#""fetch_mm":450"#), "{json}");
        assert!(json.contains(r#""onesided":1,"trail":0,"eyes""#), "{json}");
        assert!(json.contains(r#""reset":"head""#), "{json}");
        assert!(
            json.contains(r#""reach_ms":136.5,"stale":false},"#),
            "{json}"
        );
        assert!(json.contains(r#""stale":true}]}"#), "{json}");
        assert!(
            json.contains(r#""field_capture_ns":123,"prev_capture_ns":null"#),
            "{json}"
        );
    }
}
