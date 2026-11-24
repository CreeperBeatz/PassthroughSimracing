//! The play area editor: the boxes of the play area and the domes of their sides resized, moved, added
//! and removed with the hands in the headset, and the layout file of each code. Only with a marker box
//! (`debug.alvr.marker_box 1`) and Passthrough = Depth. The handles, the picking and the drags are in
//! alvr_graphics (area_edit.rs).
//!
//! Modes: hidden (racing: only the two-hand gesture is watched), shown (the outline for 3 s after
//! editing ended) and editing (the game dimmed a little and the room shown through it outside the play
//! area, the handles drawn, the hands pick and drag them, the mask follows at once; the pose of the
//! code is frozen while editing; the hands are not sent to the PC, see
//! `StreamContext::withhold_hands`).
//! While editing: a pinch on a handle of the selected box takes it; on a handle of another box it only
//! selects that box (the next pinch edits). A handle out of reach (8 cm) is picked with the ray of the
//! hand (the aim pose of XR_FB_hand_tracking_aim, else from an estimated shoulder), drawn as a thin line
//! with a dot on the handle it meets, and dragged along the ray (`FarDrag`). The user: "pinch to resize
//! is always with one hand. adding a new box or exiting edit mode is always with 2" (`PinchRoles`): a
//! pinch works handles only while the other hand is open; both hands pinched are the two-hand pinch or
//! nothing. The two-hand pinch pulled more than 20 cm apart: a new box spanning the two pinch points (a
//! ghost while the hands move, made when a hand lets go; `TwoHandPinch`); within 20 cm it may still be
//! the gesture that ends editing, whose timer stops for good in that pinch once the points were farther
//! apart.
//! The two-hand gesture starts and ends editing: both hands pinched (thumb and index tips), both palms
//! turned away from the eyes, the two pinches within 20 cm of each other, 15 to 65 cm from the eyes and
//! within 45 degrees of the view direction, held 1.5 s (the outline brightens and a square closes
//! between the hands). Hands on a wheel don't pinch, are about 30 cm apart and turned towards the rim.
//! Editing also ends with the done handle (a check mark above the code), after 120 s without a grab,
//! and when the play area goes (marker box off, another code). Without a stream with Passthrough =
//! Depth (the stream stopped and restarts, the headset was taken off) editing pauses: a grab in
//! progress is released where it was (kept), and editing goes on when the stream is back within
//! `area_edit_grace` seconds, else it ends. Ending saves if something was changed (entering and leaving
//! without a change writes no file); a grab in progress is kept.
//! While editing the hands are not limited to the play area (the handles are on and outside it).
//! A hand is used only while both its tips are tracked and its cursor is within 50 degrees of the view
//! direction (the runtime reports hands as tracked also out of view); the hand that grabs (and both
//! hands while they pull a new box open) within 75 degrees, and its pinch opens only after it was not
//! usable for 1 s (else 0.3 s). The hold of "x" removes only if its hand was usable the whole time
//! (else: a new pinch). When a hand pinches while the other is pinched: the other pinch free: the
//! two-hand pinch; the other pinch on a handle (near or by the ray) younger than 0.5 s, its grab not out
//! of its dead zone nor done: it gives way and both are the two-hand pinch; else (one hand resizing) the
//! new pinch does nothing until it opens. While editing the gesture that ends it is a two-hand pinch.
//!
//! Experimental, controlled with adb (polled about twice a second):
//! - `debug.alvr.area_edit`: a change to 1 starts editing, a change to 0 ends it (the value at the
//!   start of the app is not a request: set 0, then 1)
//! - `debug.alvr.area_pinch_on` / `debug.alvr.area_pinch_off <mm>`: gap between the surfaces of the
//!   thumb and index tips (distance of the joints minus their radii) below which a pinch closes /
//!   above which it opens (0 / 5; on -30 to 100, off from on to 105: negative is the tips pressed into
//!   each other, which measured -2 to -16 mm at contact on the Quest)
//! - `debug.alvr.area_edit_dim <percent>`: how much the game is dimmed while editing (30, 0 to 90; 60
//!   until the room showed through the game)
//! - `debug.alvr.area_edit_see <percent>`: how much the real room shows through the game outside the
//!   play area while editing (45, 0 to 100; 0: the game opaque there, as before)
//! - `debug.alvr.area_edit_grace <s>`: how long editing waits for the stream to come back (8, 0 to 60)
//! - `debug.alvr.area_edit_hands_pc 1`: the hands are sent to the PC also while editing (unset or 0:
//!   not sent while editing)
//! - `debug.alvr.area_file 0`: layout files are neither read nor written, edits last until the app
//!   stops (unset or another value: they are)
//!
//! The layout file: one per code, named from its payload (`area_file_name`), in
//! /sdcard/Android/data/alvr.client.dev/files/areas/ and in /data/local/tmp/alvr-areas/. The app storage
//! is wiped by the uninstall that every CI build needs; the second folder survives it. The app can most
//! likely only read there (SELinux keeps apps from writing to /data/local/tmp; a failed write is
//! logged once per run): tools/install_apk.sh copies the files of the first folder there with adb
//! before it uninstalls. Read once the payload is known (`debug.alvr.marker_id`, else the code seen
//! first; before any code is seen, without `marker_id`: the payload of the newest layout file): the
//! newer of the two files that reads. Written to both (by a thread of its own) on each released grab
//! that changed the layout, on a new box, remove and undo, when editing ends after a change, and when the
//! code gets measured while a layout file is used (to keep its anchor). A layout file replaces the box
//! of the `marker_box_*` properties (and `area_test`); without one the box of the properties is the
//! layout, as before. The file holds the pose of the code measured last (stage space): until the code
//! is seen in this run the area is placed from it (kept over STAGE changes, or carried by their pose
//! with `debug.alvr.marker_carry 1`) and drawn dashed.

use crate::{
    marker_region::MarkerRegion,
    passthrough::{self, debug_property},
};
use alvr_common::glam::{Mat4, Vec3, Vec4};
use alvr_graphics::{
    AreaBox, EditorHand, EditorInput, FarDrag, HandRay, Handle, HandleId, HandleKind, HoldTimer,
    OVERLAY_AMBER, OVERLAY_CYAN, OVERLAY_DIM, OVERLAY_GREEN, OVERLAY_RED, OVERLAY_WHITE,
    OverlaySegment, Pick, Pinch, PinchAction, PinchEvent, PinchRole, PinchRoles, PinchStart,
    PlacedArea, PlayArea, SavedAnchor, StageEvent, TwoHand, TwoHandPinch, UndoStack,
};
use openxr as xr;
use std::{
    collections::VecDeque,
    fs, io,
    sync::mpsc,
    thread,
    time::{Duration, Instant, SystemTime},
};

const PROPERTY_POLL_INTERVAL: Duration = Duration::from_millis(500);
// The outline stays this long after editing ended
const SHOWN_AFTER_EDIT: Duration = Duration::from_secs(3);
// Editing ends after this long without a grab (seconds)
const IDLE_TIMEOUT_S: f64 = 120.0;
// A frame without the eyes uses the last view this long, with the hands not seen (seconds): longer
// than the hand that grabs keeps its pinch, so that a grab ends as when the hand is lost
const VIEW_HOLD_S: f64 = 1.5;
// See `DEFAULT_PINCH_ON` of area_edit.rs
const DEFAULT_PINCH_ON_MM: i64 = 0;
const DEFAULT_PINCH_OFF_MM: i64 = 5;
// With the room at 45 % through the game, a dim of 30 % leaves the game at 0.55 x 0.7 = 0.39 of its
// brightness, about the 0.4 of the dim of 60 % the handles read well over in the first two headset runs
const DEFAULT_DIM_PERCENT: i64 = 30;
// How much the room shows through the game outside the play area while editing (the user asked for
// 40-50 % after the second headset run)
const DEFAULT_SEE_PERCENT: i64 = 45;
// How long editing waits for the stream to come back (seconds): on 2026-11-24 the stream stopped and
// came back within 2.9-3.4 s four times without the session pausing, and 7-13 s with the headset
// taken off for 5-10 s
const DEFAULT_GRACE_S: i64 = 8;
// STAGE changes kept with marker_carry 0 are logged at most this often (they come in pairs every
// minute or two)
const STAGE_LOG_INTERVAL_S: f64 = 60.0;
// While the fingers close (gap below `on` plus this) the lit handle stays (meters)
const HOVER_LOCK_MARGIN: f32 = 0.01;
// The gaps of the pinches are logged this often while editing (seconds)
const GAP_LOG_INTERVAL_S: f64 = 5.0;
// The two-hand gesture is logged at most this often
const GESTURE_LOG_INTERVAL: Duration = Duration::from_secs(2);
// Grabbed and lit handles are drawn this much larger
const LIT_SCALE: f32 = 1.3;
// Half the length of the guide line of a side or dome drag (meters)
const GUIDE_HALF_LENGTH: f32 = 0.4;
// Half the size of the square that closes during the gesture and around a held remove handle (meters)
const PROGRESS_HALF: f32 = 0.03;
// The outline while editing: the selected box is cyan over it
const EDIT_OUTLINE: Vec4 = Vec4::new(0.7, 0.7, 0.7, 1.0);
// A ray is drawn this much thinner than the outline
const RAY_WIDTH: f32 = 0.5;

const HAND_NAMES: [&str; 2] = ["left", "right"];

// The two folders of the layout files
fn area_dirs() -> [String; 2] {
    [
        format!("{}/areas", passthrough::FILES_DIR),
        "/data/local/tmp/alvr-areas".into(),
    ]
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Hidden,
    Shown,
    Editing,
}

// What the area is placed from
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Placement {
    None,
    // The code seen in this run
    Code,
    // The anchor saved in the layout file
    SavedAnchor,
}

struct Grab {
    hand: usize,
    id: HandleId,
    start_cursor: Vec3,
    start_layout: PlayArea,
    start_time: f64,
    // Displacements of the cursor since the pinch (stage space) and the angle swept around the
    // vertical axis of the box (turning, radians), with their times, newest last
    history: VecDeque<(f64, Vec3, f32)>,
    // Largest displacement so far (meters)
    max_move: f32,
    // Turning: the angle swept so far and the cursor it was last measured at (area frame)
    swept: f32,
    turn_from: Vec3,
    // The yaw is on a detent
    snapped: bool,
    // The remove handle did its work
    removed: bool,
    // Why the hand was last not usable during the grab, for the log
    lost_reason: Option<String>,
    // Taken by the ray (out of reach); a drag then follows the ray, see `FarDrag`
    by_ray: bool,
    far: Option<FarDrag>,
    // The dead zone and the ramp of the drag are this many times as long (1: near)
    gain: f32,
}

impl Grab {
    // The displacement and the angle to keep on release: before the fingers opened (see
    // `RELEASE_LOOKBACK_S`), the last ones when the hand was lost
    fn release_values(&self, now: f64, lost: bool) -> (Vec3, f32) {
        let last = self
            .history
            .back()
            .map_or((Vec3::ZERO, 0.0), |(_, d, a)| (*d, *a));
        if lost {
            return last;
        }
        let before = now - alvr_graphics::RELEASE_LOOKBACK_S;
        self.history
            .iter()
            .rev()
            .find(|(time, _, _)| *time <= before)
            .or(self.history.front())
            .map_or(last, |(_, d, a)| (*d, *a))
    }

    // A turn follows the cursor (area frame) of this frame around the vertical axis through `center`.
    // Per frame, so that more than half a turn adds up. A step with the cursor within
    // `TURN_MIN_RADIUS` of the axis at either end adds nothing (the angle is not steady there; a cursor
    // that crossed the axis would flip the box), and nothing adds up within the dead zone of a drag
    // (closing the fingers moves the cursor): the turn starts where the cursor leaves it. Not
    // `continuous`: the point of a far turn was found another way than before, it starts there (no jump)
    fn follow_turn(&mut self, center: Vec3, cursor: Vec3, continuous: bool) {
        let step = alvr_graphics::sweep_angle(center, self.turn_from, cursor)
            .filter(|_| continuous)
            .unwrap_or(0.0);
        self.turn_from = cursor;
        if self.max_move > alvr_graphics::DRAG_DEAD_ZONE {
            self.swept += step;
        }
    }
}

// What a drag or a turn gives for a displacement of the cursor (stage space; of a far drag what the ray
// gives) and an angle swept: the layout, the value for the log and whether a turn is on a detent
fn grab_result(grab: &Grab, to_area: &Mat4, moved: Vec3, swept: f32) -> (PlayArea, f32, bool) {
    if grab.id.kind == HandleKind::Turn {
        return alvr_graphics::apply_turn(&grab.start_layout, grab.id.box_index, swept);
    }
    let displacement =
        to_area.transform_vector3(alvr_graphics::scaled_drag_displacement(moved, grab.gain));
    let (layout, value) = alvr_graphics::apply_drag(&grab.start_layout, grab.id, displacement);
    (layout, value, false)
}

// A new box being pulled open with both hands: the two pinch points (area frame, left first) with their
// times, newest last
struct NewBox {
    corners: VecDeque<(f64, Vec3, Vec3)>,
}

impl NewBox {
    // The corners to keep when a hand lets go: before the fingers opened (see `RELEASE_LOOKBACK_S`),
    // the last ones when a hand was lost
    fn release_corners(&self, now: f64, lost: bool) -> Option<(Vec3, Vec3)> {
        let last = self.corners.back().map(|&(_, a, b)| (a, b));
        if lost {
            return last;
        }
        let before = now - alvr_graphics::RELEASE_LOOKBACK_S;
        self.corners
            .iter()
            .rev()
            .find(|(time, _, _)| *time <= before)
            .or(self.corners.front())
            .map(|&(_, a, b)| (a, b))
    }
}

// What is drawn in this frame
struct DrawFrame {
    placed: PlacedArea,
    eye: Vec3,
    // While editing
    handles: Vec<Handle>,
    selected: usize,
    // Cursors of the usable hands, and whether they pinch
    cursors: Vec<(Vec3, bool)>,
    // The handle each hand lights, and the one grabbed
    lit: Vec<HandleId>,
    grabbed: Option<HandleId>,
    // Of the gesture: between the hands, how far (0..1)
    gesture: Option<(Vec3, f32)>,
    // Of a held remove handle: where, how far
    remove: Option<(Vec3, f32)>,
    // Of a turn: the box at the grab and now (area frame), on a detent
    turn: Option<(AreaBox, AreaBox, bool)>,
    // The rays of the hands that are not near a handle, where they end on a handle, and their color
    rays: Vec<(HandRay, Option<Vec3>, Vec4)>,
    // A new box being pulled open (area frame) and its color
    new_box: Option<(AreaBox, Vec4)>,
}

struct SaveJob {
    name: String,
    text: String,
    // For the log
    summary: String,
}

pub struct AreaEditor {
    start: Instant,
    last_poll: Option<Instant>,
    // `debug.alvr.area_edit` as last read, and a change of it not handled yet (true: start)
    edit_property: Option<i64>,
    edit_request: Option<bool>,
    pinch_on: f32,
    pinch_off: f32,
    dim: f32,
    // `area_edit_see` (0..1)
    see: f32,
    // `area_edit_grace` (seconds)
    grace: f64,
    // `area_edit_hands_pc` is not 1
    withhold_hands: bool,
    files: bool,
    mode: Mode,
    shown_until: Option<Instant>,
    // The layout of the file or of the edits. None: the one of the marker region (the properties)
    layout: Option<PlayArea>,
    // Editing: the layout before it (None: the box of the properties), and whether it was changed
    entered_with: Option<PlayArea>,
    edited: bool,
    // From the file (or saved since): the pose of the code measured last
    saved_anchor: Option<SavedAnchor>,
    // The payload whose file was read (Some(None): a code without payload). None: not yet
    loaded_for: Option<Option<String>>,
    // The payload of the newest layout file was looked for (once, before any code is seen)
    newest_tried: bool,
    // Changes of the STAGE space at their change time, in order: the saved anchor and the frozen pose
    // are carried into the new space then, or kept (`debug.alvr.marker_carry 0`), in the same frame
    // as the marker region handles its pose
    stage_changes: Vec<(xr::Time, StageEvent)>,
    // The eyes and the view direction of the last input, and when (seconds)
    last_view: Option<(Vec3, Vec3, f64)>,
    placement: Placement,
    measured: bool,
    // The pose of the code while it is measured in this run
    measured_anchor: Option<SavedAnchor>,
    // Editing: the pose of the code (frozen) and what it came from
    frozen: Option<(SavedAnchor, Placement)>,
    selected: usize,
    undo: UndoStack,
    pinches: [Pinch; 2],
    // While editing: what the closed pinch of each hand is for (free, on a handle, idle)
    roles: PinchRoles,
    hovers: [Option<Pick>; 2],
    grab: Option<Grab>,
    // Both pinches free while editing (the two-hand pinch): maybe the gesture, or a new box
    two_hand: TwoHandPinch,
    two_hand_state: TwoHand,
    new_box: Option<NewBox>,
    gesture: HoldTimer,
    gesture_progress: f32,
    // When the gesture started (seconds), for the log
    gesture_since: Option<f64>,
    last_gesture_log: Option<Instant>,
    // While editing: when a handle was last grabbed or held (seconds)
    last_used: f64,
    // While editing: the last update with a stream with Passthrough = Depth, and whether editing waits
    // for it (seconds)
    last_live: f64,
    paused: bool,
    // Why each hand was last not usable (for the log when it ends a grab)
    unusable: [Option<String>; 2],
    // Kept STAGE changes: when one was last logged, and how many were not logged since
    last_stage_log: Option<f64>,
    stage_not_logged: u32,
    // Smallest and largest gap of each hand since the last log line
    gap_range: [Option<(f32, f32)>; 2],
    // Updates of each hand since the last log line with the ray of the runtime and with an estimated one
    ray_counts: [[u32; 2]; 2],
    last_gap_log: f64,
    frame: Option<DrawFrame>,
    saver: Option<mpsc::Sender<SaveJob>>,
}

impl AreaEditor {
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
            last_poll: None,
            // A value left from before is not a request
            edit_property: debug_property("debug.alvr.area_edit"),
            edit_request: None,
            pinch_on: DEFAULT_PINCH_ON_MM as f32 / 1000.0,
            pinch_off: DEFAULT_PINCH_OFF_MM as f32 / 1000.0,
            dim: DEFAULT_DIM_PERCENT as f32 / 100.0,
            see: DEFAULT_SEE_PERCENT as f32 / 100.0,
            grace: DEFAULT_GRACE_S as f64,
            withhold_hands: true,
            files: true,
            mode: Mode::Hidden,
            shown_until: None,
            layout: None,
            entered_with: None,
            edited: false,
            saved_anchor: None,
            loaded_for: None,
            newest_tried: false,
            stage_changes: Vec::new(),
            last_view: None,
            placement: Placement::None,
            measured: false,
            measured_anchor: None,
            frozen: None,
            selected: 0,
            undo: UndoStack::default(),
            pinches: [Pinch::default(); 2],
            roles: PinchRoles::default(),
            hovers: [None; 2],
            grab: None,
            two_hand: TwoHandPinch::default(),
            two_hand_state: TwoHand::None,
            new_box: None,
            gesture: HoldTimer::default(),
            gesture_progress: 0.0,
            gesture_since: None,
            last_gesture_log: None,
            last_used: 0.0,
            last_live: 0.0,
            paused: false,
            unusable: [None, None],
            last_stage_log: None,
            stage_not_logged: 0,
            gap_range: [None; 2],
            ray_counts: [[0; 2]; 2],
            last_gap_log: 0.0,
            frame: None,
            saver: None,
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

        let edit = debug_property("debug.alvr.area_edit");
        if edit != self.edit_property {
            self.edit_property = edit;
            match edit {
                Some(1) => self.edit_request = Some(true),
                Some(0) => self.edit_request = Some(false),
                _ => (),
            }
        }

        let on_mm = debug_property("debug.alvr.area_pinch_on")
            .unwrap_or(DEFAULT_PINCH_ON_MM)
            // Below 0: the tips press into each other (measured on the Quest: -2 to -16 mm at contact)
            .clamp(-30, 100);
        let off_mm = debug_property("debug.alvr.area_pinch_off")
            .unwrap_or(DEFAULT_PINCH_OFF_MM)
            // May equal the closing gap: the pinch still needs 2 updates to close and 80 ms to open
            .clamp(on_mm, 105);
        let (on, off) = (on_mm as f32 / 1000.0, off_mm as f32 / 1000.0);
        if (on, off) != (self.pinch_on, self.pinch_off) {
            crate::stream_log(&format!(
                "area editor: a pinch closes below a gap of {on_mm} mm and opens above {off_mm} mm"
            ));
            self.pinch_on = on;
            self.pinch_off = off;
        }

        let dim = debug_property("debug.alvr.area_edit_dim")
            .unwrap_or(DEFAULT_DIM_PERCENT)
            .clamp(0, 90) as f32
            / 100.0;
        if dim != self.dim {
            crate::stream_log(&format!(
                "area editor: the game is dimmed by {:.0} % while editing",
                dim * 100.0
            ));
            self.dim = dim;
        }

        let see = debug_property("debug.alvr.area_edit_see")
            .unwrap_or(DEFAULT_SEE_PERCENT)
            .clamp(0, 100) as f32
            / 100.0;
        if see != self.see {
            crate::stream_log(&format!(
                "area editor: the room shows through the game at {:.0} % outside the play area while \
                 editing (area_edit_see)",
                see * 100.0
            ));
            self.see = see;
        }

        let grace = debug_property("debug.alvr.area_edit_grace")
            .unwrap_or(DEFAULT_GRACE_S)
            .clamp(0, 60) as f64;
        if grace != self.grace {
            crate::stream_log(&format!(
                "area editor: editing waits {grace:.0} s for the stream to come back"
            ));
            self.grace = grace;
        }

        let withhold_hands = debug_property("debug.alvr.area_edit_hands_pc").unwrap_or(0) != 1;
        if withhold_hands != self.withhold_hands {
            crate::stream_log(if withhold_hands {
                "area editor: the hands are not sent to the PC while editing (area_edit_hands_pc 0)"
            } else {
                "area editor: the hands are sent to the PC also while editing (area_edit_hands_pc 1)"
            });
            self.withhold_hands = withhold_hands;
        }

        let files = debug_property("debug.alvr.area_file").unwrap_or(1) != 0;
        if files != self.files {
            self.files = files;
            crate::stream_log(if files {
                "area editor: layout files are used (area_file 1), read again"
            } else {
                "area editor: layout files are not used (area_file 0): the box of the marker_box \
                 properties, edits are not saved"
            });
            // Read again (or dropped) at the next frame
            if self.mode == Mode::Editing {
                self.leave("area_file changed");
            }
            self.loaded_for = None;
            self.newest_tried = false;
            self.layout = None;
            self.saved_anchor = None;
        }
    }

    /// How much the game is dimmed (0..1): while editing
    pub fn game_dim(&self) -> f32 {
        if self.mode == Mode::Editing {
            self.dim
        } else {
            0.0
        }
    }

    /// How much the real room shows through the game outside the play area (0..1): while editing
    pub fn game_see(&self) -> f32 {
        if self.mode == Mode::Editing {
            self.see
        } else {
            0.0
        }
    }

    /// Why the outline must be shown, whatever `debug.alvr.area_show` says. None: it need not
    pub fn forced_show(&self) -> Option<&'static str> {
        match self.mode {
            Mode::Editing => Some("editing"),
            Mode::Shown => Some("editing ended"),
            Mode::Hidden if self.gesture_progress > 0.0 => Some("edit gesture"),
            Mode::Hidden => None,
        }
    }

    /// Editing: the hands are not limited to the play area then (the handles are on and outside it)
    pub fn is_editing(&self) -> bool {
        self.mode == Mode::Editing
    }

    /// The hands are not sent to the PC: while editing, unless `area_edit_hands_pc 1`
    pub fn withholds_hands(&self) -> bool {
        self.mode == Mode::Editing && self.withhold_hands
    }

    /// The play area is drawn dashed: it is placed from the anchor saved in the layout file (the code
    /// was not seen in this run)
    pub fn dashed(&self) -> bool {
        self.placement == Placement::SavedAnchor
    }

    /// Of the outline of the play area: brightening while the gesture is held
    pub fn outline_color(&self) -> Vec4 {
        let progress = self.gesture_progress;
        match self.mode {
            Mode::Editing => EDIT_OUTLINE.lerp(OVERLAY_GREEN, progress),
            _ if progress > 0.0 => OVERLAY_DIM.lerp(OVERLAY_WHITE, progress),
            _ => OVERLAY_WHITE,
        }
    }

    /// The STAGE space changes at `change_time`, see `StageEvent`. A grab ends (kept); at the change
    /// time the saved anchor and the frozen pose of the code are carried into the new space
    /// (`debug.alvr.marker_carry 1`), kept as they are (0) or dropped (no valid pose), like the box of
    /// the marker region
    pub fn on_stage_change(&mut self, change_time: xr::Time, event: StageEvent) {
        if self.mode == Mode::Editing {
            self.keep_grab("stage space changed");
        }
        self.stage_changes.push((change_time, event));
    }

    // The changes of the STAGE space that are due at `now`, in order
    fn apply_stage_changes(&mut self, now: xr::Time) {
        while let Some(&(time, event)) = self.stage_changes.first()
            && now.as_nanos() >= time.as_nanos()
        {
            self.stage_changes.remove(0);
            match event {
                StageEvent::Carry(_) | StageEvent::Keep(_) => {
                    let has_pose = self.saved_anchor.is_some() || self.frozen.is_some();
                    self.saved_anchor = self
                        .saved_anchor
                        .and_then(|(position, yaw)| event.apply(position, yaw));
                    if let Some(((position, yaw), placement)) = self.frozen {
                        self.frozen = event.apply(position, yaw).map(|pose| (pose, placement));
                    }
                    if has_pose && matches!(event, StageEvent::Keep(_)) {
                        self.log_kept_stage_change(&event);
                    }
                }
                StageEvent::NoPose => {
                    if self.saved_anchor.is_some() || self.frozen.is_some() {
                        crate::stream_log(
                            "area editor: stage space changed without a pose, the saved anchor is \
                             dropped until the code is seen",
                        );
                    }
                    self.saved_anchor = None;
                    self.frozen = None;
                }
            }
        }
    }

    // A STAGE change whose pose is not applied: at most one line per `STAGE_LOG_INTERVAL_S`, with the
    // number left out since (on 2026-11-24 they came in pairs, +0.15 m then -0.15 m up, every minute or
    // two)
    fn log_kept_stage_change(&mut self, event: &StageEvent) {
        let now = self.start.elapsed().as_secs_f64();
        if self
            .last_stage_log
            .is_some_and(|time| now - time < STAGE_LOG_INTERVAL_S)
        {
            self.stage_not_logged += 1;
            return;
        }
        crate::stream_log(&format!(
            "area editor: stage space changed, coordinates of the saved anchor and the frozen pose \
             kept (marker_carry 0; the reported pose {} is not applied){}",
            event.describe_pose(),
            if self.stage_not_logged > 0 {
                format!(
                    "; {} more such changes since the last line, not logged (at most one line per \
                     {STAGE_LOG_INTERVAL_S:.0} s)",
                    self.stage_not_logged
                )
            } else {
                String::new()
            }
        ));
        self.last_stage_log = Some(now);
        self.stage_not_logged = 0;
    }

    // Whether the layout is the one of a file (read or to be written): not while an unchanged box of
    // the properties is edited
    fn layout_file_in_use(&self) -> bool {
        self.layout.is_some()
            && (self.mode != Mode::Editing || self.edited || self.entered_with.is_some())
    }

    /// The play area for this frame: `region` (the box of the properties placed by the marker region)
    /// unless a layout file or the edits replace it; placed from the anchor saved in the file while no
    /// code was seen in this run. `display_time`: of the frame. `input`: the eyes and the hands at the
    /// display time (None: not available). `depth_passthrough`: the stream uses Passthrough = Depth
    pub fn update(
        &mut self,
        marker_region: &MarkerRegion,
        region: Option<PlacedArea>,
        display_time: xr::Time,
        input: Option<&EditorInput>,
        depth_passthrough: bool,
    ) -> Option<PlacedArea> {
        let now = self.start.elapsed().as_secs_f64();
        self.poll_properties();
        self.apply_stage_changes(display_time);
        self.frame = None;
        if self.mode == Mode::Shown && self.shown_until.is_none_or(|until| Instant::now() >= until)
        {
            self.mode = Mode::Hidden;
        }

        let enabled = marker_region.is_enabled();
        if enabled {
            self.follow_payload(marker_region);
        }

        // The pose of the code: frozen while editing, else as estimated, else as saved
        let live = marker_region.anchor_pose().filter(|_| enabled);
        let measured =
            live.is_some() && marker_region.anchor().is_some_and(|anchor| anchor.measured);
        self.measured_anchor = live.filter(|_| measured);
        if measured && !self.measured && self.layout_file_in_use() {
            self.save("anchor measured");
        }
        self.measured = measured;
        if self.mode == Mode::Editing && self.frozen.is_none() {
            self.frozen = live
                .map(|pose| (pose, Placement::Code))
                .or(self.saved_anchor.map(|pose| (pose, Placement::SavedAnchor)));
        }
        let (anchor, placement) = match self.frozen.filter(|_| self.mode == Mode::Editing) {
            Some((pose, placement)) => (Some(pose), placement),
            None => match (
                live,
                self.saved_anchor
                    .filter(|_| enabled && self.layout.is_some()),
            ) {
                (Some(pose), _) => (Some(pose), Placement::Code),
                (None, Some(pose)) => (Some(pose), Placement::SavedAnchor),
                (None, None) => (None, Placement::None),
            },
        };
        self.log_placement(placement, anchor);

        let placed = match (anchor, self.layout) {
            (Some((position, yaw)), Some(layout)) => Some(PlacedArea {
                area_to_world: layout.area_to_world(position, yaw),
                area: layout,
            }),
            // The box of the properties, as before
            (Some(_), None) => region,
            (None, _) => None,
        };

        if self.mode == Mode::Editing && (!enabled || placed.is_none()) {
            self.leave(if !enabled {
                "marker box off"
            } else {
                "no play area"
            });
        }
        if self.mode == Mode::Editing {
            self.follow_stream(depth_passthrough, now);
        }
        // Also without hands: the game must not stay dimmed
        if self.mode == Mode::Editing && now - self.last_used > IDLE_TIMEOUT_S {
            self.leave("no grab for 120 s");
        }
        if let Some(start) = self.edit_request.take() {
            match (start, self.mode == Mode::Editing) {
                (true, false) => match (placed, anchor) {
                    (Some(placed), Some(anchor)) if depth_passthrough => {
                        self.enter("area_edit 1", &placed, (anchor, placement), now);
                    }
                    _ => crate::stream_log(
                        "area editor: can't edit (area_edit 1): no play area (marker_box off, no \
                         code seen and no saved anchor, or Passthrough is not Depth)",
                    ),
                },
                (false, true) => self.leave("area_edit 0"),
                _ => (),
            }
        }

        // A frame without the eyes (the head pose not valid): the last view with the hands not seen,
        // so that the pinches and a grab go the way of a lost hand instead of being reset
        let held_input;
        let input = match input {
            Some(input) => {
                self.last_view = Some((input.eye, input.forward, now));
                Some(input)
            }
            None => match self
                .last_view
                .filter(|&(_, _, time)| now - time <= VIEW_HOLD_S)
            {
                Some((eye, forward, _)) => {
                    held_input = EditorInput {
                        eye,
                        forward,
                        hands: [None, None],
                    };
                    Some(&held_input)
                }
                None => None,
            },
        };
        let (Some(placed), Some(anchor), Some(input), true) =
            (placed, anchor, input, depth_passthrough)
        else {
            self.reset_hands(now);
            return placed;
        };

        // The pinches of the hands that are tracked and in view; the hand that grabs, and both hands
        // that pull a new box open (wide apart), may be farther from the view direction and keep their
        // pinch longer while they are not usable
        let grabbing = self.grab.as_ref().map(|grab| grab.hand);
        let adding = self.two_hand_state == TwoHand::Adding;
        let wide = |i: usize| adding || grabbing == Some(i);
        let mut usable = [None; 2];
        for (i, hand) in input.hands.iter().enumerate() {
            let angle = if wide(i) {
                alvr_graphics::GRAB_IN_VIEW_ANGLE
            } else {
                alvr_graphics::IN_VIEW_ANGLE
            };
            match hand {
                Some(hand) => match hand.unusable_reason(input.eye, input.forward, angle) {
                    None => usable[i] = Some(*hand),
                    reason => self.unusable[i] = reason,
                },
                None => self.unusable[i] = Some("no pose".into()),
            }
        }
        let mut events = [PinchEvent::None; 2];
        for (i, event) in events.iter_mut().enumerate() {
            let lost_hold = if wide(i) {
                alvr_graphics::GRAB_LOST_HOLD_S
            } else {
                alvr_graphics::HAND_LOST_HOLD_S
            };
            *event = self.pinches[i].update(
                usable[i].map(|hand| hand.gap()),
                now,
                self.pinch_on,
                self.pinch_off,
                lost_hold,
            );
            // An open pinch has no role; `edit` gives one to a pinch that closes
            self.roles.follow(i, self.pinches[i].is_closed());
        }
        if self.mode == Mode::Editing {
            self.log_gaps(&usable, now);
        }

        // Both pinches free while editing (the two-hand pinch): the gesture or a new box. Once the pinch
        // points were more than 20 cm apart it is a new box and the gesture does not run in this pinch
        let span = match usable {
            [Some(left), Some(right)] => left.cursor().distance(right.cursor()),
            _ => f32::NAN,
        };
        let editing = self.mode == Mode::Editing;
        let both_free = editing
            && !self.paused
            && self.grab.is_none()
            && self.pinches.iter().all(Pinch::is_closed)
            && self.roles.two_hand();
        let two_hand = self.two_hand.update(both_free, span);
        self.two_hand_state = two_hand;
        if two_hand == TwoHand::Adding {
            self.gesture.cancel();
        }

        // The two-hand gesture; while editing only with the two-hand pinch ("exiting edit mode is
        // always with 2"): a pinch on a handle, or one that does nothing, never ends editing, and a grab
        // in progress is never dropped by it
        let pose = match usable {
            [Some(left), Some(right)] => {
                self.pinches.iter().all(Pinch::is_closed)
                    && (!editing || both_free)
                    && alvr_graphics::edit_gesture_pose(input.eye, input.forward, [&left, &right])
            }
            _ => false,
        };
        let (progress, fired) = self
            .gesture
            .update(pose && two_hand != TwoHand::Adding, now);
        self.gesture_progress = progress;
        self.log_gesture(progress, fired, now);
        if fired {
            if self.mode == Mode::Editing {
                self.leave("gesture");
            } else {
                self.enter("gesture", &placed, (anchor, placement), now);
            }
        }

        let gesture = (progress > 0.0)
            .then(|| match usable {
                [Some(left), Some(right)] => {
                    Some(((left.cursor() + right.cursor()) * 0.5, progress))
                }
                _ => None,
            })
            .flatten();
        let mut frame = DrawFrame {
            placed,
            eye: input.eye,
            handles: Vec::new(),
            selected: self.selected,
            cursors: Vec::new(),
            lit: Vec::new(),
            grabbed: None,
            gesture,
            remove: None,
            turn: None,
            rays: Vec::new(),
            new_box: None,
        };

        let placed = if self.mode == Mode::Editing {
            self.edit(&placed, input, usable, events, now, &mut frame)
        } else {
            placed
        };
        frame.placed = placed;
        self.frame = Some(frame);

        Some(placed)
    }

    // One frame of editing: hovers, grabs, drags, releases. Returns the area as edited
    fn edit(
        &mut self,
        placed: &PlacedArea,
        input: &EditorInput,
        usable: [Option<EditorHand>; 2],
        events: [PinchEvent; 2],
        now: f64,
        frame: &mut DrawFrame,
    ) -> PlacedArea {
        let mut layout = self.layout.unwrap_or(placed.area);
        self.selected = self.selected.min(layout.len() - 1);
        let to_area = placed.area_to_world.inverse();
        let handles = alvr_graphics::edit_handles(placed, self.selected, !self.undo.is_empty());

        // The ray of each usable hand: the aim pose of the runtime, else estimated
        let mut rays = [None; 2];
        for (i, hand) in usable.iter().enumerate() {
            if let Some(hand) = hand {
                rays[i] = hand.ray(input.eye, input.forward, i == 0);
                self.ray_counts[i][usize::from(hand.aim.is_none())] += 1;
            }
        }

        // What each hand points at: not the hand that grabs, and kept while the fingers close
        for (i, hand) in usable.iter().enumerate() {
            let (Some(hand), Some(ray)) = (hand, rays[i]) else {
                self.hovers[i] = None;
                continue;
            };
            if self.grab.as_ref().is_some_and(|grab| grab.hand == i) {
                continue;
            }
            let closing =
                self.pinches[i].is_closed() || hand.gap() < self.pinch_on + HOVER_LOCK_MARGIN;
            let still_there = self.hovers[i]
                .is_some_and(|pick| handles.iter().any(|handle| handle.id == pick.id));
            if !(closing && still_there) {
                self.hovers[i] = alvr_graphics::pick_handle(
                    &handles,
                    input.eye,
                    hand.cursor(),
                    &ray,
                    self.hovers[i].map(|pick| pick.id),
                );
            }
        }

        // A pinch that closes: on empty space it is free (with the other hand: a new box); on a handle
        // of another box it only selects that box; on a lit handle of the selected box (or undo, done)
        // it takes it. That only with the other hand open: "pinch to resize is always with one hand.
        // adding a new box or exiting edit mode is always with 2" (`PinchRoles::close`). With the other
        // pinch free, or giving way (on a handle for at most `GRAB_YIELD_S`, its grab not out of its
        // dead zone nor done), both are the two-hand pinch, whatever this one closed on; else this one
        // does nothing until it opens (one hand resizing is not disturbed). One grab at a time
        for i in 0..2 {
            let (PinchEvent::Closed, Some(hand)) = (events[i], usable[i]) else {
                continue;
            };
            let other = 1 - i;
            let other_role = self.roles.role(other);
            let other_grab = self
                .grab
                .as_ref()
                .filter(|grab| grab.hand == other)
                .map(|grab| {
                    (
                        grab.start_layout,
                        grab.id,
                        grab.removed || grab.max_move > alvr_graphics::DRAG_DEAD_ZONE,
                        now - grab.start_time,
                    )
                });
            let other_acted = other_grab.is_some_and(|(_, _, acted, _)| acted);
            let (pick, action) = match self.roles.close(
                i,
                self.hovers[i],
                self.selected,
                other_acted,
                now,
            ) {
                PinchStart::Free => continue,
                PinchStart::TwoHand { gives_way } => {
                    if gives_way {
                        let gives_way = format!(
                            "the {} pinch{} gives way",
                            HAND_NAMES[other],
                            if matches!(other_role, PinchRole::Handle { by_ray: true, .. }) {
                                " by the ray"
                            } else {
                                ""
                            }
                        );
                        let reason = format!(
                            "the {} hand pinched too: both pinches are the two-hand pinch, a new box \
                             or the gesture that ends editing",
                            HAND_NAMES[i]
                        );
                        match other_grab {
                            Some((start_layout, id, _, _)) => {
                                self.drop_grab(&format!("{gives_way}: {reason}"));
                                if id.kind.is_drag() {
                                    layout = start_layout;
                                }
                            }
                            None => {
                                crate::stream_log(&format!("area editor: {gives_way} ({reason})"))
                            }
                        }
                    }
                    continue;
                }
                PinchStart::Idle => {
                    let other_doing = match (other_role, other_grab) {
                        (PinchRole::Handle { .. }, Some((_, id, acted, age))) => format!(
                            "holds {} of box #{} ({:.1} s{})",
                            id.kind.name(),
                            id.box_index,
                            age,
                            if acted { ", moved or done" } else { "" }
                        ),
                        (PinchRole::Handle { since, .. }, None) => {
                            format!("selected a box {:.1} s ago", now - since)
                        }
                        _ => "does nothing either".into(),
                    };
                    crate::stream_log(&format!(
                        "area editor: the {} pinch does nothing until it opens: the {} pinch \
                         {other_doing} (a handle is worked with one hand; a new box and the end of \
                         editing take both pinches free: a pinch on a handle gives way only within \
                         {:.1} s and before it moved)",
                        HAND_NAMES[i],
                        HAND_NAMES[other],
                        alvr_graphics::GRAB_YIELD_S
                    ));
                    continue;
                }
                PinchStart::Handle(action) => match self.hovers[i] {
                    Some(pick) => (pick, action),
                    None => continue,
                },
            };
            if self.grab.is_some() {
                continue;
            }
            match action {
                PinchAction::Select(index) => {
                    self.selected = index;
                    self.last_used = now;
                    crate::stream_log(&format!(
                        "area editor: selected box #{index} ({} hand{}): the next pinch edits it",
                        HAND_NAMES[i],
                        if pick.far { ", by the ray" } else { "" }
                    ));
                }
                PinchAction::Grab => {
                    let handle = handles.iter().find(|handle| handle.id == pick.id);
                    let far = match (pick.far, handle, rays[i]) {
                        (true, Some(handle), Some(ray)) => {
                            FarDrag::new(handle, &ray, hand.cursor())
                        }
                        _ => None,
                    };
                    // A far turn sweeps from the point of the ray at the grab
                    let turn_from = match (far, rays[i]) {
                        (Some(mut far), Some(ray)) => far.follow(&ray, hand.cursor()).point,
                        _ => hand.cursor(),
                    };
                    crate::stream_log(&format!(
                        "area editor: grab {} of box #{} ({} hand{})",
                        pick.id.kind.name(),
                        pick.id.box_index,
                        HAND_NAMES[i],
                        match (pick.far, handle, rays[i]) {
                            (true, Some(handle), Some(ray)) => format!(
                                ", by the ray, {:.2} m away{}",
                                handle.position.distance(ray.origin),
                                far.map_or(String::new(), |far| format!(
                                    ", dead zone x{:.1}",
                                    far.gain()
                                ))
                            ),
                            _ => String::new(),
                        }
                    ));
                    self.grab = Some(Grab {
                        hand: i,
                        id: pick.id,
                        start_cursor: hand.cursor(),
                        start_layout: layout,
                        start_time: now,
                        history: VecDeque::new(),
                        max_move: 0.0,
                        swept: 0.0,
                        turn_from: to_area.transform_point3(turn_from),
                        snapped: false,
                        removed: false,
                        lost_reason: None,
                        by_ray: pick.far,
                        gain: far.map_or(1.0, |far| far.gain()),
                        far,
                    });
                    self.last_used = now;
                }
            }
        }

        let mut save_reason = None;
        let mut done = false;
        if let Some(mut grab) = self.grab.take() {
            // The idle time counts from the end of the last grab
            self.last_used = now;
            // While the hand is not usable the handle stays where it was
            match usable[grab.hand] {
                Some(hand) => {
                    // Near: the displacement of the cursor. Far: what the ray gives
                    let step = match (grab.far.as_mut(), rays[grab.hand]) {
                        (Some(far), Some(ray)) => {
                            let step = far.follow(&ray, hand.cursor());
                            Some((step.moved, step.point, step.continuous))
                        }
                        (Some(_), None) => None,
                        (None, _) => Some((hand.cursor() - grab.start_cursor, hand.cursor(), true)),
                    };
                    if let Some((moved, point, continuous)) = step {
                        grab.max_move = grab.max_move.max(moved.length() / grab.gain);
                        if let Some(center) = grab
                            .start_layout
                            .boxes()
                            .get(grab.id.box_index)
                            .map(|area_box| area_box.center)
                            .filter(|_| grab.id.kind == HandleKind::Turn)
                        {
                            grab.follow_turn(center, to_area.transform_point3(point), continuous);
                        }
                        grab.history.push_back((now, moved, grab.swept));
                        while grab.history.front().is_some_and(|(time, _, _)| {
                            now - time > 4.0 * alvr_graphics::RELEASE_LOOKBACK_S
                        }) {
                            grab.history.pop_front();
                        }
                    }
                }
                None => grab.lost_reason.clone_from(&self.unusable[grab.hand]),
            }

            match grab.id.kind {
                kind if kind.is_drag() => {
                    if let Some(&(_, moved, swept)) = grab.history.back() {
                        let (dragged, _, snapped) = grab_result(&grab, &to_area, moved, swept);
                        layout = dragged;
                        grab.snapped = snapped;
                    }
                }
                // Only with the hand usable the whole time: its pinch stays closed for
                // `GRAB_LOST_HOLD_S` after it went, longer than the hold, with fingers nobody sees
                HandleKind::Remove
                    if !grab.removed
                        && grab.lost_reason.is_none()
                        && grab.max_move < alvr_graphics::REMOVE_HOLD_MOVE
                        && now - grab.start_time >= alvr_graphics::REMOVE_HOLD_S =>
                {
                    grab.removed = true;
                    let before = layout;
                    if layout.remove_box(grab.id.box_index) {
                        self.undo.push(before);
                        self.selected = grab.id.box_index.saturating_sub(1);
                        crate::stream_log(&format!(
                            "area editor: removed box #{}, {}",
                            grab.id.box_index,
                            layout.describe()
                        ));
                        save_reason = Some("box removed");
                    }
                }
                _ => (),
            }

            let event = events[grab.hand];
            if matches!(event, PinchEvent::Opened | PinchEvent::Lost) {
                let lost = event == PinchEvent::Lost;
                let name = grab.id.kind.name();
                let index = grab.id.box_index;
                let lost_note = if lost {
                    format!(
                        " (hand lost: {})",
                        grab.lost_reason.as_deref().unwrap_or("not usable")
                    )
                } else {
                    String::new()
                };
                match grab.id.kind {
                    kind if kind.is_drag() => {
                        let (moved, swept) = grab.release_values(now, lost);
                        let (released, value, snapped) = grab_result(&grab, &to_area, moved, swept);
                        layout = released;
                        if released != grab.start_layout {
                            self.undo.push(grab.start_layout);
                            save_reason = Some("grab released");
                            let what = match kind {
                                HandleKind::Side(_) => format!("moved {value:.3} m"),
                                HandleKind::Dome(_) => format!("dome {value:.3} m"),
                                HandleKind::Turn => format!(
                                    "yaw {:.1} deg (turned by {:.1} deg{})",
                                    value.to_degrees(),
                                    alvr_graphics::wrap_angle(
                                        value - grab.start_layout.boxes()[index].yaw
                                    )
                                    .to_degrees(),
                                    if snapped { ", on a detent" } else { "" }
                                ),
                                _ => format!("moved by {value:.3} m"),
                            };
                            crate::stream_log(&format!(
                                "area editor: release {name} of box #{index}{lost_note}: {what}, {}",
                                released.describe_box(index)
                            ));
                        } else {
                            crate::stream_log(&format!(
                                "area editor: release {name} of box #{index}: no change{lost_note}"
                            ));
                        }
                    }
                    HandleKind::Remove if !grab.removed => crate::stream_log(&format!(
                        "area editor: remove of box #{index} not done (held {:.1} s, moved {:.0} \
                         mm{}; it needs {:.1} s held within {:.0} mm with the hand usable)",
                        now - grab.start_time,
                        grab.max_move * 1000.0,
                        match (&grab.lost_reason, lost) {
                            (_, true) => lost_note.clone(),
                            (Some(reason), false) =>
                                format!(", hand not usable for a while: {reason}"),
                            (None, false) => String::new(),
                        },
                        alvr_graphics::REMOVE_HOLD_S,
                        alvr_graphics::REMOVE_HOLD_MOVE * 1000.0
                    )),
                    // Buttons act on any release of a usable hand, wherever it went
                    HandleKind::Undo if !lost => match self.undo.pop() {
                        Some(previous) => {
                            layout = previous;
                            self.selected = self.selected.min(layout.len() - 1);
                            save_reason = Some("undo");
                            crate::stream_log(&format!(
                                "area editor: undo ({} more), {}",
                                self.undo.len(),
                                layout.describe()
                            ));
                        }
                        None => crate::stream_log("area editor: nothing to undo"),
                    },
                    HandleKind::Done if !lost => done = true,
                    HandleKind::Remove => (),
                    _ => crate::stream_log(&format!(
                        "area editor: release {name}{lost_note}: nothing done (a button acts only \
                         when the pinch opens with the hand in view)"
                    )),
                }
            } else {
                // A grab of a box that is gone (removed) ends with the pinch only
                self.grab = Some(grab);
            }
        }

        // A new box pulled open with both hands: made when a hand lets go
        let corners = match usable {
            [Some(left), Some(right)] => Some((
                to_area.transform_point3(left.cursor()),
                to_area.transform_point3(right.cursor()),
            )),
            _ => None,
        };
        if self.two_hand_state == TwoHand::Adding {
            let new_box = self.new_box.get_or_insert_with(|| {
                crate::stream_log(&format!(
                    "area editor: new box: both hands pinched on empty space, {} apart (more than \
                     {:.0} cm: not the gesture that ends editing, until a pinch opens)",
                    corners.map_or("-".into(), |(a, b)| format!(
                        "{:.0} cm",
                        a.distance(b) * 100.0
                    )),
                    alvr_graphics::GESTURE_MAX_SPAN * 100.0
                ));
                NewBox {
                    corners: VecDeque::new(),
                }
            });
            // While a hand is not usable the box stays as it was
            if let Some((a, b)) = corners {
                new_box.corners.push_back((now, a, b));
                while new_box.corners.front().is_some_and(|(time, _, _)| {
                    now - time > 4.0 * alvr_graphics::RELEASE_LOOKBACK_S
                }) {
                    new_box.corners.pop_front();
                }
            }
            self.last_used = now;
        } else if let Some(new_box) = self.new_box.take() {
            let lost = events.contains(&PinchEvent::Lost);
            let lost_note = if lost { " (a hand lost)" } else { "" };
            match new_box.release_corners(now, lost) {
                Some((a, b)) => match alvr_graphics::new_box_refusal(&layout, a, b) {
                    None => {
                        let before = layout;
                        if let Some(added) = layout.push_box(alvr_graphics::new_box_between(a, b)) {
                            self.undo.push(before);
                            self.selected = added;
                            save_reason = Some("box added");
                            crate::stream_log(&format!(
                                "area editor: added box #{added} (two-hand pinch{lost_note}, the \
                                 pinch points {:.0} cm apart): {}",
                                a.distance(b) * 100.0,
                                layout.describe_box(added)
                            ));
                        }
                    }
                    Some(reason) => {
                        crate::stream_log(&format!("area editor: no new box{lost_note}: {reason}"));
                    }
                },
                None => crate::stream_log(
                    "area editor: no new box: the hands were not seen while it was pulled open",
                ),
            }
        }

        self.layout = Some(layout);
        if let Some(reason) = save_reason {
            if !self.edited && self.entered_with.is_none() {
                crate::stream_log(
                    "area editor: the edited layout replaces the box of the marker_box properties from \
                     now on (its layout file; area_file 0 to use the properties)",
                );
            }
            self.edited = true;
            self.save(reason);
        }
        let placed = PlacedArea {
            area_to_world: placed.area_to_world,
            area: layout,
        };

        // To draw: the handles of the area as it is now
        frame.handles = alvr_graphics::edit_handles(&placed, self.selected, !self.undo.is_empty());
        frame.selected = self.selected;
        frame.grabbed = self
            .grab
            .as_ref()
            .filter(|grab| !grab.removed)
            .map(|grab| grab.id);
        // During the two-hand pinch no handle is lit and no ray drawn; neither for a pinch that does
        // nothing until it opens
        let two_hand = self.two_hand_state != TwoHand::None;
        let idle = |i: usize| self.roles.role(i) == PinchRole::Idle;
        frame.lit = self
            .hovers
            .iter()
            .enumerate()
            .filter(|(i, _)| {
                self.grab.as_ref().is_none_or(|grab| grab.hand != *i) && !two_hand && !idle(*i)
            })
            .filter_map(|(_, pick)| pick.map(|pick| pick.id))
            .collect();
        // The rays of the hands that are not near a handle: to the handle the ray holds or lights (a
        // dot where it meets it), else a short line
        let position_of = |id: HandleId| {
            frame
                .handles
                .iter()
                .find(|handle| handle.id == id)
                .map(|handle| handle.position)
        };
        for (i, ray) in rays.iter().enumerate() {
            let Some(ray) = ray.filter(|_| !two_hand && !idle(i)) else {
                continue;
            };
            let grab = self.grab.as_ref().filter(|grab| grab.hand == i);
            let ray_line = match (grab, self.hovers[i]) {
                (Some(grab), _) if grab.by_ray && !grab.removed => {
                    position_of(grab.id).map(|end| (ray, Some(end), OVERLAY_CYAN))
                }
                (Some(_), _) => None,
                (None, Some(pick)) if pick.far => position_of(pick.id)
                    .map(|position| (ray, Some(ray.nearest_point(position)), OVERLAY_AMBER)),
                (None, Some(_)) => None,
                (None, None) => Some((ray, None, OVERLAY_WHITE)),
            };
            frame.rays.extend(ray_line);
        }
        // The new box as it would be made: dim while too small, red without room
        frame.new_box = self
            .new_box
            .as_ref()
            .and_then(|new_box| new_box.corners.back())
            .map(|&(_, a, b)| {
                let color = if a.distance(b) < alvr_graphics::NEW_BOX_MIN_SPAN {
                    OVERLAY_DIM
                } else if alvr_graphics::new_box_refusal(&layout, a, b).is_some() {
                    OVERLAY_RED
                } else {
                    OVERLAY_GREEN
                };
                (alvr_graphics::new_box_between(a, b), color)
            });
        frame.cursors = usable
            .iter()
            .enumerate()
            .filter_map(|(i, hand)| hand.map(|hand| (hand.cursor(), self.pinches[i].is_closed())))
            .collect();
        frame.remove = self.grab.as_ref().and_then(|grab| {
            let handle = frame.handles.iter().find(|handle| handle.id == grab.id)?;
            (grab.id.kind == HandleKind::Remove && !grab.removed && grab.lost_reason.is_none())
                .then(|| {
                    let progress = (now - grab.start_time) / alvr_graphics::REMOVE_HOLD_S;
                    (handle.position, progress.min(1.0) as f32)
                })
        });
        frame.turn = self
            .grab
            .as_ref()
            .filter(|grab| grab.id.kind == HandleKind::Turn)
            .and_then(|grab| {
                let index = grab.id.box_index;
                Some((
                    *grab.start_layout.boxes().get(index)?,
                    *layout.boxes().get(index)?,
                    grab.snapped,
                ))
            });

        if done {
            self.leave("done handle");
        }

        placed
    }

    fn enter(
        &mut self,
        reason: &str,
        placed: &PlacedArea,
        anchor: (SavedAnchor, Placement),
        now: f64,
    ) {
        if self.mode == Mode::Editing {
            return;
        }
        self.entered_with = self.layout;
        self.edited = false;
        let layout = *self.layout.get_or_insert(placed.area);
        self.frozen = Some(anchor);
        self.mode = Mode::Editing;
        self.selected = self.selected.min(layout.len() - 1);
        self.undo.clear();
        self.hovers = [None; 2];
        self.grab = None;
        // The pinches of the gesture that started editing do nothing until they open (no new box
        // from them, they don't end editing again)
        self.roles
            .reset([self.pinches[0].is_closed(), self.pinches[1].is_closed()]);
        self.two_hand = TwoHandPinch::default();
        self.new_box = None;
        self.last_used = now;
        self.last_live = now;
        self.paused = false;
        self.gap_range = [None; 2];
        self.ray_counts = [[0; 2]; 2];
        self.last_gap_log = now;
        crate::stream_log(&format!(
            "area editor: editing ({reason}), game dimmed by {:.0} %, the room through it at {:.0} % \
             outside the area, placed from {}: {}",
            self.dim * 100.0,
            self.see * 100.0,
            match anchor.1 {
                Placement::SavedAnchor => "the saved anchor",
                _ => "the code",
            },
            layout.describe()
        ));
    }

    fn leave(&mut self, reason: &str) {
        if self.mode != Mode::Editing {
            return;
        }
        self.keep_grab("editing ended");
        self.drop_new_box("editing ended");
        self.paused = false;
        self.mode = Mode::Shown;
        self.shown_until = Some(Instant::now() + SHOWN_AFTER_EDIT);
        self.frozen = None;
        self.hovers = [None; 2];
        crate::stream_log(&format!(
            "area editor: editing ended ({reason}), {}{}",
            self.layout
                .map_or("no layout".into(), |layout| layout.describe()),
            if self.edited { "" } else { ", nothing changed" }
        ));
        if self.edited {
            self.save("editing ended");
        } else if self.entered_with.is_none() {
            // No layout file: the box of the properties stays the layout
            self.layout = None;
        }
    }

    // A new box being pulled open is not made
    fn drop_new_box(&mut self, reason: &str) {
        // Pinches still closed do nothing until they open
        self.roles
            .reset([self.pinches[0].is_closed(), self.pinches[1].is_closed()]);
        self.two_hand = TwoHandPinch::default();
        self.two_hand_state = TwoHand::None;
        if self.new_box.take().is_some() {
            crate::stream_log(&format!("area editor: no new box: {reason}"));
        }
    }

    // A grab ends without a change: the layout as it was before it
    fn drop_grab(&mut self, reason: &str) {
        if let Some(grab) = self.grab.take() {
            if grab.id.kind.is_drag() {
                self.layout = Some(grab.start_layout);
            }
            crate::stream_log(&format!(
                "area editor: grab of {} of box #{} dropped ({reason})",
                grab.id.kind.name(),
                grab.id.box_index
            ));
        }
    }

    // A grab ends where it is (the hands are gone, the stream stopped, editing ends): a drag keeps
    // what it changed so far, as an undo step, and is saved
    fn keep_grab(&mut self, reason: &str) {
        let Some(grab) = self.grab.take() else {
            return;
        };
        let (name, index) = (grab.id.kind.name(), grab.id.box_index);
        match self.layout {
            _ if grab.removed => crate::stream_log(&format!(
                "area editor: grab of {name} of box #{index} ended ({reason}), the box was removed \
                 during it"
            )),
            Some(layout) if grab.id.kind.is_drag() && layout != grab.start_layout => {
                self.undo.push(grab.start_layout);
                self.edited = true;
                crate::stream_log(&format!(
                    "area editor: grab of {name} of box #{index} ended ({reason}), kept as it was: {}",
                    layout.describe_box(index)
                ));
                self.save("grab ended");
            }
            _ => crate::stream_log(&format!(
                "area editor: grab of {name} of box #{index} ended ({reason}), nothing changed"
            )),
        }
    }

    // Editing without a stream with Passthrough = Depth: paused (a grab kept), resumed when it is back
    // within `area_edit_grace`, else ended. Measured from the last frame with it, so that a paused
    // session (no frames at all) counts too
    fn follow_stream(&mut self, depth_passthrough: bool, now: f64) {
        if depth_passthrough {
            if self.paused {
                self.paused = false;
                crate::stream_log(&format!(
                    "area editor: editing goes on after {:.1} s without the stream (it is back)",
                    now - self.last_live
                ));
                // The time without it does not count as idle
                self.last_used = self.last_used.max(now);
            }
            self.last_live = now;
            return;
        }
        if now - self.last_live > self.grace {
            self.leave(&format!(
                "no stream with Passthrough = Depth for {:.1} s, more than area_edit_grace {:.0} s",
                now - self.last_live,
                self.grace
            ));
            return;
        }
        if !self.paused {
            self.paused = true;
            self.keep_grab("stream stopped");
            self.drop_new_box("stream stopped");
            self.pinches = [Pinch::default(); 2];
            self.hovers = [None; 2];
            crate::stream_log(&format!(
                "area editor: editing paused: no stream with Passthrough = Depth (stopped or \
                 restarting); it goes on if the stream is back within {:.0} s, else it ends (the layout \
                 is kept either way)",
                self.grace
            ));
        }
    }

    fn reset_hands(&mut self, now: f64) {
        self.pinches = [Pinch::default(); 2];
        self.hovers = [None; 2];
        self.keep_grab("no hands");
        self.drop_new_box("no hands");
        let (progress, _) = self.gesture.update(false, now);
        self.gesture_progress = progress;
        self.log_gesture(progress, false, now);
    }

    fn log_placement(&mut self, placement: Placement, anchor: Option<SavedAnchor>) {
        if placement == self.placement {
            return;
        }
        self.placement = placement;
        crate::stream_log(&match (placement, anchor) {
            (Placement::SavedAnchor, Some((position, yaw))) => format!(
                "area editor: play area placed from the anchor saved in the layout file, the code \
                 at ({:.3}, {:.3}, {:.3}) m, heading {:.1} deg (dashed until the code is seen)",
                position.x,
                position.y,
                position.z,
                alvr_graphics::yaw_log_deg(yaw)
            ),
            (Placement::Code, _) => "area editor: play area placed from the code".into(),
            _ => "area editor: no play area placed".into(),
        });
    }

    fn log_gesture(&mut self, progress: f32, fired: bool, now: f64) {
        let logged_lately = self
            .last_gesture_log
            .is_some_and(|time| time.elapsed() < GESTURE_LOG_INTERVAL);
        if fired {
            self.gesture_since = None;
            return;
        }
        match (progress > 0.0, self.gesture_since) {
            (true, None) => {
                self.gesture_since = Some(now);
                if !logged_lately {
                    self.last_gesture_log = Some(Instant::now());
                    crate::stream_log(&format!(
                        "area editor: two-hand gesture held ({:.1} s to {} editing)",
                        alvr_graphics::GESTURE_HOLD_S,
                        if self.mode == Mode::Editing {
                            "end"
                        } else {
                            "start"
                        }
                    ));
                }
            }
            (false, Some(since)) => {
                self.gesture_since = None;
                if !logged_lately {
                    self.last_gesture_log = Some(Instant::now());
                    crate::stream_log(&format!(
                        "area editor: two-hand gesture let go after {:.1} s",
                        now - since
                    ));
                }
            }
            _ => (),
        }
    }

    // Smallest and largest gaps of the pinches, every few seconds while editing
    fn log_gaps(&mut self, usable: &[Option<EditorHand>; 2], now: f64) {
        for (range, hand) in self.gap_range.iter_mut().zip(usable) {
            if let Some(gap) = hand.map(|hand| hand.gap()).filter(|gap| gap.is_finite()) {
                *range =
                    Some(range.map_or((gap, gap), |(low, high)| (low.min(gap), high.max(gap))));
            }
        }
        if now - self.last_gap_log < GAP_LOG_INTERVAL_S {
            return;
        }
        self.last_gap_log = now;
        let text = |range: Option<(f32, f32)>| {
            range.map_or("-".into(), |(low, high)| {
                format!("{:.0}/{:.0}", low * 1000.0, high * 1000.0)
            })
        };
        crate::stream_log(&format!(
            "area editor: pinch gap min/max over {GAP_LOG_INTERVAL_S:.0} s: left {} mm, right {} mm \
             (closes below {:.0}, opens above {:.0}); rays from the aim pose / estimated: left {}/{}, \
             right {}/{} updates",
            text(self.gap_range[0]),
            text(self.gap_range[1]),
            self.pinch_on * 1000.0,
            self.pinch_off * 1000.0,
            self.ray_counts[0][0],
            self.ray_counts[0][1],
            self.ray_counts[1][0],
            self.ray_counts[1][1]
        ));
        self.gap_range = [None; 2];
        self.ray_counts = [[0; 2]; 2];
    }

    // The payload of the code in use changed: its layout file is read
    fn follow_payload(&mut self, marker_region: &MarkerRegion) {
        let payload = match (marker_region.payload(), marker_region.anchor_pose()) {
            (Some(payload), _) => {
                if self
                    .loaded_for
                    .as_ref()
                    .is_some_and(|loaded| loaded.as_deref() == Some(payload))
                {
                    return;
                }
                Some(payload.to_owned())
            }
            // A code without payload is seen
            (None, Some(_)) => {
                if self.loaded_for == Some(None) {
                    return;
                }
                None
            }
            // No code seen yet and no marker_id: the code whose layout was saved last, once (a file
            // without payload is not taken: a code without payload is seen first)
            (None, None) => {
                if self.loaded_for.is_some() || self.newest_tried || !self.files {
                    return;
                }
                self.newest_tried = true;
                match newest_saved_payload() {
                    Some(payload) => Some(payload),
                    None => return,
                }
            }
        };
        // Saved under the payload it was edited for
        self.leave("another code");
        self.loaded_for = Some(payload.clone());
        self.layout = None;
        self.saved_anchor = None;
        self.undo.clear();
        self.selected = 0;
        if self.files {
            self.load(payload.as_deref());
        }
    }

    fn load(&mut self, payload: Option<&str>) {
        let name = alvr_graphics::area_file_name(payload);
        let mut found = Vec::new();
        for dir in area_dirs() {
            let path = format!("{dir}/{name}");
            match fs::read_to_string(&path) {
                Ok(text) => {
                    let modified = fs::metadata(&path).and_then(|meta| meta.modified()).ok();
                    found.push((path, modified, text));
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => (),
                Err(e) => crate::stream_log(&format!("area editor: can't read {path}: {e}")),
            }
        }
        let payload_text = payload.map_or("a code without payload".into(), |p| format!("{p:?}"));
        if found.is_empty() {
            crate::stream_log(&format!(
                "area editor: no layout file for {payload_text} ({name} in {} or {}): the box of \
                 the marker_box properties",
                area_dirs()[0],
                area_dirs()[1]
            ));
            return;
        }
        // Newest first; a file without a time last
        found.sort_by_key(|(_, modified, _)| std::cmp::Reverse(*modified));
        let count = found.len();
        for (rank, (path, modified, text)) in found.iter().enumerate() {
            let file = match alvr_graphics::parse_area_file(text) {
                Ok(file) => file,
                Err(e) => {
                    crate::stream_log(&format!("area editor: {path} does not read: {e}"));
                    continue;
                }
            };
            let age = modified
                .and_then(|time| SystemTime::now().duration_since(time).ok())
                .map_or("unknown age".into(), |age| {
                    format!("written {:.1} h ago", age.as_secs_f64() / 3600.0)
                });
            let why = match (count, rank) {
                (1, _) => "the only one found",
                (_, 0) => "the newer of the two",
                _ => "the newer one does not read",
            };
            crate::stream_log(&format!(
                "area editor: loaded {path} ({why}, {age}): {}; anchor {}",
                file.parsed.area.describe(),
                file.anchor.map_or("none".into(), |(position, yaw)| format!(
                    "({:.3}, {:.3}, {:.3}) m, heading {:.1} deg",
                    position.x,
                    position.y,
                    position.z,
                    alvr_graphics::yaw_log_deg(yaw)
                ))
            ));
            for warning in &file.parsed.warnings {
                crate::stream_log(&format!("area editor: {path}: {warning}"));
            }
            for line in &file.parsed.ignored {
                crate::stream_log(&format!("area editor: {path}: unknown, left out: {line}"));
            }
            if file.parsed.payload.as_deref() != payload {
                crate::stream_log(&format!(
                    "area editor: {path} names the payload {:?}, used for {payload_text}",
                    file.parsed.payload
                ));
            }
            crate::stream_log(
                "area editor: the marker_box_* properties and area_test are not used while a \
                 layout file is (area_file 0 to use them)",
            );
            self.layout = Some(file.parsed.area);
            self.saved_anchor = file.anchor;
            return;
        }
    }

    // Writes the layout (and the measured pose of the code) to the file of the payload, on the saver
    // thread
    fn save(&mut self, why: &str) {
        let Some(layout) = self.layout else {
            return;
        };
        if !self.files {
            crate::stream_log(&format!("area editor: not saved ({why}): area_file 0"));
            return;
        }
        let Some(payload) = self.loaded_for.clone() else {
            crate::stream_log(&format!(
                "area editor: not saved ({why}): the payload of the code is not known yet"
            ));
            return;
        };
        // The pose of the code: as measured in this run, else the one saved before
        if let Some(pose) = self.measured_anchor {
            self.saved_anchor = Some(pose);
        }
        let job = SaveJob {
            name: alvr_graphics::area_file_name(payload.as_deref()),
            text: alvr_graphics::area_file_text(&layout, payload.as_deref(), self.saved_anchor),
            summary: format!(
                "{why}, {} box{}, {}",
                layout.len(),
                if layout.len() == 1 { "" } else { "es" },
                if self.saved_anchor.is_some() {
                    "with the anchor"
                } else {
                    "no anchor measured yet"
                }
            ),
        };

        if self.saver.is_none() {
            let (sender, receiver) = mpsc::channel();
            match thread::Builder::new()
                .name("area saver".into())
                .spawn(move || save_loop(&receiver))
            {
                Ok(_) => self.saver = Some(sender),
                Err(e) => {
                    crate::stream_log(&format!("area editor: not saved, no saver thread: {e}"));
                    return;
                }
            }
        }
        if let Some(saver) = &self.saver
            && saver.send(job).is_err()
        {
            crate::stream_log("area editor: not saved, the saver thread stopped");
            self.saver = None;
        }
    }

    /// The lines of the editor over the outline of the area, stage space, `width` as the outline: the
    /// edges of the selected box, the handles, the cursors of the hands, the square of the gesture
    pub fn draw(&self, width: f32, out: &mut Vec<OverlaySegment>) {
        let Some(frame) = &self.frame else {
            return;
        };
        if self.mode == Mode::Editing {
            out.extend(alvr_graphics::box_edges(
                &frame.placed.box_region(frame.selected),
                OVERLAY_CYAN,
                width,
            ));
            for handle in &frame.handles {
                let grabbed = frame.grabbed == Some(handle.id);
                let lit = frame.lit.contains(&handle.id);
                let base = match handle.id.kind {
                    HandleKind::Remove => OVERLAY_RED,
                    HandleKind::Done => OVERLAY_GREEN,
                    HandleKind::Center if handle.id.box_index != frame.selected => OVERLAY_DIM,
                    _ => OVERLAY_WHITE,
                };
                let (color, scale) = if grabbed {
                    (OVERLAY_CYAN, LIT_SCALE)
                } else if lit {
                    (OVERLAY_AMBER, LIT_SCALE)
                } else {
                    (base, 1.0)
                };
                out.extend(alvr_graphics::handle_glyph(
                    handle, frame.eye, scale, color, width,
                ));
                // The line a side or a dome moves along (the turn has its own guide)
                if grabbed && matches!(handle.id.kind, HandleKind::Side(_) | HandleKind::Dome(_)) {
                    out.push(OverlaySegment {
                        start: handle.position - handle.normal * GUIDE_HALF_LENGTH,
                        end: handle.position + handle.normal * GUIDE_HALF_LENGTH,
                        color: OVERLAY_AMBER,
                        width: width * 0.5,
                        dashed: true,
                    });
                }
            }
            for &(cursor, pinched) in &frame.cursors {
                out.extend(alvr_graphics::cursor_glyph(
                    cursor,
                    frame.eye,
                    pinched,
                    if pinched { OVERLAY_CYAN } else { OVERLAY_WHITE },
                    width,
                ));
            }
            if let Some((start, now, snapped)) = &frame.turn {
                out.extend(alvr_graphics::turn_guide(
                    &frame.placed.area_to_world,
                    start,
                    now,
                    *snapped,
                    width * 0.6,
                ));
            }
            for (ray, end, color) in &frame.rays {
                out.extend(alvr_graphics::ray_lines(
                    ray,
                    *end,
                    frame.eye,
                    *color,
                    width * RAY_WIDTH,
                ));
            }
            // The ghost of a new box: dashed until it is made
            if let Some((new_box, color)) = frame.new_box {
                let ghost = PlacedArea {
                    area_to_world: frame.placed.area_to_world,
                    area: PlayArea::single(new_box),
                };
                out.extend(
                    alvr_graphics::box_edges(&ghost.box_region(0), color, width).map(|edge| {
                        OverlaySegment {
                            dashed: true,
                            ..edge
                        }
                    }),
                );
            }
            if let Some((position, progress)) = frame.remove {
                out.extend(alvr_graphics::progress_square(
                    position,
                    frame.eye,
                    PROGRESS_HALF,
                    progress,
                    OVERLAY_RED,
                    width,
                ));
            }
        }
        if let Some((position, progress)) = frame.gesture {
            out.extend(alvr_graphics::progress_square(
                position,
                frame.eye,
                PROGRESS_HALF,
                progress,
                OVERLAY_AMBER,
                width,
            ));
        }
    }
}

// The saver thread: writes the newest job to both folders, through a temporary file
fn save_loop(receiver: &mpsc::Receiver<SaveJob>) {
    // A folder that can't be written (/data/local/tmp most likely) is logged once, until it can
    let mut failed = [false; 2];
    while let Ok(mut job) = receiver.recv() {
        // Only the newest layout matters
        while let Ok(newer) = receiver.try_recv() {
            job = newer;
        }
        for (dir, failed) in area_dirs().into_iter().zip(failed.iter_mut()) {
            let path = format!("{dir}/{}", job.name);
            let result = fs::create_dir_all(&dir).and_then(|()| {
                let temporary = format!("{path}.tmp");
                fs::write(&temporary, &job.text)?;
                fs::rename(&temporary, &path)
            });
            match result {
                Ok(()) => {
                    *failed = false;
                    crate::stream_log(&format!("area editor: saved {path} ({})", job.summary));
                }
                Err(e) if !*failed => {
                    *failed = true;
                    crate::stream_log(&format!(
                        "area editor: could not save {path}: {e} (not logged again for this folder \
                         until a save there works)"
                    ));
                }
                Err(_) => (),
            }
        }
    }
}

// The payload of the newest layout file of the two folders, for the restore before any code is seen
// without `marker_id`. None: no file, or the newest one names no payload
fn newest_saved_payload() -> Option<String> {
    let mut newest: Option<(SystemTime, String)> = None;
    for dir in area_dirs() {
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => {
                crate::stream_log(&format!("area editor: can't list {dir}: {e}"));
                continue;
            }
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let is_layout = path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("area-") && name.ends_with(".txt"));
            let modified = entry.metadata().and_then(|meta| meta.modified()).ok();
            if let (true, Some(modified)) = (is_layout, modified)
                && newest.as_ref().is_none_or(|(time, _)| modified > *time)
            {
                newest = Some((modified, path.to_string_lossy().into_owned()));
            }
        }
    }
    let (_, path) = newest?;
    let payload = fs::read_to_string(&path)
        .ok()
        .and_then(|text| alvr_graphics::parse_area_file(&text).ok())
        .and_then(|file| file.parsed.payload);
    crate::stream_log(&match &payload {
        Some(payload) => format!(
            "area editor: no code seen yet and no marker_id: the layout of {payload:?}, saved last \
             ({path})"
        ),
        None => format!(
            "area editor: no code seen yet and no marker_id: the newest layout file ({path}) names \
             no payload, waiting for a code"
        ),
    });
    payload
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grab(kind: HandleKind) -> Grab {
        Grab {
            hand: 0,
            id: HandleId { kind, box_index: 0 },
            start_cursor: Vec3::ZERO,
            start_layout: PlayArea::single(AreaBox::new(Vec3::ZERO, Vec3::ONE)),
            start_time: 0.0,
            history: (0..30)
                .map(|i| (i as f64 * 0.01, Vec3::X * i as f32 * 0.001, i as f32 * 0.01))
                .collect(),
            max_move: 0.029,
            swept: 0.29,
            turn_from: Vec3::ZERO,
            snapped: false,
            removed: false,
            lost_reason: None,
            by_ray: false,
            far: None,
            gain: 1.0,
        }
    }

    #[test]
    fn release_goes_back_before_the_fingers_opened() {
        let grab = grab(HandleKind::Center);
        // At 0.295 s: the displacement of 0.17 s (the newest at least 0.12 s before)
        let (kept, swept) = grab.release_values(0.295, false);
        assert!((kept.x - 0.017).abs() < 1e-6, "{kept}");
        assert!((swept - 0.17).abs() < 1e-6);
        // Lost: the last one
        let last = grab.history.back().unwrap();
        assert_eq!(grab.release_values(0.29, true), (last.1, last.2));
        // A short grab: the first one
        assert_eq!(grab.release_values(0.05, false), (Vec3::ZERO, 0.0));
    }

    #[test]
    fn new_box_keeps_the_corners_before_the_fingers_opened() {
        let new_box = NewBox {
            corners: (0..30)
                .map(|i| {
                    let t = i as f64 * 0.01;
                    (t, Vec3::ZERO, Vec3::X * (0.1 + i as f32 * 0.01))
                })
                .collect(),
        };
        // At 0.295 s: the corners of 0.17 s (the newest at least 0.12 s before)
        let (_, b) = new_box.release_corners(0.295, false).unwrap();
        assert!((b.x - 0.27).abs() < 1e-6, "{b}");
        // A hand lost: the last ones
        let (_, b) = new_box.release_corners(0.295, true).unwrap();
        assert!((b.x - 0.39).abs() < 1e-6, "{b}");
        // Opened at once: the first ones
        assert_eq!(
            new_box.release_corners(0.05, false),
            Some((Vec3::ZERO, Vec3::X * 0.1))
        );
        // Never seen: none
        let empty = NewBox {
            corners: VecDeque::new(),
        };
        assert_eq!(empty.release_corners(1.0, false), None);
    }

    #[test]
    fn a_turn_grab_turns_the_box() {
        let turn = grab(HandleKind::Turn);
        let (layout, yaw, snapped) = grab_result(&turn, &Mat4::IDENTITY, Vec3::ZERO, 0.6);
        assert!((yaw - 0.6).abs() < 1e-6 && !snapped);
        assert!((layout.boxes()[0].yaw - 0.6).abs() < 1e-6);
        // Other handles drag
        let side = grab(HandleKind::Side(alvr_graphics::Side::new(0, true)));
        let (layout, moved, _) = grab_result(&side, &Mat4::IDENTITY, Vec3::X * 0.1, 0.5);
        assert!((moved - 0.1).abs() < 1e-5);
        assert_eq!(layout.boxes()[0].yaw, 0.0);
    }

    #[test]
    fn a_turn_follows_the_cursor_around_the_axis() {
        // A point at this yaw angle (degrees) and radius around the axis through the origin
        let at = |degrees: f32, radius: f32| {
            let (sin, cos) = degrees.to_radians().sin_cos();
            Vec3::new(cos * radius, 0.0, -sin * radius)
        };
        let deg = |swept: f32| swept.to_degrees();
        let mut turn = grab(HandleKind::Turn);
        turn.swept = 0.0;
        // Within the dead zone: the cursor is followed but nothing adds up
        turn.max_move = 0.005;
        turn.turn_from = at(0.0, 0.1);
        turn.follow_turn(Vec3::ZERO, at(5.0, 0.1), true);
        assert_eq!(turn.swept, 0.0);
        assert_eq!(turn.turn_from, at(5.0, 0.1));
        // Past it: from where the cursor is
        turn.max_move = 0.02;
        turn.follow_turn(Vec3::ZERO, at(15.0, 0.1), true);
        assert!((deg(turn.swept) - 10.0).abs() < 1e-3, "{}", deg(turn.swept));
        // Grabbed near the axis: the turn starts once the cursor is out of it, not stuck
        turn.swept = 0.0;
        turn.turn_from = at(0.0, 0.01);
        turn.follow_turn(Vec3::ZERO, at(90.0, 0.1), true);
        assert_eq!(turn.swept, 0.0);
        turn.follow_turn(Vec3::ZERO, at(100.0, 0.1), true);
        assert!((deg(turn.swept) - 10.0).abs() < 1e-3, "{}", deg(turn.swept));
        // Across the axis: no flip
        turn.follow_turn(Vec3::ZERO, at(100.0, 0.01), true);
        turn.follow_turn(Vec3::ZERO, at(-80.0, 0.1), true);
        assert!((deg(turn.swept) - 10.0).abs() < 1e-3, "{}", deg(turn.swept));
    }

    #[test]
    fn layout_files_go_to_both_folders() {
        let dirs = area_dirs();
        assert!(dirs[0].starts_with(passthrough::FILES_DIR));
        assert_eq!(dirs[1], "/data/local/tmp/alvr-areas");
    }
}
