//! The box of the depth mask region placed from a marker (QR code) on the rig, instead of the region
//! of the settings. Experimental, controlled with adb (polled about twice a second):
//! - `debug.alvr.marker_box 1`: use the box while a marker is known (0: the region of the settings)
//! - `debug.alvr.marker_id <text>`: use the marker with this payload (unset: the first one found, then
//!   markers with the same payload as that one). A change forgets the box at once
//! - `debug.alvr.marker_box_x` / `_y` / `_z <mm>`: center of the box from the marker, to the right of
//!   the heading, up, along the heading (0, 150, 0)
//! - `debug.alvr.marker_box_w` / `_h` / `_d <mm>`: width (across the heading), height, depth (along the
//!   heading) (1200, 900, 1000)
//! - `debug.alvr.marker_box_yaw <deg>`: added to the heading, turns the box about the marker (0)
//! - `debug.alvr.marker_window <ms>`: time window of the poses of the estimate (60000, 100 to 600000,
//!   at most 240 distinct poses). The rig never moves, so a long window gives a more precise pose
//! - `debug.alvr.marker_move_ms <ms>`: poses away from the estimate must agree for this long to be a
//!   move (1000, 0 to 60000; at least 3000 against a measured estimate)
//! - `debug.alvr.marker_scan_poses <n>`: distinct poses (over at least 8 s) that measure the pose of
//!   the marker (20, 3 to 240)
//! - `debug.alvr.marker_scan <n>`: another positive value measures the pose again from the next pose
//!   reported (the value at the start of the app is not a request)
//! - `debug.alvr.marker_hands 0`: the hands of the color key show also outside the box (1: only
//!   inside it, faded at its surface)
//! - `debug.alvr.area_test <n>`: a test layout of the play area instead of the one box, made from the
//!   box of the properties (see `PlayArea`): 1: two boxes and a dome on the top of the first, 8: that
//!   box cut into 8 boxes with 24 domes (the worst case for the GPU). Unset or 0: the one box
//!
//! The play area: the mask shows the real room inside up to 8 boxes in the frame of the code, each
//! with a dome on any of its sides. Without a test layout it is the one box of the properties above.
//! A layout file of the code, or the edits in the headset, replace both (area_editor.rs).
//! The frame of the box is level (stage +Y up). Its heading: for a code lying flat (normal within 45
//! degrees of vertical) the direction of the top of the code, for a standing code the direction out of
//! its front. So for a standing code facing the driver +x is the driver's LEFT and +z comes TOWARDS the
//! driver; for a flat code whose top points away from the driver +x is the driver's right and +z away.
//! Only trusted poses place the box (the size gate of the tracker, `debug.alvr.marker_sizes`): a pose
//! of another size than the baseline of the code is wrong by up to a meter.
//! The pose is a robust estimate of the distinct poses of the window (`PoseSmoother`): the median of
//! the positions, the circular mean of the headings near their median. Once it has `marker_scan_poses`
//! poses over 8 s it is measured and frozen (logged once): the code can then be hidden. It is kept
//! while the marker is not seen, also without a tracker (the tracker failed or the session stopped;
//! `debug.alvr.marker 0` forgets it). Single poses away from the estimate are left out; three in a row
//! at the same new place over `marker_move_ms` are a move and start a new measurement (five over at
//! least 3 s once measured). The same code under a new entity id keeps the estimate. A change of the
//! STAGE space (only that one, not LOCAL) with a valid pose carries the box into the new space at the
//! change time (poses before it are in the old space); without a valid pose the box is kept up to the
//! change time, then dropped until the marker is seen again.

use crate::{
    extra_extensions::{Marker, MarkerState, SpatialMarkerTracker},
    passthrough::{debug_property, debug_property_string},
};
use alvr_common::glam::{Quat, Vec2, Vec3};
use alvr_graphics::{
    MarkerBoxParams, PlacedArea, PlayArea, PoseSmoother, RegionOverride, StageChange,
};
use openxr as xr;
use std::time::{Duration, Instant};

const PROPERTY_POLL_INTERVAL: Duration = Duration::from_millis(500);
// The window of the estimate of a still marker, the poses that measure it, and the move that starts
// it again
const DEFAULT_WINDOW_MS: i64 = 60000;
const MAX_WINDOW_MS: i64 = 600000;
const DEFAULT_SCAN_POSES: i64 = alvr_graphics::DEFAULT_MEASURE_POSES as i64;
const DEFAULT_MOVE_HOLD_MS: i64 = 1000;
const RESET_DISTANCE: f32 = 0.02;
const RESET_ANGLE_DEG: f32 = 3.0;
const RESET_COUNT: usize = 3;
// The placement is logged when a corner of the box moved by more than this, before the pose is
// measured at most this often
const LOG_SHIFT: f32 = 0.01;
const MOVE_LOG_INTERVAL: Duration = Duration::from_secs(1);
const WAIT_LOG_INTERVAL: Duration = Duration::from_secs(10);
const REJECTED_LOG_INTERVAL: Duration = Duration::from_secs(2);

fn millimeters(property: &str, default: i64, min: i64, max: i64) -> f32 {
    debug_property(property).unwrap_or(default).clamp(min, max) as f32 / 1000.0
}

pub struct MarkerRegion {
    enabled: bool,
    marker_id: Option<String>,
    params: MarkerBoxParams,
    last_poll: Option<Instant>,
    smoother: PoseSmoother,
    // Entity id of the marker used, and the time of its last pose that was used
    selected: Option<u64>,
    last_sample: Option<Instant>,
    // Payload of the marker used: without `marker_id` the box stays with codes of this payload
    selected_payload: Option<String>,
    start: Instant,
    // Estimated position and yaw, kept while the marker is not seen
    kept: Option<(Vec3, f32)>,
    // Of the last pose used: the orientation of the code in the level frame of its heading (its tilt),
    // and its size as reported (width, height)
    code_tilt: Quat,
    code_size: Vec2,
    // Times the box was carried over a stage change
    carried: u64,
    // Changes of the stage space at their change time, in order: the kept pose is carried into the
    // new space then (or dropped without a valid pose)
    stage_changes: Vec<(xr::Time, Option<StageChange>)>,
    // `debug.alvr.marker_scan` as last read
    last_scan_request: i64,
    // `debug.alvr.marker_hands`
    hands_limited: bool,
    // The boxes in the frame of the code, from the properties (`debug.alvr.area_test`, else the box
    // of `params`), and the layout as last logged
    layout: PlayArea,
    area_test: i64,
    logged_layout: Option<PlayArea>,
    last_logged: Option<RegionOverride>,
    last_move_log: Option<Instant>,
    last_wait_log: Option<Instant>,
    // Outliers logged so far, and when
    logged_rejected: usize,
    last_rejected_log: Option<Instant>,
    // The line about no tracker was logged
    no_tracker_logged: bool,
}

impl MarkerRegion {
    pub fn new() -> Self {
        let params = MarkerBoxParams {
            offset: Vec3::new(0.0, 0.15, 0.0),
            size: Vec3::new(1.2, 0.9, 1.0),
            extra_yaw: 0.0,
        };
        Self {
            enabled: false,
            marker_id: None,
            params,
            last_poll: None,
            smoother: {
                let mut smoother = PoseSmoother::new(
                    DEFAULT_WINDOW_MS as f64 / 1000.0,
                    RESET_DISTANCE,
                    RESET_ANGLE_DEG.to_radians(),
                    RESET_COUNT,
                );
                smoother.set_move_hold(DEFAULT_MOVE_HOLD_MS as f64 / 1000.0);
                smoother.set_measure_poses(DEFAULT_SCAN_POSES as usize);
                smoother
            },
            selected: None,
            last_sample: None,
            selected_payload: None,
            start: Instant::now(),
            kept: None,
            code_tilt: Quat::IDENTITY,
            code_size: Vec2::ZERO,
            carried: 0,
            stage_changes: Vec::new(),
            // A value left from before is not a request
            last_scan_request: debug_property("debug.alvr.marker_scan").unwrap_or(0),
            hands_limited: true,
            layout: PlayArea::from_marker_box(&params),
            area_test: 0,
            logged_layout: None,
            last_logged: None,
            last_move_log: None,
            last_wait_log: None,
            logged_rejected: 0,
            last_rejected_log: None,
            no_tracker_logged: false,
        }
    }

    fn poll_properties(&mut self) {
        if self
            .last_poll
            .is_some_and(|time| time.elapsed() < PROPERTY_POLL_INTERVAL)
        {
            return;
        }
        self.last_poll = Some(Instant::now());

        self.enabled = debug_property("debug.alvr.marker_box").unwrap_or(0) != 0;

        let marker_id = debug_property_string("debug.alvr.marker_id");
        if marker_id != self.marker_id {
            // Another code is asked for: the box of the old one must not stay
            crate::stream_log(&format!(
                "marker box: marker_id is now {}",
                marker_id
                    .as_ref()
                    .map_or("unset".into(), |id| format!("{id:?}"))
            ));
            self.forget();
            self.marker_id = marker_id;
        }

        let window_s = debug_property("debug.alvr.marker_window")
            .unwrap_or(DEFAULT_WINDOW_MS)
            .clamp(100, MAX_WINDOW_MS) as f64
            / 1000.0;
        if window_s != self.smoother.window() {
            // A shorter window never holds the poses of a measurement
            let short = if window_s < alvr_graphics::MEASURE_MIN_SPAN_S {
                format!(
                    " (shorter than the {:.0} s a measurement needs: never measured)",
                    alvr_graphics::MEASURE_MIN_SPAN_S
                )
            } else {
                String::new()
            };
            crate::stream_log(&format!(
                "marker box: averaging window {window_s:.1} s{short}"
            ));
            self.smoother.set_window(window_s);
        }
        let hold_s = debug_property("debug.alvr.marker_move_ms")
            .unwrap_or(DEFAULT_MOVE_HOLD_MS)
            .clamp(0, 60000) as f64
            / 1000.0;
        if hold_s != self.smoother.move_hold() {
            crate::stream_log(&format!("marker box: a move must hold {hold_s:.1} s"));
            self.smoother.set_move_hold(hold_s);
        }
        let scan_poses = debug_property("debug.alvr.marker_scan_poses")
            .unwrap_or(DEFAULT_SCAN_POSES)
            .clamp(3, alvr_graphics::MAX_POSES as i64) as usize;
        if scan_poses != self.smoother.measure_poses() {
            crate::stream_log(&format!(
                "marker box: a measurement takes {scan_poses} poses over {:.0} s",
                alvr_graphics::MEASURE_MIN_SPAN_S
            ));
            self.smoother.set_measure_poses(scan_poses);
        }
        let scan = debug_property("debug.alvr.marker_scan").unwrap_or(0);
        if scan != self.last_scan_request {
            self.last_scan_request = scan;
            if scan > 0 {
                // The box stays where it is until the next pose reported after the request
                // (`last_sample` stays: the last pose again would place it from that one raw pose
                // while the code is hidden)
                crate::stream_log(&format!("marker box: measuring again (marker_scan {scan})"));
                self.smoother.reset();
            }
        }
        let hands_limited = debug_property("debug.alvr.marker_hands").unwrap_or(1) != 0;
        if hands_limited != self.hands_limited {
            crate::stream_log(if hands_limited {
                "marker box: the hands show inside the box only (marker_hands 1)"
            } else {
                "marker box: the hands show also outside the box (marker_hands 0)"
            });
            self.hands_limited = hands_limited;
        }

        self.params = MarkerBoxParams {
            offset: Vec3::new(
                millimeters("debug.alvr.marker_box_x", 0, -3000, 3000),
                millimeters("debug.alvr.marker_box_y", 150, -3000, 3000),
                millimeters("debug.alvr.marker_box_z", 0, -3000, 3000),
            ),
            size: Vec3::new(
                millimeters("debug.alvr.marker_box_w", 1200, 10, 5000),
                millimeters("debug.alvr.marker_box_h", 900, 10, 5000),
                millimeters("debug.alvr.marker_box_d", 1000, 10, 5000),
            ),
            extra_yaw: (debug_property("debug.alvr.marker_box_yaw")
                .unwrap_or(0)
                .clamp(-360, 360) as f32)
                .to_radians(),
        };

        let area_test = debug_property("debug.alvr.area_test").unwrap_or(0);
        self.layout = match area_test {
            1 => PlayArea::test_layout(&self.params),
            8 => PlayArea::stress_layout(&self.params),
            _ => PlayArea::from_marker_box(&self.params),
        };
        if area_test != self.area_test {
            self.area_test = area_test;
            crate::stream_log(&match area_test {
                0 => "play area: the box of the marker_box properties (area_test 0)".to_owned(),
                1 | 8 => format!("play area: test layout {area_test}"),
                _ => {
                    format!("play area: area_test {area_test} is not a layout (1, 8), the one box")
                }
            });
        }
        // The layout as the mask uses it, each time it changes (properties)
        if self.logged_layout != Some(self.layout) {
            self.logged_layout = Some(self.layout);
            crate::stream_log(&format!("play area: {}", self.layout.describe()));
        }
    }

    /// The estimated pose of the code, while there is a box. Its orientation is the tilt of the last
    /// pose turned to the estimated heading
    pub fn anchor(&self) -> Option<MarkerAnchor> {
        let (position, yaw) = self.kept?;

        Some(MarkerAnchor {
            position,
            orientation: Quat::from_rotation_y(yaw) * self.code_tilt,
            size: self.code_size,
            measured: self.smoother.is_measured(),
        })
    }

    /// The estimated position and heading yaw of the code (stage space), while there is a box. The play
    /// area is placed from it (`PlayArea::area_to_world`)
    pub fn anchor_pose(&self) -> Option<(Vec3, f32)> {
        self.kept
    }

    /// `debug.alvr.marker_box` is on
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// The payload of the code the box is placed from: `debug.alvr.marker_id`, else that of the code in
    /// use. None: not known yet (or a code without payload)
    pub fn payload(&self) -> Option<&str> {
        self.wanted_payload()
    }

    /// How many times the box was carried over a change of the stage space so far
    pub fn carried_count(&self) -> u64 {
        self.carried
    }

    /// Whether the hands of the color key show only inside the box (`debug.alvr.marker_hands`)
    pub fn limits_hands(&self) -> bool {
        self.hands_limited
    }

    fn forget(&mut self) {
        self.smoother.reset();
        self.selected = None;
        self.last_sample = None;
        self.selected_payload = None;
        self.kept = None;
        self.code_size = Vec2::ZERO;
        self.stage_changes.clear();
        self.last_logged = None;
        self.last_move_log = None;
        // The waiting line comes at once
        self.last_wait_log = None;
        self.no_tracker_logged = false;
    }

    /// `debug.alvr.marker 0`: the box goes with the tracker
    pub fn stop(&mut self) {
        if self.kept.is_some() {
            crate::stream_log(
                "marker box: off (debug.alvr.marker 0), the region of the settings applies",
            );
        }
        self.forget();
    }

    /// The STAGE space changes at `change_time`: the kept pose is valid until then, in the old space.
    /// With the pose of the new space in the old one (`change`) the box and the poses of its estimate
    /// are carried into the new space then; without it the estimate starts again (the tracker reports
    /// no pose before `change_time`) and the box is dropped then
    pub fn on_reference_space_change(
        &mut self,
        change_time: xr::Time,
        change: Option<StageChange>,
    ) {
        self.last_sample = None;
        if change.is_none() {
            self.smoother.reset();
        }
        if self.kept.is_some() || !self.smoother.is_empty() {
            self.stage_changes.push((change_time, change));
        }
    }

    // The changes of the stage space that are due at `now`, in order
    fn apply_stage_changes(&mut self, now: xr::Time) {
        while let Some(&(time, change)) = self.stage_changes.first()
            && now.as_nanos() >= time.as_nanos()
        {
            self.stage_changes.remove(0);
            let Some(change) = change else {
                if self.kept.is_some() {
                    crate::stream_log("marker box: stage space changed, waiting for the marker");
                }
                // Changes after this one have nothing to carry
                self.smoother.reset();
                self.kept = None;
                self.last_logged = None;
                continue;
            };
            self.smoother.transform(&change);
            let Some((position, yaw)) = self.kept else {
                continue;
            };
            let old = alvr_graphics::marker_box(position, yaw, &self.params);
            let (position, yaw) = change.apply(position, yaw);
            self.kept = Some((position, yaw));
            let new = alvr_graphics::marker_box(position, yaw, &self.params);
            self.carried += 1;
            // The same place in the room: no "moved by" line for it
            self.last_logged = Some(new);
            let moved = new.center() - old.center();
            crate::stream_log(&format!(
                "marker box: stage space changed, box carried over (moved by ({:.3}, {:.3}, {:.3}) \
                 m, yaw {:.1} deg in the new space)",
                moved.x,
                moved.y,
                moved.z,
                alvr_graphics::yaw_difference(new.yaw(), old.yaw()).to_degrees()
            ));
        }
    }

    // The payload the box is placed from: `debug.alvr.marker_id`, else that of the marker in use.
    // None: any
    fn wanted_payload(&self) -> Option<&str> {
        self.marker_id
            .as_deref()
            .or(self.selected_payload.as_deref())
    }

    fn is_wanted(&self, marker: &Marker) -> bool {
        self.wanted_payload()
            .is_none_or(|payload| marker.payload.as_deref() == Some(payload))
    }

    // The marker to place the box from, among the markers with a trusted pose and the wanted
    // payload: the marker in use while it is tracked; else one that is tracked now (the one trusted
    // last); else the marker in use (paused); else the one trusted last
    fn choose<'a>(&self, tracker: &'a SpatialMarkerTracker) -> Option<&'a Marker> {
        let candidates = || {
            tracker
                .markers()
                .filter(|marker| marker.trusted_updated.is_some() && self.is_wanted(marker))
        };
        let tracked = |marker: &&Marker| marker.state == MarkerState::Tracking;
        let in_use = |marker: &&Marker| Some(marker.id) == self.selected;

        candidates()
            .find(|marker| in_use(marker) && tracked(marker))
            .or_else(|| {
                candidates()
                    .filter(tracked)
                    .max_by_key(|marker| marker.trusted_updated)
            })
            .or_else(|| candidates().find(in_use))
            .or_else(|| candidates().max_by_key(|marker| marker.trusted_updated))
    }

    /// The play area for the frame displayed at `now`, placed at the code. None: the region of the
    /// settings
    pub fn update(
        &mut self,
        tracker: Option<&SpatialMarkerTracker>,
        now: xr::Time,
    ) -> Option<PlacedArea> {
        self.poll_properties();
        if !self.enabled {
            if self.kept.is_some() {
                crate::stream_log(
                    "marker box: off (marker_box 0), the region of the settings applies",
                );
            }
            self.forget();
            return None;
        }

        // Before the poses of this frame: the tracker takes poses in the new space from the change
        // time on
        self.apply_stage_changes(now);

        if let Some(tracker) = tracker {
            self.no_tracker_logged = false;
            self.follow(tracker);
        } else if self.kept.is_some() && !self.no_tracker_logged {
            // The tracker failed or the session stopped: the code may no longer be shown
            self.no_tracker_logged = true;
            crate::stream_log("marker box: no tracker, keeping the last pose");
        }

        let rejected = self.smoother.rejected();
        if rejected > self.logged_rejected
            && self
                .last_rejected_log
                .is_none_or(|time| time.elapsed() > REJECTED_LOG_INTERVAL)
        {
            self.logged_rejected = rejected;
            self.last_rejected_log = Some(Instant::now());
            crate::stream_log(&format!(
                "marker box: {rejected} poses away from the average left out so far"
            ));
        }

        let (position, yaw) = self.kept?;
        let region = alvr_graphics::marker_box(position, yaw, &self.params);
        let shift = self.last_logged.map(|last| last.max_corner_shift(&region));
        // Before it is measured the box follows the estimate: "moved by" at most once a second
        let move_logged_lately = !self.smoother.is_measured()
            && self
                .last_move_log
                .is_some_and(|time| time.elapsed() < MOVE_LOG_INTERVAL);
        if shift.is_none_or(|shift| shift > LOG_SHIFT && !move_logged_lately) {
            self.last_logged = Some(region);
            self.last_move_log = Some(Instant::now());
            let center = region.center();
            let size = region.size();
            crate::stream_log(&format!(
                "marker box: {} at ({:.3}, {:.3}, {:.3}) m, heading {:.1} deg, size {:.3} x {:.3} x {:.3} m, \
                 marker #{} at ({:.3}, {:.3}, {:.3}), {} samples over {:.1} s (window {:.1} s), {} \
                 outliers left out",
                shift.map_or("placed".into(), |shift| format!(
                    "moved by {:.1} cm,",
                    shift * 100.0
                )),
                center.x,
                center.y,
                center.z,
                alvr_graphics::yaw_log_deg(region.yaw()),
                size.x,
                size.y,
                size.z,
                self.selected.unwrap_or(0),
                position.x,
                position.y,
                position.z,
                self.smoother.len(),
                self.smoother.span(),
                self.smoother.window(),
                self.smoother.rejected(),
            ));
        }

        // The first box of the layout is `region` unless a test layout is on
        Some(PlacedArea {
            area_to_world: self.layout.area_to_world(position, yaw),
            area: self.layout,
        })
    }

    // The pose is measured: logged once per measurement
    fn log_measured(&self) {
        let Some((position, yaw)) = self.smoother.value() else {
            return;
        };
        let scatter = alvr_graphics::scatter(self.smoother.samples().map(|(p, _)| p))
            .map_or(Vec3::ZERO, |(deviation, _)| deviation * 1000.0);
        let heading_scatter = alvr_graphics::yaw_scatter(self.smoother.samples().map(|(_, y)| y))
            .map_or(0.0, |(deviation, _)| deviation.to_degrees());
        crate::stream_log(&format!(
            "marker box: measured: position ({:.4}, {:.4}, {:.4}) m, heading {:.2} deg from {} poses \
             over {:.1} s, scatter x/y/z {:.1}/{:.1}/{:.1} mm, heading scatter {:.2} deg",
            position.x,
            position.y,
            position.z,
            alvr_graphics::yaw_log_deg(yaw),
            self.smoother.len(),
            self.smoother.span(),
            scatter.x,
            scatter.y,
            scatter.z,
            heading_scatter
        ));
    }

    // Takes the pose of the marker chosen, or logs what it waits for
    fn follow(&mut self, tracker: &SpatialMarkerTracker) {
        if let Some(marker) = self.choose(tracker) {
            if self.selected != Some(marker.id) {
                // Another marker: start from its pose. The same code under a new entity id (lost and
                // found again, or a new tracker after the session restarted) keeps the estimate, a
                // measured one included: the move rule decides whether its poses differ
                let same_code = marker.payload.is_some() && marker.payload == self.selected_payload;
                if !same_code {
                    self.smoother.reset();
                }
                self.selected = Some(marker.id);
                self.last_sample = None;
            }
            if marker.payload.is_some() {
                self.selected_payload.clone_from(&marker.payload);
            }
            if marker.trusted_updated != self.last_sample
                && let Some(updated) = marker.trusted_updated
            {
                self.last_sample = Some(updated);
                let heading = alvr_graphics::marker_heading(marker.trusted_orientation);
                let sample_yaw = alvr_graphics::heading_yaw(heading.direction);
                self.code_tilt =
                    Quat::from_rotation_y(sample_yaw).inverse() * marker.trusted_orientation;
                // The size belongs to the trusted pose only while that is the latest one
                if marker.trusted || self.code_size == Vec2::ZERO {
                    self.code_size = marker.extents;
                }
                let time = updated.saturating_duration_since(self.start).as_secs_f64();
                let was_measured = self.smoother.is_measured();
                // The smoother counts a repeated pose once
                self.kept = Some(
                    self.smoother
                        .push(time, marker.trusted_position, sample_yaw),
                );
                match (was_measured, self.smoother.is_measured()) {
                    (false, true) => self.log_measured(),
                    (true, false) => {
                        crate::stream_log("marker box: the marker moved, measuring its pose again")
                    }
                    _ => (),
                }
            }
        } else if self
            .last_wait_log
            .is_none_or(|time| time.elapsed() > WAIT_LOG_INTERVAL)
        {
            self.last_wait_log = Some(Instant::now());
            let known = tracker
                .markers()
                .map(crate::extra_extensions::describe)
                .collect::<Vec<_>>();
            // Markers that fit have poses, but none of the size baseline (or no baseline yet)
            let untrusted = tracker
                .markers()
                .any(|marker| marker.updated.is_some() && self.is_wanted(marker));
            crate::stream_log(&format!(
                "marker box: {}waiting for {}{} (known: {})",
                if self.kept.is_some() {
                    "keeping the last pose, "
                } else {
                    ""
                },
                self.wanted_payload()
                    .map_or("a marker with a pose".into(), |id| format!(
                        "the marker {id:?}"
                    )),
                if untrusted {
                    ", none has a trusted pose yet (size gate)"
                } else {
                    ""
                },
                if known.is_empty() {
                    "none".into()
                } else {
                    known.join("; ")
                }
            ));
        }
    }
}

/// The pose of the code as the box uses it, see `MarkerRegion::anchor`
#[derive(Clone, Copy, Debug)]
pub struct MarkerAnchor {
    /// Center of the code (stage space)
    pub position: Vec3,
    /// Of the code: +X to its right, +Y to its top, +Z out of its front
    pub orientation: Quat,
    /// Width and height (meters)
    pub size: Vec2,
    /// The pose is measured and frozen
    pub measured: bool,
}
