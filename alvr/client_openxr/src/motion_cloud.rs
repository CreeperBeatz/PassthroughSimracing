//! The client side of the depth cloud (`debug.alvr.cloud`, design/depthcloud/final.md; the cloud itself is
//! `motion_cloud.rs` and `motion_cloud_gpu.rs` of alvr_graphics): its properties, the A/B alternation of
//! `cloud_ab`, whether today's arm shift stays on, the key layer defaults while it is on, and the `cloud:`
//! line.
//!
//! Pure maths and state, so it can be tested on the PC.

use alvr_graphics::{CloudInput, CloudParams, CloudStatus, ResetReason};
use std::{fmt::Write, time::Duration};

pub const MAX_GAIN: i64 = 400;
/// `cloud_window` (cells): 12, today's, up to 24. `cloud_cap`, `cloud_fixed` and `cloud_trail` go up to
/// 2 x the window (depth texels: the truncation of the field's distances)
pub const MIN_WINDOW: i64 = 12;
pub const MAX_WINDOW: i64 = 24;
pub const MIN_CAP: i64 = 4;
pub const MAX_HOLD_MS: i64 = 500;
/// Tenths of a depth texel per pair
pub const MIN_DEAD: i64 = 15;
pub const MAX_DEAD: i64 = 60;
pub const MAX_LAMBDA_MS: i64 = 120;
pub const MIN_FETCH_MM: i64 = 200;
pub const MAX_FETCH_MM: i64 = 1500;
pub const MAX_HEAD_DEG_S: i64 = 1000;
/// `cloud_trail_age` (depth images, 0: no age test)
pub const MAX_TRAIL_AGE: i64 = 255;
/// `cloud_own` (mm from the capsules, 0: off)
pub const MAX_OWN_MM: i64 = 40;
/// `cloud_ab 1` shows the cloud and today's picture for this long each
pub const AB_PHASE: Duration = Duration::from_secs(2);

/// The values of `debug.alvr.cloud*` as read (None: unset)
#[derive(Clone, Copy, Debug, Default)]
pub struct CloudValues {
    pub mode: Option<i64>,
    pub gain: Option<i64>,
    pub cap: Option<i64>,
    pub hold_ms: Option<i64>,
    pub dead: Option<i64>,
    pub lambda_ms: Option<i64>,
    pub fetch_mm: Option<i64>,
    pub head_max: Option<i64>,
    pub fixed: Option<i64>,
    pub ab: Option<i64>,
    pub onesided: Option<i64>,
    pub trail: Option<i64>,
    pub trail_age: Option<i64>,
    pub trail_hands: Option<i64>,
    pub window: Option<i64>,
    pub own: Option<i64>,
}

/// `debug.alvr.cloud*`. None: `cloud` is unset or 0 (off, the default)
#[cfg(target_os = "android")]
pub fn read_params() -> Option<CloudParams> {
    use crate::passthrough::debug_property;

    params_from_values(&CloudValues {
        mode: debug_property("debug.alvr.cloud"),
        gain: debug_property("debug.alvr.cloud_gain"),
        cap: debug_property("debug.alvr.cloud_cap"),
        hold_ms: debug_property("debug.alvr.cloud_hold_ms"),
        dead: debug_property("debug.alvr.cloud_dead"),
        lambda_ms: debug_property("debug.alvr.cloud_lambda_ms"),
        fetch_mm: debug_property("debug.alvr.cloud_fetch_mm"),
        head_max: debug_property("debug.alvr.cloud_head_max"),
        fixed: debug_property("debug.alvr.cloud_fixed"),
        ab: debug_property("debug.alvr.cloud_ab"),
        onesided: debug_property("debug.alvr.cloud_onesided"),
        trail: debug_property("debug.alvr.cloud_trail"),
        trail_age: debug_property("debug.alvr.cloud_trail_age"),
        trail_hands: debug_property("debug.alvr.cloud_trail_hands"),
        window: debug_property("debug.alvr.cloud_window"),
        own: debug_property("debug.alvr.cloud_own"),
    })
}

/// No properties off the headset: off
#[cfg(not(target_os = "android"))]
pub fn read_params() -> Option<CloudParams> {
    params_from_values(&CloudValues::default())
}

/// Unset values are the defaults; out of range ones are clamped. `cloud` 1 is the depth cloud, 2 or more
/// the cloud on top of today's arm shift; unset, 0 or negative: off. The cap, `cloud_fixed` and
/// `cloud_trail` go up to 2 x `cloud_window`. `cloud_own` in mm, 0..`MAX_OWN_MM` (unset or 0: off)
pub fn params_from_values(values: &CloudValues) -> Option<CloudParams> {
    let mode = values.mode.filter(|mode| *mode >= 1)?.min(2) as u8;
    let defaults = CloudParams::default();
    let value = |value: Option<i64>, default: f32, min: i64, max: i64| {
        value.map_or(default, |value| value.clamp(min, max) as f32)
    };
    let window = values.window.map_or(defaults.window, |window| {
        window.clamp(MIN_WINDOW, MAX_WINDOW) as u8
    });
    let reach_max = 2 * i64::from(window);

    Some(CloudParams {
        mode,
        gain: value(values.gain, defaults.gain, 0, MAX_GAIN),
        cap: value(values.cap, defaults.cap, MIN_CAP, reach_max),
        lambda_ms: value(values.lambda_ms, defaults.lambda_ms, 0, MAX_LAMBDA_MS),
        hold_ms: value(values.hold_ms, defaults.hold_ms, 0, MAX_HOLD_MS),
        dead: value(values.dead, defaults.dead * 10.0, MIN_DEAD, MAX_DEAD) / 10.0,
        fetch_m: value(
            values.fetch_mm,
            defaults.fetch_m * 1000.0,
            MIN_FETCH_MM,
            MAX_FETCH_MM,
        ) / 1000.0,
        head_max: value(values.head_max, defaults.head_max, 0, MAX_HEAD_DEG_S),
        fixed: value(values.fixed, defaults.fixed, 0, reach_max),
        ab: values.ab.is_some_and(|ab| ab != 0),
        onesided: values
            .onesided
            .map_or(defaults.onesided, |onesided| onesided.clamp(0, 2) as u8),
        trail: value(values.trail, defaults.trail, 0, reach_max),
        trail_age: values
            .trail_age
            .map_or(defaults.trail_age, |age| age.clamp(0, MAX_TRAIL_AGE) as u8),
        trail_hands: values
            .trail_hands
            .map_or(defaults.trail_hands, |hands| hands != 0),
        window,
        own: values
            .own
            .map_or(defaults.own, |own| own.clamp(0, MAX_OWN_MM) as f32 / 1000.0),
    })
}

fn params_part(params: &CloudParams) -> String {
    let mut text = format!(
        "cloud: mode {}, gain {:.0} cap {:.0} hold {:.0} dead {:.1} lambda {:.0} fetch {:.0} fixed {:.0} ab {} \
         onesided {} trail {:.0}/{}{} window {}",
        params.mode,
        params.gain,
        params.cap,
        params.hold_ms,
        params.dead,
        params.lambda_ms,
        params.fetch_m * 1000.0,
        params.fixed,
        u8::from(params.ab),
        params.onesided,
        params.trail,
        params.trail_age,
        // `cloud_trail_hands 1`: "h" while the trail is on
        if params.trail > 0.0 && params.trail_hands {
            "h"
        } else {
            ""
        },
        params.window
    );
    // `cloud_own`: only while set, so that the line is today's without it
    if params.own > 0.0 {
        let _ = write!(text, " own {:.0}", params.own * 1000.0);
    }
    text
}

/// The `cloud:` line of the settings, on each change
pub fn params_text(params: Option<&CloudParams>) -> String {
    match params {
        Some(params) => params_part(params),
        None => "cloud: off (debug.alvr.cloud is unset or 0): today's picture".into(),
    }
}

// `cloud_leak`: up to this leak of the key the band is as set; at the upper one it has its smallest scale
const LEAK_FULL: f32 = 0.03;
const LEAK_SMALLEST: f32 = 0.20;
const MIN_LEAK_SCALE: f32 = 0.30;

/// `debug.alvr.cloud_leak` 1: the scale of the band's gain and cap for the key's measured leak (0..1, see
/// `AutoColorKey::key_leak`): 1 up to 0.03, then down linearly, at least 0.30
pub fn leak_scale(leak: f32) -> f32 {
    (1.0 - (leak - LEAK_FULL) / (LEAK_SMALLEST - LEAK_FULL)).clamp(MIN_LEAK_SCALE, 1.0)
}

/// `debug.alvr.cloud_leak`: the key's leak and the scale it gives (1 without a fresh leak), for the
/// parameters of a frame and the `cloud:` line
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LeakFollow {
    pub leak: Option<f32>,
    pub scale: f32,
}

impl LeakFollow {
    /// `leak`: the fresh leak of the key, None if there is none
    pub fn new(leak: Option<f32>) -> Self {
        Self {
            leak,
            scale: leak.map_or(1.0, leak_scale),
        }
    }

    /// The parameters with the gain and the cap scaled
    pub fn apply(&self, params: CloudParams) -> CloudParams {
        CloudParams {
            gain: params.gain * self.scale,
            cap: params.cap * self.scale,
            ..params
        }
    }

    /// The part of the `cloud:` line after the parameters
    fn text(&self) -> String {
        match self.leak {
            Some(leak) => format!(" leak {leak:.2} x{:.2}", self.scale),
            None => format!(" leak - x{:.2}", self.scale),
        }
    }
}

/// The parameters of a frame: with `cloud_leak` (`follow`) on, the gain and cap scaled by the key's leak
pub fn frame_params(params: CloudParams, follow: Option<&LeakFollow>) -> CloudParams {
    follow.map_or(params, |follow| follow.apply(params))
}

/// `cloud_ab 1`: whether the cloud shows (true) or today's picture (false), `elapsed` since the
/// alternation started: the cloud first, then 2 s each
pub fn ab_shown(elapsed: Duration) -> bool {
    (elapsed.as_millis() / AB_PHASE.as_millis()).is_multiple_of(2)
}

/// Whether today's arm shift (`debug.alvr.arm_shift`) stays on: not while the zone of `cloud 1` shows
/// (the skeleton-free arm path). `zone_shown`: the mask shows the zone in this frame
/// (`alvr_graphics::cloud_shown`: the cloud runs, not today's phase of `cloud_ab`, not the Show depth
/// view); where it does not, the picture stays today's, arm shift included
pub fn arm_shift_on(cloud: Option<&CloudInput>, zone_shown: bool) -> bool {
    !(zone_shown && cloud.is_some_and(|cloud| cloud.params.mode == 1))
}

/// `debug.alvr.key_layers` and `debug.alvr.key_crop` (layers, crop). Unset: 4 (the extra layers with
/// hands in view) and 1 (the lower game layer cropped to the hands), today's. While the depth cloud is
/// on (`debug.alvr.cloud` 1 or more) unset ones are 5 (the extra layers always) and 0 (not cropped): the
/// zone opens the key only where the lower layer is drawn, and it can be anywhere in the view. Set
/// values win
pub fn key_layers(layers: Option<i64>, crop: Option<i64>, cloud: Option<i64>) -> (i64, i64) {
    let cloud_on = cloud.is_some_and(|cloud| cloud >= 1);
    let (layers_default, crop_default) = if cloud_on { (5, 0) } else { (4, 1) };

    (
        layers.unwrap_or(layers_default),
        crop.unwrap_or(crop_default),
    )
}

/// Why the cloud does not run while it is set
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloudOff {
    NoColourKey,
    DistanceRegion,
    NoDepth,
}

impl CloudOff {
    /// `depth`: depth passthrough is on, `colour_key`: the key is on, `volume`: the region is made of
    /// boxes. All three and still not running: no depth image yet
    pub fn reason(depth: bool, colour_key: bool, volume: bool) -> Self {
        if !depth {
            Self::NoDepth
        } else if !colour_key {
            Self::NoColourKey
        } else if !volume {
            Self::DistanceRegion
        } else {
            Self::NoDepth
        }
    }

    pub fn text(self) -> &'static str {
        match self {
            Self::NoColourKey => "no colour key",
            Self::DistanceRegion => "distance region",
            Self::NoDepth => "no depth",
        }
    }
}

/// What the `cloud:` line reports since the last one
#[derive(Default)]
pub struct CloudLog {
    /// The renderer's counts at the last line
    last: CloudStatus,
    /// Over the display frames since the last line, of the left eye's field: the sum of its age and of
    /// how far the live picture is ahead of it (ms), the frames, and of these the frames on which it
    /// was stale (older than 300 ms: no zone)
    age_ms: f64,
    a_ms: f64,
    frames: u32,
    stale: u32,
}

impl CloudLog {
    /// A display frame of the running cloud, after the renderer ran
    pub fn count(&mut self, status: &CloudStatus, display_ns: i64) {
        let eye = &status.eyes[0];
        if let Some(capture_ns) = eye.field_capture_ns {
            self.age_ms += (display_ns - capture_ns) as f64 / 1e6;
            self.a_ms += f64::from(eye.a_ms);
            self.frames += 1;
            self.stale += u32::from(eye.stale);
        }
    }

    fn empty_frames(&mut self) {
        self.age_ms = 0.0;
        self.a_ms = 0.0;
        self.frames = 0;
        self.stale = 0;
    }

    /// The `cloud:` line every 2 s while `cloud` is set, `seconds` since the last one. `state`: the
    /// renderer's status while the cloud runs, else why not. `layers`: the key layers and crop in use.
    /// `follow`: the key's leak and its scale with `cloud_leak` on (None: off). Empties the counts
    pub fn text(
        &mut self,
        params: &CloudParams,
        state: Result<&CloudStatus, CloudOff>,
        seconds: f32,
        layers: (i64, i64),
        follow: Option<&LeakFollow>,
    ) -> String {
        let status = match state {
            Ok(status) => status,
            Err(off) => {
                self.empty_frames();
                // The renderer dropped the cloud: its next one counts from 0
                self.last = CloudStatus::default();
                return format!("cloud: mode {}, off: {}", params.mode, off.text());
            }
        };
        let mut text = params_part(params);
        if let Some(follow) = follow {
            text.push_str(&follow.text());
        }
        let seconds = seconds.max(1e-3);
        // The renderer's counts start again from 0 when it creates the cloud again (another depth size,
        // switched off and on): if any count went back, the line counts from 0
        let last = &self.last;
        let restarted = (0..2).any(|eye| {
            status.pairs[eye] < last.pairs[eye]
                || (0..4).any(|i| status.resets[eye][i] < last.resets[eye][i])
        }) || (0..3).any(|i| status.intervals[i] < last.intervals[i]);
        if restarted {
            self.last = CloudStatus::default();
        }
        // The left eye: one pair and one reset per depth image (eye 1 has the same, a frame later)
        let pairs = status.pairs[0] - self.last.pairs[0];
        let resets = [0, 1, 2, 3].map(|i| status.resets[0][i] - self.last.resets[0][i]);
        let intervals = [0, 1, 2].map(|i| status.intervals[i] - self.last.intervals[i]);
        self.last = *status;

        let _ = write!(text, " | depth {:.1} /s", pairs as f32 / seconds);
        let timed = intervals.iter().sum::<u32>();
        if timed > 0 {
            let share = |count: u32| count as f32 * 100.0 / timed as f32;
            let _ = write!(
                text,
                " (33 ms {:.0} %, 67 ms {:.0} %, 100 ms {:.0} %)",
                share(intervals[0]),
                share(intervals[1]),
                share(intervals[2])
            );
        }
        if self.frames > 0 {
            let frames = f64::from(self.frames);
            let _ = write!(
                text,
                ", age {:.0} ms, a {:.0} ms",
                self.age_ms / frames,
                self.a_ms / frames
            );
        } else {
            text.push_str(", age -, a -");
        }
        let _ = write!(text, ", stale {}", self.stale);
        let reason = |reason: ResetReason| {
            let index = ResetReason::ALL.iter().position(|r| *r == reason).unwrap();
            format!("{} {}", reason.name(), resets[index])
        };
        let _ = write!(
            text,
            " | pairs {}: reset {} ({}) | layers {} crop {}",
            pairs,
            resets.iter().sum::<u32>(),
            ResetReason::ALL.map(reason).join(", "),
            layers.0,
            layers.1
        );
        self.empty_frames();

        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alvr_graphics::CloudEyeRecord;

    #[test]
    fn cloud_params_from_values() {
        // Unset, 0 or negative: off, whatever the others say
        assert_eq!(params_from_values(&CloudValues::default()), None);
        for mode in [0, -1] {
            let values = CloudValues {
                mode: Some(mode),
                gain: Some(200),
                ..Default::default()
            };
            assert_eq!(params_from_values(&values), None);
        }
        assert_eq!(
            params_text(None),
            "cloud: off (debug.alvr.cloud is unset or 0): today's picture"
        );

        // The defaults of C.5
        let on = CloudValues {
            mode: Some(1),
            ..Default::default()
        };
        let params = params_from_values(&on).unwrap();
        assert_eq!(params, CloudParams::default());
        assert_eq!(
            (params.mode, params.gain, params.cap, params.hold_ms),
            (1, 150.0, 14.0, 200.0)
        );
        assert_eq!((params.dead, params.lambda_ms), (2.5, 40.0));
        assert!((params.fetch_m - 0.45).abs() < 1e-6);
        assert_eq!((params.head_max, params.fixed), (120.0, 0.0));
        assert!(!params.ab);
        assert_eq!(
            (
                params.onesided,
                params.trail,
                params.trail_age,
                params.window
            ),
            (1, 0.0, 16, 12)
        );
        assert_eq!(
            params_text(Some(&params)),
            "cloud: mode 1, gain 150 cap 14 hold 200 dead 2.5 lambda 40 fetch 450 fixed 0 ab 0 onesided 1 trail 0/16 \
             window 12"
        );

        // Clamped below
        let low = params_from_values(&CloudValues {
            mode: Some(2),
            gain: Some(-5),
            cap: Some(1),
            hold_ms: Some(-1),
            dead: Some(3),
            lambda_ms: Some(-10),
            fetch_mm: Some(50),
            head_max: Some(-1),
            fixed: Some(-3),
            ab: Some(0),
            onesided: Some(-1),
            trail: Some(-4),
            trail_age: Some(-1),
            trail_hands: Some(-1),
            window: Some(3),
            own: Some(-5),
        })
        .unwrap();
        assert_eq!(
            (low.mode, low.gain, low.cap, low.hold_ms),
            (2, 0.0, 4.0, 0.0)
        );
        assert_eq!((low.dead, low.lambda_ms, low.head_max), (1.5, 0.0, 0.0));
        assert!((low.fetch_m - 0.2).abs() < 1e-6);
        assert_eq!((low.fixed, low.ab, low.onesided), (0.0, false, 0));
        assert_eq!((low.trail, low.trail_age, low.window), (0.0, 0, 12));
        assert!(low.trail_hands);
        assert_eq!(low.own, 0.0);
        assert!(!params_text(Some(&low)).contains(" own"));

        // Clamped above
        let high = params_from_values(&CloudValues {
            mode: Some(7),
            gain: Some(1000),
            cap: Some(99),
            hold_ms: Some(9000),
            dead: Some(200),
            lambda_ms: Some(500),
            fetch_mm: Some(5000),
            head_max: Some(5000),
            fixed: Some(50),
            ab: Some(3),
            onesided: Some(9),
            trail: Some(99),
            trail_age: Some(999),
            trail_hands: Some(7),
            window: Some(99),
            own: Some(400),
        })
        .unwrap();
        assert_eq!(
            (high.mode, high.gain, high.cap, high.hold_ms),
            (2, 400.0, 48.0, 500.0)
        );
        assert_eq!(
            (high.dead, high.lambda_ms, high.head_max),
            (6.0, 120.0, 1000.0)
        );
        assert!((high.fetch_m - 1.5).abs() < 1e-6);
        assert_eq!((high.fixed, high.ab, high.onesided), (48.0, true, 2));
        assert_eq!((high.trail, high.trail_age, high.window), (48.0, 255, 24));
        assert!((high.own - 0.04).abs() < 1e-6);
        assert!(params_text(Some(&high)).ends_with(" trail 48/255h window 24 own 40"));

        // Without `cloud_window` the cap, the constant reach and the trail stop at 24 (today's)
        let today = params_from_values(&CloudValues {
            window: None,
            ..high_values()
        })
        .unwrap();
        assert_eq!(
            (today.cap, today.fixed, today.trail, today.window),
            (24.0, 24.0, 24.0, 12)
        );
        // `cloud_window 18`: up to 36
        let window = params_from_values(&CloudValues {
            window: Some(18),
            ..high_values()
        })
        .unwrap();
        assert_eq!(
            (window.cap, window.fixed, window.trail, window.window),
            (36.0, 36.0, 36.0, 18)
        );
        assert!(params_text(Some(&window)).ends_with(" trail 36/255h window 18"));

        // `cloud_onesided` 0 and 2 as set
        for onesided in [0, 2] {
            let params = params_from_values(&CloudValues {
                onesided: Some(onesided),
                ..on
            })
            .unwrap();
            assert_eq!(i64::from(params.onesided), onesided);
        }

        // `cloud_trail` as set, in the line
        let trail = params_from_values(&CloudValues {
            trail: Some(6),
            ..on
        })
        .unwrap();
        assert_eq!(trail.trail, 6.0);
        assert!(params_text(Some(&trail)).ends_with(" onesided 1 trail 6/16h window 12"));

        // `cloud_trail_hands`: 1 by default, 0 as set (no "h" in the line), anything else 1
        assert!(trail.trail_hands);
        let plain = params_from_values(&CloudValues {
            trail: Some(6),
            trail_hands: Some(0),
            ..on
        })
        .unwrap();
        assert!(!plain.trail_hands);
        assert!(params_text(Some(&plain)).ends_with(" onesided 1 trail 6/16 window 12"));
        for hands in [-1, 2] {
            let params = params_from_values(&CloudValues {
                trail_hands: Some(hands),
                ..on
            })
            .unwrap();
            assert!(params.trail_hands, "{hands}");
        }

        // `cloud_own`: off by default (0, not in the line), mm as set, in the line after the window
        assert_eq!(params.own, 0.0);
        let own = params_from_values(&CloudValues {
            own: Some(20),
            ..on
        })
        .unwrap();
        assert!((own.own - 0.02).abs() < 1e-6);
        assert!(params_text(Some(&own)).ends_with(" onesided 1 trail 0/16 window 12 own 20"));
        let zero = params_from_values(&CloudValues { own: Some(0), ..on }).unwrap();
        assert_eq!(zero.own, 0.0);
        assert_eq!(params_text(Some(&zero)), params_text(Some(&params)));
    }

    // Every value above its range (`cloud_window` 99: 24)
    fn high_values() -> CloudValues {
        CloudValues {
            mode: Some(7),
            gain: Some(1000),
            cap: Some(99),
            hold_ms: Some(9000),
            dead: Some(200),
            lambda_ms: Some(500),
            fetch_mm: Some(5000),
            head_max: Some(5000),
            fixed: Some(50),
            ab: Some(3),
            onesided: Some(9),
            trail: Some(99),
            trail_age: Some(999),
            trail_hands: Some(7),
            window: Some(99),
            own: None,
        }
    }

    #[test]
    fn leak_scale_points() {
        let close = |leak: f32, expected: f32| {
            let scale = leak_scale(leak);
            assert!((scale - expected).abs() < 1e-5, "{leak}: {scale}");
        };
        close(0.0, 1.0);
        close(0.03, 1.0);
        close(0.08, 1.0 - 0.05 / 0.17);
        close(0.115, 0.5);
        // The floor from 0.03 + 0.7 x 0.17 = 0.149 on
        close(0.149, 0.3);
        close(0.17, 0.3);
        close(0.2, 0.3);
        close(1.0, 0.3);
        assert!(leak_scale(0.148) > 0.3);
    }

    #[test]
    fn cloud_leak_scales_gain_and_cap() {
        let params = CloudParams {
            trail: 6.0,
            ..Default::default()
        };
        // Off: as set
        assert_eq!(frame_params(params, None), params);
        // On without a fresh leak: as set
        let none = LeakFollow::new(None);
        assert_eq!(none.scale, 1.0);
        assert_eq!(frame_params(params, Some(&none)), params);
        // A small leak: as set
        assert_eq!(
            frame_params(params, Some(&LeakFollow::new(Some(0.02)))),
            params
        );
        // Half: the gain and the cap only
        let half = frame_params(params, Some(&LeakFollow::new(Some(0.115))));
        assert!((half.gain - 75.0).abs() < 1e-3 && (half.cap - 7.0).abs() < 1e-4);
        assert_eq!(
            CloudParams {
                gain: params.gain,
                cap: params.cap,
                ..half
            },
            params
        );
        let floor = frame_params(params, Some(&LeakFollow::new(Some(0.5))));
        assert!((floor.gain - 45.0).abs() < 1e-3 && (floor.cap - 4.2).abs() < 1e-4);

        // The line shows it after the parameters
        let status = CloudStatus::default();
        let mut log = CloudLog::default();
        let text = log.text(
            &params,
            Ok(&status),
            2.0,
            (5, 0),
            Some(&LeakFollow::new(Some(0.08))),
        );
        assert!(
            text.starts_with(
                "cloud: mode 1, gain 150 cap 14 hold 200 dead 2.5 lambda 40 fetch 450 fixed 0 ab 0 onesided 1 \
                 trail 6/16h window 12 leak 0.08 x0.71 | depth "
            ),
            "{text}"
        );
        let text = log.text(&params, Ok(&status), 2.0, (5, 0), Some(&none));
        assert!(
            text.contains(" trail 6/16h window 12 leak - x1.00 | depth "),
            "{text}"
        );
        // Off: unchanged
        let text = log.text(&params, Ok(&status), 2.0, (5, 0), None);
        assert!(text.contains(" trail 6/16h window 12 | depth "), "{text}");
    }

    #[test]
    fn cloud_ab_alternates() {
        let ms = Duration::from_millis;
        // The cloud first, then today's picture, 2 s each
        assert!(ab_shown(ms(0)));
        assert!(ab_shown(ms(1999)));
        assert!(!ab_shown(ms(2000)));
        assert!(!ab_shown(ms(3999)));
        assert!(ab_shown(ms(4000)));
        assert!(!ab_shown(ms(6500)));
        assert!(ab_shown(ms(120_100)));
    }

    #[test]
    fn cloud_1_switches_the_arm_shift_off() {
        use alvr_session::{
            CockpitVolumeConfig, DepthMaskRegion, DepthPassthroughConfig,
            DepthPassthroughDebugView, settings_schema::Switch,
        };

        let input = |mode, shown| CloudInput {
            params: CloudParams {
                mode,
                ..Default::default()
            },
            capture_ns: Some(0),
            display_ns: 100_000_000,
            shown,
            own_gate: [0.0; 2],
        };
        // The zone is shown as the renderer decides it (`cloud_shown`)
        let config = DepthPassthroughConfig {
            region: DepthMaskRegion::Volume(CockpitVolumeConfig {
                center_x: 0.0,
                center_y: 1.0,
                center_z: -0.5,
                yaw_deg: 0.0,
                width: 1.0,
                height: 1.0,
                depth: 1.0,
            }),
            edge_feathering: 0.01,
            lighting: Switch::Disabled,
            debug_view: DepthPassthroughDebugView::Off,
            flip_depth_y: false,
        };
        let on = |config: &DepthPassthroughConfig, cloud: Option<&CloudInput>, runs| {
            arm_shift_on(cloud, alvr_graphics::cloud_shown(config, cloud, runs))
        };
        // Off: today's arm shift
        assert!(on(&config, None, true));
        // `cloud 1` shown: off
        assert!(!on(&config, Some(&input(1, true)), true));
        // Today's phase of `cloud_ab`: on
        assert!(on(&config, Some(&input(1, false)), true));
        // `cloud 2` keeps it
        assert!(on(&config, Some(&input(2, true)), true));
        // The cloud cannot run (no colour key, the distance region): today's picture
        assert!(on(&config, Some(&input(1, true)), false));
        // The Show depth view hides the zone: today's arm shift stays
        let show_depth = DepthPassthroughConfig {
            debug_view: DepthPassthroughDebugView::ShowDepth,
            ..config.clone()
        };
        assert!(on(&show_depth, Some(&input(1, true)), true));
    }

    #[test]
    fn key_layer_defaults_follow_the_cloud() {
        // Unset cloud: today's defaults exactly
        assert_eq!(key_layers(None, None, None), (4, 1));
        assert_eq!(key_layers(None, None, Some(0)), (4, 1));
        assert_eq!(key_layers(Some(2), Some(2), None), (2, 2));
        // The cloud on: the extra layers always, not cropped
        assert_eq!(key_layers(None, None, Some(1)), (5, 0));
        assert_eq!(key_layers(None, None, Some(2)), (5, 0));
        // Set values win
        assert_eq!(key_layers(Some(4), None, Some(1)), (4, 0));
        assert_eq!(key_layers(None, Some(1), Some(1)), (5, 1));
        assert_eq!(key_layers(Some(30), Some(2), Some(2)), (30, 2));
    }

    #[test]
    fn cloud_log_text() {
        let params = CloudParams::default();
        let mut log = CloudLog::default();
        let ms = 1_000_000;
        // Both eyes count their pairs and resets; the line reports the left eye's (one per depth image)
        let mut status = CloudStatus {
            pairs: [34, 33],
            resets: [[1, 0, 1, 0], [1, 0, 1, 0]],
            intervals: [14, 20, 0],
            eyes: [
                CloudEyeRecord {
                    field_capture_ns: Some(1000 * ms),
                    a_ms: 91.0,
                    ..Default::default()
                },
                CloudEyeRecord::default(),
            ],
        };
        // Two display frames, 131 and 141 ms after the capture
        log.count(&status, 1131 * ms);
        status.eyes[0].a_ms = 101.0;
        log.count(&status, 1141 * ms);
        let text = log.text(&params, Ok(&status), 2.0, (5, 0), None);
        assert_eq!(
            text,
            "cloud: mode 1, gain 150 cap 14 hold 200 dead 2.5 lambda 40 fetch 450 fixed 0 ab 0 onesided 1 trail 0/16 \
             window 12 | depth 17.0 /s (33 ms 41 %, 67 ms 59 %, 100 ms 0 %), age 136 ms, a 96 ms, stale 0 | pairs 34: \
             reset 2 (start 1, dt 0, head 1, region 0) | layers 5 crop 0"
        );

        // The next line counts from the last one. A gap of 200 ms is one Dt reset in each eye: dt 1
        status.pairs = [36, 35];
        status.resets[0][1] += 1;
        status.resets[1][1] += 1;
        status.intervals[1] += 1;
        let text = log.text(&params, Ok(&status), 2.0, (4, 1), None);
        assert!(
            text.ends_with(
                "| depth 1.0 /s (33 ms 0 %, 67 ms 100 %, 100 ms 0 %), age -, a -, stale 0 | pairs 2: \
                 reset 1 (start 0, dt 1, head 0, region 0) | layers 4 crop 1"
            ),
            "{text}"
        );

        // The depth stalls: the field gets old, and stale past 300 ms (no zone)
        status.eyes[0].a_ms = 250.0;
        log.count(&status, 1250 * ms);
        status.eyes[0].stale = true;
        log.count(&status, 1400 * ms);
        log.count(&status, 1600 * ms);
        let text = log.text(&params, Ok(&status), 2.0, (5, 0), None);
        assert!(
            text.contains(
                "| depth 0.0 /s, age 417 ms, a 250 ms, stale 2 | pairs 0: reset 0 (start 0, dt 0, \
                 head 0, region 0)"
            ),
            "{text}"
        );

        // A renderer that created the cloud again counts from 0: its first image is counted
        let again = CloudStatus {
            pairs: [3, 2],
            resets: [[1, 0, 0, 0]; 2],
            intervals: [2, 0, 0],
            ..Default::default()
        };
        let text = log.text(&params, Ok(&again), 2.0, (5, 0), None);
        assert!(
            text.contains(
                "| depth 1.5 /s (33 ms 100 %, 67 ms 0 %, 100 ms 0 %), age -, a -, stale 0 | pairs 3: \
                 reset 1 (start 1, "
            ),
            "{text}"
        );
        // Also after a line without a cloud (dropped): its next one counts from 0, though its counts
        // did not go back below the last line's
        log.text(&params, Err(CloudOff::NoDepth), 2.0, (5, 0), None);
        let text = log.text(&params, Ok(&again), 2.0, (5, 0), None);
        assert!(text.contains("| depth 1.5 /s"), "{text}");
        assert!(text.contains("| pairs 3: reset 1 (start 1,"), "{text}");

        // Why it does not run
        let mut off = |depth, key, volume| {
            log.text(
                &params,
                Err(CloudOff::reason(depth, key, volume)),
                2.0,
                (5, 0),
                None,
            )
        };
        assert_eq!(off(true, false, true), "cloud: mode 1, off: no colour key");
        assert_eq!(
            off(true, true, false),
            "cloud: mode 1, off: distance region"
        );
        assert_eq!(off(false, true, true), "cloud: mode 1, off: no depth");
        assert_eq!(off(true, true, true), "cloud: mode 1, off: no depth");
    }
}
