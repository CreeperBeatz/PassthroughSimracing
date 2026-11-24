//! The client side of the room memory (`debug.alvr.mem_roll`, U3 of `design/universal/final.md`; the
//! memory itself is `room_memory.rs` of alvr_graphics): its properties, the hand capsules at the depth's
//! capture time, and the `memory:` line.
//!
//! The capsules come from the arm's own sample history (`arm_history.rs`), which keeps all 26 joints of
//! each sample while the memory is on: the joints are already located there every frame, so there is no
//! extra lookup, and the runtime answers past-time lookups with its newest sample anyway [measured,
//! logs/analysis-20261125-wave]. When the capture time is not in the history (a gap, a hand just found,
//! no capture time), the hand's last known capsules grown by 3 cm stand in for it (for up to 1 s after
//! they were seen): a failed lookup must never let the runtime's fill be written over the memory.
//!
//! Pure maths and state, so it can be tested on the PC.

use crate::arm_history::HandHistory;
use alvr_common::glam::Vec4;
use alvr_graphics::{
    CaptureHand, DEFAULT_MARGIN_MM, DEFAULT_PITCH_DEG, FALLBACK_GROWTH, HandShape, MEMORY_SIZES,
    MemoryParams, MemoryStatus,
};
use std::fmt::Write;

pub const MIN_MARGIN_MM: i64 = 20;
pub const MAX_MARGIN_MM: i64 = 150;
pub const MAX_OUTER_MM: i64 = 250;
pub const MAX_PITCH_DEG: i64 = 80;
/// The last known joints of a hand stand in for those at the capture time this long at most (ns)
/// [assumed: the depth is 50-150 ms old when shown, a hand lost for longer is no longer removed]
pub const LAST_KNOWN_NS: i64 = 1_000_000_000;
/// Today's reach of the zone near a hand around its bounding sphere (`PLAYZONE_RANGE` of
/// focus_area.wgsl), m
const PLAYZONE_RANGE: f32 = 0.06;

/// `debug.alvr.mem_*`. None: `mem_roll` is not 1 (off, the default)
#[cfg(target_os = "android")]
pub fn read_params() -> Option<MemoryParams> {
    use crate::passthrough::debug_property;

    params_from_values(
        debug_property("debug.alvr.mem_roll"),
        debug_property("debug.alvr.mem_margin_mm"),
        debug_property("debug.alvr.mem_outer_mm"),
        debug_property("debug.alvr.mem_view"),
        debug_property("debug.alvr.mem_size"),
        debug_property("debug.alvr.mem_pitch_deg"),
        debug_property("debug.alvr.mem_moving"),
    )
}

/// No properties off the headset: off
#[cfg(not(target_os = "android"))]
pub fn read_params() -> Option<MemoryParams> {
    params_from_values(None, None, None, None, None, None, None)
}

/// Unset values are the defaults; out of range ones are clamped. `mem_size` other than 384 is 512
pub fn params_from_values(
    roll: Option<i64>,
    margin_mm: Option<i64>,
    outer_mm: Option<i64>,
    view: Option<i64>,
    size: Option<i64>,
    pitch_deg: Option<i64>,
    moving: Option<i64>,
) -> Option<MemoryParams> {
    if roll != Some(1) {
        return None;
    }
    let size = size
        .and_then(|size| u32::try_from(size).ok())
        .filter(|size| MEMORY_SIZES.contains(size))
        .unwrap_or(MEMORY_SIZES[0]);

    Some(MemoryParams {
        margin: margin_mm
            .unwrap_or(DEFAULT_MARGIN_MM)
            .clamp(MIN_MARGIN_MM, MAX_MARGIN_MM) as f32
            / 1000.0,
        outer: outer_mm.unwrap_or(0).clamp(0, MAX_OUTER_MM) as f32 / 1000.0,
        size,
        pitch_deg: pitch_deg
            .unwrap_or(DEFAULT_PITCH_DEG)
            .clamp(0, MAX_PITCH_DEG) as f32,
        view: view.unwrap_or(0).clamp(0, 2) as u8,
        skip_moving: moving.unwrap_or(0) != 0,
    })
}

/// The `memory:` line of the settings, at the start and on each change
pub fn params_text(params: Option<&MemoryParams>) -> String {
    let Some(params) = params else {
        return "memory: off (debug.alvr.mem_roll is not 1): today's playzone copy".into();
    };
    let outer = if params.outer > params.margin {
        format!("outer ring {:.0} mm", params.outer * 1000.0)
    } else {
        "no outer ring".into()
    };
    let view = match params.view {
        0 => "no view",
        1 => "view: what only it shows is blue",
        _ => "view: blue, paler with age",
    };
    let moving = if params.skip_moving {
        "texels marked moving are not read"
    } else {
        "texels marked moving are read"
    };

    format!(
        "memory: rolling, margin {:.0} mm, {outer}, {} x {} texels, aimed {:.0} deg down, {view}, \
         {moving}",
        params.margin * 1000.0,
        params.size,
        params.size,
        params.pitch_deg
    )
}

fn grown(joints: &[Vec4; 26], by: f32) -> [Vec4; 26] {
    joints.map(|joint| joint + Vec4::new(0.0, 0.0, 0.0, by))
}

/// The capsules of a hand at the depth's capture time (None: not known, use the last known ones), from
/// its history. `last_known`: the newest joints seen of this hand and their time, kept after the history
/// was emptied
pub fn capture_hand(
    history: &HandHistory,
    last_known: Option<(i64, [Vec4; 26])>,
    capture_ns: Option<i64>,
) -> (Option<HandShape>, CaptureHand) {
    if let Some(joints) = capture_ns.and_then(|time| history.joints_at(time)) {
        return (Some(HandShape::from_joints(&joints)), CaptureHand::Exact);
    }
    let known = history
        .newest()
        .and_then(|sample| Some((sample.time_ns, sample.joints?)))
        .or(last_known);
    match known {
        Some((time_ns, joints))
            if capture_ns.is_none_or(|capture| (capture - time_ns).abs() <= LAST_KNOWN_NS) =>
        {
            (
                Some(HandShape::from_joints(&grown(&joints, FALLBACK_GROWTH))),
                CaptureHand::Fallback,
            )
        }
        _ => (None, CaptureHand::None),
    }
}

/// How much farther the zone near a hand reaches than today's 6 cm (m): the wrist's travel since the
/// depth capture (`travel`, 0 without an arm motion), and the reach of the frozen texels beyond 6 cm
pub fn near_reach(params: &MemoryParams, travel: f32) -> f32 {
    let travel = if travel.is_finite() { travel } else { 0.0 };
    travel.max(0.0) + (params.reach() - PLAYZONE_RANGE).max(0.0)
}

/// What the `memory:` line reports since the last one
#[derive(Default)]
pub struct MemoryLog {
    /// Per hand, frames with capsules from the history, from the last known ones, and none
    pub sources: [[u32; 3]; 2],
    /// The updates of the renderer at the last line
    pub updates: [u32; 2],
}

impl MemoryLog {
    pub fn count(&mut self, sources: [CaptureHand; 2]) {
        for (counts, source) in self.sources.iter_mut().zip(sources) {
            counts[match source {
                CaptureHand::Exact => 0,
                CaptureHand::Fallback => 1,
                CaptureHand::None => 2,
            }] += 1;
        }
    }

    /// The `memory:` line every 2 s while it is on, `seconds` since the last one. Empties the counts
    pub fn text(&mut self, status: Option<&MemoryStatus>, seconds: f32) -> String {
        let mut text = "memory:".to_string();
        let Some(status) = status else {
            text.push_str(" on, no memory in the renderer (needs the color key and the depth)");
            self.sources = Default::default();
            return text;
        };
        let seconds = seconds.max(1e-3);
        let updates = [0, 1].map(|eye| {
            // The renderer's counts start again when it creates the memory again (another size)
            status.updates[eye].saturating_sub(self.updates[eye]) as f32 / seconds
        });
        self.updates = status.updates;
        let _ = write!(
            text,
            " {} x {}, anchors {} clean / {} with hands, updates {:.1} / {:.1} per s",
            status.size,
            status.size,
            status.anchor.clean,
            status.anchor.with_hands,
            updates[0],
            updates[1]
        );
        if status.anchor.camera.is_none() {
            text.push_str(", not anchored yet");
        }
        match status.stats {
            Some((stats, age)) => {
                let _ = write!(
                    text,
                    ", left eye: frozen {:.2} %, no live value {:.0} %, empty {:.0} %, moving {:.1} %, \
                     oldest frozen {} (stats {:.1} s old)",
                    stats.frozen * 100.0,
                    stats.no_live * 100.0,
                    stats.empty * 100.0,
                    stats.moving * 100.0,
                    stats
                        .oldest_frozen_s
                        .map_or("-".into(), |age| if age >= 60.0 {
                            "60 s or more".into()
                        } else {
                            format!("{age:.1} s")
                        }),
                    age
                );
            }
            None => text.push_str(", no stats yet"),
        }
        let hands = ["left", "right"].map(|name| name.to_string());
        let parts = hands
            .iter()
            .zip(&self.sources)
            .map(|(name, [exact, fallback, none])| {
                let total = (exact + fallback + none).max(1) as f32;
                format!(
                    "{name} history {:.0} % last known {:.0} % none {:.0} %",
                    *exact as f32 * 100.0 / total,
                    *fallback as f32 * 100.0 / total,
                    *none as f32 * 100.0 / total
                )
            })
            .collect::<Vec<_>>();
        let _ = write!(text, ", capsules at the capture time: {}", parts.join(", "));
        self.sources = Default::default();

        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alvr_common::glam::Vec3;
    use alvr_graphics::{MemoryAnchor, MemoryStats};

    const MS: i64 = 1_000_000;

    #[test]
    fn unset_is_off() {
        assert_eq!(
            params_from_values(None, None, None, None, None, None, None),
            None
        );
        assert_eq!(
            params_from_values(Some(0), Some(80), None, None, None, None, None),
            None
        );
        assert_eq!(
            params_from_values(Some(2), None, None, None, None, None, None),
            None
        );
        assert_eq!(
            params_text(None),
            "memory: off (debug.alvr.mem_roll is not 1): today's playzone copy"
        );
    }

    #[test]
    fn values_are_clamped() {
        let params = params_from_values(Some(1), None, None, None, None, None, None).unwrap();
        assert_eq!(params, MemoryParams::default());
        assert_eq!(params.size, 512);
        assert!((params.margin - 0.06).abs() < 1e-6);
        assert_eq!(params.pitch_deg, 35.0);

        let params = params_from_values(
            Some(1),
            Some(5),
            Some(900),
            Some(7),
            Some(384),
            Some(-10),
            Some(1),
        )
        .unwrap();
        assert!((params.margin - 0.02).abs() < 1e-6);
        assert!((params.outer - 0.25).abs() < 1e-6);
        assert_eq!((params.view, params.size, params.pitch_deg), (2, 384, 0.0));
        assert!(params.skip_moving);
        // Other sizes are 512
        let params =
            params_from_values(Some(1), Some(500), None, None, Some(1000), Some(99), None).unwrap();
        assert_eq!((params.size, params.pitch_deg), (512, 80.0));
        assert!((params.margin - 0.15).abs() < 1e-6);

        let text = params_text(Some(&params));
        assert!(
            text.starts_with("memory: rolling, margin 150 mm, no outer ring, 512 x 512"),
            "{text}"
        );
    }

    fn joints(x: f32) -> [Vec4; 26] {
        std::array::from_fn(|j| Vec4::new(x + j as f32 * 0.005, 1.0, -0.4, 0.01))
    }

    #[test]
    fn capsules_from_the_history_or_the_last_known_ones() {
        let mut history = HandHistory::default();
        let wrist = |x: f32| Vec3::new(x, 1.0, -0.4);
        let knuckle = |x: f32| Vec3::new(x, 1.0, -0.48);
        // Samples timed 1000 and 1020 ms (seen 35 ms later)
        for (k, x) in [0.0, 0.02].into_iter().enumerate() {
            let seen = 1035 * MS + k as i64 * 20 * MS;
            history.push_joints(
                seen,
                wrist(x),
                knuckle(x),
                Some(seen - 35 * MS),
                35 * MS,
                Some(joints(x)),
            );
        }

        // At a capture time inside the history: interpolated, as they were
        let (shape, source) = capture_hand(&history, None, Some(1010 * MS));
        assert_eq!(source, CaptureHand::Exact);
        let expected = HandShape::from_joints(&joints(0.01));
        let shape = shape.unwrap();
        for (got, want) in shape
            .segments
            .as_flattened()
            .iter()
            .zip(expected.segments.as_flattened())
        {
            assert!(got.distance(*want) < 1e-6, "{got} {want}");
        }

        // Before the oldest sample: the newest one, grown by 3 cm
        let (shape, source) = capture_hand(&history, None, Some(900 * MS));
        assert_eq!(source, CaptureHand::Fallback);
        let shape = shape.unwrap();
        let newest = HandShape::from_joints(&joints(0.02));
        // A segment away from the tips (which are pulled back by their radius) keeps its ends
        assert!((shape.segments[1][0].w - (newest.segments[1][0].w + 0.03)).abs() < 1e-6);
        assert!(
            shape.segments[1][0]
                .truncate()
                .distance(newest.segments[1][0].truncate())
                < 1e-6
        );
        // No capture time: the newest too
        assert_eq!(capture_hand(&history, None, None).1, CaptureHand::Fallback);

        // The hand was lost: the last known joints for 1 s after their time, then none
        let last_known = Some((1020 * MS, joints(0.02)));
        history.clear();
        assert_eq!(
            capture_hand(&history, last_known, Some(1900 * MS)).1,
            CaptureHand::Fallback
        );
        let (shape, source) = capture_hand(&history, last_known, Some(2100 * MS));
        assert!(shape.is_none());
        assert_eq!(source, CaptureHand::None);
        assert_eq!(
            capture_hand(&history, None, Some(1000 * MS)).1,
            CaptureHand::None
        );
    }

    #[test]
    fn reach_is_the_travel_and_the_margin_beyond_6_cm() {
        let params = MemoryParams::default();
        assert_eq!(near_reach(&params, 0.0), 0.0);
        assert!((near_reach(&params, 0.03) - 0.03).abs() < 1e-6);
        assert_eq!(near_reach(&params, f32::NAN), 0.0);
        let params = MemoryParams {
            margin: 0.08,
            outer: 0.12,
            ..params
        };
        assert!((near_reach(&params, 0.01) - 0.07).abs() < 1e-6);
    }

    #[test]
    fn log_line() {
        let mut log = MemoryLog::default();
        log.count([CaptureHand::Exact, CaptureHand::None]);
        log.count([CaptureHand::Fallback, CaptureHand::None]);
        let status = MemoryStatus {
            anchor: MemoryAnchor {
                camera: None,
                clean: 1,
                with_hands: 2,
            },
            size: 384,
            updates: [16, 14],
            stats: Some((
                MemoryStats {
                    frozen: 0.0125,
                    no_live: 0.41,
                    empty: 0.3,
                    moving: 0.005,
                    oldest_frozen_s: Some(3.24),
                    oldest_s: 40.0,
                },
                0.4,
            )),
        };
        let text = log.text(Some(&status), 2.0);
        assert_eq!(
            text,
            "memory: 384 x 384, anchors 1 clean / 2 with hands, updates 8.0 / 7.0 per s, not anchored yet, \
             left eye: frozen 1.25 %, no live value 41 %, empty 30 %, moving 0.5 %, oldest frozen 3.2 s \
             (stats 0.4 s old), capsules at the capture time: left history 50 % last known 50 % none 0 %, \
             right history 0 % last known 0 % none 100 %"
        );
        // The counts start again; the updates count from the last line
        let text = log.text(Some(&status), 2.0);
        assert!(text.contains("updates 0.0 / 0.0 per s"), "{text}");
        assert!(
            text.contains("left history 0 % last known 0 % none 0 %"),
            "{text}"
        );
        assert!(log.text(None, 2.0).contains("no memory in the renderer"));
    }
}
