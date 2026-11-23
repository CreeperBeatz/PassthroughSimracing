//! The box of the depth mask region placed from a marker (a QR code) on the rig, see
//! `debug.alvr.marker_box` in the client. Pure geometry, stage space (meters, +Y up), and the size
//! gate of the marker poses (`debug.alvr.marker_sizes`).
//!
//! The frame of the box is level: it takes only a heading from the marker, never its tilt. A tilt error
//! of a few degrees would move the far end of a 1 m box by several centimeters. That holds for a flat
//! code only: the heading of a standing code is the horizontal part of its normal, a rotation out of
//! its plane, which goes 1:1 into the yaw of the box (1 degree is about 1.9 cm at the far corner of
//! the default box).
//!
//! Marker axes (`XR_EXT_spatial_marker_tracking`): looking at the printed code, +X points to its right,
//! +Y to its top (the reading direction), +Z out of the paper towards the viewer.

use crate::RegionOverride;
use alvr_common::glam::{Mat4, Quat, Vec2, Vec3};
use std::collections::VecDeque;

/// A marker whose normal is within 45 degrees of vertical lies flat
const FLAT_LIMIT_COS: f32 = std::f32::consts::FRAC_1_SQRT_2;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MarkerHeading {
    /// Horizontal unit vector
    pub direction: Vec3,
    /// The marker lies flat (or hangs face down): the heading is its "up" (the top of the code). Else
    /// it stands and the heading is its normal, pointing out of the code
    pub flat: bool,
    /// Angle of the normal from stage +Y: 0 facing up, 90 standing, 180 facing down
    pub normal_from_vertical_deg: f32,
}

/// The level heading of a marker with this orientation (stage space)
pub fn marker_heading(orientation: Quat) -> MarkerHeading {
    let normal = orientation * Vec3::Z;
    let up = orientation * Vec3::Y;
    let flat = normal.y.abs() >= FLAT_LIMIT_COS;
    let source = if flat { up } else { normal };
    // At least sin(45 degrees) long for a unit quaternion: up is perpendicular to the normal
    let direction = Vec3::new(source.x, 0.0, source.z).normalize_or(Vec3::NEG_Z);

    MarkerHeading {
        direction,
        flat,
        // Not acos: precise near vertical
        normal_from_vertical_deg: f32::atan2(Vec2::new(normal.x, normal.z).length(), normal.y)
            .to_degrees(),
    }
}

/// Yaw about stage +Y (radians, counter-clockwise seen from above) that turns -Z into `direction`,
/// like `CockpitVolumeConfig::yaw_deg`
pub fn heading_yaw(direction: Vec3) -> f32 {
    f32::atan2(-direction.x, -direction.z)
}

/// Horizontal unit vector of a yaw, the inverse of `heading_yaw`
pub fn yaw_direction(yaw: f32) -> Vec3 {
    Quat::from_rotation_y(yaw) * Vec3::NEG_Z
}

/// Degrees for the log: -90..270, so that headings near 0 and near 180 never wrap
pub fn yaw_log_deg(yaw: f32) -> f32 {
    let deg = yaw.to_degrees();
    if deg < -90.0 { deg + 360.0 } else { deg }
}

/// a - b, in -pi..pi
pub fn yaw_difference(a: f32, b: f32) -> f32 {
    let difference = (a - b).rem_euclid(std::f32::consts::TAU);
    if difference > std::f32::consts::PI {
        difference - std::f32::consts::TAU
    } else {
        difference
    }
}

/// Standard deviation of the points per axis (population), and their number. None: no points
pub fn scatter(points: impl IntoIterator<Item = Vec3>) -> Option<(Vec3, usize)> {
    let points = points.into_iter().collect::<Vec<_>>();
    if points.is_empty() {
        return None;
    }
    let count = points.len() as f32;
    let mean = points.iter().sum::<Vec3>() / count;
    let variance = points
        .iter()
        .map(|point| (*point - mean) * (*point - mean))
        .sum::<Vec3>()
        / count;

    Some((
        Vec3::new(variance.x.sqrt(), variance.y.sqrt(), variance.z.sqrt()),
        points.len(),
    ))
}

/// Standard deviation and peak-to-peak range (radians) of yaws about their circular mean, so that
/// yaws around +-180 degrees are close. None: no yaws
pub fn yaw_scatter(yaws: impl IntoIterator<Item = f32>) -> Option<(f32, f32)> {
    let yaws = yaws.into_iter().collect::<Vec<_>>();
    let first = *yaws.first()?;
    // Opposite yaws cancel out: measure from the first one
    let mean = yaws
        .iter()
        .map(|yaw| yaw_direction(*yaw))
        .sum::<Vec3>()
        .try_normalize()
        .map_or(first, heading_yaw);
    let differences = yaws
        .iter()
        .map(|yaw| yaw_difference(*yaw, mean))
        .collect::<Vec<_>>();
    let variance = differences.iter().map(|d| d * d).sum::<f32>() / differences.len() as f32;
    let min = differences.iter().copied().fold(f32::INFINITY, f32::min);
    let max = differences
        .iter()
        .copied()
        .fold(f32::NEG_INFINITY, f32::max);

    Some((variance.sqrt(), max - min))
}

// A runtime reports the size of a code it has not measured as a whole multiple of 50 mm (Quest: 50.0
// and 100.0 mm, with wrong poses)
const DEFAULT_SIZE_STEP: f32 = 0.05;
const DEFAULT_SIZE_MARGIN: f32 = 0.000_05;
/// A learned size needs this many reported sizes
pub const MIN_LEARNED_SIZES: usize = 3;
/// Largest size of `debug.alvr.marker_sizes` (mm)
const MAX_GIVEN_SIZE_MM: f32 = 2000.0;

/// Whether a reported size (width, height in meters) is a default size of the runtime: both within
/// 0.05 mm of the same whole multiple of 50 mm
pub fn is_default_marker_size(size: Vec2) -> bool {
    let multiple = (size.x / DEFAULT_SIZE_STEP).round();
    let default = multiple * DEFAULT_SIZE_STEP;

    multiple >= 1.0
        && (size.x - default).abs() <= DEFAULT_SIZE_MARGIN
        && (size.y - default).abs() <= DEFAULT_SIZE_MARGIN
}

/// Median of the widths of the reported sizes that are not a default size (meters). None: fewer than
/// `MIN_LEARNED_SIZES` of them
pub fn learned_marker_size(sizes: impl IntoIterator<Item = Vec2>) -> Option<f32> {
    let mut widths = sizes
        .into_iter()
        .filter(|size| size.x > 0.0 && !is_default_marker_size(*size))
        .map(|size| size.x)
        .collect::<Vec<_>>();
    if widths.len() < MIN_LEARNED_SIZES {
        return None;
    }
    widths.sort_by(f32::total_cmp);
    let middle = widths.len() / 2;

    Some(if widths.len().is_multiple_of(2) {
        (widths[middle - 1] + widths[middle]) / 2.0
    } else {
        widths[middle]
    })
}

/// Side lengths of the codes given with `debug.alvr.marker_sizes` (meters)
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MarkerSizes {
    /// Of every code without its own entry
    pub all: Option<f32>,
    /// By payload (case sensitive)
    pub by_payload: Vec<(String, f32)>,
}

impl MarkerSizes {
    /// The size of a code with this payload. None: not given
    pub fn get(&self, payload: Option<&str>) -> Option<f32> {
        payload
            .and_then(|payload| {
                self.by_payload
                    .iter()
                    .find(|(other, _)| other == payload)
                    .map(|(_, size)| *size)
            })
            .or(self.all)
    }
}

/// Parses `debug.alvr.marker_sizes`: one size in mm for every code ("144.2") or payload:mm pairs
/// separated by commas ("RIG1:144.2,SCR1:150"; a number without payload is the size of the other
/// codes). Returns the sizes and the entries that are malformed (left out)
pub fn parse_marker_sizes(text: &str) -> (MarkerSizes, Vec<String>) {
    let mut sizes = MarkerSizes::default();
    let mut malformed = vec![];
    for entry in text
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
    {
        // The last colon: a payload may contain colons
        let (payload, mm) = match entry.rsplit_once(':') {
            Some((payload, mm)) => (Some(payload.trim()), mm.trim()),
            None => (None, entry),
        };
        match (payload, mm.parse::<f32>()) {
            (Some(""), _) => malformed.push(entry.to_owned()),
            (_, Ok(mm)) if mm.is_finite() && mm > 0.0 && mm <= MAX_GIVEN_SIZE_MM => {
                let size = mm / 1000.0;
                if let Some(payload) = payload {
                    sizes.by_payload.retain(|(other, _)| other != payload);
                    sizes.by_payload.push((payload.to_owned(), size));
                } else {
                    sizes.all = Some(size);
                }
            }
            _ => malformed.push(entry.to_owned()),
        }
    }

    (sizes, malformed)
}

/// The size of a code that poses are checked against (meters)
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SizeBaseline {
    /// `debug.alvr.marker_sizes`
    Given(f32),
    /// `learned_marker_size` of the sizes the runtime reported
    Learned(f32),
}

impl SizeBaseline {
    pub fn size(self) -> f32 {
        match self {
            Self::Given(size) | Self::Learned(size) => size,
        }
    }
}

/// Whether a pose reported with this size is trusted: its width is within `tolerance` (a fraction)
/// of the baseline. A default size is never trusted, unless the baseline is given and is itself a
/// multiple of 50 mm
pub fn marker_size_trusted(size: Vec2, baseline: SizeBaseline, tolerance: f32) -> bool {
    let default_allowed = match baseline {
        SizeBaseline::Given(given) => is_default_marker_size(Vec2::splat(given)),
        SizeBaseline::Learned(_) => false,
    };
    if !default_allowed && is_default_marker_size(size) {
        return false;
    }

    (size.x - baseline.size()).abs() <= baseline.size() * tolerance
}

/// Where the box is relative to the marker
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MarkerBoxParams {
    /// Of the center of the box from the marker: x to the right of the heading, y up, z along the
    /// heading (meters). For a standing code facing the viewer the heading points at the viewer: +x is
    /// their left, +z comes towards them
    pub offset: Vec3,
    /// Full size: width (across the heading), height, depth (along the heading), meters
    pub size: Vec3,
    /// Added to the heading of the marker (radians, counter-clockwise seen from above). It turns the
    /// offset too: the box turns about the marker
    pub extra_yaw: f32,
}

/// The box for a marker at `position` with the level heading `yaw` (see `heading_yaw`). In the box
/// frame +X is to the right of the heading, +Y up and -Z along the heading
pub fn marker_box(position: Vec3, yaw: f32, params: &MarkerBoxParams) -> RegionOverride {
    let rotation = Quat::from_rotation_y(yaw + params.extra_yaw);
    let offset = rotation * Vec3::new(params.offset.x, params.offset.y, -params.offset.z);

    RegionOverride {
        volume_to_world: Mat4::from_rotation_translation(rotation, position + offset),
        half_extents: params.size.abs() * 0.5,
    }
}

/// A pose away from the average that is reported unchanged for this long without a gap is taken as a
/// move, even alone (a runtime that stops refreshing the pose of a still marker). Poses further apart
/// than `MAX_REPEAT_GAP_S` (the marker was not seen, or a marker_rate below 2 Hz, or poses from the
/// discoveries only) never make up this time: a move then needs `reset_count` distinct poses
const UNCHANGED_MOVE_S: f64 = 2.0;
const MAX_REPEAT_GAP_S: f64 = 0.5;

/// Average of the pose of a marker over a time window while it doesn't move. A pose away from the
/// average by more than the limits is left out, unless `reset_count` poses in a row are away, agree
/// with each other and span the move hold (or one stays for `UNCHANGED_MOVE_S`): that is a move, and
/// the average starts again from them. A pose equal to the one before it is not counted again (the
/// runtime may report the same pose several times)
pub struct PoseSmoother {
    window_s: f64,
    reset_distance: f32,
    reset_angle: f32,
    reset_count: usize,
    // The poses away from the average must span at least this long to be a move (s)
    move_hold_s: f64,
    // Time (s), position, heading direction
    samples: VecDeque<(f64, Vec3, Vec3)>,
    // The last poses in a row away from the average (time, position, yaw): outliers or the start of
    // a move
    pending: Vec<(f64, Vec3, f32)>,
    // The newest pose and time pushed, and since when that pose is reported without a gap
    last_pose: Option<(Vec3, f32)>,
    last_time: Option<f64>,
    unchanged_since: f64,
    rejected: usize,
}

impl PoseSmoother {
    /// `reset_angle` in radians
    pub fn new(window_s: f64, reset_distance: f32, reset_angle: f32, reset_count: usize) -> Self {
        Self {
            window_s,
            reset_distance,
            reset_angle,
            reset_count: reset_count.max(1),
            move_hold_s: 0.0,
            samples: VecDeque::new(),
            pending: Vec::new(),
            last_pose: None,
            last_time: None,
            unchanged_since: 0.0,
            rejected: 0,
        }
    }

    /// Time the poses of a move must span, from the first to the last (seconds, 0 by default)
    pub fn set_move_hold(&mut self, hold_s: f64) {
        self.move_hold_s = hold_s.max(0.0);
    }

    pub fn move_hold(&self) -> f64 {
        self.move_hold_s
    }

    pub fn reset(&mut self) {
        self.samples.clear();
        self.pending.clear();
        self.last_pose = None;
        self.last_time = None;
    }

    /// Length of the average (seconds). A shorter window drops the older samples now
    pub fn set_window(&mut self, window_s: f64) {
        self.window_s = window_s;
        self.drop_old();
    }

    pub fn window(&self) -> f64 {
        self.window_s
    }

    /// Number of samples in the average
    pub fn len(&self) -> usize {
        self.samples.len()
    }

    /// Time from the oldest to the newest sample (s)
    pub fn span(&self) -> f64 {
        match (self.samples.front(), self.samples.back()) {
            (Some((oldest, _, _)), Some((newest, _, _))) => newest - oldest,
            _ => 0.0,
        }
    }

    /// Poses left out as outliers since the smoother was made (not cleared by `reset`)
    pub fn rejected(&self) -> usize {
        self.rejected
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// Mean position and the yaw of the mean heading. None: no samples
    pub fn value(&self) -> Option<(Vec3, f32)> {
        if self.samples.is_empty() {
            return None;
        }
        let count = self.samples.len() as f32;
        let position = self.samples.iter().map(|(_, p, _)| *p).sum::<Vec3>() / count;
        let direction = self.samples.iter().map(|(_, _, d)| *d).sum::<Vec3>();
        // Opposite headings cancel out: keep the newest one
        let direction = direction
            .try_normalize()
            .unwrap_or(self.samples.back().map_or(Vec3::NEG_Z, |(_, _, d)| *d));

        Some((position, heading_yaw(direction)))
    }

    /// Add a sample, returns the smoothed position and yaw. A time older than the last sample is
    /// treated as the same time
    pub fn push(&mut self, time_s: f64, position: Vec3, yaw: f32) -> (Vec3, f32) {
        let time_s = self.last_time.map_or(time_s, |last| time_s.max(last));
        let gap = self
            .last_time
            .is_some_and(|last| time_s - last > MAX_REPEAT_GAP_S);
        self.last_time = Some(time_s);
        let repeat = self.last_pose == Some((position, yaw));
        self.last_pose = Some((position, yaw));
        // The time the marker was not seen doesn't count as unchanged
        if !repeat || gap {
            self.unchanged_since = time_s;
        }

        let Some((mean_position, mean_yaw)) = self.value() else {
            self.samples
                .push_back((time_s, position, yaw_direction(yaw)));
            return (position, yaw);
        };
        let (reset_distance, reset_angle) = (self.reset_distance, self.reset_angle);
        let near = move |other_position: Vec3, other_yaw: f32| {
            position.distance(other_position) <= reset_distance
                && yaw_difference(yaw, other_yaw).abs() <= reset_angle
        };

        if near(mean_position, mean_yaw) {
            // Back at the average: the poses away from it were outliers
            self.rejected += self.pending.len();
            self.pending.clear();
            if !repeat {
                self.samples
                    .push_back((time_s, position, yaw_direction(yaw)));
            }
        } else if repeat && let Some(&(first, _, _)) = self.pending.first() {
            // The same pose again: it counts once, unless it stays
            if time_s - self.unchanged_since < UNCHANGED_MOVE_S || time_s - first < self.move_hold_s
            {
                return (mean_position, mean_yaw);
            }
            self.take_pending();
        } else {
            // Away from the average: a move only if the poses before it are at the same new place
            let count = self.pending.len();
            self.pending
                .retain(|(_, other_position, other_yaw)| near(*other_position, *other_yaw));
            self.rejected += count - self.pending.len();
            self.pending.push((time_s, position, yaw));
            if self.pending.len() < self.reset_count
                || time_s - self.pending[0].0 < self.move_hold_s
            {
                return (mean_position, mean_yaw);
            }
            self.take_pending();
        }

        self.drop_old();
        self.value().unwrap_or((position, yaw))
    }

    // The poses away from the average are a move: start the average from them
    fn take_pending(&mut self) {
        self.samples.clear();
        self.samples.extend(
            self.pending
                .drain(..)
                .map(|(time, position, yaw)| (time, position, yaw_direction(yaw))),
        );
    }

    // Samples older than the window before the newest one
    fn drop_old(&mut self) {
        let Some(&(newest, _, _)) = self.samples.back() else {
            return;
        };
        while self
            .samples
            .front()
            .is_some_and(|(time, _, _)| *time < newest - self.window_s)
        {
            self.samples.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::{FRAC_PI_2, PI};

    // A code lying face up on the desk, its top towards stage -Z, then turned by `yaw`
    fn flat(yaw: f32) -> Quat {
        Quat::from_rotation_y(yaw) * Quat::from_rotation_x(-FRAC_PI_2)
    }

    fn angle_between(a: Vec3, b: Vec3) -> f32 {
        a.angle_between(b).to_degrees()
    }

    #[test]
    fn flat_marker_heads_to_its_top() {
        let orientation = flat(0.0);
        // Sanity check of the test pose: normal up, top of the code towards -Z
        assert!((orientation * Vec3::Z - Vec3::Y).length() < 1e-6);
        assert!((orientation * Vec3::Y - Vec3::NEG_Z).length() < 1e-6);

        let heading = marker_heading(orientation);
        assert!(heading.flat);
        assert!(heading.normal_from_vertical_deg.abs() < 1e-3);
        assert!((heading.direction - Vec3::NEG_Z).length() < 1e-6);
        assert!(heading_yaw(heading.direction).abs() < 1e-6);

        for yaw_deg in [-170.0_f32, -90.0, -30.0, 15.0, 90.0, 179.0] {
            let heading = marker_heading(flat(yaw_deg.to_radians()));
            assert!(heading.flat);
            let yaw = heading_yaw(heading.direction);
            assert!(
                yaw_difference(yaw, yaw_deg.to_radians()).abs() < 1e-5,
                "{yaw_deg} {}",
                yaw.to_degrees()
            );
        }
    }

    // A tilt of a few degrees (a code on a sloped surface, or a tilt error of the tracking) must not
    // turn the heading by more than a tiny fraction of it
    #[test]
    fn tilt_does_not_turn_a_flat_heading() {
        for yaw_deg in [0.0_f32, 37.0, -120.0] {
            let base = flat(yaw_deg.to_radians());
            for (axis, tilt_deg) in [
                (Vec3::X, 5.0_f32),
                (Vec3::Z, 5.0),
                (Vec3::new(1.0, 0.0, 1.0).normalize(), -4.0),
                (Vec3::new(-1.0, 0.0, 2.0).normalize(), 6.0),
            ] {
                let tilted = Quat::from_axis_angle(axis, tilt_deg.to_radians()) * base;
                let heading = marker_heading(tilted);
                assert!(heading.flat);
                assert!(heading.direction.y == 0.0);
                let error = yaw_difference(heading_yaw(heading.direction), yaw_deg.to_radians())
                    .to_degrees()
                    .abs();
                // Second order in the tilt: (6 degrees)^2 / 2 in radians is about 0.3 degrees
                assert!(error < 0.35, "{yaw_deg} {axis} {tilt_deg}: {error}");
                // The normal shows the tilt
                assert!((heading.normal_from_vertical_deg - tilt_deg.abs()).abs() < 1e-2);
            }
        }
    }

    #[test]
    fn standing_marker_heads_out_of_the_code() {
        // Identity: the code stands, its front faces +Z
        let heading = marker_heading(Quat::IDENTITY);
        assert!(!heading.flat);
        assert!((heading.normal_from_vertical_deg - 90.0).abs() < 1e-3);
        assert!((heading.direction - Vec3::Z).length() < 1e-6);
        assert!((yaw_difference(heading_yaw(heading.direction), PI)).abs() < 1e-6);

        // Leaning back by 30 degrees (like a monitor) and turned: still standing, still its normal
        let orientation = Quat::from_rotation_y(0.4) * Quat::from_rotation_x(-30_f32.to_radians());
        let heading = marker_heading(orientation);
        assert!(!heading.flat);
        assert!((heading.normal_from_vertical_deg - 60.0).abs() < 1e-3);
        let normal = orientation * Vec3::Z;
        assert!(angle_between(heading.direction, Vec3::new(normal.x, 0.0, normal.z)) < 1e-3);

        // Over 45 degrees back it counts as lying
        let orientation = Quat::from_rotation_x(-50_f32.to_radians());
        assert!(marker_heading(orientation).flat);
        // Face down (under a desk top) also counts as lying
        let orientation = Quat::from_rotation_x(FRAC_PI_2);
        let heading = marker_heading(orientation);
        assert!(heading.flat);
        assert!((heading.normal_from_vertical_deg - 180.0).abs() < 1e-3);
        assert!((heading.direction - Vec3::Z).length() < 1e-6);
    }

    #[test]
    fn yaw_conversions() {
        for yaw_deg in [-179.0_f32, -90.0, 0.0, 45.0, 90.0, 180.0] {
            let yaw = yaw_deg.to_radians();
            let direction = yaw_direction(yaw);
            assert!(direction.y.abs() < 1e-6);
            assert!(yaw_difference(heading_yaw(direction), yaw).abs() < 1e-5);
        }
        assert!(
            (yaw_difference(179_f32.to_radians(), -179_f32.to_radians()).to_degrees() + 2.0).abs()
                < 1e-3
        );
        assert!(
            (yaw_difference(-179_f32.to_radians(), 179_f32.to_radians()).to_degrees() - 2.0).abs()
                < 1e-3
        );
    }

    #[test]
    fn scatter_per_axis() {
        assert_eq!(scatter([]), None);
        let (deviation, count) = scatter([Vec3::new(1.0, 2.0, 3.0)]).unwrap();
        assert_eq!((deviation, count), (Vec3::ZERO, 1));
        let points = [
            Vec3::new(0.001, 0.5, -0.002),
            Vec3::new(-0.001, 0.5, 0.002),
            Vec3::new(0.001, 0.5, -0.002),
            Vec3::new(-0.001, 0.5, 0.002),
        ];
        let (deviation, count) = scatter(points).unwrap();
        assert_eq!(count, 4);
        assert!(
            (deviation - Vec3::new(0.001, 0.0, 0.002)).length() < 1e-6,
            "{deviation}"
        );
    }

    #[test]
    fn box_follows_the_marker_frame() {
        let params = MarkerBoxParams {
            offset: Vec3::new(0.1, 0.15, 0.2),
            size: Vec3::new(1.2, 0.9, 1.0),
            extra_yaw: 0.0,
        };
        let position = Vec3::new(0.3, 0.75, -0.4);

        // Heading -Z: right is +X
        let region = marker_box(position, 0.0, &params);
        assert!((region.center() - (position + Vec3::new(0.1, 0.15, -0.2))).length() < 1e-6);
        assert_eq!(region.half_extents, Vec3::new(0.6, 0.45, 0.5));

        // Heading -X (yaw 90 degrees): right is -Z
        let region = marker_box(position, FRAC_PI_2, &params);
        let expected = position + Vec3::new(-0.2, 0.15, -0.1);
        assert!(
            (region.center() - expected).length() < 1e-6,
            "{}",
            region.center()
        );
        assert!((region.yaw() - FRAC_PI_2).abs() < 1e-6);
        // The depth is along the heading, the width across it
        let world_to_volume = region.volume_to_world.inverse();
        let inside = |point: Vec3| {
            let local = world_to_volume.transform_point3(point);
            (local.abs() - region.half_extents).max_element() < 0.0
        };
        let heading = Vec3::NEG_X;
        let right = Vec3::NEG_Z;
        assert!(inside(region.center() + heading * 0.49));
        assert!(!inside(region.center() + heading * 0.51));
        assert!(inside(region.center() + right * 0.59));
        assert!(!inside(region.center() + right * 0.61));
        assert!(inside(region.center() + Vec3::Y * 0.44));
        assert!(!inside(region.center() + Vec3::Y * 0.46));

        // The extra yaw turns the box about the marker
        let turned = marker_box(
            position,
            0.0,
            &MarkerBoxParams {
                extra_yaw: FRAC_PI_2,
                ..params
            },
        );
        assert!((turned.center() - expected).length() < 1e-6);
    }

    // A standing code faces the driver: its heading points towards the driver, so +x of the offset is
    // the driver's left and +z comes towards the driver (the opposite of a flat code read by them)
    #[test]
    fn standing_marker_offset_is_mirrored_for_the_viewer() {
        let params = MarkerBoxParams {
            offset: Vec3::new(0.2, 0.15, 0.3),
            size: Vec3::new(1.2, 0.9, 1.0),
            extra_yaw: 0.0,
        };
        let position = Vec3::new(0.0, 1.0, -0.6);

        // Standing, its front towards +Z where the driver sits (driver looks along -Z, their right is
        // +X)
        let heading = marker_heading(Quat::IDENTITY);
        assert!(!heading.flat);
        let region = marker_box(position, heading_yaw(heading.direction), &params);
        let expected = position + Vec3::new(-0.2, 0.15, 0.3);
        assert!(
            (region.center() - expected).length() < 1e-5,
            "{}",
            region.center()
        );

        // The same code lying flat with its top away from the driver (towards -Z)
        let heading = marker_heading(flat(0.0));
        assert!(heading.flat);
        let region = marker_box(position, heading_yaw(heading.direction), &params);
        let expected = position + Vec3::new(0.2, 0.15, -0.3);
        assert!(
            (region.center() - expected).length() < 1e-5,
            "{}",
            region.center()
        );
    }

    // From a marker pose to the box, a few degrees of tilt move the far end of the box by
    // millimeters only
    #[test]
    fn tilt_barely_moves_the_box() {
        let params = MarkerBoxParams {
            offset: Vec3::new(0.0, 0.15, 0.0),
            size: Vec3::new(1.2, 0.9, 1.0),
            extra_yaw: 0.0,
        };
        let position = Vec3::new(0.0, 0.75, -0.5);
        let level = marker_box(
            position,
            heading_yaw(marker_heading(flat(0.3)).direction),
            &params,
        );
        let tilted_orientation = Quat::from_axis_angle(Vec3::X, 4_f32.to_radians())
            * Quat::from_axis_angle(Vec3::Z, 3_f32.to_radians())
            * flat(0.3);
        let tilted = marker_box(
            position,
            heading_yaw(marker_heading(tilted_orientation).direction),
            &params,
        );
        let shift = level.max_corner_shift(&tilted);
        assert!(shift < 0.003, "{shift}");
    }

    #[test]
    fn yaw_scatter_about_the_circular_mean() {
        assert_eq!(yaw_scatter([]), None);
        assert_eq!(yaw_scatter([0.7]), Some((0.0, 0.0)));

        // Around +-180 degrees: close, not 358 degrees apart
        let (deviation, range) =
            yaw_scatter([179_f32.to_radians(), -179_f32.to_radians()]).unwrap();
        assert!((deviation.to_degrees() - 1.0).abs() < 1e-3, "{deviation}");
        assert!((range.to_degrees() - 2.0).abs() < 1e-3, "{range}");

        let (deviation, range) = yaw_scatter(
            [10.0_f32, 12.0, 14.0, 12.0]
                .into_iter()
                .map(f32::to_radians),
        )
        .unwrap();
        assert!((deviation.to_degrees() - 2_f32.sqrt()).abs() < 1e-3);
        assert!((range.to_degrees() - 4.0).abs() < 1e-3);
    }

    // Still poses of a code with a little noise, distinct from each other like real ones
    fn still(center: Vec3, i: usize) -> Vec3 {
        let sign = if i.is_multiple_of(2) { 1.0 } else { -1.0 };
        center + Vec3::new(0.001, -0.0005, 0.0005) * sign * (1 + i % 3) as f32
    }

    #[test]
    fn smoother_averages_a_still_marker() {
        let mut smoother = PoseSmoother::new(1.0, 0.02, 3_f32.to_radians(), 3);
        let center = Vec3::new(0.2, 0.8, -0.5);
        let mut last = (Vec3::ZERO, 0.0);
        // +-5 mm and +-1 degree of jitter at 10 Hz
        for i in 0..30 {
            let sign = if i % 2 == 0 { 1.0 } else { -1.0 };
            let noise = Vec3::new(0.005, -0.004, 0.003) * sign;
            last = smoother.push(
                i as f64 * 0.1,
                center + noise,
                0.5 + 1_f32.to_radians() * sign,
            );
        }
        // About 1 s of samples
        assert!((10..=11).contains(&smoother.len()), "{}", smoother.len());
        assert!((last.0 - center).length() < 0.001, "{}", last.0);
        assert!(yaw_difference(last.1, 0.5).abs() < 0.2_f32.to_radians());
    }

    // A move is taken after three poses in a row at the new place (one pose used to be enough)
    #[test]
    fn smoother_restarts_when_the_marker_moves() {
        let mut smoother = PoseSmoother::new(1.0, 0.02, 3_f32.to_radians(), 3);
        let start = Vec3::new(0.0, 0.8, -0.5);
        for i in 0..5 {
            smoother.push(i as f64 * 0.1, still(start, i), 0.0);
        }
        assert_eq!(smoother.len(), 5);

        // 1.5 cm: still averaged
        let (position, _) = smoother.push(0.5, start + Vec3::X * 0.015, 0.0);
        assert_eq!(smoother.len(), 6);
        assert!((position - start).length() < 0.005);
        let mean = position;

        // 3 cm: the first two poses are held back, the third starts the average again
        let moved = start + Vec3::X * 0.03;
        for i in 0..2 {
            let (position, _) = smoother.push(0.6 + i as f64 * 0.1, still(moved, i), 0.0);
            assert_eq!(smoother.len(), 6);
            assert_eq!(position, mean);
        }
        let (position, _) = smoother.push(0.8, still(moved, 2), 0.0);
        assert_eq!(smoother.len(), 3);
        assert!((position - moved).length() < 0.002, "{position}");
        assert_eq!(smoother.rejected(), 0);

        // 4 degrees: starts again after three poses
        let mean = smoother.value().unwrap();
        for (i, yaw_deg) in [4.0_f32, 4.1, 4.2].into_iter().enumerate() {
            let value = smoother.push(0.9 + i as f64 * 0.1, moved, yaw_deg.to_radians());
            if i < 2 {
                assert_eq!(smoother.len(), 3);
                assert_eq!(value, mean);
            }
        }
        let (_, yaw) = smoother.value().unwrap();
        assert_eq!(smoother.len(), 3);
        assert!((yaw - 4.1_f32.to_radians()).abs() < 1e-4);

        // Headings around +-180 degrees are close, not opposite
        smoother.reset();
        smoother.push(1.2, moved, 179_f32.to_radians());
        let (_, yaw) = smoother.push(1.3, moved, -179_f32.to_radians());
        assert_eq!(smoother.len(), 2);
        assert!(yaw_difference(yaw, PI).abs() < 1e-3, "{}", yaw.to_degrees());

        smoother.reset();
        assert!(smoother.is_empty());
        assert_eq!(smoother.value(), None);
    }

    #[test]
    fn smoother_leaves_out_single_outliers() {
        let mut smoother = PoseSmoother::new(10.0, 0.02, 3_f32.to_radians(), 3);
        let center = Vec3::new(0.2, 0.8, -0.5);
        let mut time = 0.0;
        let mut push = |smoother: &mut PoseSmoother, position: Vec3, yaw_deg: f32| {
            time += 0.1;
            smoother.push(time, position, yaw_deg.to_radians())
        };
        for i in 0..10 {
            push(&mut smoother, still(center, i), 0.0);
        }
        let (mean, _) = smoother.value().unwrap();

        // 2.5 cm off, then 3.5 degrees off, each a single pose
        let (position, yaw) = push(&mut smoother, center + Vec3::X * 0.025, 0.0);
        assert!((position - mean).length() < 1e-6 && yaw.abs() < 1e-6);
        push(&mut smoother, still(center, 10), 0.0);
        let (position, yaw) = push(&mut smoother, center, 3.5);
        assert!((position - mean).length() < 0.001 && yaw.abs() < 1e-6);
        for i in 11..20 {
            push(&mut smoother, still(center, i), 0.0);
        }

        let (position, yaw) = smoother.value().unwrap();
        assert!((position - center).length() < 0.001, "{position}");
        assert!(yaw.to_degrees().abs() < 0.1);
        // The average keeps growing: 20 still poses
        assert_eq!(smoother.len(), 20);
        assert_eq!(smoother.rejected(), 2);

        // Three outliers in a row that don't agree with each other are no move either
        push(&mut smoother, center + Vec3::X * 0.03, 0.0);
        push(&mut smoother, center - Vec3::X * 0.03, 0.0);
        push(&mut smoother, center + Vec3::Z * 0.03, 0.0);
        push(&mut smoother, still(center, 20), 0.0);
        assert_eq!(smoother.len(), 21);
        assert_eq!(smoother.rejected(), 5);
    }

    // The runtime may report a pose again until it refreshes it (10 Hz snapshots, 2 Hz refresh)
    #[test]
    fn smoother_counts_a_repeated_pose_once() {
        let mut smoother = PoseSmoother::new(10.0, 0.02, 3_f32.to_radians(), 3);
        let center = Vec3::new(0.0, 0.8, -0.5);
        let mut time = 0.0;
        for i in 0..4 {
            for _ in 0..5 {
                time += 0.1;
                smoother.push(time, still(center, i), 0.0);
            }
        }
        assert_eq!(smoother.len(), 4);

        // An outlier repeated 5 times is still one outlier
        let outlier = center + Vec3::X * 0.05;
        for _ in 0..5 {
            time += 0.1;
            let (position, _) = smoother.push(time, outlier, 0.0);
            assert!((position - center).length() < 0.002);
        }
        time += 0.1;
        smoother.push(time, still(center, 4), 0.0);
        assert_eq!(smoother.rejected(), 1);

        // A moved code whose pose is no longer refreshed: followed after 2 s
        let moved = center + Vec3::X * 0.1;
        let start = time;
        let mut position = center;
        while time < start + 2.25 {
            time += 0.1;
            (position, _) = smoother.push(time, moved, 0.0);
            // The first one came at start + 0.1
            if time < start + 2.05 {
                assert!((position - center).length() < 0.002, "{time}");
            }
        }
        assert_eq!(position, moved);
        assert_eq!(smoother.len(), 1);
    }

    // A real move whose poses repeat until the runtime refreshes them: followed at the third distinct
    // pose, not before
    #[test]
    fn smoother_follows_a_move_with_repeated_poses() {
        let mut smoother = PoseSmoother::new(10.0, 0.02, 3_f32.to_radians(), 3);
        let center = Vec3::new(0.0, 0.8, -0.5);
        let mut time = 0.0;
        for i in 0..10 {
            time += 0.1;
            smoother.push(time, still(center, i), 0.0);
        }
        let mean = smoother.value().unwrap();

        let moved = center + Vec3::X * 0.03;
        for i in 0..3 {
            for repeat in 0..2 {
                time += 0.1;
                let (position, yaw) = smoother.push(time, still(moved, i), 0.0);
                if i < 2 {
                    assert_eq!((position, yaw), mean, "{i} {repeat}");
                } else {
                    assert!(
                        (position - moved).length() < 0.005,
                        "{i} {repeat} {position}"
                    );
                }
            }
        }
        assert_eq!(smoother.rejected(), 0);
    }

    // The pose last seen before the marker left the view, reported again when it is back, is not
    // taken as unchanged for the whole time it was not seen
    #[test]
    fn smoother_does_not_count_a_gap_as_unchanged() {
        let mut smoother = PoseSmoother::new(10.0, 0.02, 3_f32.to_radians(), 3);
        let center = Vec3::new(0.0, 0.8, -0.5);
        let mut time = 0.0;
        for i in 0..10 {
            time += 0.1;
            smoother.push(time, still(center, i), 0.0);
        }
        let mean = smoother.value().unwrap();

        let outlier = center + Vec3::X * 0.03;
        time += 0.1;
        assert_eq!(smoother.push(time, outlier, 0.0), mean);
        time += 30.0;
        assert_eq!(smoother.push(time, outlier, 0.0), mean);
        for i in 10..15 {
            time += 0.1;
            smoother.push(time, still(center, i), 0.0);
        }
        let (position, _) = smoother.value().unwrap();
        assert!((position - center).length() < 0.002, "{position}");
        assert_eq!(smoother.rejected(), 1);
    }

    // With a move hold, agreeing poses away from the average are a move only once they span it
    #[test]
    fn smoother_holds_a_move() {
        let mut smoother = PoseSmoother::new(10.0, 0.02, 3_f32.to_radians(), 3);
        smoother.set_move_hold(0.95);
        let center = Vec3::new(0.0, 0.8, -0.5);
        for i in 0..10 {
            smoother.push(i as f64 * 0.1, still(center, i), 0.0);
        }
        let mean = smoother.value().unwrap();

        let moved = center + Vec3::X * 0.03;
        // Over 0.9 s: held back
        for i in 0..10 {
            let value = smoother.push(1.0 + i as f64 * 0.1, still(moved, i), 0.0);
            assert_eq!(value, mean, "{i}");
        }
        // Over 1 s: a move
        let (position, _) = smoother.push(2.0, still(moved, 10), 0.0);
        assert!((position - moved).length() < 0.005, "{position}");
        assert_eq!(smoother.len(), 11);
        assert_eq!(smoother.rejected(), 0);
    }

    fn square(mm: f32) -> Vec2 {
        Vec2::splat(mm / 1000.0)
    }

    #[test]
    fn marker_default_sizes() {
        assert!(is_default_marker_size(square(100.0)));
        assert!(is_default_marker_size(square(50.0)));
        assert!(is_default_marker_size(square(150.0)));
        assert!(is_default_marker_size(Vec2::new(0.10004, 0.09996)));
        assert!(!is_default_marker_size(square(145.6)));
        assert!(!is_default_marker_size(square(99.2)));
        assert!(!is_default_marker_size(square(100.1)));
        // Both must be the same multiple
        assert!(!is_default_marker_size(Vec2::new(0.1, 0.05)));
        assert!(!is_default_marker_size(Vec2::ZERO));
    }

    #[test]
    fn marker_size_learned_without_defaults() {
        let sizes = [145.4, 100.0, 146.1, 50.0, 145.6].map(square);
        let learned = learned_marker_size(sizes).unwrap();
        assert!((learned - 0.1456).abs() < 1e-6, "{learned}");
        // Even count: between the middle two
        let learned = learned_marker_size([145.0, 146.0, 100.0, 147.0, 144.0].map(square)).unwrap();
        assert!((learned - 0.1455).abs() < 1e-6, "{learned}");
        // Fewer than 3 that are not defaults
        assert_eq!(
            learned_marker_size([145.4, 100.0, 146.1, 50.0].map(square)),
            None
        );
        assert_eq!(learned_marker_size([]), None);
    }

    #[test]
    fn marker_sizes_parse() {
        let (sizes, malformed) = parse_marker_sizes("RIG1:144.2,SCR1:150");
        assert!(malformed.is_empty());
        assert_eq!(sizes.all, None);
        assert!((sizes.get(Some("RIG1")).unwrap() - 0.1442).abs() < 1e-6);
        assert!((sizes.get(Some("SCR1")).unwrap() - 0.15).abs() < 1e-6);
        // Case sensitive
        assert_eq!(sizes.get(Some("rig1")), None);
        assert_eq!(sizes.get(None), None);

        let (sizes, malformed) = parse_marker_sizes("144.2");
        assert!(malformed.is_empty());
        assert!((sizes.get(Some("RIG2")).unwrap() - 0.1442).abs() < 1e-6);
        assert!((sizes.get(None).unwrap() - 0.1442).abs() < 1e-6);

        let (sizes, malformed) = parse_marker_sizes("RIG1:abc");
        assert_eq!(malformed, ["RIG1:abc"]);
        assert_eq!(sizes, MarkerSizes::default());

        // Spaces, a payload with a colon, the others kept beside malformed entries
        let (sizes, malformed) = parse_marker_sizes(" RIG1 : 95.6 , a:b:67.4,:12,0,-5,SCR1:");
        assert_eq!(malformed, [":12", "0", "-5", "SCR1:"]);
        assert!((sizes.get(Some("RIG1")).unwrap() - 0.0956).abs() < 1e-6);
        assert!((sizes.get(Some("a:b")).unwrap() - 0.0674).abs() < 1e-6);
    }

    #[test]
    fn marker_size_gate() {
        let learned = SizeBaseline::Learned(0.1456);
        assert!(!marker_size_trusted(square(100.0), learned, 0.03));
        assert!(marker_size_trusted(square(146.9), learned, 0.03));
        assert!(marker_size_trusted(square(143.1), learned, 0.03));
        assert!(!marker_size_trusted(square(151.0), learned, 0.03));
        assert!(marker_size_trusted(square(151.0), learned, 0.05));

        // A default size inside the band: trusted only against a given size that is one too
        let learned = SizeBaseline::Learned(0.1012);
        assert!(!marker_size_trusted(square(100.0), learned, 0.03));
        assert!(!marker_size_trusted(
            square(100.0),
            SizeBaseline::Given(0.1012),
            0.03
        ));
        assert!(marker_size_trusted(
            square(100.0),
            SizeBaseline::Given(0.1),
            0.03
        ));
        assert!(!marker_size_trusted(
            square(50.0),
            SizeBaseline::Given(0.1),
            0.03
        ));
        assert!(marker_size_trusted(
            square(144.9),
            SizeBaseline::Given(0.1442),
            0.03
        ));
    }

    #[test]
    fn yaw_for_the_log_does_not_wrap_near_180() {
        assert!((yaw_log_deg(179_f32.to_radians()) - 179.0).abs() < 1e-3);
        assert!((yaw_log_deg(-179_f32.to_radians()) - 181.0).abs() < 1e-3);
        assert!((yaw_log_deg(-30_f32.to_radians()) + 30.0).abs() < 1e-3);
    }

    #[test]
    fn smoother_window_can_change() {
        let mut smoother = PoseSmoother::new(1.0, 0.02, 3_f32.to_radians(), 3);
        let center = Vec3::new(0.0, 0.8, -0.5);
        smoother.set_window(5.0);
        assert_eq!(smoother.window(), 5.0);
        for i in 0..50 {
            smoother.push(i as f64 * 0.1, still(center, i), 0.0);
        }
        assert_eq!(smoother.len(), 50);
        assert!((smoother.span() - 4.9).abs() < 1e-9);
        // Shorter: the older samples go at once
        smoother.set_window(1.0);
        assert!((10..=11).contains(&smoother.len()), "{}", smoother.len());
    }
}
