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
//! - `debug.alvr.marker_window <ms>`: length of the average of the pose (1000, 100 to 60000). The
//!   rig never moves, so a long average gives a more precise pose
//! - `debug.alvr.marker_move_ms <ms>`: poses away from the average must agree for this long to be a
//!   move (1000, 0 to 60000)
//!
//! The frame of the box is level (stage +Y up). Its heading: for a code lying flat (normal within 45
//! degrees of vertical) the direction of the top of the code, for a standing code the direction out of
//! its front. So for a standing code facing the driver +x is the driver's LEFT and +z comes TOWARDS the
//! driver; for a flat code whose top points away from the driver +x is the driver's right and +z away.
//! The pose is averaged over the window while the marker is still, and kept while the marker is not
//! seen, also without a tracker (the tracker failed or the session stopped; `debug.alvr.marker 0`
//! forgets it). Single poses away from the average are left out; three in a row at the same new place
//! over `marker_move_ms` are a move and start the average again. A change of the STAGE space (only
//! that one, not LOCAL) keeps the box up to the change time (poses before it are valid in the old
//! space), then drops it until the marker is seen again.

use crate::{
    extra_extensions::{Marker, MarkerState, SpatialMarkerTracker},
    passthrough::{debug_property, debug_property_string},
};
use alvr_common::glam::Vec3;
use alvr_graphics::{MarkerBoxParams, PoseSmoother, RegionOverride};
use openxr as xr;
use std::time::{Duration, Instant};

const PROPERTY_POLL_INTERVAL: Duration = Duration::from_millis(500);
// The average of a still marker, and the move that starts it again
const DEFAULT_WINDOW_MS: i64 = 1000;
const DEFAULT_MOVE_HOLD_MS: i64 = 1000;
const RESET_DISTANCE: f32 = 0.02;
const RESET_ANGLE_DEG: f32 = 3.0;
const RESET_COUNT: usize = 3;
// The placement is logged when a corner of the box moved by more than this
const LOG_SHIFT: f32 = 0.01;
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
    // Smoothed position and yaw, kept while the marker is not seen
    kept: Option<(Vec3, f32)>,
    // The stage space changes at this time: the kept pose is dropped then, unless a pose in the new
    // space came first
    drop_at: Option<xr::Time>,
    last_logged: Option<RegionOverride>,
    last_wait_log: Option<Instant>,
    // Outliers logged so far, and when
    logged_rejected: usize,
    last_rejected_log: Option<Instant>,
    // The line about no tracker was logged
    no_tracker_logged: bool,
}

impl MarkerRegion {
    pub fn new() -> Self {
        Self {
            enabled: false,
            marker_id: None,
            params: MarkerBoxParams {
                offset: Vec3::new(0.0, 0.15, 0.0),
                size: Vec3::new(1.2, 0.9, 1.0),
                extra_yaw: 0.0,
            },
            last_poll: None,
            smoother: {
                let mut smoother = PoseSmoother::new(
                    DEFAULT_WINDOW_MS as f64 / 1000.0,
                    RESET_DISTANCE,
                    RESET_ANGLE_DEG.to_radians(),
                    RESET_COUNT,
                );
                smoother.set_move_hold(DEFAULT_MOVE_HOLD_MS as f64 / 1000.0);
                smoother
            },
            selected: None,
            last_sample: None,
            selected_payload: None,
            start: Instant::now(),
            kept: None,
            drop_at: None,
            last_logged: None,
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
            .clamp(100, 60000) as f64
            / 1000.0;
        if window_s != self.smoother.window() {
            crate::stream_log(&format!("marker box: averaging window {window_s:.1} s"));
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
    }

    fn forget(&mut self) {
        self.smoother.reset();
        self.selected = None;
        self.last_sample = None;
        self.selected_payload = None;
        self.kept = None;
        self.drop_at = None;
        self.last_logged = None;
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
    /// The samples are not averaged with the poses in the new space (the tracker reports none before
    /// `change_time`)
    pub fn on_reference_space_change(&mut self, change_time: xr::Time) {
        self.smoother.reset();
        self.last_sample = None;
        if self.kept.is_some() {
            let time = self.drop_at.map_or(change_time.as_nanos(), |pending| {
                pending.as_nanos().max(change_time.as_nanos())
            });
            self.drop_at = Some(xr::Time::from_nanos(time));
        }
    }

    // The payload the box is placed from: `debug.alvr.marker_id`, else that of the marker in use.
    // None: any
    fn wanted_payload(&self) -> Option<&str> {
        self.marker_id
            .as_deref()
            .or(self.selected_payload.as_deref())
    }

    // The marker to place the box from, among the markers with a pose and the wanted payload: the
    // marker in use while it is tracked; else one that is tracked now (the one seen last); else the
    // marker in use (paused); else the one seen last
    fn choose<'a>(&self, tracker: &'a SpatialMarkerTracker) -> Option<&'a Marker> {
        let payload = self.wanted_payload();
        let candidates = || {
            tracker.markers().filter(move |marker| {
                marker.updated.is_some()
                    && payload.is_none_or(|payload| marker.payload.as_deref() == Some(payload))
            })
        };
        let tracked = |marker: &&Marker| marker.state == MarkerState::Tracking;
        let in_use = |marker: &&Marker| Some(marker.id) == self.selected;

        candidates()
            .find(|marker| in_use(marker) && tracked(marker))
            .or_else(|| {
                candidates()
                    .filter(tracked)
                    .max_by_key(|marker| marker.updated)
            })
            .or_else(|| candidates().find(in_use))
            .or_else(|| candidates().max_by_key(|marker| marker.updated))
    }

    /// The box for the frame displayed at `now`. None: the region of the settings
    pub fn update(
        &mut self,
        tracker: Option<&SpatialMarkerTracker>,
        now: xr::Time,
    ) -> Option<RegionOverride> {
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

        if self
            .drop_at
            .is_some_and(|time| now.as_nanos() >= time.as_nanos())
        {
            crate::stream_log("marker box: stage space changed, waiting for the marker");
            self.drop_at = None;
            self.kept = None;
            self.last_logged = None;
        }

        let (position, yaw) = self.kept?;
        let region = alvr_graphics::marker_box(position, yaw, &self.params);
        let shift = self.last_logged.map(|last| last.max_corner_shift(&region));
        if shift.is_none_or(|shift| shift > LOG_SHIFT) {
            self.last_logged = Some(region);
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

        Some(region)
    }

    // Takes the pose of the marker chosen, or logs what it waits for
    fn follow(&mut self, tracker: &SpatialMarkerTracker) {
        if let Some(marker) = self.choose(tracker) {
            if self.selected != Some(marker.id) {
                // Another marker: start from its pose
                self.smoother.reset();
                self.selected = Some(marker.id);
                self.last_sample = None;
            }
            if marker.payload.is_some() {
                self.selected_payload.clone_from(&marker.payload);
            }
            if marker.updated != self.last_sample
                && let Some(updated) = marker.updated
            {
                self.last_sample = Some(updated);
                // A pose in the current stage space
                self.drop_at = None;
                let heading = alvr_graphics::marker_heading(marker.orientation);
                let time = updated.saturating_duration_since(self.start).as_secs_f64();
                // The smoother counts a repeated pose once
                self.kept = Some(self.smoother.push(
                    time,
                    marker.position,
                    alvr_graphics::heading_yaw(heading.direction),
                ));
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
            crate::stream_log(&format!(
                "marker box: {}waiting for {} (known: {})",
                if self.kept.is_some() {
                    "keeping the last pose, "
                } else {
                    ""
                },
                self.wanted_payload()
                    .map_or("a marker with a pose".into(), |id| format!(
                        "the marker {id:?}"
                    )),
                if known.is_empty() {
                    "none".into()
                } else {
                    known.join("; ")
                }
            ));
        }
    }
}
