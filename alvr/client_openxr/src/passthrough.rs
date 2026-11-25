use crate::extra_extensions::{COLOR_KEY_SIZE, PassthroughFB, PassthroughHTC};
use alvr_common::{
    anyhow::{Result, bail},
    error,
};
use alvr_graphics::{HandSweep, HandZones, SweepParams};
use alvr_system_info::Platform;
use openxr::{self as xr};
use std::{
    fs,
    marker::PhantomData,
    ops::Deref,
    ptr,
    time::{Duration, Instant},
};

pub struct PassthroughLayer<'a> {
    handle_fb: Option<PassthroughFB>,
    handle_htc: Option<PassthroughHTC>,
    // Version of the applied color key, 0: none
    color_key_version: u32,
    _marker: PhantomData<&'a ()>,
}

impl PassthroughLayer<'_> {
    pub fn new(session: &xr::Session<xr::OpenGlEs>, platform: Platform) -> Result<Self> {
        let mut handle_fb = None;
        let mut handle_htc = None;

        let exts = session.instance().exts();
        if exts.fb_passthrough.is_some() {
            handle_fb = Some(PassthroughFB::new(session, platform)?);
        } else if exts.htc_passthrough.is_some() {
            handle_htc = Some(PassthroughHTC::new(session)?);
        } else {
            bail!("No passthrough extension available");
        };

        Ok(Self {
            handle_fb,
            handle_htc,
            color_key_version: 0,
            _marker: PhantomData,
        })
    }

    /// Apply or remove the color key. Returns whether the passthrough is color keyed.
    pub fn update_color_key(&mut self, key: Option<&ColorKey>) -> bool {
        let Some(handle) = &mut self.handle_fb else {
            return false;
        };

        let version = key.map_or(0, |key| key.version);
        if version != self.color_key_version {
            match handle.set_color_key(key.map(|key| key.lut.as_slice())) {
                Ok(()) => (),
                Err(e) => error!("Passthrough color key: {e}"),
            }
            // Don't retry a failed key every frame
            self.color_key_version = version;
        }

        handle.color_key_layer().is_some()
    }

    /// Replace the colors of the key, until the next change of its version
    pub fn set_color_key_lut(&mut self, lut: &[u8]) {
        if let Some(handle) = &mut self.handle_fb
            && let Err(e) = handle.update_color_key(lut)
        {
            error!("Passthrough color key update: {e}");
        }
    }

    /// The color keyed layer, if `update_color_key` returned true
    pub fn color_key_layer(&self) -> Option<&xr::CompositionLayerBase<'_, xr::OpenGlEs>> {
        let layer = self.handle_fb.as_ref()?.color_key_layer()?;

        Some(unsafe { &*ptr::from_ref(layer).cast() })
    }
}

/// Experimental passthrough color key, controlled with adb:
/// - push a LUT (see `COLOR_KEY_SIZE`) to one of `COLOR_KEY_PATHS`
/// - `setprop debug.alvr.color_key <n>`: 0 disables the key, another value (re)loads the LUT
/// - `setprop debug.alvr.key_rim <mm>`: width of the color keyed rim around the hands
/// - `setprop debug.alvr.key_inset <mm>`: how much the solid inside of the hands is shrunk
/// - `setprop debug.alvr.key_falloff <mm>`: how gradually the solid inside fades out
/// - `setprop debug.alvr.key_error <ms>`: assumed timing error, the solid core shrinks with the speed
/// - `setprop debug.alvr.key_rim_error <ms>`: worst timing error, the rim grows with the speed
/// - `setprop debug.alvr.key_rim_max <mm>`: limit of that growth
/// - `setprop debug.alvr.key_speed <n>`: that speed is of the wrist (2), of the palm, the wrist and the
///   knuckles (1: a hand that turns in place grows the rim) or of the fastest joint (0: also moving fingers)
/// - `setprop debug.alvr.key_sweep 0`: the rim grows in all directions, not along the motion of the hand
/// - `setprop debug.alvr.key_sweep_max <mm>`: limit of the growth along the motion, ahead and behind
/// - `setprop debug.alvr.key_sweep_iso <percent>`: share of the growth that stays in all directions
/// - `setprop debug.alvr.key_sweep_bias <percent>`: -100..100, positive grows further ahead than behind
/// - `setprop debug.alvr.key_unknown <percent>`: how much to keep colors that were never seen
/// - `setprop debug.alvr.key_bias <percent>`: preference for skin if the background looks the same
/// - `setprop debug.alvr.key_tight <percent>`: how similar to the skin a color must be
/// - `setprop debug.alvr.key_smooth <percent>`: width of the smoothing of the skin colors, of a LUT texel
///   (70..100). `key_tight` is scaled by its cube: 70 keeps rarer skin colors, not more around common ones
/// - `setprop debug.alvr.key_confirm <percent>`: `key_tight` of a second table (never uploaded) for the points
///   on the axis of each segment when hand segments are verified, and for its reliability. At most
///   `key_tight`. Default 12, 0: off, the key table
/// - `setprop debug.alvr.key_steady 0`: a hand is verified only if all its joints are tracked and slow, as
///   before. 1: if its palm is, each segment whose two joints are. Its reliability then comes from its own
///   background only
/// - `setprop debug.alvr.playzone <n>`: another positive value remembers the playzone at the next depth image
///   (keep the hands away). The value at the start of the stream is not a request
/// - `setprop debug.alvr.playzone_auto 0`: don't remember the playzone when no hand was near the view of the
///   depth camera for 1.5 s (then every 0.5 s while none is)
/// - `setprop debug.alvr.playzone_view 1`: with the "Tint mask" debug view, what only the remembered playzone
///   shows is half opaque blue, and the area near the hands where it is used faintly blue
/// - `setprop debug.alvr.auto_key 1` before starting the app: learn the colors with the camera
/// - `setprop debug.alvr.key_solid <n>`: capsules that are always solid, 0: none, 6: wrist, 9: palm, 28: whole hand
/// - `setprop debug.alvr.key_scale <percent>`: thickness of the hands
/// - `setprop debug.alvr.key_delay <ms>`: how late (may be negative) the passthrough image is, to locate the hands at
///   the moment it shows
/// - `setprop debug.alvr.key_layers <n>`: 4: the extra layers only with hands in view, 2: never, 5: always,
///   30 / 31: always, three layers for measurements (without the upper game layer / the keyed passthrough).
///   Unset while `debug.alvr.cloud` is 1 or more: 5
/// - `setprop debug.alvr.key_crop 0`: don't crop the lower game layer to the hands, 2: crop with the y origin
///   at the top of the image. Unset while `debug.alvr.cloud` is 1 or more: 0
/// - `setprop debug.alvr.cloud 1`: the depth cloud (docs/depth-cloud.md, motion_cloud.rs): around the edges of
///   the depth mask that moved since the depth image before, a band of measured speed x age of the depth in
///   which the key decides; today's arm shift is off while it shows. 2: on top of today's arm shift. 0 or
///   unset: off, today's picture (the default). Needs the key and a play area of boxes. `cloud_gain`
///   (150 %, 0..400), `cloud_cap` (14 depth texels, 4..2 x `cloud_window`), `cloud_hold_ms` (200, 0..500),
///   `cloud_dead` (25 tenths of a texel per pair, 15..60), `cloud_lambda_ms` (40, 0..120), `cloud_fetch_mm`
///   (450, 200..1500), `cloud_head_max` (120 deg/s, 0..1000, 0: no guard), `cloud_fixed` (0, 0..2 x
///   `cloud_window` texels: a constant reach), `cloud_onesided` (1, 0..2: where an edge is in only one depth
///   image of a pair, the pair claims no speed anywhere (0), none in the border band of the image (1) or a
///   speed everywhere (2)), `cloud_window` (12 cells, 12..24: how far the field searches for edges; the band
///   reaches at most 2 x this texels), `cloud_trail_age` (16 depth images, 0..255, 0: no age test: the trail
///   hands only surface to the key that arrived in the mask within these), `cloud_ab 1`: 2 s the cloud, 2 s today's picture, with a green / red
///   mark. A `cloud:` line every 2 s
/// - `setprop debug.alvr.key_hands 0`: no hands in the GPU passes (to measure them)
/// - `setprop debug.alvr.arm_shift 0`: plain depth for the arms, not shifted by the motion of the wrist (also
///   without the key, see `StreamContext`). 1 (default): shifted with the display-time wrist, a smoothstep to
///   45 cm. 2: from the arm's own history of hand samples (`arm_history.rs`): moved from the wrist at the
///   depth's capture time to the wrist of `arm_estimator`, a linear weight
/// - `setprop debug.alvr.arm_hands <n>`: arms that use `arm_shift 2` (1 left, 2 right, 3 both), the others 1
/// - `setprop debug.alvr.arm_estimator <n>`: with `arm_shift 2`, 0 (default): the newest hand sample as it
///   is; 1: the line through the two newest samples at the display time minus `arm_target_ms`; 2: the
///   velocity of a parabola over 120 ms, to the same time. At most 6 cm from the newest sample
/// - `setprop debug.alvr.arm_target_ms <ms>` / `arm_weight_mm <mm>`: with `arm_shift 2` and estimator 1 or
///   2 the arm is placed for the display time minus `arm_target_ms` (0..150, 40); the weight falls linearly
///   to 0 over `arm_weight_mm` (100..600, 300). `arm_history_ms` is no longer used
/// - `setprop debug.alvr.arm_sample_age_ms <ms>`: how old a hand sample is when the app first gets it
///   (0..150, 35), its time when the runtime gives no capture time; `arm_capture_time 0`: never use the
///   runtime's capture time (XR_META_hand_tracking_unextrapolated_poses)
/// - `setprop debug.alvr.edge_cut <percent>`: where the mask edge is cut, 20 (default, also 0): today's, a
///   rim of room around still objects; 50 / 65 / 80 draw the edge further on the object (17..84). Also
///   without the key
/// - `setprop debug.alvr.arm_gap_mm <mm>`: the strip the runtime removes behind the wrist is filled with the
///   forearm's depth from this far up the arm (0: off, default, ..80); `arm_gap_hands <n>`: on which arms
///   (1 left, 2 right, 3 both, default)
/// - `setprop debug.alvr.lever_boxes <n,n>`: play-area boxes that are levers (shifter, handbrake), by their
///   `#<index>` in the area editor's log (from 0, at most 4, separated by commas: `1` or `1,2`; unset or
///   empty: off, the default). While a joint of a tracked hand is within `lever_gate_mm` (130, 0..500) of
///   such a box, every view ray that crosses the box shows the room, whatever the depth says: a held lever
///   is removed from the depth with the hand. The window closes once the depth shown was captured
///   `lever_close_ms` (300, 0..3000) after the hand left (1 s later at most without newer depth), then
///   fades over 150 ms. Needs the play area placed from the marker. `lever_shrink 1`: the window is the
///   palm's path inside the box plus 2 cm in x and z, the whole height of the box, learned for the session
///   (0, the default: the whole box); `lever_forget <n>`: another value forgets the learned paths. Box
///   indices follow the editor's order: removing a box shifts them. A `lever:` line every 2 s (U2 of
///   design/universal/final.md, see lever.rs)
/// - `setprop debug.alvr.mem_roll 1`: the room memory (U3 of design/universal/final.md, room_memory.rs) instead
///   of today's playzone copy (0 or unset: today's, the default). A depth map in a wide camera fixed in the
///   room (120 x 120 degrees, at the head when anchored, along its yaw, `mem_pitch_deg` (35, 0..80) below
///   the horizon), written from the live depth on every new depth image, kept ("frozen") within
///   `mem_margin_mm` (60, 20..150) of the hand capsules at the depth's capture time. The mask reads only
///   frozen texels: a moved bottle, legs or the chair leave no ghost; a held object stays whole. Needs the
///   color key. `mem_outer_mm <mm>` (0: off, ..250): beyond the margin up to this distance a texel is also
///   kept if the live depth there is the far fill (8 cm or more behind it). `mem_size 384`: smaller
///   textures (512 default, read when they are created). `mem_view 1`: with the "Tint mask" debug view
///   what only the memory shows is blue; 2: paler the older it is. `mem_moving 1`: texels marked moving (the
///   live depth changed by 3 cm with no hand within 20 cm, in the last 5 s) are not read either. A
///   `memory:` line every 2 s; `debug.alvr.playzone -1` forgets it too
/// - `setprop debug.alvr.playzone -1`: forget the remembered playzone
/// - `setprop debug.alvr.key_leak <percent>`: narrow the rim where the background around a hand looks like
///   the hand (needs `auto_key`), 0: off, 100: on
/// - `setprop debug.alvr.key_leak_lo <percent>` / `key_leak_hi`: share of the background around the hand
///   that is kept by the key, from where the rim is narrowed to where it is narrowest
/// - `setprop debug.alvr.key_leak_inset <mm>`: the narrowest rim, inside of the hand
/// - `setprop debug.alvr.key_leak_force <percent>`: narrow the rim of both hands this much, without the camera
/// - `setprop debug.alvr.key_leak_view 1`: with the "Tint mask" debug view the narrowed rim is red. 2: the
///   right hand is replaced by spheres where the leak map says the room leaks (and the map is learned)
/// - `setprop debug.alvr.key_leak_map <percent>`: weight of the leak map (directions around the head where the
///   key showed the room), 0: off, the map is not learned
/// - `setprop debug.alvr.key_leak_map_lo <percent>` / `key_leak_map_hi`: leak of the cells behind the hand,
///   from where the rim is narrowed to where it is narrowest
/// - `setprop debug.alvr.key_leak_memory <s>`: how long the leak map remembers a direction
/// - `setprop debug.alvr.key_leak_clear <n>`: another value forgets the leak map
/// - `setprop debug.alvr.key_leak_dump <n>`: another value logs the leak map of the camera view (`ALVR_CAM`),
///   a value other than 0 also learns the map
/// - `setprop debug.alvr.key_leak_probe <cm>`: distance of the spheres of `key_leak_view 2`
/// - `setprop debug.alvr.problem_view 1` (U4): the problem view, 0 (default): off, the picture is unchanged.
///   The room shows through the dimmed game and each direction around the head is tinted from the leak
///   map: nothing under `problem_view_lo`, amber hatching up to `problem_view_hi`, red cross-hatching
///   above, grey dots where the map doesn't know the direction. Depth passthrough only, never while the
///   play area is edited. It also learns the leak map (without narrowing the rim like `key_leak_map`).
///   Also logged every 2 s as `sep:` (`ALVR_CAM`, with the key learner): recall of the hand labels, the
///   leak of each hand, the known cells and the 10 leakiest cells (yaw/pitch in degrees @ leak)
/// - `setprop debug.alvr.problem_view_lo <percent>` / `problem_view_hi`: the thresholds of the tint (cell
///   leak), default `key_leak_map_lo` / `key_leak_map_hi` (3 / 15)
/// - `setprop debug.alvr.problem_view_room <percent>` / `problem_view_dim`: how much the room shows through
///   the game (default 85) and how much the game is dimmed (default 50) in the problem view
/// - `setprop debug.alvr.key_lut_dump <n>`: another value writes the learned LUT to the files folder of the app
/// - `setprop debug.alvr.key_dump <n>`: another value other than 0 writes the next learning passes to the files
///   folder of the app, `alvr_dump_<n>_<index>`: the camera image as the learner reads it (`.rgb`), the hands,
///   the camera and the counts of the pass (`.json`), with the first pass the key table (`_key.lut`), the
///   alpha of the key table as the learner uses it (`_key_alpha.lut`) and of the confirm table (`_confirm.lut`)
///   The render thread writes the depth of the next frame with a depth image once per new value (also without
///   `auto_key`): `alvr_dump_<n>_depth.json`, the depth and the remembered playzone of each eye in meters
///   (`_depth_left.f32`, `_playzone_left.f32`, ...), the depth mask and the focus area (`_mask_left.rgba`,
///   `_focus_left.rgba`, ...). With `mem_roll 1` also the room memory of each eye (`_memory_left.f32`, 4
///   floats per texel: distance, moving, frozen, age in s) and its camera (`memory` in the JSON)
/// - `setprop debug.alvr.key_dump_burst <n>`: that many consecutive passes (1..30)
/// - `setprop debug.alvr.burst <n>`: another value other than 0 records `debug.alvr.burst_ms` (3000, 100 to
///   6000) of consecutive frames into the folder `alvr_burst_<n>` of the files folder: every new depth frame
///   (both eyes), one record per display frame (poses, hands, the depth frame the mask used), the camera
///   images of the learner, and one depth dump at the start (see burst.rs). Written after the burst. The
///   index (version 5 on) and the "burst: <n> started" / "ended" log lines carry the headset's wall clock,
///   monotonic clock and XrTime read back to back at the start and the end (`clocks`), to line up logs of
///   other machines. Version 6: the display records have `memory` (`mem_roll`, null when off). Version 7: `cloud`
///   (`debug.alvr.cloud`, null when off)
/// - `setprop debug.alvr.hand_removal 0`: asks the runtime to keep the hands in the depth map with the key on.
///   It does not: on 2026-11-24 the hands were still removed with 0 (a probe; the answer is logged)
/// - `setprop debug.alvr.key_wrist_len <mm>`: length of the key zone from the wrist towards the forearm, its
///   direction is chosen with the camera (`auto_key`), 0: off
/// - `setprop debug.alvr.key_wrist_scale <percent>`: its radius, of the radius of the wrist joint
/// - `setprop debug.alvr.key_wrist_conf <percent>`: how sure the camera must be of its direction for the
///   solid fill
/// - `setprop debug.alvr.key_wrist_follow <percent>`: below this score the camera doesn't see the forearm,
///   the bridge goes back to the default direction. 0: always the best candidate
pub struct ColorKey {
    pub version: u32,
    lut: Vec<u8>,
    pub params: ColorKeyParams,
}

#[derive(Clone, Copy)]
pub struct ColorKeyParams {
    /// Of a hand that doesn't move
    pub zones: HandZones,
    /// How wrong the timing of the hand pose may be: for the solid core, and in the worst case
    pub core_error_s: f32,
    pub rim_error_s: f32,
    pub max_rim_growth_m: f32,
    /// Joints whose speed grows the rim, see `debug.alvr.key_speed`
    pub speed_joints: i64,
    /// The rim grows along the motion of the hand. None: in all directions
    pub sweep: Option<SweepParams>,
    pub radius_scale: f32,
    /// Negative: predict ahead
    pub passthrough_delay_ms: i64,
    /// Whether the runtime takes the hands out of the depth map, see `debug.alvr.hand_removal`
    pub hand_removal: bool,
    /// Composition layers of the key, see `debug.alvr.key_layers`
    pub layers: i64,
    /// 0: the lower game layer is not cropped, 1: cropped, 2: cropped, y origin at the top
    pub crop: i64,
    /// Whether the GPU passes get the hands
    pub hands: bool,
    /// Narrower rim over backgrounds that look like the hand: gain (0..1), the shares of the background
    /// that is kept where it starts and where it is complete, the narrowest rim (inside of the hand), a
    /// fixed amount (0..1), and how it is shown (`debug.alvr.key_leak_view`)
    pub leak_gain: f32,
    pub leak_lo: f32,
    pub leak_hi: f32,
    pub leak_inset_m: f32,
    pub leak_force: f32,
    pub leak_view: i64,
    /// The leak map: weight (0..1, 0: not used), leak of the cells behind the hand where the narrowing
    /// starts and where it is complete, and the distance of the debug spheres
    pub leak_map: f32,
    pub leak_map_lo: f32,
    pub leak_map_hi: f32,
    pub leak_probe_m: f32,
    /// Key zone from the wrist towards the forearm. None: off
    pub wrist: Option<WristParams>,
    /// Debug view of the remembered playzone (`debug.alvr.playzone_view`)
    pub playzone_view: bool,
}

/// Key zone from the wrist towards the forearm, its direction chosen by the camera
/// (`debug.alvr.key_wrist_*`)
#[derive(Clone, Copy)]
pub struct WristParams {
    pub length_m: f32,
    /// Of the radius of the wrist joint
    pub radius_scale: f32,
    /// Solid only if the camera is at least this sure of the direction (0..1)
    pub min_confidence: f32,
    /// Below this score of the best direction (0..1) the default direction is used
    pub follow: f32,
}

impl WristParams {
    /// None if it is off
    pub fn load() -> Option<Self> {
        let length_m = millimeters("debug.alvr.key_wrist_len", 0);

        (length_m > 0.0).then(|| Self {
            length_m,
            radius_scale: debug_property("debug.alvr.key_wrist_scale")
                .unwrap_or(100)
                .clamp(0, 300) as f32
                / 100.0,
            min_confidence: percent("debug.alvr.key_wrist_conf", 60),
            follow: percent("debug.alvr.key_wrist_follow", 30),
        })
    }
}

fn millimeters(property: &str, default: i64) -> f32 {
    debug_property(property).unwrap_or(default).clamp(0, 300) as f32 / 1000.0
}

// 0..1
fn percent(property: &str, default: i64) -> f32 {
    debug_property(property).unwrap_or(default).clamp(0, 100) as f32 / 100.0
}

/// `debug.alvr.key_auto` 1: the colour learner chooses the bias itself (`key_bias` is ignored), see
/// `alvr_graphics::AutoBias`
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(all(not(target_os = "android"), not(test)), expect(dead_code))]
pub struct KeyAutoParams {
    /// `key_auto_weight`, percent (0..1000, default 300): how much a leak costs against the skin kept
    pub weight: f32,
    /// `key_auto_scene`, see `scene_share`
    pub scene_share: f32,
}

impl KeyAutoParams {
    /// None while it is off (the default)
    #[cfg_attr(not(target_os = "android"), expect(dead_code))]
    pub fn load() -> Option<Self> {
        Self::from_values(
            debug_property("debug.alvr.key_auto"),
            debug_property("debug.alvr.key_auto_weight"),
            debug_property("debug.alvr.key_auto_scene"),
        )
    }

    /// From the property values, None: unset. Off unless `auto` is 1 or more
    #[cfg_attr(all(not(target_os = "android"), not(test)), expect(dead_code))]
    pub fn from_values(auto: Option<i64>, weight: Option<i64>, scene: Option<i64>) -> Option<Self> {
        (auto.unwrap_or(0) > 0).then(|| Self {
            weight: weight.unwrap_or(300).clamp(0, 1000) as f32 / 100.0,
            scene_share: Self::scene_share(scene),
        })
    }

    /// `key_auto_scene`, percent (0..100, default 50): how much of the scene that the key keeps counts as a
    /// leak. Also for the leak published to the depth cloud (`debug.alvr.cloud_leak`) while `key_auto` is
    /// off
    #[cfg_attr(all(not(target_os = "android"), not(test)), expect(dead_code))]
    pub fn scene_share(scene: Option<i64>) -> f32 {
        scene.unwrap_or(50).clamp(0, 100) as f32 / 100.0
    }
}

/// The end of the `auto key:` line while `key_auto` is on (`on`), nothing while it is off. `status`: of the
/// last LUT, None before the first one
#[cfg_attr(all(not(target_os = "android"), not(test)), expect(dead_code))]
pub fn auto_bias_text(on: bool, status: Option<&alvr_graphics::AutoBiasStatus>) -> String {
    match (on, status) {
        (false, _) => String::new(),
        (true, None) => ", auto bias -".into(),
        (true, Some(status)) => format!(
            ", auto bias {:.2} (cand {:.2}, skin {:.2} leak {:.2})",
            status.bias, status.candidate, status.skin_kept, status.leak
        ),
    }
}

/// `debug.alvr.problem_view`, see the properties above
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ProblemViewParams {
    /// Cell leak (0..1) where the amber and the red tint start
    pub lo: f32,
    pub hi: f32,
    /// How much the room shows through the game and how much the game is dimmed (0..1)
    pub room: f32,
    pub dim: f32,
}

impl ProblemViewParams {
    /// None while it is off (the default)
    pub fn load() -> Option<Self> {
        (debug_property("debug.alvr.problem_view").unwrap_or(0) > 0).then(|| {
            Self::from_values(
                debug_property("debug.alvr.problem_view_lo")
                    .or_else(|| debug_property("debug.alvr.key_leak_map_lo")),
                debug_property("debug.alvr.problem_view_hi")
                    .or_else(|| debug_property("debug.alvr.key_leak_map_hi")),
                debug_property("debug.alvr.problem_view_room"),
                debug_property("debug.alvr.problem_view_dim"),
            )
        })
    }

    /// From the property values in percent, None: unset
    pub fn from_values(
        lo: Option<i64>,
        hi: Option<i64>,
        room: Option<i64>,
        dim: Option<i64>,
    ) -> Self {
        let percent =
            |value: Option<i64>, default| value.unwrap_or(default).clamp(0, 100) as f32 / 100.0;
        Self {
            lo: percent(lo, 3),
            hi: percent(hi, 15),
            room: percent(room, 85),
            dim: percent(dim, 50),
        }
    }
}

impl ColorKeyParams {
    fn load() -> Self {
        let solid_count = debug_property("debug.alvr.key_solid")
            .unwrap_or(alvr_graphics::WRIST_SEGMENTS as i64)
            .clamp(0, alvr_graphics::HAND_SEGMENTS as i64) as usize;
        let mut solid = [0.0; alvr_graphics::HAND_SEGMENTS];
        solid[..solid_count].fill(1.0);
        // While the depth cloud is on, unset ones are 5 and 0 (motion_cloud.rs)
        let (layers, crop) = crate::motion_cloud::key_layers(
            debug_property("debug.alvr.key_layers"),
            debug_property("debug.alvr.key_crop"),
            debug_property("debug.alvr.cloud"),
        );

        Self {
            zones: HandZones {
                core: -millimeters("debug.alvr.key_inset", 4),
                falloff: millimeters("debug.alvr.key_falloff", 4),
                rim: millimeters("debug.alvr.key_rim", 0),
                solid,
                sweep: HandSweep::NONE,
            },
            // Milliseconds
            core_error_s: millimeters("debug.alvr.key_error", 10),
            rim_error_s: millimeters("debug.alvr.key_rim_error", 45),
            max_rim_growth_m: millimeters("debug.alvr.key_rim_max", 8),
            speed_joints: debug_property("debug.alvr.key_speed").unwrap_or(2),
            sweep: (debug_property("debug.alvr.key_sweep").unwrap_or(1) != 0).then(|| {
                SweepParams {
                    max: millimeters("debug.alvr.key_sweep_max", 40),
                    iso: percent("debug.alvr.key_sweep_iso", 25),
                    bias: debug_property("debug.alvr.key_sweep_bias")
                        .unwrap_or(0)
                        .clamp(-100, 100) as f32
                        / 100.0,
                }
            }),
            radius_scale: debug_property("debug.alvr.key_scale")
                .unwrap_or(100)
                .clamp(0, 300) as f32
                / 100.0,
            passthrough_delay_ms: debug_property("debug.alvr.key_delay")
                .unwrap_or(0)
                .clamp(-100, 200),
            // 0: the hands stay in the depth map with the key on. The runtime takes out an uneven zone
            // of 3-15 cm around the hands with them (legs under resting hands, a held object)
            hand_removal: debug_property("debug.alvr.hand_removal").unwrap_or(1) != 0,
            layers,
            crop,
            hands: debug_property("debug.alvr.key_hands").unwrap_or(1) != 0,
            leak_gain: percent("debug.alvr.key_leak", 0),
            leak_lo: percent("debug.alvr.key_leak_lo", 5),
            leak_hi: percent("debug.alvr.key_leak_hi", 20),
            leak_inset_m: millimeters("debug.alvr.key_leak_inset", 3),
            leak_force: percent("debug.alvr.key_leak_force", 0),
            leak_view: debug_property("debug.alvr.key_leak_view").unwrap_or(0),
            leak_map: percent("debug.alvr.key_leak_map", 0),
            leak_map_lo: percent("debug.alvr.key_leak_map_lo", 3),
            leak_map_hi: percent("debug.alvr.key_leak_map_hi", 15),
            leak_probe_m: debug_property("debug.alvr.key_leak_probe")
                .unwrap_or(60)
                .clamp(10, 500) as f32
                / 100.0,
            wrist: WristParams::load(),
            playzone_view: debug_property("debug.alvr.playzone_view").unwrap_or(0) != 0,
        }
    }

    /// Whether the frame has the lower game layer and the keyed passthrough (`debug.alvr.key_layers`).
    /// Otherwise only one game layer: the lower one if it is not cropped, else the upper one.
    pub fn extra_layers(&self, hands_in_view: bool) -> bool {
        match self.layers {
            2 => false,
            5 | 30 | 31 => true,
            _ => hands_in_view,
        }
    }
}

/// Files folder of the app, for the dumps
pub const FILES_DIR: &str = "/sdcard/Android/data/alvr.client.dev/files";

const COLOR_KEY_PATHS: [&str; 2] = [
    "/data/local/tmp/alvr_color_key.lut",
    "/sdcard/Android/data/alvr.client.dev/files/alvr_color_key.lut",
];

pub struct ColorKeyLoader {
    key: Option<ColorKey>,
    last_version: u32,
    last_poll: Instant,
}

impl ColorKeyLoader {
    pub fn new() -> Self {
        Self {
            key: None,
            last_version: 0,
            last_poll: Instant::now(),
        }
    }

    pub fn poll(&mut self) -> Option<&ColorKey> {
        if self.last_poll.elapsed() > Duration::from_millis(500) {
            self.last_poll = Instant::now();

            let version = debug_property("debug.alvr.color_key")
                .and_then(|version| u32::try_from(version).ok())
                .unwrap_or(0);
            if version != self.last_version {
                self.last_version = version;
                self.key = (version != 0).then(|| load_color_key(version)).flatten();
            }
            if let Some(key) = &mut self.key {
                key.params = ColorKeyParams::load();
            }
        }

        self.key.as_ref()
    }
}

fn load_color_key(version: u32) -> Option<ColorKey> {
    for path in COLOR_KEY_PATHS {
        match fs::read(path) {
            Ok(lut) if lut.len() == COLOR_KEY_SIZE => {
                return Some(ColorKey {
                    version,
                    lut,
                    params: ColorKeyParams::load(),
                });
            }
            Ok(lut) => error!("Color key {path}: unexpected size {}", lut.len()),
            Err(e) => error!("Color key {path}: {e}"),
        }
    }

    None
}

/// Integer Android system property. `debug.*` properties can be set with adb.
#[allow(unused_variables)]
pub fn debug_property(name: &str) -> Option<i64> {
    #[cfg(target_os = "android")]
    {
        let name = std::ffi::CString::new(name).unwrap();
        let mut value = [0_u8; libc::PROP_VALUE_MAX as usize];
        let length =
            unsafe { libc::__system_property_get(name.as_ptr(), value.as_mut_ptr().cast()) };

        String::from_utf8_lossy(&value[..length.max(0) as usize])
            .trim()
            .parse()
            .ok()
    }
    #[cfg(not(target_os = "android"))]
    None
}

/// Text Android system property, trimmed. None if it is not set or empty
#[allow(unused_variables)]
pub fn debug_property_string(name: &str) -> Option<String> {
    #[cfg(target_os = "android")]
    {
        let name = std::ffi::CString::new(name).ok()?;
        let mut value = [0_u8; libc::PROP_VALUE_MAX as usize];
        let length =
            unsafe { libc::__system_property_get(name.as_ptr(), value.as_mut_ptr().cast()) };
        let text = String::from_utf8_lossy(&value[..length.clamp(0, value.len() as i32) as usize])
            .trim()
            .to_owned();

        (!text.is_empty()).then_some(text)
    }
    #[cfg(not(target_os = "android"))]
    None
}

impl<'a> Deref for PassthroughLayer<'a> {
    type Target = xr::CompositionLayerBase<'a, xr::OpenGlEs>;

    fn deref(&self) -> &Self::Target {
        if let Some(handle) = &self.handle_fb {
            unsafe { &*ptr::from_ref(handle.layer()).cast() }
        } else if let Some(handle) = &self.handle_htc {
            unsafe { &*ptr::from_ref(handle.layer()).cast() }
        } else {
            panic!("No passthrough extension available");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn problem_view_params_defaults_and_clamps() {
        assert_eq!(
            ProblemViewParams::from_values(None, None, None, None),
            ProblemViewParams {
                lo: 0.03,
                hi: 0.15,
                room: 0.85,
                dim: 0.5,
            }
        );
        let params = ProblemViewParams::from_values(Some(10), Some(250), Some(-5), Some(100));
        assert_eq!(
            (params.lo, params.hi, params.room, params.dim),
            (0.1, 1.0, 0.0, 1.0)
        );
    }

    #[test]
    fn key_auto_params_defaults_and_clamps() {
        // Unset, 0 or negative: off, whatever the others say
        assert_eq!(KeyAutoParams::from_values(None, None, None), None);
        assert_eq!(KeyAutoParams::from_values(Some(0), Some(100), None), None);
        assert_eq!(KeyAutoParams::from_values(Some(-1), None, Some(20)), None);
        assert_eq!(
            KeyAutoParams::from_values(Some(1), None, None),
            Some(KeyAutoParams {
                weight: 3.0,
                scene_share: 0.5,
            })
        );
        assert_eq!(
            KeyAutoParams::from_values(Some(1), Some(-5), Some(250)),
            Some(KeyAutoParams {
                weight: 0.0,
                scene_share: 1.0,
            })
        );
        assert_eq!(
            KeyAutoParams::from_values(Some(1), Some(5000), Some(-3)),
            Some(KeyAutoParams {
                weight: 10.0,
                scene_share: 0.0,
            })
        );
        assert_eq!(
            KeyAutoParams::from_values(Some(1), Some(150), Some(20)),
            Some(KeyAutoParams {
                weight: 1.5,
                scene_share: 0.2,
            })
        );
        assert_eq!(KeyAutoParams::scene_share(None), 0.5);
    }

    #[test]
    fn auto_bias_log_text() {
        let status = alvr_graphics::AutoBiasStatus {
            bias: 0.6213,
            candidate: 0.6,
            skin_kept: 0.7612,
            background_kept: 0.004,
            scene_kept: 0.02,
            leak: 0.01,
        };
        // Off: the line is unchanged
        assert_eq!(auto_bias_text(false, Some(&status)), "");
        assert_eq!(auto_bias_text(false, None), "");
        assert_eq!(auto_bias_text(true, None), ", auto bias -");
        assert_eq!(
            auto_bias_text(true, Some(&status)),
            ", auto bias 0.62 (cand 0.60, skin 0.76 leak 0.01)"
        );
    }
}
