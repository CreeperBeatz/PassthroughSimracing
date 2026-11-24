//! The arm's own history of hand samples (step 1b of `docs/arm-mask-build-plan.md`). The runtime doesn't
//! answer joint lookups for past times: any time before its newest sample returns that sample, any later
//! time its prediction (measured on bursts 14 and 15, `logs/analysis-20261125-wave`). So the arm keeps the
//! samples it saw, each with the time it shows the hand: the capture time of
//! XR_META_hand_tracking_unextrapolated_poses, or the time it was first seen minus
//! `debug.alvr.arm_sample_age_ms` (35: the depth's forearm lines up with the samples 32-38 ms after they
//! were first seen). From it come the wrist at the depth's capture time (where the arm is in the depth)
//! and the wrist the arm is placed with (`ArmEstimator`). While the room memory is on (`debug.alvr.mem_roll`)
//! each sample also keeps all 26 joints, for the hand capsules at the depth's capture time (`joints_at`).
//! Pure maths, so it can be tested on the PC

use alvr_common::glam::{Vec3, Vec4};
use std::collections::VecDeque;

/// Samples are kept this long before the newest one (ns). The depth is 50-130 ms old when shown
pub const HISTORY_NS: i64 = 300_000_000;
/// A capture time is believed if the sample was at most this old when first seen (ns)
const MAX_CAPTURE_AGE_NS: i64 = 300_000_000;
/// Two samples further apart than this give no velocity and no wrist between them: the hand was lost or
/// not sampled in between (ns)
pub const MAX_VELOCITY_GAP_NS: i64 = 100_000_000;
/// The line and the parabola take a velocity only from this many samples on: the first samples after the
/// hand came back are often off
pub const MIN_VELOCITY_SAMPLES: usize = 3;
/// The wrist is moved ahead from the newest sample for this long at most (ns), so an old velocity is not
/// carried on while no new sample comes
pub const MAX_HORIZON_NS: i64 = 150_000_000;
/// `ArmEstimator::Parabola` fits the samples of this long before the newest one (ns)
pub const PARABOLA_WINDOW_NS: i64 = 120_000_000;

/// `debug.alvr.arm_estimator`: how the arm's wrist is placed from the history
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArmEstimator {
    /// 0 (default): the newest sample as it is. The calmest: no velocity, no jitter of its own
    Hold,
    /// 1: the line through the two newest samples, at the target time
    Line,
    /// 2: the newest sample moved to the target time with the velocity of a parabola fitted over the
    /// last 120 ms (fewer than 4 samples: a line)
    Parabola,
}

impl ArmEstimator {
    /// 1 line, 2 parabola, anything else hold
    pub fn from_value(value: i64) -> Self {
        match value {
            1 => Self::Line,
            2 => Self::Parabola,
            _ => Self::Hold,
        }
    }

    pub fn value(self) -> i64 {
        match self {
            Self::Hold => 0,
            Self::Line => 1,
            Self::Parabola => 2,
        }
    }

    /// For the `arm:` line
    pub fn name(self) -> &'static str {
        match self {
            Self::Hold => "hold the newest sample",
            Self::Line => "line through the last two samples",
            Self::Parabola => "parabola over 120 ms",
        }
    }
}

/// A hand sample as the arm keeps it
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HandSample {
    /// When the sample shows the hand (XrTime)
    pub time_ns: i64,
    /// When the app got it first (the runtime's now when the frame was rendered)
    pub seen_ns: i64,
    pub wrist: Vec3,
    /// The middle knuckle (`MIDDLE_PROXIMAL`): with the wrist the axis of the wrist gap
    pub knuckle: Vec3,
    /// `time_ns` is the runtime's capture time, else the time seen minus the sample age
    pub from_capture_time: bool,
    /// All 26 joints (xyz: position, w: radius), kept only while the room memory is on
    pub joints: Option<[Vec4; 26]>,
}

/// The wrist the arm is placed with
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placed {
    pub wrist: Vec3,
    /// The time it stands for: of the newest sample when held, else the target time (at most
    /// `MAX_HORIZON_NS` after the newest sample)
    pub time_ns: i64,
    /// Distance from the newest sample (meters, at most the cap): ahead of it, or back for a target
    /// inside the history
    pub extrapolated: f32,
    /// Of the velocity used (m/s, 0 when held)
    pub speed: f32,
}

/// The samples of one hand, oldest first
#[derive(Default)]
pub struct HandHistory {
    samples: VecDeque<HandSample>,
    // Of the newest sample: the capture time the runtime gave, to tell the next one
    last_capture_ns: Option<i64>,
}

impl HandHistory {
    /// Forgets every sample (the hand was lost). Whether there were any
    pub fn clear(&mut self) -> bool {
        let had_samples = !self.samples.is_empty();
        self.samples.clear();
        self.last_capture_ns = None;

        had_samples
    }

    pub fn newest(&self) -> Option<&HandSample> {
        self.samples.back()
    }

    pub fn oldest(&self) -> Option<&HandSample> {
        self.samples.front()
    }

    /// Every sample kept, oldest first
    pub fn samples(&self) -> impl Iterator<Item = &HandSample> {
        self.samples.iter()
    }

    /// `push_joints` without the joints: the tests' shorthand
    #[cfg(test)]
    pub fn push(
        &mut self,
        seen_ns: i64,
        wrist: Vec3,
        knuckle: Vec3,
        capture_ns: Option<i64>,
        age_ns: i64,
    ) -> Option<HandSample> {
        self.push_joints(seen_ns, wrist, knuckle, capture_ns, age_ns, None)
    }

    /// The hand as located in a frame rendered at `seen_ns`, with the runtime's capture time if it gave
    /// one. A new sample is kept and returned: with a capture time a new capture time, without one other
    /// joints than the newest sample's (its time is then `seen_ns - age_ns`). `joints`: all 26 joints,
    /// kept with it while the room memory is on (None, or not all finite: none kept)
    pub fn push_joints(
        &mut self,
        seen_ns: i64,
        wrist: Vec3,
        knuckle: Vec3,
        capture_ns: Option<i64>,
        age_ns: i64,
        joints: Option<[Vec4; 26]>,
    ) -> Option<HandSample> {
        if !wrist.is_finite() || !knuckle.is_finite() {
            return None;
        }
        let joints = joints.filter(|joints| joints.iter().all(|joint| joint.is_finite()));
        let capture_ns =
            capture_ns.filter(|&time| time <= seen_ns && seen_ns - time <= MAX_CAPTURE_AGE_NS);
        let newest = self.samples.back();
        let is_new = match (newest, capture_ns) {
            (None, _) => true,
            (Some(_), Some(time)) => self.last_capture_ns != Some(time),
            (Some(newest), None) => newest.wrist != wrist || newest.knuckle != knuckle,
        };
        if !is_new {
            return None;
        }
        let sample = HandSample {
            time_ns: capture_ns.unwrap_or(seen_ns - age_ns),
            seen_ns,
            wrist,
            knuckle,
            from_capture_time: capture_ns.is_some(),
            joints,
        };
        // Times only go forward. A sample that would go back (a capture time before the time guessed
        // for the one before) is left out
        if newest.is_some_and(|newest| sample.time_ns <= newest.time_ns) {
            return None;
        }
        self.last_capture_ns = capture_ns;
        self.samples.push_back(sample);
        while self
            .samples
            .front()
            .is_some_and(|oldest| oldest.time_ns < sample.time_ns - HISTORY_NS)
        {
            self.samples.pop_front();
        }

        Some(sample)
    }

    /// The wrist and the middle knuckle at `time_ns`, interpolated between the two samples around it; the
    /// newest sample for a later time. None (a buffer miss): no sample at or before it, or the two around it
    /// are more than `MAX_VELOCITY_GAP_NS` apart (where the hand was in between is not known)
    pub fn at(&self, time_ns: i64) -> Option<(Vec3, Vec3)> {
        let newest = self.samples.back()?;
        if time_ns >= newest.time_ns {
            return Some((newest.wrist, newest.knuckle));
        }
        // The first sample after the time; the one before it exists unless the time is before the oldest
        let after = self
            .samples
            .iter()
            .position(|sample| sample.time_ns > time_ns)?;
        let before = &self.samples[after.checked_sub(1)?];
        let after = &self.samples[after];
        let gap_ns = after.time_ns - before.time_ns;
        if gap_ns > MAX_VELOCITY_GAP_NS && time_ns != before.time_ns {
            return None;
        }
        let f = (time_ns - before.time_ns) as f32 / gap_ns as f32;

        Some((
            before.wrist.lerp(after.wrist, f),
            before.knuckle.lerp(after.knuckle, f),
        ))
    }

    /// All 26 joints at `time_ns`, by the rule of `at` (between two samples: each joint and radius
    /// interpolated). None: a miss of `at`, or a sample without joints
    pub fn joints_at(&self, time_ns: i64) -> Option<[Vec4; 26]> {
        let newest = self.samples.back()?;
        if time_ns >= newest.time_ns {
            return newest.joints;
        }
        let after = self
            .samples
            .iter()
            .position(|sample| sample.time_ns > time_ns)?;
        let before = &self.samples[after.checked_sub(1)?];
        let after = &self.samples[after];
        let gap_ns = after.time_ns - before.time_ns;
        if gap_ns > MAX_VELOCITY_GAP_NS && time_ns != before.time_ns {
            return None;
        }
        let f = (time_ns - before.time_ns) as f32 / gap_ns as f32;
        let (before, after) = (before.joints?, after.joints?);

        Some(std::array::from_fn(|j| before[j].lerp(after[j], f)))
    }

    /// The wrist the arm is placed with for `target_ns`, at most `cap` (meters) from the newest sample.
    /// None: no sample
    pub fn place(&self, estimator: ArmEstimator, target_ns: i64, cap: f32) -> Option<Placed> {
        let newest = *self.samples.back()?;
        let held = Placed {
            wrist: newest.wrist,
            time_ns: newest.time_ns,
            extrapolated: 0.0,
            speed: 0.0,
        };
        if estimator == ArmEstimator::Hold {
            return Some(held);
        }

        // A target inside the history: the samples around it (the line through them). None there (before
        // the oldest sample, or in a gap): held
        if target_ns <= newest.time_ns {
            let Some((wrist, _)) = self.at(target_ns) else {
                return Some(held);
            };
            let step = (wrist - newest.wrist).clamp_length_max(cap);
            return Some(Placed {
                wrist: newest.wrist + step,
                time_ns: target_ns,
                extrapolated: step.length(),
                speed: 0.0,
            });
        }

        let velocity = match estimator {
            ArmEstimator::Parabola => self.fitted_velocity(),
            _ => self.last_velocity(),
        };
        let Some(velocity) = velocity.filter(|velocity| velocity.is_finite()) else {
            return Some(held);
        };
        let horizon_ns = (target_ns - newest.time_ns).min(MAX_HORIZON_NS);
        let step = (velocity * (horizon_ns as f32 * 1e-9)).clamp_length_max(cap);

        Some(Placed {
            wrist: newest.wrist + step,
            time_ns: newest.time_ns + horizon_ns,
            extrapolated: step.length(),
            speed: velocity.length(),
        })
    }

    // Of the two newest samples (m/s). None: fewer than `MIN_VELOCITY_SAMPLES` since the history started,
    // or the two too far apart
    fn last_velocity(&self) -> Option<Vec3> {
        let len = self.samples.len();
        if len < MIN_VELOCITY_SAMPLES {
            return None;
        }
        let (before, newest) = (self.samples.get(len - 2)?, self.samples.back()?);
        let gap_ns = newest.time_ns - before.time_ns;
        (gap_ns > 0 && gap_ns <= MAX_VELOCITY_GAP_NS)
            .then(|| (newest.wrist - before.wrist) / (gap_ns as f32 * 1e-9))
    }

    // At the newest sample, of a least-squares parabola through the samples of the last
    // `PARABOLA_WINDOW_NS` (4 or more), or of a least-squares line (3), or of the two newest samples
    fn fitted_velocity(&self) -> Option<Vec3> {
        let newest = *self.samples.back()?;
        if self.samples.len() < MIN_VELOCITY_SAMPLES {
            return None;
        }
        // The samples are in time order: the window is the end of the history (walked without an
        // allocation, it runs every frame)
        let start = self
            .samples
            .iter()
            .position(|sample| sample.time_ns >= newest.time_ns - PARABOLA_WINDOW_NS)?;
        let count = self.samples.len() - start;
        let window = self.samples.range(start..);
        // A gap in the window: the hand was lost or not sampled in between
        let has_gap = window
            .clone()
            .zip(window.clone().skip(1))
            .any(|(before, after)| after.time_ns - before.time_ns > MAX_VELOCITY_GAP_NS);
        if count < 3 || has_gap {
            return self.last_velocity();
        }
        let points = window.map(move |sample| {
            let offset = sample.wrist - newest.wrist;
            (
                (sample.time_ns - newest.time_ns) as f64 * 1e-9,
                [offset.x as f64, offset.y as f64, offset.z as f64],
            )
        });

        let slope = if count >= 4 {
            parabola_slope(points.clone()).or_else(|| line_slope(points))
        } else {
            line_slope(points)
        }?;

        Some(Vec3::new(slope[0] as f32, slope[1] as f32, slope[2] as f32))
    }
}

// The (t, p) of the samples fitted, walked once per sum
trait Points: Iterator<Item = (f64, [f64; 3])> + Clone {}
impl<T: Iterator<Item = (f64, [f64; 3])> + Clone> Points for T {}

// Slope at t = 0 of the least-squares line through (t, p), per axis
fn line_slope(points: impl Points) -> Option<[f64; 3]> {
    let n = points.clone().count() as f64;
    let mean_t = points.clone().map(|(t, _)| t).sum::<f64>() / n;
    let spread = points
        .clone()
        .map(|(t, _)| (t - mean_t).powi(2))
        .sum::<f64>();
    if spread <= 0.0 {
        return None;
    }

    Some(std::array::from_fn(|axis| {
        let mean_p = points.clone().map(|(_, p)| p[axis]).sum::<f64>() / n;
        points
            .clone()
            .map(|(t, p)| (t - mean_t) * (p[axis] - mean_p))
            .sum::<f64>()
            / spread
    }))
}

// Slope at t = 0 of the least-squares parabola a + b t + c t² through (t, p), per axis: b of the normal
// equations, by Cramer's rule. None if they are close to singular
fn parabola_slope(points: impl Points) -> Option<[f64; 3]> {
    let s = |k: i32| points.clone().map(|(t, _)| t.powi(k)).sum::<f64>();
    let (s0, s1, s2, s3, s4) = (s(0), s(1), s(2), s(3), s(4));
    let det3 = |m: [[f64; 3]; 3]| {
        m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
            - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
            + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
    };
    let det = det3([[s0, s1, s2], [s1, s2, s3], [s2, s3, s4]]);
    // Relative to the diagonal: times that all but coincide make it 0
    if !det.is_finite() || det.abs() <= 1e-9 * s0 * s2 * s4 {
        return None;
    }

    Some(std::array::from_fn(|axis| {
        let r = |k: i32| {
            points
                .clone()
                .map(|(t, p)| t.powi(k) * p[axis])
                .sum::<f64>()
        };
        let (r0, r1, r2) = (r(0), r(1), r(2));

        det3([[s0, r0, s2], [s1, r1, s3], [s2, r2, s4]]) / det
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: i64 = 1_000_000;
    const AGE: i64 = 35 * MS;
    const CAP: f32 = 0.06;

    fn knuckle(wrist: Vec3) -> Vec3 {
        wrist + Vec3::new(0.0, 0.0, -0.08)
    }

    // A history with the wrist at `wrist(t)` for samples seen every `interval` ms from 1 s on, timed by
    // the capture time `seen - 35 ms`
    fn history(count: i64, interval: i64, wrist: impl Fn(f32) -> Vec3) -> HandHistory {
        let mut history = HandHistory::default();
        for k in 0..count {
            let seen_ns = 1000 * MS + k * interval * MS;
            let time_ns = seen_ns - AGE;
            let w = wrist(time_ns as f32 * 1e-9);
            history.push(seen_ns, w, knuckle(w), Some(time_ns), AGE);
        }

        history
    }

    fn close(a: Vec3, b: Vec3) -> bool {
        a.distance(b) < 1e-5
    }

    #[test]
    fn a_new_sample_is_new_joints_or_a_new_capture_time() {
        let mut history = HandHistory::default();
        let w = Vec3::new(0.1, 1.0, -0.4);

        // Without a capture time: the time seen minus the age, and the same joints are the same sample
        let first = history.push(1000 * MS, w, knuckle(w), None, AGE).unwrap();
        assert_eq!(first.time_ns, 1000 * MS - AGE);
        assert!(!first.from_capture_time);
        assert_eq!(history.push(1014 * MS, w, knuckle(w), None, AGE), None);
        let moved = w + Vec3::X * 0.001;
        let second = history
            .push(1028 * MS, moved, knuckle(moved), None, AGE)
            .unwrap();
        assert_eq!(second.time_ns, 1028 * MS - AGE);
        assert_eq!(history.samples.len(), 2);

        // With a capture time: a new one is a new sample even with the same joints, the same one is not
        let third = history
            .push(1042 * MS, moved, knuckle(moved), Some(1010 * MS), AGE)
            .unwrap();
        assert_eq!(third.time_ns, 1010 * MS);
        assert!(third.from_capture_time);
        let other = moved + Vec3::Y * 0.002;
        assert_eq!(
            history.push(1056 * MS, other, knuckle(other), Some(1010 * MS), AGE),
            None
        );
        assert!(
            history
                .push(1070 * MS, moved, knuckle(moved), Some(1030 * MS), AGE)
                .is_some()
        );

        // A capture time in the future or too old is not believed: the age is used
        let mut history = HandHistory::default();
        let sample = history
            .push(1000 * MS, w, knuckle(w), Some(1001 * MS), AGE)
            .unwrap();
        assert_eq!(sample.time_ns, 1000 * MS - AGE);
        assert!(!sample.from_capture_time);
        let sample = history
            .push(2000 * MS, moved, knuckle(moved), Some(1500 * MS), AGE)
            .unwrap();
        assert_eq!(sample.time_ns, 2000 * MS - AGE);

        // Times only go forward
        assert_eq!(
            history.push(2010 * MS, w, knuckle(w), Some(1900 * MS), AGE),
            None
        );
        // Not finite: left out
        assert_eq!(
            history.push(2020 * MS, Vec3::NAN, knuckle(w), None, AGE),
            None
        );
    }

    #[test]
    fn the_history_keeps_300_ms() {
        let history = history(60, 15, |t| Vec3::new(t, 1.0, -0.4));
        let newest = history.newest().unwrap().time_ns;
        let oldest = history.samples[0].time_ns;
        assert!(newest - oldest <= HISTORY_NS, "{}", newest - oldest);
        assert!(newest - oldest > HISTORY_NS - 15 * MS);
        assert_eq!(history.samples.len(), 21);
    }

    #[test]
    fn the_capture_time_is_interpolated() {
        // 0.5 m/s along x, a sample every 15 ms
        let history = history(10, 15, |t| Vec3::new(0.5 * t, 1.0, -0.4));
        let oldest = history.samples[0];
        let newest = *history.newest().unwrap();

        // Between two samples: on the line
        let time_ns = oldest.time_ns + 20 * MS;
        let (wrist, knuckle) = history.at(time_ns).unwrap();
        let expected = Vec3::new(0.5 * time_ns as f32 * 1e-9, 1.0, -0.4);
        assert!(close(wrist, expected), "{wrist} {expected}");
        assert!(close(knuckle, expected + Vec3::new(0.0, 0.0, -0.08)));
        // On a sample: that sample
        assert_eq!(history.at(oldest.time_ns).unwrap().0, oldest.wrist);
        // After the newest: the newest
        assert_eq!(
            history.at(newest.time_ns + 50 * MS).unwrap().0,
            newest.wrist
        );
        // Before the oldest: a miss
        assert_eq!(history.at(oldest.time_ns - 1), None);
        assert_eq!(HandHistory::default().at(0), None);
    }

    #[test]
    fn no_wrist_inside_a_gap() {
        // The hand stays active but no sample comes for 200 ms (held through an occlusion): between the
        // two samples around the gap the place is not known, a miss (plain depth), not a blend
        let mut history = HandHistory::default();
        let a = Vec3::new(0.0, 1.0, -0.4);
        let b = Vec3::new(0.2, 1.0, -0.4);
        let c = Vec3::new(0.21, 1.0, -0.4);
        history.push(1000 * MS, a, knuckle(a), None, AGE).unwrap();
        history.push(1200 * MS, b, knuckle(b), None, AGE).unwrap();
        history.push(1215 * MS, c, knuckle(c), None, AGE).unwrap();
        assert_eq!(history.at(1100 * MS - AGE), None);
        // On the sample before the gap, and after it: as before
        assert_eq!(history.at(1000 * MS - AGE).unwrap().0, a);
        let (wrist, _) = history.at(1205 * MS - AGE).unwrap();
        assert!(close(wrist, b.lerp(c, 1.0 / 3.0)), "{wrist}");
        // A line target inside the gap: held
        let placed = history
            .place(ArmEstimator::Line, 1100 * MS - AGE, CAP)
            .unwrap();
        assert_eq!(placed.wrist, c);
        assert_eq!(placed.extrapolated, 0.0);
    }

    #[test]
    fn hold_places_the_newest_sample() {
        let history = history(10, 15, |t| Vec3::new(0.5 * t, 1.0, -0.4));
        let newest = *history.newest().unwrap();
        let placed = history
            .place(ArmEstimator::Hold, newest.time_ns + 60 * MS, CAP)
            .unwrap();
        assert_eq!(placed.wrist, newest.wrist);
        assert_eq!(placed.time_ns, newest.time_ns);
        assert_eq!((placed.extrapolated, placed.speed), (0.0, 0.0));
        assert_eq!(
            HandHistory::default().place(ArmEstimator::Hold, 0, CAP),
            None
        );
    }

    #[test]
    fn line_and_parabola_follow_constant_motion() {
        // 0.3 m/s along x and -0.2 along y: 40 ms ahead is 12 mm and 8 mm
        let motion = |t: f32| Vec3::new(0.3 * t, 1.0 - 0.2 * t, -0.4);
        let history = history(10, 15, motion);
        let newest = *history.newest().unwrap();
        let target_ns = newest.time_ns + 40 * MS;
        let expected = motion(target_ns as f32 * 1e-9);
        for estimator in [ArmEstimator::Line, ArmEstimator::Parabola] {
            let placed = history.place(estimator, target_ns, CAP).unwrap();
            assert!(
                close(placed.wrist, expected),
                "{estimator:?} {}",
                placed.wrist
            );
            assert_eq!(placed.time_ns, target_ns);
            assert!((placed.extrapolated - 0.0144).abs() < 1e-4, "{placed:?}");
            assert!((placed.speed - 0.3606).abs() < 1e-3, "{placed:?}");
        }

        // A target inside the history: interpolated, not extrapolated
        let inside_ns = newest.time_ns - 20 * MS;
        let placed = history.place(ArmEstimator::Line, inside_ns, CAP).unwrap();
        assert!(close(placed.wrist, motion(inside_ns as f32 * 1e-9)));
        assert_eq!(placed.time_ns, inside_ns);
    }

    #[test]
    fn parabola_takes_the_velocity_at_the_newest_sample() {
        // x = 2 t² (accelerating): the velocity at the newest sample is 4 t, the line through the last two
        // samples is slower by 2 x 15 ms
        let history = history(10, 15, |t| Vec3::new(2.0 * t * t, 1.0, -0.4));
        let newest = *history.newest().unwrap();
        let t = newest.time_ns as f32 * 1e-9;
        let horizon = 0.010;
        let target_ns = newest.time_ns + 10 * MS;

        let placed = history
            .place(ArmEstimator::Parabola, target_ns, CAP)
            .unwrap();
        let expected = newest.wrist + Vec3::X * 4.0 * t * horizon;
        assert!(close(placed.wrist, expected), "{} {expected}", placed.wrist);

        let placed = history.place(ArmEstimator::Line, target_ns, CAP).unwrap();
        let expected = newest.wrist + Vec3::X * 4.0 * (t - 0.0075) * horizon;
        assert!(close(placed.wrist, expected), "{} {expected}", placed.wrist);
    }

    #[test]
    fn the_step_is_capped() {
        // 3 m/s for 40 ms would be 12 cm: 6 cm in the same direction
        let history = history(10, 15, |t| Vec3::new(0.0, 1.0, -3.0 * t));
        let newest = *history.newest().unwrap();
        for estimator in [ArmEstimator::Line, ArmEstimator::Parabola] {
            let placed = history
                .place(estimator, newest.time_ns + 40 * MS, CAP)
                .unwrap();
            assert!((placed.extrapolated - CAP).abs() < 1e-6, "{placed:?}");
            assert!(close(placed.wrist, newest.wrist + Vec3::NEG_Z * CAP));
        }

        // Far targets are not followed longer than `MAX_HORIZON_NS`: 0.1 m/s, 1 s ahead is 15 mm
        let history = self::history(10, 15, |t| Vec3::new(0.1 * t, 1.0, -0.4));
        let newest = *history.newest().unwrap();
        let placed = history
            .place(ArmEstimator::Line, newest.time_ns + 1000 * MS, CAP)
            .unwrap();
        assert!((placed.extrapolated - 0.015).abs() < 1e-5, "{placed:?}");
        assert_eq!(placed.time_ns, newest.time_ns + MAX_HORIZON_NS);
    }

    #[test]
    fn samples_far_apart_give_no_velocity() {
        let mut history = HandHistory::default();
        let a = Vec3::new(0.0, 1.0, -0.4);
        let b = Vec3::new(0.2, 1.0, -0.4);
        let c = Vec3::new(0.4, 1.0, -0.4);
        history.push(1000 * MS, a, knuckle(a), None, AGE);
        history.push(1200 * MS, b, knuckle(b), None, AGE);
        history.push(1400 * MS, c, knuckle(c), None, AGE);
        for estimator in [ArmEstimator::Line, ArmEstimator::Parabola] {
            let placed = history.place(estimator, 1400 * MS + 40 * MS, CAP).unwrap();
            assert_eq!(placed.wrist, c, "{estimator:?}");
            assert_eq!(placed.extrapolated, 0.0);
        }
    }

    #[test]
    fn the_first_two_samples_give_no_velocity() {
        // 0.5 m/s: with two samples the line would step 20 mm, from the third on it does
        let motion = |t: f32| Vec3::new(0.5 * t, 1.0, -0.4);
        let mut history = HandHistory::default();
        for k in 0..3 {
            let seen_ns = 1000 * MS + k * 15 * MS;
            let w = motion((seen_ns - AGE) as f32 * 1e-9);
            let newest = history.push(seen_ns, w, knuckle(w), None, AGE).unwrap();
            for estimator in [ArmEstimator::Line, ArmEstimator::Parabola] {
                let placed = history
                    .place(estimator, newest.time_ns + 40 * MS, CAP)
                    .unwrap();
                if k < 2 {
                    assert_eq!(placed.wrist, w, "{estimator:?} {k}");
                    assert_eq!(placed.extrapolated, 0.0);
                } else {
                    assert!((placed.extrapolated - 0.02).abs() < 1e-5, "{placed:?}");
                }
            }
        }
    }

    #[test]
    fn a_reset_gives_no_jump() {
        // The hand moves slowly on the left, is lost, and comes back 40 cm to the right
        let mut history = history(10, 15, |t| Vec3::new(0.05 * t, 1.0, -0.4));
        assert!(history.clear());
        assert!(!history.clear());
        assert_eq!(history.at(0), None);
        assert_eq!(history.place(ArmEstimator::Line, 0, CAP), None);

        let back = Vec3::new(0.45, 1.0, -0.4);
        let seen_ns = 2000 * MS;
        history
            .push(seen_ns, back, knuckle(back), None, AGE)
            .unwrap();
        // One sample: every estimator holds it, no velocity
        for estimator in [
            ArmEstimator::Hold,
            ArmEstimator::Line,
            ArmEstimator::Parabola,
        ] {
            let placed = history.place(estimator, seen_ns + 10 * MS, CAP).unwrap();
            assert_eq!(placed.wrist, back);
            assert_eq!(placed.extrapolated, 0.0);
        }
        // A depth image captured before the hand came back: a miss (plain depth), no displacement
        // from the old place
        assert_eq!(history.at(seen_ns - 100 * MS), None);
    }

    #[test]
    fn joints_follow_the_samples() {
        let joints =
            |x: f32| std::array::from_fn(|j| Vec4::new(x, 1.0, -0.4 - j as f32 * 0.01, 0.01));
        let mut history = HandHistory::default();
        let a = Vec3::new(0.0, 1.0, -0.4);
        let b = Vec3::new(0.02, 1.0, -0.4);
        history
            .push_joints(1000 * MS, a, knuckle(a), None, AGE, Some(joints(0.0)))
            .unwrap();
        history
            .push_joints(1020 * MS, b, knuckle(b), None, AGE, Some(joints(0.02)))
            .unwrap();
        // Halfway: every joint halfway; after the newest: the newest; before the oldest: a miss
        let half = history.joints_at(1010 * MS - AGE).unwrap();
        assert!((half[25].x - 0.01).abs() < 1e-6 && (half[25].z + 0.65).abs() < 1e-6);
        assert_eq!(history.joints_at(2000 * MS).unwrap(), joints(0.02));
        assert_eq!(history.joints_at(900 * MS), None);
        // A sample without joints (pushed while the memory was off) gives none
        let c = Vec3::new(0.04, 1.0, -0.4);
        history.push(1040 * MS, c, knuckle(c), None, AGE).unwrap();
        assert_eq!(history.joints_at(1030 * MS - AGE), None);
        // Not finite joints are not kept
        let mut bad = joints(0.0);
        bad[3].x = f32::NAN;
        let d = Vec3::new(0.06, 1.0, -0.4);
        let sample = history
            .push_joints(1060 * MS, d, knuckle(d), None, AGE, Some(bad))
            .unwrap();
        assert_eq!(sample.joints, None);
    }

    #[test]
    fn estimator_values() {
        for (value, estimator) in [
            (0, ArmEstimator::Hold),
            (1, ArmEstimator::Line),
            (2, ArmEstimator::Parabola),
            (3, ArmEstimator::Hold),
            (-1, ArmEstimator::Hold),
        ] {
            assert_eq!(ArmEstimator::from_value(value), estimator, "{value}");
        }
        assert_eq!(ArmEstimator::Parabola.value(), 2);
    }
}
