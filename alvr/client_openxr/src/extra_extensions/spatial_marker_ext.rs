//! XR_EXT_spatial_entity + XR_EXT_spatial_marker_tracking (with XR_EXT_future): poses and payloads
//! of the QR codes that the runtime sees, in the STAGE space.
//!
//! Nothing here blocks: the spatial context and the discovery snapshots are asynchronous and their
//! futures are polled once per `update`, the update snapshots (fresh poses of known markers) are
//! synchronous and rate limited. Every result code that is not a success is logged once per call and
//! code. A failure of the context or of the discovery snapshots that can't be retried turns the
//! tracker off (it then only answers `is_off`); a failed update snapshot only stops the update
//! snapshots, the poses then come from the discoveries.
//!
//! For the log only, the head (VIEW space) is located in the stage space at most 20 times a second:
//! the marker lines then show the distance and viewing angle of the head and how fast it moved.
//!
//! The extensions are enabled by name (`SPATIAL_MARKER_EXTENSION_NAMES` in `ExtensionSet::other`), not
//! with the flags of `xr::ExtensionSet`: with the flags the `openxr` crate resolves the 20 functions of
//! XR_EXT_spatial_entity inside `create_instance` and returns an error if one is missing, which would
//! end the app. Here `SpatialMarkerFns::load` does it after the instance exists and can fail softly.
//!
//! Experimental, controlled with adb (read when the tracker is created, except where noted):
//! - `debug.alvr.marker_discovery <s>`: discovery snapshot this often without the runtime's
//!   discovery-recommended event, which starts one at most every 250 ms (1..60, default 2, polled)
//! - `debug.alvr.marker_rate <Hz>`: update snapshots of the known markers (0..90, default 10, polled)
//! - `debug.alvr.marker_static 1`: tell the runtime that the markers don't move (only if it lists the
//!   feature; not on a motion rig)
//! - `debug.alvr.marker_size <mm>`: side length of every code, decimals allowed (e.g. 95.6; only if
//!   the runtime lists the feature)

use crate::passthrough::{debug_property, debug_property_string};
use alvr_common::glam::{Quat, Vec2, Vec3};
use openxr::{
    self as xr, raw,
    sys::{self, Handle},
};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    ffi::c_void,
    ptr,
    time::{Duration, Instant},
};

/// To enable at instance creation, all three or none (each one depends on the one before)
pub const SPATIAL_MARKER_EXTENSION_NAMES: [&[u8]; 3] = [
    raw::FutureEXT::NAME,
    raw::SpatialEntityEXT::NAME,
    raw::SpatialMarkerTrackingEXT::NAME,
];

// Components of the QR code capability that are used: the pose and size, and the payload
static COMPONENTS: [sys::SpatialComponentTypeEXT; 2] = [
    sys::SpatialComponentTypeEXT::BOUNDED_2D,
    sys::SpatialComponentTypeEXT::MARKER,
];

// A future that is not ready by then is cancelled
const FUTURE_TIMEOUT: Duration = Duration::from_secs(10);
// After XR_ERROR_PERMISSION_INSUFFICIENT (the spatial data permission may be granted meanwhile)
const PERMISSION_RETRY: Duration = Duration::from_secs(10);
// OpenXR calls that take longer are logged
const SLOW_CALL: Duration = Duration::from_millis(2);
// Interval of the pose changes and head speed over this time
const STATS_WINDOW: Duration = Duration::from_secs(2);
// Scatter of the distinct poses over this time (the runtime may update a pose only every few seconds)
const SCATTER_WINDOW: Duration = Duration::from_secs(10);
// A pose counts as changed when it moved or turned by more than this
const MIN_MOVE: f32 = 0.0001;
const MIN_TURN_DEG: f32 = 0.01;
const HEAD_SAMPLE_INTERVAL: Duration = Duration::from_millis(50);
const SUMMARY_INTERVAL: Duration = Duration::from_secs(2);
// While no marker is known
const IDLE_SUMMARY_INTERVAL: Duration = Duration::from_secs(10);
const PROPERTY_POLL_INTERVAL: Duration = Duration::from_millis(500);
// Discoveries the runtime asks for, at most this often
const MIN_DISCOVERY_GAP: Duration = Duration::from_millis(250);
// Of a payload in the log
const MAX_LOGGED_PAYLOAD: usize = 64;
// The payload cache is cleared when it holds more (a runtime may give new buffer ids per snapshot)
const MAX_CACHED_PAYLOADS: usize = 64;

fn log(text: &str) {
    crate::stream_log(&format!("marker: {text}"));
}

/// Function tables of XR_EXT_future and XR_EXT_spatial_entity
#[derive(Clone, Copy)]
pub struct SpatialMarkerFns {
    future: raw::FutureEXT,
    spatial: raw::SpatialEntityEXT,
}

impl SpatialMarkerFns {
    /// The instance must have been created with `SPATIAL_MARKER_EXTENSION_NAMES`
    pub fn load(instance: &xr::Instance) -> xr::Result<Self> {
        unsafe {
            Ok(Self {
                future: raw::FutureEXT::load(instance.entry(), instance.as_raw())?,
                spatial: raw::SpatialEntityEXT::load(instance.entry(), instance.as_raw())?,
            })
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarkerState {
    Tracking,
    /// May be tracked again
    Paused,
    /// Lost for good (removed from the list right after)
    Stopped,
}

impl MarkerState {
    fn from_xr(state: sys::SpatialEntityTrackingStateEXT) -> Self {
        match state {
            sys::SpatialEntityTrackingStateEXT::TRACKING => Self::Tracking,
            sys::SpatialEntityTrackingStateEXT::STOPPED => Self::Stopped,
            _ => Self::Paused,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Tracking => "tracking",
            Self::Paused => "paused",
            Self::Stopped => "stopped",
        }
    }
}

/// A marker as last reported, stage space
#[derive(Clone, Debug)]
pub struct Marker {
    /// Spatial entity id, unique within the session
    pub id: u64,
    /// The content of the code as text (bytes that are not UTF-8 are replaced). None: not decoded yet
    pub payload: Option<String>,
    /// Center of the code; +X to its right, +Y to its top, +Z out of its front face
    pub position: Vec3,
    pub orientation: Quat,
    /// Full width and height (meters)
    pub extents: Vec2,
    pub state: MarkerState,
    /// When the pose was last reported (state tracking). None: never since the tracker started or the
    /// stage space changed, the pose is not valid
    pub updated: Option<Instant>,
}

struct TrackedMarker {
    marker: Marker,
    // NULL if it couldn't be created: no update snapshots for this marker
    entity: sys::SpatialEntityEXT,
    // Buffer id of the payload last read, 0: none
    payload_buffer: u64,
    // When the reported pose changed by more than `MIN_MOVE` or `MIN_TURN_DEG`, and when it changed
    // at all (bit-exact compare), over `STATS_WINDOW`
    changes: VecDeque<Instant>,
    exact_changes: VecDeque<Instant>,
    // The poses that counted as changed, with their heading yaw (radians), over `SCATTER_WINDOW`
    samples: VecDeque<(Instant, Vec3, f32)>,
}

impl TrackedMarker {
    fn clear_stats(&mut self) {
        self.changes.clear();
        self.exact_changes.clear();
        self.samples.clear();
    }

    fn prune_stats(&mut self) {
        while self
            .changes
            .front()
            .is_some_and(|t| t.elapsed() > STATS_WINDOW)
        {
            self.changes.pop_front();
        }
        while self
            .exact_changes
            .front()
            .is_some_and(|t| t.elapsed() > STATS_WINDOW)
        {
            self.exact_changes.pop_front();
        }
        while self
            .samples
            .front()
            .is_some_and(|(t, _, _)| t.elapsed() > SCATTER_WINDOW)
        {
            self.samples.pop_front();
        }
    }
}

// The head in the stage space
struct HeadSample {
    at: Instant,
    time: xr::Time,
    position: Vec3,
    orientation: Quat,
}

// One entity of a snapshot
struct EntityData {
    id: sys::SpatialEntityIdEXT,
    state: sys::SpatialEntityTrackingStateEXT,
    bounds: sys::SpatialBounded2DDataEXT,
    payload: Option<String>,
    // Buffer id of the payload, 0: none
    payload_buffer: u64,
}

enum ContextState {
    Creating {
        future: sys::FutureEXT,
        since: Instant,
    },
    RetryAt(Instant),
    Ready,
    Off,
}

#[derive(Default)]
struct SlowCall {
    last_log: Option<Instant>,
    // Since the last log
    count: u32,
    max: Duration,
}

pub struct SpatialMarkerTracker {
    fns: SpatialMarkerFns,
    session: xr::Session<xr::OpenGlEs>,
    space: xr::Space,
    // None: no VIEW space, the lines have no head part
    view_space: Option<xr::Space>,
    // Head poses in the stage space over `STATS_WINDOW`
    head: VecDeque<HeadSample>,
    // Features of the QR code capability that the runtime lists
    fixed_size_supported: bool,
    static_supported: bool,
    state: ContextState,
    context: sys::SpatialContextEXT,
    started: Instant,
    discovery: Option<(sys::FutureEXT, Instant)>,
    discovery_recommended: bool,
    recommended_events: u32,
    last_discovery: Option<Instant>,
    last_discovery_count: Option<usize>,
    discovery_count: u32,
    discovery_time: Duration,
    discovery_interval: Duration,
    update_interval: Option<Duration>,
    // An update snapshot failed for good: none from then on, the poses come from the discoveries
    updates_failed: bool,
    last_update_snapshot: Option<Instant>,
    // Update snapshots since the last summary, and their total time
    update_count: u32,
    update_time: Duration,
    markers: Vec<TrackedMarker>,
    // Payloads by buffer id: the same id always has the same content. Bounded by
    // `MAX_CACHED_PAYLOADS`
    payloads: HashMap<u64, String>,
    // Logged once: the runtime gave a known marker a new payload buffer id
    buffer_id_change_logged: bool,
    logged_errors: HashSet<(&'static str, i32)>,
    slow_calls: HashMap<&'static str, SlowCall>,
    last_summary: Instant,
    last_property_poll: Instant,
    // Poses before this time are in the old stage space
    ignore_before: Option<xr::Time>,
}

impl SpatialMarkerTracker {
    /// Lists the capabilities and starts to create the spatial context. None if the runtime can't
    /// track QR codes (logged)
    pub fn new(
        session: &xr::Session<xr::OpenGlEs>,
        system: xr::SystemId,
        fns: SpatialMarkerFns,
    ) -> Option<Self> {
        let space = match session
            .create_reference_space(xr::ReferenceSpaceType::STAGE, xr::Posef::IDENTITY)
        {
            Ok(space) => space,
            Err(e) => {
                log(&format!("off, no stage space: {e:?}"));
                return None;
            }
        };
        // Only for the log: without it the lines have no head part
        let view_space = session
            .create_reference_space(xr::ReferenceSpaceType::VIEW, xr::Posef::IDENTITY)
            .inspect_err(|e| log(&format!("no view space, lines without the head: {e:?}")))
            .ok();

        let mut this = Self {
            fns,
            session: session.clone(),
            space,
            view_space,
            head: VecDeque::new(),
            fixed_size_supported: false,
            static_supported: false,
            state: ContextState::Off,
            context: sys::SpatialContextEXT::NULL,
            started: Instant::now(),
            discovery: None,
            discovery_recommended: false,
            recommended_events: 0,
            last_discovery: None,
            last_discovery_count: None,
            discovery_count: 0,
            discovery_time: Duration::ZERO,
            discovery_interval: Duration::from_secs(2),
            update_interval: Some(Duration::from_millis(100)),
            updates_failed: false,
            last_update_snapshot: None,
            update_count: 0,
            update_time: Duration::ZERO,
            markers: vec![],
            payloads: HashMap::new(),
            buffer_id_change_logged: false,
            logged_errors: HashSet::new(),
            slow_calls: HashMap::new(),
            last_summary: Instant::now(),
            last_property_poll: Instant::now(),
            ignore_before: None,
        };
        this.poll_properties(true);

        if !this.check_capabilities(system) {
            return None;
        }
        this.start_context();

        (!matches!(this.state, ContextState::Off)).then_some(this)
    }

    /// The tracker gave up (logged): drop it
    pub fn is_off(&self) -> bool {
        matches!(self.state, ContextState::Off)
    }

    /// Known markers, in the order they were found
    pub fn markers(&self) -> impl Iterator<Item = &Marker> {
        self.markers.iter().map(|tracked| &tracked.marker)
    }

    /// `xr::Event::SpatialDiscoveryRecommendedEXT`
    pub fn on_discovery_recommended(&mut self, context: sys::SpatialContextEXT) {
        if context != sys::SpatialContextEXT::NULL && context == self.context {
            if self.recommended_events == 0 {
                log(&format!(
                    "first discovery-recommended event after {:.1} s",
                    self.started.elapsed().as_secs_f32()
                ));
            }
            self.recommended_events += 1;
            self.discovery_recommended = true;
        }
    }

    /// The stage space changes at `change_time`: the known poses are no longer valid
    pub fn on_reference_space_change(&mut self, change_time: xr::Time) {
        match self
            .session
            .create_reference_space(xr::ReferenceSpaceType::STAGE, xr::Posef::IDENTITY)
        {
            Ok(space) => self.space = space,
            Err(e) => self.log_error("xrCreateReferenceSpace", e),
        }
        self.ignore_before = Some(change_time);
        for tracked in &mut self.markers {
            tracked.marker.updated = None;
            tracked.clear_stats();
        }
        // In the old stage space
        self.head.clear();
        // Discover again, the poses come with the next snapshots
        self.discovery_recommended = true;
        log("stage space changed, poses dropped until the markers are seen again");
    }

    /// Call once per frame with the predicted display time
    pub fn update(&mut self, now: xr::Time) {
        self.poll_properties(false);

        match self.state {
            ContextState::Off => return,
            ContextState::RetryAt(time) => {
                if Instant::now() >= time {
                    self.start_context();
                }
                return;
            }
            ContextState::Creating { future, since } => {
                self.poll_context(future, since);
                if !matches!(self.state, ContextState::Ready) {
                    return;
                }
            }
            ContextState::Ready => (),
        }

        // Before the discovery, so that a `new` line has the head
        self.sample_head(now);
        self.update_discovery(now);
        if self.is_off() {
            return;
        }
        if !self.updates_failed
            && let Some(interval) = self.update_interval
            && self
                .last_update_snapshot
                .is_none_or(|time| time.elapsed() >= interval)
        {
            self.last_update_snapshot = Some(Instant::now());
            self.update_snapshot(now);
        }

        self.log_summary();
    }

    fn poll_properties(&mut self, force: bool) {
        if !force && self.last_property_poll.elapsed() < PROPERTY_POLL_INTERVAL {
            return;
        }
        self.last_property_poll = Instant::now();

        self.discovery_interval = Duration::from_secs(
            debug_property("debug.alvr.marker_discovery")
                .unwrap_or(2)
                .clamp(1, 60) as u64,
        );
        let rate = debug_property("debug.alvr.marker_rate")
            .unwrap_or(10)
            .clamp(0, 90);
        self.update_interval = (rate > 0).then(|| Duration::from_secs_f32(1.0 / rate as f32));
    }

    // Head pose for the log, at most every `HEAD_SAMPLE_INTERVAL`. A failure only drops the head
    // part of the lines
    fn sample_head(&mut self, now: xr::Time) {
        while self
            .head
            .front()
            .is_some_and(|sample| sample.at.elapsed() > STATS_WINDOW)
        {
            self.head.pop_front();
        }
        if self
            .head
            .back()
            .is_some_and(|sample| sample.at.elapsed() < HEAD_SAMPLE_INTERVAL)
        {
            return;
        }
        let Some(view) = &self.view_space else {
            return;
        };

        match view.locate(&self.space, now) {
            Ok(location)
                if location.location_flags.contains(
                    xr::SpaceLocationFlags::POSITION_VALID
                        | xr::SpaceLocationFlags::ORIENTATION_VALID,
                ) =>
            {
                self.head.push_back(HeadSample {
                    at: Instant::now(),
                    time: now,
                    position: crate::from_xr_vec3(location.pose.position),
                    orientation: crate::from_xr_quat(location.pose.orientation).normalize(),
                });
            }
            Ok(_) => (),
            Err(e) => self.log_error("xrLocateSpace (view)", e),
        }
    }

    // Distance from the head to the code, angle between the code's normal and the direction to the
    // head (0: seen head-on, 90: edge-on), fastest move and turn of the head over `STATS_WINDOW`.
    // Empty without a head pose
    fn head_text(&self, marker: &Marker) -> String {
        let Some(last) = self.head.back() else {
            return String::new();
        };
        if marker.updated.is_none() {
            return String::new();
        }
        let to_head = last.position - marker.position;
        let angle = (marker.orientation * Vec3::Z)
            .angle_between(to_head)
            .to_degrees();

        let mut speed = None::<(f32, f32)>;
        for (a, b) in self.head.iter().zip(self.head.iter().skip(1)) {
            let dt = (b.time.as_nanos() - a.time.as_nanos()) as f32 * 1e-9;
            if dt > 0.0 {
                let (moved, turned) = speed.unwrap_or_default();
                speed = Some((
                    moved.max((b.position - a.position).length() / dt),
                    turned.max(turn_deg(a.orientation, b.orientation) / dt),
                ));
            }
        }
        let speed = speed.map_or("-".into(), |(moved, turned)| {
            format!("{moved:.2} m/s and {turned:.0} deg/s")
        });

        format!(
            ", head {:.2} m away, {angle:.0} deg off the normal, head up to {speed} in {} s",
            to_head.length(),
            STATS_WINDOW.as_secs()
        )
    }

    // Logs once per call and code. Returns whether it is a success
    fn check(&mut self, call: &'static str, result: sys::Result) -> bool {
        if result.into_raw() >= 0 {
            if result != sys::Result::SUCCESS {
                self.log_error(call, result);
            }
            true
        } else {
            self.log_error(call, result);
            false
        }
    }

    fn log_error(&mut self, call: &'static str, result: sys::Result) {
        if self.logged_errors.insert((call, result.into_raw())) {
            log(&format!("{call}: {result:?} ({})", result.into_raw()));
        }
    }

    // Runs an OpenXR call and logs it if it was slow (at most every 2 s per call, with the count and
    // the longest time since the last line)
    fn timed<R>(&mut self, call: &'static str, f: impl FnOnce(&Self) -> R) -> (R, Duration) {
        let start = Instant::now();
        let result = f(&*self);
        let elapsed = start.elapsed();

        if elapsed > SLOW_CALL {
            let slow = self.slow_calls.entry(call).or_default();
            slow.count += 1;
            slow.max = slow.max.max(elapsed);
            if slow
                .last_log
                .is_none_or(|time| time.elapsed() > STATS_WINDOW)
            {
                log(&format!(
                    "{call} took {:.1} ms ({} calls over {:.0} ms since the last line, longest {:.1} ms)",
                    elapsed.as_secs_f32() * 1000.0,
                    slow.count,
                    SLOW_CALL.as_secs_f32() * 1000.0,
                    slow.max.as_secs_f32() * 1000.0
                ));
                slow.last_log = Some(Instant::now());
                slow.count = 0;
                slow.max = Duration::ZERO;
            }
        }

        (result, elapsed)
    }

    // Two-call idiom: the count, then the values
    fn enumerate<T: Copy>(
        &mut self,
        call: &'static str,
        fill: T,
        f: impl Fn(&Self, u32, *mut u32, *mut T) -> sys::Result,
    ) -> Option<Vec<T>> {
        let mut count = 0;
        let (result, _) = self.timed(call, |this| f(this, 0, &mut count, ptr::null_mut()));
        if !self.check(call, result) {
            return None;
        }
        let mut values = vec![fill; count as usize];
        if count > 0 {
            let (result, _) =
                self.timed(call, |this| f(this, count, &mut count, values.as_mut_ptr()));
            if !self.check(call, result) {
                return None;
            }
            values.truncate(count as usize);
        }

        Some(values)
    }

    // Whether QR codes can be tracked with the components that are used. Logs what the runtime lists
    fn check_capabilities(&mut self, system: xr::SystemId) -> bool {
        let instance = self.session.instance().as_raw();
        let Some(capabilities) = self.enumerate(
            "xrEnumerateSpatialCapabilitiesEXT",
            sys::SpatialCapabilityEXT::from_raw(0),
            |this, capacity, count, values| unsafe {
                (this.fns.spatial.enumerate_spatial_capabilities)(
                    instance, system, capacity, count, values,
                )
            },
        ) else {
            log("off, the capabilities can't be listed");
            return false;
        };

        let qr = sys::SpatialCapabilityEXT::MARKER_TRACKING_QR_CODE;
        if !capabilities.contains(&qr) {
            log(&format!(
                "off, the runtime lists no QR code tracking. Capabilities: {capabilities:?}"
            ));
            return false;
        }

        let components = {
            let call = "xrEnumerateSpatialCapabilityComponentTypesEXT";
            let mut list = sys::SpatialCapabilityComponentTypesEXT {
                ty: sys::SpatialCapabilityComponentTypesEXT::TYPE,
                next: ptr::null_mut(),
                component_type_capacity_input: 0,
                component_type_count_output: 0,
                component_types: ptr::null_mut(),
            };
            let (result, _) = self.timed(call, |this| unsafe {
                (this
                    .fns
                    .spatial
                    .enumerate_spatial_capability_component_types)(
                    instance, system, qr, &mut list
                )
            });
            let mut values = vec![
                sys::SpatialComponentTypeEXT::from_raw(0);
                list.component_type_count_output as usize
            ];
            let mut ok = self.check(call, result);
            if ok && !values.is_empty() {
                list.component_type_capacity_input = values.len() as u32;
                list.component_types = values.as_mut_ptr();
                let (result, _) = self.timed(call, |this| unsafe {
                    (this
                        .fns
                        .spatial
                        .enumerate_spatial_capability_component_types)(
                        instance, system, qr, &mut list,
                    )
                });
                ok = self.check(call, result);
                values.truncate(list.component_type_count_output as usize);
            }
            ok.then_some(values)
        };

        let features = self.enumerate(
            "xrEnumerateSpatialCapabilityFeaturesEXT",
            sys::SpatialCapabilityFeatureEXT::from_raw(0),
            |this, capacity, count, values| unsafe {
                (this.fns.spatial.enumerate_spatial_capability_features)(
                    instance, system, qr, capacity, count, values,
                )
            },
        );

        log(&format!(
            "capabilities {capabilities:?}; QR code: components {}, features {}",
            components
                .as_ref()
                .map_or("(failed)".into(), |c| format!("{c:?}")),
            features
                .as_ref()
                .map_or("(failed)".into(), |f| format!("{f:?}"))
        ));

        let Some(components) = components else {
            log("off, the components of QR codes can't be listed");
            return false;
        };
        if let Some(missing) = COMPONENTS.iter().find(|c| !components.contains(c)) {
            log(&format!("off, QR codes lack the component {missing:?}"));
            return false;
        }
        let features = features.unwrap_or_default();
        self.fixed_size_supported = features
            .contains(&sys::SpatialCapabilityFeatureEXT::MARKER_TRACKING_FIXED_SIZE_MARKERS);
        self.static_supported =
            features.contains(&sys::SpatialCapabilityFeatureEXT::MARKER_TRACKING_STATIC_MARKERS);

        true
    }

    fn start_context(&mut self) {
        let call = "xrCreateSpatialContextAsyncEXT";
        let static_markers =
            self.static_supported && debug_property("debug.alvr.marker_static").unwrap_or(0) != 0;
        // Millimetres, decimals allowed
        let marker_size = match debug_property_string("debug.alvr.marker_size") {
            Some(text) => match text.parse::<f32>() {
                Ok(mm) if mm.is_finite() => mm.clamp(0.0, 2000.0) / 1000.0,
                _ => {
                    log(&format!("marker_size {text:?} is not a number, ignored"));
                    0.0
                }
            },
            None => 0.0,
        };
        let fixed_size = self.fixed_size_supported && marker_size > 0.0;

        // Feature structs go on the capability configuration, and only for listed features
        let mut static_info = sys::SpatialMarkerStaticOptimizationEXT {
            ty: sys::SpatialMarkerStaticOptimizationEXT::TYPE,
            next: ptr::null(),
            optimize_for_static_marker: true.into(),
        };
        let mut size_info = sys::SpatialMarkerSizeEXT {
            ty: sys::SpatialMarkerSizeEXT::TYPE,
            next: ptr::null(),
            marker_side_length: marker_size,
        };
        let mut next: *const c_void = ptr::null();
        if static_markers {
            static_info.next = next;
            next = (&raw const static_info).cast();
        }
        if fixed_size {
            size_info.next = next;
            next = (&raw const size_info).cast();
        }
        let qr_config = sys::SpatialCapabilityConfigurationQrCodeEXT {
            ty: sys::SpatialCapabilityConfigurationQrCodeEXT::TYPE,
            next,
            capability: sys::SpatialCapabilityEXT::MARKER_TRACKING_QR_CODE,
            enabled_component_count: COMPONENTS.len() as u32,
            enabled_components: COMPONENTS.as_ptr(),
        };
        let configs: [*const sys::SpatialCapabilityConfigurationBaseHeaderEXT; 1] =
            [(&raw const qr_config).cast()];
        let info = sys::SpatialContextCreateInfoEXT {
            ty: sys::SpatialContextCreateInfoEXT::TYPE,
            next: ptr::null(),
            capability_config_count: configs.len() as u32,
            capability_configs: configs.as_ptr(),
        };

        let mut future = sys::FutureEXT::default();
        let (result, _) = self.timed(call, |this| unsafe {
            (this.fns.spatial.create_spatial_context_async)(
                this.session.as_raw(),
                &info,
                &mut future,
            )
        });
        if self.check(call, result) {
            log(&format!(
                "creating the spatial context (static markers: {static_markers}, fixed size: {})",
                if fixed_size {
                    format!("{:.1} mm", marker_size * 1000.0)
                } else {
                    "no".into()
                }
            ));
            self.state = ContextState::Creating {
                future,
                since: Instant::now(),
            };
        } else if result == sys::Result::ERROR_PERMISSION_INSUFFICIENT {
            // The spatial data permission may not be granted yet
            self.state = ContextState::RetryAt(Instant::now() + PERMISSION_RETRY);
        } else {
            log("off, the spatial context can't be created");
            self.state = ContextState::Off;
        }
    }

    // Ok(true): ready
    fn poll_future(&mut self, call: &'static str, future: sys::FutureEXT) -> Result<bool, ()> {
        let info = sys::FuturePollInfoEXT {
            ty: sys::FuturePollInfoEXT::TYPE,
            next: ptr::null(),
            future,
        };
        let mut poll = sys::FuturePollResultEXT {
            ty: sys::FuturePollResultEXT::TYPE,
            next: ptr::null_mut(),
            state: sys::FutureStateEXT::from_raw(0),
        };
        let (result, _) = self.timed(call, |this| unsafe {
            (this.fns.future.poll_future)(this.session.instance().as_raw(), &info, &mut poll)
        });
        if self.check(call, result) {
            Ok(poll.state == sys::FutureStateEXT::READY)
        } else {
            Err(())
        }
    }

    fn cancel_future(&mut self, future: sys::FutureEXT) {
        let info = sys::FutureCancelInfoEXT {
            ty: sys::FutureCancelInfoEXT::TYPE,
            next: ptr::null(),
            future,
        };
        let (result, _) = self.timed("xrCancelFutureEXT", |this| unsafe {
            (this.fns.future.cancel_future)(this.session.instance().as_raw(), &info)
        });
        self.check("xrCancelFutureEXT", result);
    }

    fn poll_context(&mut self, future: sys::FutureEXT, since: Instant) {
        let ready = match self.poll_future("xrPollFutureEXT (context)", future) {
            Ok(ready) => ready,
            Err(()) => {
                // The future may still be valid
                self.cancel_future(future);
                log("off, the creation of the spatial context can't be polled");
                self.state = ContextState::Off;
                return;
            }
        };
        if !ready {
            if since.elapsed() > FUTURE_TIMEOUT {
                self.cancel_future(future);
                log(&format!(
                    "off, the spatial context was not created within {} s",
                    FUTURE_TIMEOUT.as_secs()
                ));
                self.state = ContextState::Off;
            }
            return;
        }

        let call = "xrCreateSpatialContextCompleteEXT";
        let mut completion = sys::CreateSpatialContextCompletionEXT {
            ty: sys::CreateSpatialContextCompletionEXT::TYPE,
            next: ptr::null_mut(),
            future_result: sys::Result::SUCCESS,
            spatial_context: sys::SpatialContextEXT::NULL,
        };
        let (result, _) = self.timed(call, |this| unsafe {
            (this.fns.spatial.create_spatial_context_complete)(
                this.session.as_raw(),
                future,
                &mut completion,
            )
        });
        if !self.check(call, result) {
            // The future may still be valid
            self.cancel_future(future);
            log("off, the spatial context can't be completed");
            self.state = ContextState::Off;
            return;
        }
        if !self.check("spatial context creation", completion.future_result)
            || completion.spatial_context == sys::SpatialContextEXT::NULL
        {
            log("off, the runtime failed to create the spatial context");
            self.state = ContextState::Off;
            return;
        }

        self.context = completion.spatial_context;
        self.state = ContextState::Ready;
        // Discover right away
        self.discovery_recommended = true;
        log(&format!(
            "spatial context ready after {:.0} ms",
            since.elapsed().as_secs_f32() * 1000.0
        ));
    }

    // Starts a discovery snapshot when due, completes one that is ready
    fn update_discovery(&mut self, now: xr::Time) {
        if let Some((future, since)) = self.discovery {
            let ready = match self.poll_future("xrPollFutureEXT (discovery)", future) {
                Ok(ready) => ready,
                Err(()) => {
                    self.discovery = None;
                    // The future may still be valid
                    self.cancel_future(future);
                    return;
                }
            };
            if ready {
                self.discovery = None;
                self.complete_discovery(future, since, now);
            } else if since.elapsed() > FUTURE_TIMEOUT {
                self.discovery = None;
                self.cancel_future(future);
                log(&format!(
                    "discovery: no result within {} s, cancelled",
                    FUTURE_TIMEOUT.as_secs()
                ));
            }
            return;
        }

        // A recommended discovery waits for the gap and stays due until then
        let due = self.last_discovery.is_none_or(|time| {
            time.elapsed() >= self.discovery_interval
                || (self.discovery_recommended && time.elapsed() >= MIN_DISCOVERY_GAP)
        });
        if !due {
            return;
        }
        self.discovery_recommended = false;
        self.last_discovery = Some(Instant::now());

        let call = "xrCreateSpatialDiscoverySnapshotAsyncEXT";
        let info = sys::SpatialDiscoverySnapshotCreateInfoEXT {
            ty: sys::SpatialDiscoverySnapshotCreateInfoEXT::TYPE,
            next: ptr::null(),
            component_type_count: COMPONENTS.len() as u32,
            component_types: COMPONENTS.as_ptr(),
        };
        let mut future = sys::FutureEXT::default();
        let (result, _) = self.timed(call, |this| unsafe {
            (this.fns.spatial.create_spatial_discovery_snapshot_async)(
                this.context,
                &info,
                &mut future,
            )
        });
        if self.check(call, result) {
            self.discovery = Some((future, Instant::now()));
        } else if !is_transient(result) {
            log("off, discovery snapshots can't be created");
            self.turn_off();
        }
    }

    fn complete_discovery(&mut self, future: sys::FutureEXT, since: Instant, now: xr::Time) {
        let call = "xrCreateSpatialDiscoverySnapshotCompleteEXT";
        let info = sys::CreateSpatialDiscoverySnapshotCompletionInfoEXT {
            ty: sys::CreateSpatialDiscoverySnapshotCompletionInfoEXT::TYPE,
            next: ptr::null(),
            base_space: self.space.as_raw(),
            time: now,
            future,
        };
        let mut completion = sys::CreateSpatialDiscoverySnapshotCompletionEXT {
            ty: sys::CreateSpatialDiscoverySnapshotCompletionEXT::TYPE,
            next: ptr::null_mut(),
            future_result: sys::Result::SUCCESS,
            snapshot: sys::SpatialSnapshotEXT::NULL,
        };
        let (result, elapsed) = self.timed(call, |this| unsafe {
            (this.fns.spatial.create_spatial_discovery_snapshot_complete)(
                this.context,
                &info,
                &mut completion,
            )
        });
        if !self.check(call, result) {
            // The future may still be valid
            self.cancel_future(future);
            return;
        }
        if !self.check("discovery snapshot", completion.future_result)
            || completion.snapshot == sys::SpatialSnapshotEXT::NULL
        {
            return;
        }

        let entities = self.read_snapshot(completion.snapshot, true);
        self.destroy_snapshot(completion.snapshot);
        let Some(entities) = entities else {
            return;
        };

        self.discovery_count += 1;
        self.discovery_time = since.elapsed();
        if self.last_discovery_count != Some(entities.len()) {
            log(&format!(
                "discovery: {} markers ({} tracking), {:.0} ms after the request, completion {:.1} ms",
                entities.len(),
                entities
                    .iter()
                    .filter(|e| e.state == sys::SpatialEntityTrackingStateEXT::TRACKING)
                    .count(),
                self.discovery_time.as_secs_f32() * 1000.0,
                elapsed.as_secs_f32() * 1000.0
            ));
            self.last_discovery_count = Some(entities.len());
        }

        for data in entities {
            self.apply(&data, now, true);
        }
    }

    // Fresh poses of the known markers
    fn update_snapshot(&mut self, now: xr::Time) {
        let entities = self
            .markers
            .iter()
            .filter(|tracked| tracked.entity != sys::SpatialEntityEXT::NULL)
            .map(|tracked| tracked.entity)
            .collect::<Vec<_>>();
        if entities.is_empty() {
            return;
        }

        let call = "xrCreateSpatialUpdateSnapshotEXT";
        let info = sys::SpatialUpdateSnapshotCreateInfoEXT {
            ty: sys::SpatialUpdateSnapshotCreateInfoEXT::TYPE,
            next: ptr::null(),
            entity_count: entities.len() as u32,
            entities: entities.as_ptr(),
            component_type_count: COMPONENTS.len() as u32,
            component_types: COMPONENTS.as_ptr(),
            base_space: self.space.as_raw(),
            time: now,
        };
        let start = Instant::now();
        let mut snapshot = sys::SpatialSnapshotEXT::NULL;
        let (result, _) = self.timed(call, |this| unsafe {
            (this.fns.spatial.create_spatial_update_snapshot)(this.context, &info, &mut snapshot)
        });
        if !self.check(call, result) {
            // A lost session or context also fails the next discovery, which turns the tracker off
            if !is_transient(result) {
                self.updates_failed = true;
                log(
                    "update snapshots can't be created: none from now on, poses come from the \
                     discoveries only (debug.alvr.marker 0, then 1 tries again)",
                );
            }
            return;
        }
        if snapshot == sys::SpatialSnapshotEXT::NULL {
            return;
        }

        let data = self.read_snapshot(snapshot, false);
        self.destroy_snapshot(snapshot);
        self.update_count += 1;
        self.update_time += start.elapsed();

        for data in data.unwrap_or_default() {
            self.apply(&data, now, false);
        }
    }

    fn destroy_snapshot(&mut self, snapshot: sys::SpatialSnapshotEXT) {
        let (result, _) = self.timed("xrDestroySpatialSnapshotEXT", |this| unsafe {
            (this.fns.spatial.destroy_spatial_snapshot)(snapshot)
        });
        self.check("xrDestroySpatialSnapshotEXT", result);
    }

    // All entities of a snapshot with the marker and bounded 2D components, with their payloads
    // (`read_known_payloads` false: only of markers whose payload is not known yet)
    fn read_snapshot(
        &mut self,
        snapshot: sys::SpatialSnapshotEXT,
        read_known_payloads: bool,
    ) -> Option<Vec<EntityData>> {
        let call = "xrQuerySpatialComponentDataEXT";
        let condition = sys::SpatialComponentDataQueryConditionEXT {
            ty: sys::SpatialComponentDataQueryConditionEXT::TYPE,
            next: ptr::null(),
            component_type_count: COMPONENTS.len() as u32,
            component_types: COMPONENTS.as_ptr(),
        };
        let mut query = sys::SpatialComponentDataQueryResultEXT {
            ty: sys::SpatialComponentDataQueryResultEXT::TYPE,
            next: ptr::null_mut(),
            entity_id_capacity_input: 0,
            entity_id_count_output: 0,
            entity_ids: ptr::null_mut(),
            entity_state_capacity_input: 0,
            entity_state_count_output: 0,
            entity_states: ptr::null_mut(),
        };
        // First the count, without the lists (they must not be empty)
        let (result, _) = self.timed(call, |this| unsafe {
            (this.fns.spatial.query_spatial_component_data)(snapshot, &condition, &mut query)
        });
        if !self.check(call, result) {
            return None;
        }
        let count = query.entity_id_count_output as usize;
        if count == 0 {
            return Some(vec![]);
        }

        let mut ids = vec![sys::SpatialEntityIdEXT::default(); count];
        let mut states = vec![sys::SpatialEntityTrackingStateEXT::from_raw(0); count];
        let mut bounds = vec![sys::SpatialBounded2DDataEXT::default(); count];
        // No Default
        let mut markers: Vec<sys::SpatialMarkerDataEXT> =
            (0..count).map(|_| unsafe { std::mem::zeroed() }).collect();
        let mut marker_list = sys::SpatialComponentMarkerListEXT {
            ty: sys::SpatialComponentMarkerListEXT::TYPE,
            next: ptr::null_mut(),
            marker_count: count as u32,
            markers: markers.as_mut_ptr(),
        };
        let mut bounds_list = sys::SpatialComponentBounded2DListEXT {
            ty: sys::SpatialComponentBounded2DListEXT::TYPE,
            next: (&raw mut marker_list).cast(),
            bound_count: count as u32,
            bounds: bounds.as_mut_ptr(),
        };
        query.next = (&raw mut bounds_list).cast();
        query.entity_id_capacity_input = count as u32;
        query.entity_ids = ids.as_mut_ptr();
        query.entity_state_capacity_input = count as u32;
        query.entity_states = states.as_mut_ptr();
        let (result, _) = self.timed(call, |this| unsafe {
            (this.fns.spatial.query_spatial_component_data)(snapshot, &condition, &mut query)
        });
        if !self.check(call, result) {
            return None;
        }
        let count = count.min(query.entity_id_count_output as usize);

        let mut entities = Vec::with_capacity(count);
        for i in 0..count {
            // The runtime writes the data of tracked entities only
            let tracking = states[i] == sys::SpatialEntityTrackingStateEXT::TRACKING;
            let buffer = markers[i].data;
            let buffer_id = if tracking {
                buffer.buffer_id.into_raw()
            } else {
                0
            };
            let known = self
                .markers
                .iter()
                .find(|tracked| tracked.marker.id == ids[i].into_raw())
                .filter(|tracked| tracked.marker.payload.is_some());
            if let Some(tracked) = known
                && buffer_id != 0
                && tracked.payload_buffer != 0
                && buffer_id != tracked.payload_buffer
                && !self.buffer_id_change_logged
            {
                self.buffer_id_change_logged = true;
                log(&format!(
                    "the payload buffer id of #{} changed from {} to {} (logged once): the runtime \
                     doesn't reuse buffer ids, update snapshots don't read known payloads again",
                    tracked.marker.id, tracked.payload_buffer, buffer_id
                ));
            }
            // An update snapshot doesn't read the payload of a known marker again (each new buffer
            // id costs two calls); discoveries do and see a changed payload
            let payload = if buffer_id == 0 || (!read_known_payloads && known.is_some()) {
                None
            } else {
                self.read_payload(snapshot, buffer)
            };
            entities.push(EntityData {
                id: ids[i],
                state: states[i],
                bounds: bounds[i],
                payload_buffer: if payload.is_some() { buffer_id } else { 0 },
                payload,
            });
        }

        Some(entities)
    }

    // The content of a code, as text. None: not decoded yet, or failed (logged)
    fn read_payload(
        &mut self,
        snapshot: sys::SpatialSnapshotEXT,
        buffer: sys::SpatialBufferEXT,
    ) -> Option<String> {
        let id = buffer.buffer_id.into_raw();
        if id == 0 {
            return None;
        }
        if let Some(payload) = self.payloads.get(&id) {
            return Some(payload.clone());
        }

        let info = sys::SpatialBufferGetInfoEXT {
            ty: sys::SpatialBufferGetInfoEXT::TYPE,
            next: ptr::null(),
            buffer_id: buffer.buffer_id,
        };
        let bytes = match buffer.buffer_type {
            sys::SpatialBufferTypeEXT::STRING => {
                let mut bytes = self.enumerate(
                    "xrGetSpatialBufferStringEXT",
                    0 as std::ffi::c_char,
                    |this, capacity, count, values| unsafe {
                        (this.fns.spatial.get_spatial_buffer_string)(
                            snapshot, &info, capacity, count, values,
                        )
                    },
                )?;
                // Up to the terminating NUL
                if let Some(end) = bytes.iter().position(|c| *c == 0) {
                    bytes.truncate(end);
                }
                // c_char is i8 or u8 depending on the target
                bytes
                    .into_iter()
                    .map(|c| c.to_ne_bytes()[0])
                    .collect::<Vec<_>>()
            }
            sys::SpatialBufferTypeEXT::UINT8 => self.enumerate(
                "xrGetSpatialBufferUint8EXT",
                0_u8,
                |this, capacity, count, values| unsafe {
                    (this.fns.spatial.get_spatial_buffer_uint8)(
                        snapshot, &info, capacity, count, values,
                    )
                },
            )?,
            other => {
                if self
                    .logged_errors
                    .insert(("payload buffer type", other.into_raw()))
                {
                    log(&format!("payload of unexpected buffer type {other:?}"));
                }
                return None;
            }
        };

        let payload = String::from_utf8_lossy(&bytes).into_owned();
        if self.payloads.len() >= MAX_CACHED_PAYLOADS {
            self.payloads.clear();
        }
        self.payloads.insert(id, payload.clone());

        Some(payload)
    }

    // Applies the data of a snapshot to the known markers. Only a discovery adds markers
    fn apply(&mut self, data: &EntityData, now: xr::Time, from_discovery: bool) {
        let id = data.id.into_raw();
        let state = MarkerState::from_xr(data.state);
        if ![
            sys::SpatialEntityTrackingStateEXT::TRACKING,
            sys::SpatialEntityTrackingStateEXT::PAUSED,
            sys::SpatialEntityTrackingStateEXT::STOPPED,
        ]
        .contains(&data.state)
            && self
                .logged_errors
                .insert(("unknown tracking state", data.state.into_raw()))
        {
            log(&format!(
                "unknown tracking state {} of #{id} (logged once, taken as paused)",
                data.state.into_raw()
            ));
        }
        let index = match self.markers.iter().position(|t| t.marker.id == id) {
            Some(index) => index,
            None if from_discovery && state != MarkerState::Stopped => {
                let entity = self.create_entity(data.id);
                self.markers.push(TrackedMarker {
                    marker: Marker {
                        id,
                        payload: None,
                        position: Vec3::ZERO,
                        orientation: Quat::IDENTITY,
                        extents: Vec2::ZERO,
                        state,
                        updated: None,
                    },
                    entity,
                    payload_buffer: 0,
                    changes: VecDeque::new(),
                    exact_changes: VecDeque::new(),
                    samples: VecDeque::new(),
                });
                let index = self.markers.len() - 1;
                self.update_marker(index, data, now);
                let marker = &self.markers[index].marker;
                log(&format!(
                    "new {}{}",
                    describe(marker),
                    self.head_text(marker)
                ));
                return;
            }
            None => return,
        };

        let old_state = self.markers[index].marker.state;
        let old_payload = self.markers[index].marker.payload.clone();
        self.update_marker(index, data, now);
        let marker = &self.markers[index].marker;
        if state != old_state {
            log(&format!(
                "{} -> {}: {}{}",
                old_state.name(),
                state.name(),
                describe(marker),
                self.head_text(marker)
            ));
        } else if old_payload.is_some() && marker.payload != old_payload {
            log(&format!(
                "payload changed from {}: {}",
                payload_text(old_payload.as_deref()),
                describe(marker)
            ));
        }

        if state == MarkerState::Stopped {
            log(&format!("lost #{id}"));
            let tracked = self.markers.remove(index);
            self.destroy_entity(tracked.entity);
        }
    }

    fn update_marker(&mut self, index: usize, data: &EntityData, now: xr::Time) {
        let ignore_pose = self
            .ignore_before
            .is_some_and(|time| now.as_nanos() < time.as_nanos());
        let tracked = &mut self.markers[index];
        tracked.marker.state = MarkerState::from_xr(data.state);
        if tracked.marker.state != MarkerState::Tracking {
            return;
        }
        if data.payload.is_some() {
            tracked.marker.payload.clone_from(&data.payload);
            tracked.payload_buffer = data.payload_buffer;
        }
        if ignore_pose {
            return;
        }

        let position = crate::from_xr_vec3(data.bounds.center.position);
        // A zero quaternion gives NaN
        let orientation = crate::from_xr_quat(data.bounds.center.orientation).normalize();
        if !position.is_finite() || !orientation.is_finite() {
            if self.logged_errors.insert(("invalid marker pose", 0)) {
                let center = data.bounds.center;
                log(&format!(
                    "invalid pose from the runtime for #{} (logged once, dropped): center ({}, {}, \
                     {}), orientation ({}, {}, {}, {}), extents {} x {}",
                    data.id.into_raw(),
                    center.position.x,
                    center.position.y,
                    center.position.z,
                    center.orientation.x,
                    center.orientation.y,
                    center.orientation.z,
                    center.orientation.w,
                    data.bounds.extents.width,
                    data.bounds.extents.height
                ));
            }
            return;
        }

        let time = Instant::now();
        let first = tracked.marker.updated.is_none();
        let moved = first
            || position.distance(tracked.marker.position) > MIN_MOVE
            || turn_deg(tracked.marker.orientation, orientation) > MIN_TURN_DEG;
        if moved {
            let yaw =
                alvr_graphics::heading_yaw(alvr_graphics::marker_heading(orientation).direction);
            tracked.changes.push_back(time);
            tracked.samples.push_back((time, position, yaw));
        }
        if first || position != tracked.marker.position || orientation != tracked.marker.orientation
        {
            tracked.exact_changes.push_back(time);
        }
        tracked.marker.position = position;
        tracked.marker.orientation = orientation;
        tracked.marker.extents = Vec2::new(data.bounds.extents.width, data.bounds.extents.height);
        tracked.marker.updated = Some(time);
        tracked.prune_stats();
    }

    fn create_entity(&mut self, id: sys::SpatialEntityIdEXT) -> sys::SpatialEntityEXT {
        let call = "xrCreateSpatialEntityFromIdEXT";
        let info = sys::SpatialEntityFromIdCreateInfoEXT {
            ty: sys::SpatialEntityFromIdCreateInfoEXT::TYPE,
            next: ptr::null(),
            entity_id: id,
        };
        let mut entity = sys::SpatialEntityEXT::NULL;
        let (result, _) = self.timed(call, |this| unsafe {
            (this.fns.spatial.create_spatial_entity_from_id)(this.context, &info, &mut entity)
        });
        if self.check(call, result) {
            entity
        } else {
            sys::SpatialEntityEXT::NULL
        }
    }

    fn destroy_entity(&mut self, entity: sys::SpatialEntityEXT) {
        if entity != sys::SpatialEntityEXT::NULL {
            let (result, _) = self.timed("xrDestroySpatialEntityEXT", |this| unsafe {
                (this.fns.spatial.destroy_spatial_entity)(entity)
            });
            self.check("xrDestroySpatialEntityEXT", result);
        }
    }

    // One line per marker every 2 s while markers are known, a short one every 10 s while none is
    fn log_summary(&mut self) {
        let interval = if self.markers.is_empty() {
            IDLE_SUMMARY_INTERVAL
        } else {
            SUMMARY_INTERVAL
        };
        let elapsed = self.last_summary.elapsed();
        if elapsed < interval {
            return;
        }
        self.last_summary = Instant::now();

        let updates = if self.updates_failed {
            "failed (poses from the discoveries)".into()
        } else if self.update_count > 0 {
            format!(
                "{:.1}/s, {:.2} ms each",
                self.update_count as f32 / elapsed.as_secs_f32(),
                self.update_time.as_secs_f32() * 1000.0 / self.update_count as f32
            )
        } else {
            "none".into()
        };
        self.update_count = 0;
        self.update_time = Duration::ZERO;
        log(&format!(
            "{} known; discoveries {} (last took {:.0} ms, {} events); update snapshots {updates}",
            self.markers.len(),
            self.discovery_count,
            self.discovery_time.as_secs_f32() * 1000.0,
            self.recommended_events,
        ));

        // A marker that is not tracked gets no snapshot data that would prune its windows
        for tracked in &mut self.markers {
            tracked.prune_stats();
        }
        for tracked in &self.markers {
            let marker = &tracked.marker;
            let Some(updated) = marker.updated else {
                log(&format!("  {}, no pose yet", describe(marker)));
                continue;
            };
            let interval = match (tracked.changes.front(), tracked.changes.back()) {
                (Some(first), Some(last)) if tracked.changes.len() > 1 => format!(
                    "{:.0} ms",
                    last.duration_since(*first).as_secs_f32() * 1000.0
                        / (tracked.changes.len() - 1) as f32
                ),
                _ => "-".into(),
            };
            // Over the distinct poses, not the repeated snapshots
            let scatter = alvr_graphics::scatter(tracked.samples.iter().map(|(_, p, _)| *p))
                .map_or("- (no distinct pose)".into(), |(deviation, count)| {
                    let mm = deviation * 1000.0;
                    format!(
                        "{:.1}/{:.1}/{:.1} mm over {count} distinct poses",
                        mm.x, mm.y, mm.z
                    )
                });
            let heading_scatter = alvr_graphics::yaw_scatter(
                tracked.samples.iter().map(|(_, _, yaw)| *yaw),
            )
            .map_or("-".into(), |(deviation, range)| {
                format!(
                    "{:.2} deg (range {:.2})",
                    deviation.to_degrees(),
                    range.to_degrees()
                )
            });
            log(&format!(
                "  {}{}, pose changes every {interval} ({} > {:.1} mm / {MIN_TURN_DEG} deg, {} \
                 bit-exact in {} s), scatter x/y/z {scatter} in {} s, heading scatter \
                 {heading_scatter}, seen {:.1} s ago",
                describe(marker),
                self.head_text(marker),
                tracked.changes.len(),
                MIN_MOVE * 1000.0,
                tracked.exact_changes.len(),
                STATS_WINDOW.as_secs(),
                SCATTER_WINDOW.as_secs(),
                updated.elapsed().as_secs_f32()
            ));
        }
    }

    // Destroys the entities and the context, cancels the futures. The tracker is off after this
    fn release(&mut self) {
        if let ContextState::Creating { future, .. } = self.state {
            self.cancel_future(future);
        }
        self.state = ContextState::Off;
        if let Some((future, _)) = self.discovery.take() {
            self.cancel_future(future);
        }
        for tracked in std::mem::take(&mut self.markers) {
            self.destroy_entity(tracked.entity);
        }
        if self.context != sys::SpatialContextEXT::NULL {
            let context = self.context;
            let (result, _) = self.timed("xrDestroySpatialContextEXT", |this| unsafe {
                (this.fns.spatial.destroy_spatial_context)(context)
            });
            self.check("xrDestroySpatialContextEXT", result);
            self.context = sys::SpatialContextEXT::NULL;
        }
    }

    fn turn_off(&mut self) {
        self.release();
    }
}

impl Drop for SpatialMarkerTracker {
    fn drop(&mut self) {
        self.release();
    }
}

// Errors after which the next attempt may work
fn is_transient(result: sys::Result) -> bool {
    matches!(
        result,
        sys::Result::ERROR_LIMIT_REACHED | sys::Result::ERROR_TIME_INVALID
    )
}

// Angle of the rotation from a to b (degrees). Unlike `Quat::angle_between` (acos) it resolves
// hundredths of a degree
fn turn_deg(a: Quat, b: Quat) -> f32 {
    let delta = a.conjugate() * b;
    (2.0 * delta.xyz().length().atan2(delta.w.abs())).to_degrees()
}

fn payload_text(payload: Option<&str>) -> String {
    match payload {
        Some(payload) if payload.chars().count() > MAX_LOGGED_PAYLOAD => format!(
            "{:?}...",
            payload.chars().take(MAX_LOGGED_PAYLOAD).collect::<String>()
        ),
        Some(payload) => format!("{payload:?}"),
        None => "(no payload yet)".into(),
    }
}

/// Payload, id, state, position, angle of the normal from vertical, heading (yaw of the direction
/// the box takes from the code), size
pub fn describe(marker: &Marker) -> String {
    let pose = if marker.updated.is_some() {
        let heading = alvr_graphics::marker_heading(marker.orientation);
        format!(
            " at ({:.4}, {:.4}, {:.4}) m, normal {:.1} deg from vertical ({}), heading {:.2} deg, \
             {:.1} x {:.1} mm",
            marker.position.x,
            marker.position.y,
            marker.position.z,
            heading.normal_from_vertical_deg,
            if heading.flat { "lying" } else { "standing" },
            alvr_graphics::yaw_log_deg(alvr_graphics::heading_yaw(heading.direction)),
            marker.extents.x * 1000.0,
            marker.extents.y * 1000.0
        )
    } else {
        String::new()
    };

    format!(
        "{} #{} {}{pose}",
        payload_text(marker.payload.as_deref()),
        marker.id,
        marker.state.name()
    )
}
