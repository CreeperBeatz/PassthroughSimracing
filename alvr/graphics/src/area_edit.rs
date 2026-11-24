//! The play area editor, its pure parts: the handles and how a hand picks and drags them, the pinch of
//! a hand, the two-hand gesture that starts and ends editing, the shapes drawn for the handles, the undo
//! stack and the layout file. The client (area_editor.rs) keeps the state, reads the hands and writes
//! the files. Positions are in stage space unless the name says area frame.
//!
//! Handles of the selected box (in its own frame, which turns with its yaw): one on each side, on top
//! of its dome (drag along the normal of the side: that side moves, the opposite one stays), a smaller
//! one next to it for the dome (drag out: higher, back to the side: flat, snapped below `DOME_SNAP`), one
//! at its center (moves the box), a curved arrow beside the center (turns the box about its vertical
//! axis by the angle the cursor sweeps around that axis, with detents every `TURN_DETENT`) and "x" (held
//! for `REMOVE_HOLD_S`: removes the box; not on the last one). The other boxes show their center handle
//! only; the first pinch on a handle of another box only selects it (`pinch_action`), the next one
//! edits. Above the code: undo (takes the last change back) and done (ends editing). Buttons (undo,
//! done) act when the pinch opens with the hand usable, however far the hand moved: they have no drag.
//!
//! One hand or two (`PinchRoles`), the rule of the user: "pinch to resize is always with one hand.
//! adding a new box or exiting edit mode is always with 2". A pinch works handles (grab, select, the
//! buttons, the hold of "x") only while the other hand is open; alone on empty space it is free and does
//! nothing. Whenever both hands are pinched it is the two-hand pinch or nothing, wherever the pinches
//! closed. Two hands never close in the same frame, so when a hand closes while the other is pinched:
//! the other pinch free: both are the two-hand pinch; the other pinch on a handle since at most
//! `GRAB_YIELD_S` (near or by the ray) and its grab not out of its dead zone nor done: it gives way
//! (the grab dropped without a change, a button does nothing on release, a selection stays) and both are
//! the two-hand pinch; else (one hand resizing) the new pinch does nothing until it opens and does not
//! disturb the grab. The pinches closed when editing began do nothing until they open.
//!
//! A new box: the two-hand pinch (both pinches free; `TwoHandPinch`). While the pinch points stay within
//! `GESTURE_MAX_SPAN` of each other it may be the gesture that ends editing (its timer runs); once they
//! were farther apart in this pinch it is a new box, drawn as a ghost that spans the two points
//! (`new_box_between`: level, unturned in the area frame, each side at least `NEW_BOX_MIN_SIZE`), and
//! the timer of the gesture does not run again until a pinch opens. The box is made when a hand lets go,
//! unless its points are nearer than `NEW_BOX_MIN_SPAN` or there is no room (`new_box_refusal`).
//!
//! Picking: the cursor of a hand is the midpoint of its thumb and index tips. The handle nearest to the
//! cursor within `HANDLE_REACH` is lit; handles about as near as that one (`HANDLE_TIE`) go by the line
//! of sight from the eyes through the cursor. With none in reach, the handle nearest to the ray of the
//! hand (`HandRay`: the aim pose of the runtime, else from an estimated shoulder through the cursor)
//! within `RAY_PICK_ANGLE` is lit (handles out of reach, like the far side of the box). The lit handle
//! stays lit against small changes, and the client keeps it while the fingers close: what is lit before
//! the pinch is what is taken. A grab moves the handle by the displacement of the cursor since the pinch
//! (no jump), after a small dead zone that is caught up over `DRAG_RAMP`. A handle taken by the ray
//! follows the ray instead (`FarDrag`): a side or a dome to where the ray passes nearest to its axis, the
//! center on the sphere at the distance of the grab, the turn handle around the axis of the box where
//! the ray meets the plane of the handle; the dead zone grows with the distance (`FarDrag::gain`).
//!
//! A hand is usable while both its tips are tracked and its cursor is within `IN_VIEW_ANGLE` of the
//! view direction; the hand that holds a grab, and both hands while they pull a new box open, within
//! `GRAB_IN_VIEW_ANGLE` (an arm reaching for a far side leaves the middle of the view, a wide box takes
//! the hands apart), and their pinches open only after they were not usable for `GRAB_LOST_HOLD_S`
//! (else `HAND_LOST_HOLD_S`).

use crate::{
    AreaBox, MAX_AREA_BOXES, MAX_BOX_CENTER, MAX_BOX_HALF, OVERLAY_AMBER, OVERLAY_GREEN,
    OverlaySegment, ParsedArea, PlacedArea, PlayArea, Side,
};
use alvr_common::glam::{Mat4, Vec2, Vec3, Vec4};
use std::fmt::Write;

/// A handle within this of the cursor is picked by nearness (meters)
pub const HANDLE_REACH: f32 = 0.08;
/// Handles this much farther than the nearest one go by the line of sight too (meters)
pub const HANDLE_TIE: f32 = 0.015;
/// Without a handle in reach: the handle nearest to the ray of the hand within this (radians). Wider
/// than the 4 deg of the line of sight that picked far handles before: the ray of a hand shakes more
/// than the head
pub const RAY_PICK_ANGLE: f32 = 5.0 * std::f32::consts::PI / 180.0;
/// A lit handle stays lit up to this angle from the ray (radians)
pub const RAY_KEEP_ANGLE: f32 = 8.0 * std::f32::consts::PI / 180.0;
/// Handles within this of the best angle: the one nearer to the hand (radians)
pub const RAY_TIE_ANGLE: f32 = 1.5 * std::f32::consts::PI / 180.0;
/// The ray of a hand without the aim pose of the runtime: from a shoulder this far below, to the side
/// of and behind the eyes (meters, level frame of the head) through the cursor
pub const SHOULDER_DROP: f32 = 0.15;
pub const SHOULDER_SIDE: f32 = 0.18;
pub const SHOULDER_BACK: f32 = 0.08;
/// Drawn ray without a handle on it (meters)
pub const RAY_DRAW_LENGTH: f32 = 1.0;

/// A far drag (a handle taken by the ray, `FarDrag`): the dead zone and the ramp of a drag times the
/// distance of the handle from the hand at the grab over this, at least 1 (meters)
pub const FAR_HAND_REACH: f32 = 0.45;
/// A side or a dome follows the ray only while the ray is at least this far from parallel to its axis;
/// nearer, the hand moving along the axis moves it (times the gain), so that pulling a far side straight
/// away works (radians)
pub const FAR_AXIS_MIN_ANGLE: f32 = 20.0 * std::f32::consts::PI / 180.0;
/// The turn handle follows where the ray meets the level plane of the handle while the ray is at least
/// this steep and meets it within `FAR_PLANE_MAX_FACTOR` times the distance of the grab; else the point
/// of the ray at that distance (radians)
pub const FAR_PLANE_MIN_ANGLE: f32 = 15.0 * std::f32::consts::PI / 180.0;
pub const FAR_PLANE_MAX_FACTOR: f32 = 2.0;
/// A pinch that works a handle (a grab of any handle, near or by the ray, or a selection) gives way when
/// the other hand pinches within this time, a grab only while it has not left its dead zone nor removed
/// its box (dropped: no change, a button does nothing on release; a selection stays): both pinches are
/// then the two-hand pinch (`PinchRoles`). Two hands never close in the same frame, and hands held up
/// for a new box often have a handle at a pinch point or under a ray. Was `FAR_GRAB_YIELD_S`, for pinches
/// by the ray only (seconds)
pub const GRAB_YIELD_S: f64 = 0.5;

/// A new box (two-hand pinch): each side at least this (meters), so that a flat pull gives a box
pub const NEW_BOX_MIN_SIZE: f32 = 0.15;
/// The pinch points of a new box at least this far apart when a hand lets go, else no box is made (a
/// pinch that never opened; meters)
pub const NEW_BOX_MIN_SPAN: f32 = 0.10;
/// A lit handle in reach stays lit unless another one is nearer by this (meters), and up to this
/// share of the reach beyond it
const KEEP_DISTANCE: f32 = 0.01;
const KEEP_REACH_FACTOR: f32 = 1.25;

/// The hand moves this far before the handle moves (meters), and the handle catches up with the hand
/// over the next `DRAG_RAMP`
pub const DRAG_DEAD_ZONE: f32 = 0.008;
pub const DRAG_RAMP: f32 = 0.03;
/// A dome dragged lower than this is flat (meters)
pub const DOME_SNAP: f32 = 0.015;
/// The remove handle is held this long (seconds), the cursor within `REMOVE_HOLD_MOVE` of where the
/// pinch closed: on 2026-11-24 a held pinch drifted 23-50 mm (closing and holding the fingers moves
/// their midpoint), a hand pulled away on purpose 189 mm
pub const REMOVE_HOLD_S: f64 = 0.8;
pub const REMOVE_HOLD_MOVE: f32 = 0.08;
/// Turning a box: the yaw snaps to multiples of `TURN_DETENT` within `TURN_DETENT_WINDOW` (radians).
/// The cursor sweeps around the vertical axis of the box only farther than `TURN_MIN_RADIUS` from it
/// (meters; nearer, the angle is not steady)
pub const TURN_DETENT: f32 = 15.0 * std::f32::consts::PI / 180.0;
pub const TURN_DETENT_WINDOW: f32 = 2.0 * std::f32::consts::PI / 180.0;
pub const TURN_MIN_RADIUS: f32 = 0.03;
/// Changes that can be taken back
pub const UNDO_DEPTH: usize = 20;

/// The dome handle sits beside the side handle, this share of the half extent of the side along it,
/// within the two distances (meters)
const DOME_HANDLE_SHARE: f32 = 0.35;
const DOME_HANDLE_MIN: f32 = 0.04;
const DOME_HANDLE_MAX: f32 = 0.12;
/// "x": above the center of the box, to its right (meters, frame of the box)
const BUTTON_HEIGHT: f32 = 0.09;
const BUTTON_GAP: f32 = 0.05;
/// The turn handle: beside the center towards the +x +z corner (frame of the box), this share of the
/// shorter horizontal half extent away from the vertical axis, within the two distances (meters)
const TURN_HANDLE_SHARE: f32 = 0.6;
const TURN_HANDLE_MIN: f32 = 0.06;
const TURN_HANDLE_MAX: f32 = 0.15;
/// Most segments of the arc of the turn guide
const TURN_GUIDE_SEGMENTS: usize = 6;
/// Undo and done: above the code, to its left and right (meters, area frame)
const TOOLBAR_HEIGHT: f32 = 0.12;
const TOOLBAR_GAP: f32 = 0.05;

/// Half the size of a handle as drawn: this angle seen from the eyes, at least the minimum (meters)
const HANDLE_ANGULAR_SIZE: f32 = 0.012;
const HANDLE_MIN_RADIUS: f32 = 0.015;
/// Of the cursor of a hand (meters)
const CURSOR_RADIUS: f32 = 0.008;
/// The dot at the end of a ray: this share of the size of a handle there
const RAY_DOT_SCALE: f32 = 0.45;

/// Pinch: the gap between the surfaces of the thumb and index tips (center distance minus the two
/// radii) below `on` for this many updates closes it, above `off` for this long opens it (seconds).
/// The tips touch at a gap of 0 or less: on the Quest the joints of tips in contact were -2 to -16 mm
/// apart (2026-11-24; with on at 10 mm the pinch closed 1-2 cm before the fingers met). `off` may equal
/// `on`: the updates and the hold keep it from flickering
pub const DEFAULT_PINCH_ON: f32 = 0.0;
pub const DEFAULT_PINCH_OFF: f32 = 0.005;
pub const PINCH_ON_UPDATES: u32 = 2;
pub const PINCH_OFF_HOLD_S: f64 = 0.08;
/// A closed pinch of a hand that is not usable (not tracked, out of view) opens after this (seconds);
/// of the hand that holds a grab (and of both hands pulling a new box open) after `GRAB_LOST_HOLD_S`
pub const HAND_LOST_HOLD_S: f64 = 0.3;
pub const GRAB_LOST_HOLD_S: f64 = 1.0;
/// On release the drag goes back to where it was this long before the release was confirmed: the
/// fingers moved the cursor while they opened (seconds)
pub const RELEASE_LOOKBACK_S: f64 = 0.12;

/// The gesture that starts and ends editing: held this long (seconds), a drop of the pose shorter
/// than the tolerance is ignored
pub const GESTURE_HOLD_S: f64 = 1.5;
pub const GESTURE_TOLERANCE_S: f64 = 0.15;
/// The two pinch points at most this far apart (meters)
pub const GESTURE_MAX_SPAN: f32 = 0.2;
/// Each pinch point this far from the eyes (meters)
pub const GESTURE_MIN_DISTANCE: f32 = 0.15;
pub const GESTURE_MAX_DISTANCE: f32 = 0.65;
/// Each pinch point within this angle of the view direction (radians)
pub const GESTURE_CONE: f32 = 45.0 * std::f32::consts::PI / 180.0;
/// The palms face away from the eyes: cosine of the angle between the normal out of the palm and the
/// direction from the eyes to the palm, at least this
pub const PALM_AWAY_COS: f32 = 0.5;
/// A hand is used only while its cursor is within this angle of the view direction: the runtime
/// reports hands as tracked also out of view (radians). The hand that holds a grab, and both hands
/// pulling a new box open: within `GRAB_IN_VIEW_ANGLE` (still in the field of view of the Quest 3,
/// about 110 deg wide)
pub const IN_VIEW_ANGLE: f32 = 50.0 * std::f32::consts::PI / 180.0;
pub const GRAB_IN_VIEW_ANGLE: f32 = 75.0 * std::f32::consts::PI / 180.0;

// Colors of the editor, as in the stream image (sRGB values)
pub const OVERLAY_CYAN: Vec4 = Vec4::new(0.2, 0.9, 1.0, 1.0);
pub const OVERLAY_RED: Vec4 = Vec4::new(1.0, 0.2, 0.15, 1.0);
pub const OVERLAY_DIM: Vec4 = Vec4::new(0.45, 0.45, 0.45, 1.0);

/// What a handle does
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandleKind {
    /// Resizes the box: the side moves along its normal
    Side(Side),
    /// The height of the dome of a side
    Dome(Side),
    /// Moves the box
    Center,
    /// Turns the box about its vertical axis
    Turn,
    /// Held: removes this box
    Remove,
    /// Takes the last change back
    Undo,
    /// Ends editing
    Done,
}

impl HandleKind {
    /// For the log
    pub fn name(self) -> String {
        match self {
            Self::Side(side) => format!("side {}", side.name()),
            Self::Dome(side) => format!("dome {}", side.name()),
            Self::Center => "center".into(),
            Self::Turn => "turn".into(),
            Self::Remove => "remove".into(),
            Self::Undo => "undo".into(),
            Self::Done => "done".into(),
        }
    }

    /// Dragging it changes the layout
    pub fn is_drag(self) -> bool {
        matches!(
            self,
            Self::Side(_) | Self::Dome(_) | Self::Center | Self::Turn
        )
    }

    /// A button: it acts when the pinch opens, wherever the hand went (undo, done)
    pub fn is_button(self) -> bool {
        matches!(self, Self::Undo | Self::Done)
    }

    /// It belongs to a box (pinching it selects that box)
    pub fn of_box(self) -> bool {
        !matches!(self, Self::Undo | Self::Done)
    }
}

/// What a pinch on a lit handle does
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PinchAction {
    /// A handle of a box that is not the selected one: that box is selected, nothing else happens in
    /// this pinch (no drag, no undo step, nothing saved)
    Select(usize),
    /// The handle is taken
    Grab,
}

/// What a pinch on handle `id` does with box `selected` selected
pub fn pinch_action(id: HandleId, selected: usize) -> PinchAction {
    if id.kind.of_box() && id.box_index != selected {
        PinchAction::Select(id.box_index)
    } else {
        PinchAction::Grab
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HandleId {
    pub kind: HandleKind,
    /// The box it belongs to (0 for undo and done)
    pub box_index: usize,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Handle {
    pub id: HandleId,
    /// Stage space
    pub position: Vec3,
    /// Of side and dome handles the outward normal of the side (stage space), else zero
    pub normal: Vec3,
}

/// Where the dome handle of a side sits from its side handle (area frame): beside it, up the side
/// for the sides that stand, along the longer edge for the top and the bottom
pub fn dome_handle_offset(area_box: &AreaBox, side: Side) -> Vec3 {
    let along = if side.axis != 1 {
        1
    } else if area_box.half.x >= area_box.half.z {
        0
    } else {
        2
    };
    let mut offset = Vec3::ZERO;
    offset[along] =
        (DOME_HANDLE_SHARE * area_box.half[along]).clamp(DOME_HANDLE_MIN, DOME_HANDLE_MAX);
    area_box.turn(offset)
}

/// Where the turn handle of a box sits from its center (area frame): at its height, towards the +x +z
/// corner of its own frame, away from the side handles
pub fn turn_handle_offset(area_box: &AreaBox) -> Vec3 {
    let radius = (TURN_HANDLE_SHARE * area_box.half.x.min(area_box.half.z))
        .clamp(TURN_HANDLE_MIN, TURN_HANDLE_MAX);
    area_box.turn(Vec3::new(1.0, 0.0, 1.0) * (radius * std::f32::consts::FRAC_1_SQRT_2))
}

/// The handles while editing, stage space. Of the selected box all of them, of the others the center
/// only; undo (when there is something to take back) and done above the code
pub fn edit_handles(placed: &PlacedArea, selected: usize, can_undo: bool) -> Vec<Handle> {
    let boxes = placed.area.boxes();
    let selected = selected.min(boxes.len().saturating_sub(1));
    let handle = |kind, box_index, position: Vec3, normal: Vec3| Handle {
        id: HandleId { kind, box_index },
        position: placed.area_to_world.transform_point3(position),
        normal: placed.area_to_world.transform_vector3(normal),
    };

    let mut handles = Vec::new();
    for (index, area_box) in boxes.iter().enumerate() {
        if index != selected {
            handles.push(handle(
                HandleKind::Center,
                index,
                area_box.center,
                Vec3::ZERO,
            ));
            continue;
        }
        for side in Side::ALL {
            let top = area_box.side_handle(side);
            let normal = area_box.side_normal(side);
            handles.push(handle(HandleKind::Side(side), index, top, normal));
            handles.push(handle(
                HandleKind::Dome(side),
                index,
                top + dome_handle_offset(area_box, side),
                normal,
            ));
        }
        handles.push(handle(
            HandleKind::Center,
            index,
            area_box.center,
            Vec3::ZERO,
        ));
        handles.push(handle(
            HandleKind::Turn,
            index,
            area_box.center + turn_handle_offset(area_box),
            Vec3::ZERO,
        ));
        if boxes.len() > 1 {
            handles.push(handle(
                HandleKind::Remove,
                index,
                area_box.to_area(Vec3::new(BUTTON_GAP, BUTTON_HEIGHT, 0.0)),
                Vec3::ZERO,
            ));
        }
    }
    if can_undo {
        let position = Vec3::new(-TOOLBAR_GAP, TOOLBAR_HEIGHT, 0.0);
        handles.push(handle(HandleKind::Undo, 0, position, Vec3::ZERO));
    }
    let position = Vec3::new(TOOLBAR_GAP, TOOLBAR_HEIGHT, 0.0);
    handles.push(handle(HandleKind::Done, 0, position, Vec3::ZERO));

    handles
}

/// A ray of a hand, stage space, like the system pointer of the Quest
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HandRay {
    pub origin: Vec3,
    /// Unit
    pub direction: Vec3,
}

impl HandRay {
    /// None: a direction of zero length or not finite
    pub fn new(origin: Vec3, direction: Vec3) -> Option<Self> {
        let direction = direction.normalize_or_zero();
        (direction != Vec3::ZERO && direction.is_finite() && origin.is_finite())
            .then_some(Self { origin, direction })
    }

    pub fn at(&self, distance: f32) -> Vec3 {
        self.origin + self.direction * distance
    }

    /// Angle between the ray and the direction from its origin to `point` (radians)
    pub fn angle_to(&self, point: Vec3) -> f32 {
        self.direction
            .angle_between((point - self.origin).normalize_or(self.direction))
    }

    /// The point of the ray nearest to `point` (not behind its origin)
    pub fn nearest_point(&self, point: Vec3) -> Vec3 {
        self.at((point - self.origin).dot(self.direction).max(0.0))
    }
}

/// A handle a hand points at
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pick {
    pub id: HandleId,
    /// Out of reach of the cursor: by the ray of the hand
    pub far: bool,
}

/// The handle a hand points at (see the module), `ray`: of the hand, `current`: the one lit until now.
/// None: none
pub fn pick_handle(
    handles: &[Handle],
    eye: Vec3,
    cursor: Vec3,
    ray: &HandRay,
    current: Option<HandleId>,
) -> Option<Pick> {
    let sight = (cursor - eye).normalize_or_zero();
    if sight == Vec3::ZERO || !sight.is_finite() {
        return None;
    }
    let sight_angle =
        |handle: &Handle| sight.angle_between((handle.position - eye).normalize_or(sight));
    let distance = |handle: &Handle| handle.position.distance(cursor);
    let current = current.and_then(|id| handles.iter().find(|handle| handle.id == id));
    let near = |handle: &Handle| Pick {
        id: handle.id,
        far: false,
    };

    // In reach: the nearest, about as near ones by the line of sight
    let nearest = handles
        .iter()
        .map(distance)
        .filter(|&d| d < HANDLE_REACH)
        .min_by(f32::total_cmp);
    if let Some(nearest) = nearest {
        let chosen = handles
            .iter()
            .filter(|handle| distance(handle) <= nearest + HANDLE_TIE)
            .min_by(|a, b| sight_angle(a).total_cmp(&sight_angle(b)))?;
        if let Some(current) = current
            && distance(current) < HANDLE_REACH * KEEP_REACH_FACTOR
            && distance(current) <= distance(chosen) + KEEP_DISTANCE
        {
            return Some(near(current));
        }
        return Some(near(chosen));
    }
    // The lit one a little out of reach stays
    if let Some(current) = current
        && distance(current) < HANDLE_REACH * KEEP_REACH_FACTOR
    {
        return Some(near(current));
    }

    // Out of reach: by the ray of the hand, about as good ones by the distance from the hand
    let angle = |handle: &Handle| ray.angle_to(handle.position);
    let best = handles
        .iter()
        .map(angle)
        .filter(|&a| a < RAY_PICK_ANGLE)
        .min_by(f32::total_cmp);
    let chosen = best.and_then(|best| {
        handles
            .iter()
            .filter(|handle| angle(handle) <= best + RAY_TIE_ANGLE)
            .min_by(|a, b| {
                a.position
                    .distance(ray.origin)
                    .total_cmp(&b.position.distance(ray.origin))
            })
    });
    let far = |handle: &Handle| Pick {
        id: handle.id,
        far: true,
    };
    if let Some(current) = current
        && angle(current) < RAY_KEEP_ANGLE
        && chosen.is_none_or(|chosen| angle(current) <= angle(chosen) + RAY_TIE_ANGLE)
    {
        return Some(far(current));
    }

    chosen.map(far)
}

/// The displacement a drag applies for a displacement of the hand: nothing within the dead zone,
/// then faster than the hand until it caught up `DRAG_RAMP` later, then the same
pub fn drag_displacement(hand: Vec3) -> Vec3 {
    let length = hand.length();
    // Also NaN
    if length.is_nan() || length <= DRAG_DEAD_ZONE {
        return Vec3::ZERO;
    }
    let caught_up = DRAG_DEAD_ZONE + DRAG_RAMP;
    let applied = if length >= caught_up {
        length
    } else {
        (length - DRAG_DEAD_ZONE) * caught_up / DRAG_RAMP
    };
    hand * (applied / length)
}

/// `drag_displacement` with the dead zone and the ramp `gain` times as long (a far drag, see
/// `FarDrag::gain`; 1: the same)
pub fn scaled_drag_displacement(moved: Vec3, gain: f32) -> Vec3 {
    let gain = if gain.is_finite() { gain.max(1.0) } else { 1.0 };
    drag_displacement(moved / gain) * gain
}

/// The layout `start` with handle `id` dragged by `displacement` (area frame, after
/// `drag_displacement`), and the value it took for the log: how far the side moved (along the normal of
/// the side of the box as it is turned), the height of the dome, how far the box moved (meters). Other
/// handles change nothing (turning: `apply_turn`)
pub fn apply_drag(start: &PlayArea, id: HandleId, displacement: Vec3) -> (PlayArea, f32) {
    let mut area = *start;
    let index = id.box_index;
    let Some(area_box) = start.boxes().get(index) else {
        return (area, 0.0);
    };
    let value = match id.kind {
        HandleKind::Side(side) => {
            area.drag_side(index, side, displacement.dot(area_box.side_normal(side)))
        }
        HandleKind::Dome(side) => {
            let height = area_box.dome_height(side) + displacement.dot(area_box.side_normal(side));
            area.set_dome(index, side, if height < DOME_SNAP { 0.0 } else { height })
        }
        HandleKind::Center => area.move_box(index, displacement).length(),
        _ => 0.0,
    };

    (area, value)
}

/// The angle a point sweeps around the vertical axis through `center` going from `from` to `to`
/// (radians, counter-clockwise seen from above like a yaw, -pi..pi). None: one of them is within
/// `TURN_MIN_RADIUS` of the axis
pub fn sweep_angle(center: Vec3, from: Vec3, to: Vec3) -> Option<f32> {
    // The angle of a yaw: +x is 0, -z (where a yaw of pi/2 turns +x) is pi/2
    let angle = |point: Vec3| {
        let offset = point - center;
        (Vec2::new(offset.x, offset.z).length() >= TURN_MIN_RADIUS)
            .then(|| (-offset.z).atan2(offset.x))
    };
    Some(crate::wrap_angle(angle(to)? - angle(from)?))
}

/// The yaw of a box turned from `start` by `swept` (radians), snapped to a multiple of `TURN_DETENT`
/// within `TURN_DETENT_WINDOW`, and whether it snapped
pub fn turned_yaw(start: f32, swept: f32) -> (f32, bool) {
    let yaw = crate::wrap_angle(start + swept);
    let detent = (yaw / TURN_DETENT).round() * TURN_DETENT;
    if (yaw - detent).abs() <= TURN_DETENT_WINDOW {
        (crate::wrap_angle(detent), true)
    } else {
        (yaw, false)
    }
}

/// The layout `start` with box `index` turned by `swept` (radians, see `turned_yaw`) about its vertical
/// axis, its yaw (radians) and whether it is on a detent
pub fn apply_turn(start: &PlayArea, index: usize, swept: f32) -> (PlayArea, f32, bool) {
    let mut area = *start;
    let Some(area_box) = start.boxes().get(index) else {
        return (area, 0.0, false);
    };
    let (yaw, snapped) = turned_yaw(area_box.yaw, swept);
    let yaw = area.turn_box(index, yaw);
    (area, yaw, snapped)
}

/// Where the ray passes nearest to the line through `point` along `axis` (unit): the distance along the
/// axis from `point`. None: the ray within `FAR_AXIS_MIN_ANGLE` of parallel to it, or nearest behind
/// its origin
pub fn ray_axis_param(ray: &HandRay, point: Vec3, axis: Vec3) -> Option<f32> {
    let b = ray.direction.dot(axis);
    if b.abs() > FAR_AXIS_MIN_ANGLE.cos() || !b.is_finite() {
        return None;
    }
    let w = ray.origin - point;
    let (d, e) = (ray.direction.dot(w), axis.dot(w));
    let denom = 1.0 - b * b;
    let along_ray = (b * e - d) / denom;
    (along_ray >= 0.0).then(|| (e - b * d) / denom)
}

/// What a far drag gives in an update (stage space)
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FarStep {
    /// Displacement since the grab, to apply like the displacement of the cursor of a near drag (with
    /// `scaled_drag_displacement` and `FarDrag::gain`). Of a turn: of `point` from where it was at the
    /// grab (added up over changes of the way it is found: zero at the grab, no jump at a change)
    pub moved: Vec3,
    /// Of a turn: the point that sweeps around the axis of the box
    pub point: Vec3,
    /// Of a turn: `point` was found the same way as in the update before (else the client takes it as
    /// the new start of the sweep: no jump when the way changes)
    pub continuous: bool,
}

/// A handle taken by the ray of a hand (out of reach): it follows the ray, not the small motion of the
/// hand. A side or a dome: where the ray passes nearest to its axis (the line through the handle along
/// the normal of the side), added up per update; while the ray is near parallel to that axis the motion
/// of the cursor along it times the gain. The center: the point of the ray at the distance of the grab
/// (a sphere around the hand). The turn handle: where the ray meets the level plane of the handle, else
/// the point at the distance of the grab (see `FAR_PLANE_MIN_ANGLE`). Relative to the grab: no jump
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FarDrag {
    kind: HandleKind,
    /// The handle at the grab
    start: Vec3,
    normal: Vec3,
    /// From the origin of the ray to the handle at the grab (meters)
    distance: f32,
    /// The point of the ray at `distance` at the grab; of a turn the point it follows at the grab,
    /// moved along at each change of the way it is found (`moved` stays where it was)
    grab_point: Vec3,
    /// See `gain`
    gain: f32,
    last_cursor: Vec3,
    last_axis: Option<f32>,
    // Of a turn: the way the last point was found (true: on the plane)
    last_on_plane: Option<bool>,
    moved: Vec3,
}

impl FarDrag {
    /// None: not a handle that is dragged (buttons, remove)
    pub fn new(handle: &Handle, ray: &HandRay, cursor: Vec3) -> Option<Self> {
        if !handle.id.kind.is_drag() {
            return None;
        }
        let distance = ray.origin.distance(handle.position);
        let mut drag = Self {
            kind: handle.id.kind,
            start: handle.position,
            normal: handle.normal.normalize_or_zero(),
            distance,
            grab_point: ray.at(distance),
            gain: (distance / FAR_HAND_REACH).max(1.0),
            last_cursor: cursor,
            last_axis: None,
            last_on_plane: None,
            moved: Vec3::ZERO,
        };
        drag.last_axis = drag.axis_param(ray);
        if drag.kind == HandleKind::Turn {
            let (point, on_plane) = drag.turn_point(ray);
            drag.grab_point = point;
            drag.last_on_plane = Some(on_plane);
        }
        Some(drag)
    }

    /// The dead zone and the ramp of the drag are this many times as long as for a hand at the handle:
    /// the distance of the handle from the hand at the grab over `FAR_HAND_REACH`, at least 1
    pub fn gain(&self) -> f32 {
        self.gain
    }

    fn axis_param(&self, ray: &HandRay) -> Option<f32> {
        matches!(self.kind, HandleKind::Side(_) | HandleKind::Dome(_))
            .then(|| ray_axis_param(ray, self.start, self.normal))
            .flatten()
    }

    // The point a turn follows and whether it is on the plane
    fn turn_point(&self, ray: &HandRay) -> (Vec3, bool) {
        let dy = ray.direction.y;
        if dy.abs() >= FAR_PLANE_MIN_ANGLE.sin() {
            let t = (self.start.y - ray.origin.y) / dy;
            if t > 0.0 && t <= FAR_PLANE_MAX_FACTOR * self.distance {
                return (ray.at(t), true);
            }
        }
        (ray.at(self.distance), false)
    }

    /// The drag for the ray and the cursor of the hand in this update
    pub fn follow(&mut self, ray: &HandRay, cursor: Vec3) -> FarStep {
        let step = match self.kind {
            HandleKind::Side(_) | HandleKind::Dome(_) => {
                let param = self.axis_param(ray);
                let delta = match (param, self.last_axis) {
                    (Some(now), Some(last)) => now - last,
                    // Near parallel: the hand along the axis, faster by the gain
                    (None, _) => (cursor - self.last_cursor).dot(self.normal) * self.gain,
                    // Out of parallel again: from here on, no jump
                    (Some(_), None) => 0.0,
                };
                if delta.is_finite() {
                    self.moved += self.normal * delta;
                }
                self.last_axis = param;
                FarStep {
                    moved: self.moved,
                    point: self.start + self.moved,
                    continuous: true,
                }
            }
            HandleKind::Turn => {
                let (point, on_plane) = self.turn_point(ray);
                let continuous = self.last_on_plane == Some(on_plane);
                self.last_on_plane = Some(on_plane);
                if point.is_finite() {
                    if !continuous {
                        self.grab_point = point - self.moved;
                    }
                    self.moved = point - self.grab_point;
                }
                FarStep {
                    moved: self.moved,
                    point,
                    continuous,
                }
            }
            _ => {
                let point = ray.at(self.distance);
                FarStep {
                    moved: point - self.grab_point,
                    point,
                    continuous: true,
                }
            }
        };
        self.last_cursor = cursor;
        if step.moved.is_finite() {
            step
        } else {
            FarStep {
                moved: Vec3::ZERO,
                point: self.start,
                continuous: false,
            }
        }
    }
}

/// What is drawn while a box is turned (stage space): a dashed line from its center through the turn
/// handle as it was at the grab, a line through it now (green on a detent) and the arc between them at
/// the radius of the handle (at most `TURN_GUIDE_SEGMENTS`). `start` and `now`: the box at the grab and
/// now (area frame)
pub fn turn_guide(
    area_to_world: &Mat4,
    start: &AreaBox,
    now: &AreaBox,
    snapped: bool,
    width: f32,
) -> Vec<OverlaySegment> {
    let center = now.center;
    let offset = turn_handle_offset(start);
    // A little beyond the handle
    let reach = 1.6;
    let swept = crate::wrap_angle(now.yaw - start.yaw);
    let turned = |angle: f32| {
        let (sin, cos) = angle.sin_cos();
        Vec3::new(
            cos * offset.x + sin * offset.z,
            0.0,
            cos * offset.z - sin * offset.x,
        )
    };
    let segment = |a: Vec3, b: Vec3, color: Vec4, dashed: bool| OverlaySegment {
        start: area_to_world.transform_point3(a),
        end: area_to_world.transform_point3(b),
        color,
        width,
        dashed,
    };
    let mut lines = vec![
        segment(center, center + offset * reach, OVERLAY_DIM, true),
        segment(
            center,
            center + turned(swept) * reach,
            if snapped {
                OVERLAY_GREEN
            } else {
                OVERLAY_AMBER
            },
            false,
        ),
    ];
    if swept.abs() > 1e-3 {
        let count = ((swept.abs() / (std::f32::consts::PI / 8.0)).ceil() as usize)
            .clamp(1, TURN_GUIDE_SEGMENTS);
        lines.extend((0..count).map(|i| {
            let a = swept * i as f32 / count as f32;
            let b = swept * (i + 1) as f32 / count as f32;
            segment(center + turned(a), center + turned(b), OVERLAY_AMBER, false)
        }));
    }
    lines
}

/// The box a two-hand pinch makes (area frame): the two pinch points at opposite corners, level and
/// unturned in the area frame (the frame of the code, which is mounted square on the rig; the yaw of
/// the selected box would depend on which box happened to be edited last), each side at least
/// `NEW_BOX_MIN_SIZE` around the middle of the two points, at most `MAX_BOX_HALF` half, no domes
pub fn new_box_between(a: Vec3, b: Vec3) -> AreaBox {
    let half = ((a - b).abs() * 0.5)
        .max(Vec3::splat(0.5 * NEW_BOX_MIN_SIZE))
        .min(Vec3::splat(MAX_BOX_HALF));
    AreaBox::new((a + b) * 0.5, half)
}

/// Why the box of a two-hand pinch with the pinch points `a` and `b` (area frame) can't be made, for
/// the log. None: it can. The cue: a ghost too small is drawn dim, one without room red
pub fn new_box_refusal(area: &PlayArea, a: Vec3, b: Vec3) -> Option<String> {
    let span = a.distance(b);
    if span.is_nan() || span < NEW_BOX_MIN_SPAN {
        return Some(format!(
            "the pinch points were {:.0} mm apart, a new box needs {:.0} mm",
            span * 1000.0,
            NEW_BOX_MIN_SPAN * 1000.0
        ));
    }
    if area.len() >= MAX_AREA_BOXES {
        return Some(format!(
            "{MAX_AREA_BOXES} boxes already, at most {MAX_AREA_BOXES}"
        ));
    }
    let center = new_box_between(a, b).center;
    if !center.is_finite() || center.abs().max_element() > MAX_BOX_CENTER {
        return Some(format!(
            "its center would be more than {MAX_BOX_CENTER:.0} m from the code along an axis of its              frame"
        ));
    }
    None
}

/// What the two-hand pinch (both pinches free, `PinchRoles`) does while editing
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TwoHand {
    /// Not both pinches free
    None,
    /// The pinch points have stayed within `GESTURE_MAX_SPAN`: maybe the gesture that ends editing
    Undecided,
    /// They were farther apart in this pinch: a new box
    Adding,
}

/// The two-hand pinch while editing (see the module): once the pinch points were farther apart than
/// `GESTURE_MAX_SPAN` it is a new box until a pinch opens. The client runs the timer of the gesture
/// that ends editing only while this is not `Adding`, and cancels it (`HoldTimer::cancel`) while it is
#[derive(Clone, Copy, Debug, Default)]
pub struct TwoHandPinch {
    adding: bool,
}

impl TwoHandPinch {
    /// `both_free`: both pinches closed and free (`PinchRoles::two_hand`). `span`: the distance between
    /// the two pinch points (meters)
    pub fn update(&mut self, both_free: bool, span: f32) -> TwoHand {
        if !both_free {
            self.adding = false;
            return TwoHand::None;
        }
        if span > GESTURE_MAX_SPAN {
            self.adding = true;
        }
        if self.adding {
            TwoHand::Adding
        } else {
            TwoHand::Undecided
        }
    }
}

/// What the closed pinch of a hand is for while editing (`PinchRoles`)
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum PinchRole {
    /// Open
    #[default]
    Open,
    /// Closed on empty space with the other hand open, or one of the two-hand pinch: alone it does
    /// nothing
    Free,
    /// Works a handle (a grab, a selection, a button, the hold of "x") since `since` (seconds);
    /// `by_ray`: picked by the ray of the hand (out of reach)
    Handle { since: f64, by_ray: bool },
    /// Does nothing until it opens: it closed while the other hand worked a handle that no longer
    /// gives way (or did nothing itself), or it was closed when editing began
    Idle,
}

/// What a pinch that closes while editing does (`PinchRoles::close`)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PinchStart {
    /// The other hand is open, this one closed on empty space: free (alone it does nothing)
    Free,
    /// The other hand is open, this one closed on a handle: what it does with it (`pinch_action`)
    Handle(PinchAction),
    /// The other pinch is free, or gives way: both are free, the two-hand pinch (a new box or the
    /// gesture that ends editing), whatever this one closed on. `gives_way`: the other pinch worked a
    /// handle and gives it up (its grab is dropped without a change, a button does nothing on release,
    /// a selection stays)
    TwoHand { gives_way: bool },
    /// Does nothing until it opens (no grab, no selection, no button, not free): the other hand works a
    /// handle that no longer gives way (resizing with one hand), or its pinch does nothing either
    Idle,
}

/// The pinches of both hands while editing, by the rule of the user: "pinch to resize is always with
/// one hand. adding a new box or exiting edit mode is always with 2". Whenever both hands are pinched
/// it is the two-hand pinch (a new box, or the gesture that ends editing) or nothing; one pinch works
/// handles only while the other hand is open, and one pinch alone never adds a box or ends editing.
/// Two hands never close in the same frame, so the order decides (see `close`)
#[derive(Clone, Copy, Debug, Default)]
pub struct PinchRoles {
    roles: [PinchRole; 2],
}

impl PinchRoles {
    /// Editing begins (or a new box is dropped, editing pauses): a pinch closed then (the gesture that
    /// started editing) does nothing until it opens, so that it starts no box and does not end editing
    /// again
    pub fn reset(&mut self, closed: [bool; 2]) {
        self.roles = closed.map(|closed| {
            if closed {
                PinchRole::Idle
            } else {
                PinchRole::Open
            }
        });
    }

    pub fn role(&self, hand: usize) -> PinchRole {
        self.roles[hand]
    }

    /// Every update, whether the pinch of `hand` is closed: an open pinch has no role
    pub fn follow(&mut self, hand: usize, closed: bool) {
        if !closed {
            self.roles[hand] = PinchRole::Open;
        }
    }

    /// Both pinches are free: the two-hand pinch
    pub fn two_hand(&self) -> bool {
        self.roles == [PinchRole::Free; 2]
    }

    /// The pinch of `hand` closes. `pick`: the handle it closed on (None: empty space), `selected`: the
    /// selected box, `other_acted`: the grab of the other hand has left its dead zone or removed its
    /// box, `now`: seconds. With the other hand open it works the handle (or is free); with the other
    /// pinch free both are the two-hand pinch; with the other pinch on a handle since at most
    /// `GRAB_YIELD_S` and its grab not acted, that one gives way and both are the two-hand pinch; else
    /// it does nothing until it opens (one hand is resizing: the other does not disturb it)
    pub fn close(
        &mut self,
        hand: usize,
        pick: Option<Pick>,
        selected: usize,
        other_acted: bool,
        now: f64,
    ) -> PinchStart {
        let other = 1 - hand;
        let start = match self.roles[other] {
            PinchRole::Open => match pick {
                None => PinchStart::Free,
                Some(pick) => PinchStart::Handle(pinch_action(pick.id, selected)),
            },
            PinchRole::Free => PinchStart::TwoHand { gives_way: false },
            PinchRole::Handle { since, .. } if !other_acted && now - since <= GRAB_YIELD_S => {
                PinchStart::TwoHand { gives_way: true }
            }
            PinchRole::Handle { .. } | PinchRole::Idle => PinchStart::Idle,
        };
        match start {
            PinchStart::Free => self.roles[hand] = PinchRole::Free,
            PinchStart::Handle(_) => {
                self.roles[hand] = PinchRole::Handle {
                    since: now,
                    by_ray: pick.is_some_and(|pick| pick.far),
                }
            }
            PinchStart::TwoHand { .. } => self.roles = [PinchRole::Free; 2],
            PinchStart::Idle => self.roles[hand] = PinchRole::Idle,
        }
        start
    }
}

/// Half the size of a handle as drawn (meters): larger far away, so it stays visible
pub fn handle_radius(position: Vec3, eye: Vec3) -> f32 {
    (HANDLE_ANGULAR_SIZE * position.distance(eye)).max(HANDLE_MIN_RADIUS)
}

// Right and up of a plane facing the eyes at `position`
fn billboard(position: Vec3, eye: Vec3) -> (Vec3, Vec3) {
    let view = (position - eye).normalize_or(Vec3::NEG_Z);
    let right = view.cross(Vec3::Y).normalize_or(Vec3::X);
    (right, right.cross(view))
}

// Polylines in the plane facing the eyes, in units of `radius`
fn glyph(
    position: Vec3,
    eye: Vec3,
    radius: f32,
    lines: &[&[Vec2]],
    color: Vec4,
    width: f32,
) -> Vec<OverlaySegment> {
    let (right, up) = billboard(position, eye);
    let at = |point: Vec2| position + (right * point.x + up * point.y) * radius;
    lines
        .iter()
        .flat_map(|line| line.windows(2))
        .map(|pair| OverlaySegment {
            start: at(pair[0]),
            end: at(pair[1]),
            color,
            width,
            dashed: false,
        })
        .collect()
}

/// The lines of a handle, facing the eyes, `scale` times its size: a square (side), a triangle
/// pointing out of the side (dome, smaller), a diamond (center), a curved arrow (turn), a cross
/// (remove), an arrow to the left (undo), a check mark (done). At most 4 segments
pub fn handle_glyph(
    handle: &Handle,
    eye: Vec3,
    scale: f32,
    color: Vec4,
    width: f32,
) -> Vec<OverlaySegment> {
    let radius = handle_radius(handle.position, eye) * scale;
    let p = Vec2::new;
    match handle.id.kind {
        HandleKind::Side(_) => {
            let square = [
                p(-1.0, -1.0),
                p(1.0, -1.0),
                p(1.0, 1.0),
                p(-1.0, 1.0),
                p(-1.0, -1.0),
            ];
            glyph(handle.position, eye, radius, &[&square], color, width)
        }
        HandleKind::Dome(_) => {
            // Pointing along the normal as seen, up when it points at the eyes
            let (right, up) = billboard(handle.position, eye);
            let seen = Vec2::new(handle.normal.dot(right), handle.normal.dot(up));
            let tip = if seen.length() > 0.2 {
                seen.normalize()
            } else {
                Vec2::Y
            };
            let across = tip.perp();
            let triangle = [
                tip * 0.7,
                -tip * 0.5 + across * 0.6,
                -tip * 0.5 - across * 0.6,
                tip * 0.7,
            ];
            glyph(handle.position, eye, radius, &[&triangle], color, width)
        }
        HandleKind::Center => {
            let diamond = [
                p(0.0, -1.3),
                p(1.3, 0.0),
                p(0.0, 1.3),
                p(-1.3, 0.0),
                p(0.0, -1.3),
            ];
            glyph(handle.position, eye, radius, &[&diamond], color, width)
        }
        HandleKind::Turn => {
            // Three quarters of a circle and the barb of the arrow at its end
            let at = |degrees: f32| {
                let (sin, cos) = degrees.to_radians().sin_cos();
                p(cos, sin)
            };
            let tip = at(-45.0);
            let arc = [at(225.0), at(135.0), at(45.0), tip];
            let barb = [tip, tip + p(-0.7, 0.05)];
            glyph(handle.position, eye, radius, &[&arc, &barb], color, width)
        }
        HandleKind::Remove => glyph(
            handle.position,
            eye,
            radius,
            &[&[p(-0.8, -0.8), p(0.8, 0.8)], &[p(-0.8, 0.8), p(0.8, -0.8)]],
            color,
            width,
        ),
        HandleKind::Undo => glyph(
            handle.position,
            eye,
            radius,
            &[
                &[p(1.0, 0.0), p(-1.0, 0.0)],
                &[p(-0.4, 0.6), p(-1.0, 0.0), p(-0.4, -0.6)],
            ],
            color,
            width,
        ),
        HandleKind::Done => glyph(
            handle.position,
            eye,
            radius,
            &[&[p(-0.9, 0.1), p(-0.3, -0.6), p(1.0, 0.8)]],
            color,
            width,
        ),
    }
}

/// The cursor of a hand: a small plus, a small square while pinched
pub fn cursor_glyph(
    position: Vec3,
    eye: Vec3,
    pinched: bool,
    color: Vec4,
    width: f32,
) -> Vec<OverlaySegment> {
    let p = Vec2::new;
    if pinched {
        let square = [
            p(-0.6, -0.6),
            p(0.6, -0.6),
            p(0.6, 0.6),
            p(-0.6, 0.6),
            p(-0.6, -0.6),
        ];
        glyph(position, eye, CURSOR_RADIUS, &[&square], color, width)
    } else {
        glyph(
            position,
            eye,
            CURSOR_RADIUS,
            &[&[p(-1.0, 0.0), p(1.0, 0.0)], &[p(0.0, -1.0), p(0.0, 1.0)]],
            color,
            width,
        )
    }
}

/// The ray of a hand as drawn: a thin line from its origin to `end` (where it meets the handle it
/// picks or holds) with a small diamond there, or `RAY_DRAW_LENGTH` long without one. At most 5
/// segments
pub fn ray_lines(
    ray: &HandRay,
    end: Option<Vec3>,
    eye: Vec3,
    color: Vec4,
    width: f32,
) -> Vec<OverlaySegment> {
    let tip = end.unwrap_or_else(|| ray.at(RAY_DRAW_LENGTH));
    let mut lines = vec![OverlaySegment {
        start: ray.origin,
        end: tip,
        color,
        width,
        dashed: false,
    }];
    if let Some(end) = end {
        let p = Vec2::new;
        let diamond = [
            p(0.0, -1.0),
            p(1.0, 0.0),
            p(0.0, 1.0),
            p(-1.0, 0.0),
            p(0.0, -1.0),
        ];
        let radius = handle_radius(end, eye) * RAY_DOT_SCALE;
        lines.extend(glyph(end, eye, radius, &[&diamond], color, width));
    }
    lines
}

/// A square of half size `half` facing the eyes, drawn clockwise from its top left corner up to
/// `progress` (0..1) of its outline. At most 4 segments
pub fn progress_square(
    center: Vec3,
    eye: Vec3,
    half: f32,
    progress: f32,
    color: Vec4,
    width: f32,
) -> Vec<OverlaySegment> {
    let corners = [
        Vec2::new(-1.0, 1.0),
        Vec2::new(1.0, 1.0),
        Vec2::new(1.0, -1.0),
        Vec2::new(-1.0, -1.0),
        Vec2::new(-1.0, 1.0),
    ];
    let drawn = progress.clamp(0.0, 1.0) * 4.0;
    let mut lines = Vec::new();
    for i in 0..4 {
        let part = (drawn - i as f32).clamp(0.0, 1.0);
        if part <= 0.0 {
            break;
        }
        lines.push([corners[i], corners[i].lerp(corners[i + 1], part)]);
    }
    let lines = lines.iter().map(|line| &line[..]).collect::<Vec<_>>();
    glyph(center, eye, half, &lines, color, width)
}

/// A tracked hand as the editor uses it, stage space
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EditorHand {
    pub thumb_tip: Vec3,
    pub index_tip: Vec3,
    /// Sum of the radii of the two tips (meters)
    pub tip_radii: f32,
    pub palm: Vec3,
    /// Out of the palm (the side the fingers close to), unit, zero when unknown
    pub palm_normal: Vec3,
    /// Both tips tracked, not only estimated
    pub tracked: bool,
    /// The aim pose of the runtime (XR_FB_hand_tracking_aim, the ray of the system pointer). None: not
    /// available, `ray` estimates one
    pub aim: Option<HandRay>,
}

impl EditorHand {
    /// Midpoint of the thumb and index tips
    pub fn cursor(&self) -> Vec3 {
        (self.thumb_tip + self.index_tip) * 0.5
    }

    /// Between the surfaces of the thumb and index tips (meters, negative when they overlap)
    pub fn gap(&self) -> f32 {
        self.thumb_tip.distance(self.index_tip) - self.tip_radii.max(0.0)
    }

    /// The ray of the hand: the aim pose of the runtime, else from an estimated shoulder (`SHOULDER_*`
    /// from the eyes, in the level frame of the view) through the cursor, starting at the cursor. `left`:
    /// the left hand. None: no direction (the cursor at the shoulder)
    pub fn ray(&self, eye: Vec3, forward: Vec3, left: bool) -> Option<HandRay> {
        if let Some(aim) = self.aim {
            return Some(aim);
        }
        let ahead = Vec3::new(forward.x, 0.0, forward.z).normalize_or(Vec3::NEG_Z);
        let right = ahead.cross(Vec3::Y);
        let side = if left { -SHOULDER_SIDE } else { SHOULDER_SIDE };
        let shoulder = eye - Vec3::Y * SHOULDER_DROP + right * side - ahead * SHOULDER_BACK;
        HandRay::new(self.cursor(), self.cursor() - shoulder)
    }

    /// Tracked and in view (within `IN_VIEW_ANGLE`)
    pub fn usable(&self, eye: Vec3, forward: Vec3) -> bool {
        self.unusable_reason(eye, forward, IN_VIEW_ANGLE).is_none()
    }

    /// Why the hand is not usable with its cursor within `angle` of the view direction, for the log.
    /// None: it is
    pub fn unusable_reason(&self, eye: Vec3, forward: Vec3, angle: f32) -> Option<String> {
        if !self.tracked {
            return Some("tips not tracked".into());
        }
        if in_view(eye, forward, self.cursor(), angle) {
            return None;
        }
        Some(format!(
            "{:.0} deg from the view direction",
            forward.angle_between(self.cursor() - eye).to_degrees()
        ))
    }
}

/// The eyes and the hands at the display time of a frame, stage space
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EditorInput {
    /// Midpoint of the eyes
    pub eye: Vec3,
    /// View direction, unit
    pub forward: Vec3,
    /// Left, right. None: no pose
    pub hands: [Option<EditorHand>; 2],
}

/// Whether a point is within `angle` of the view direction (and not at the eyes)
pub fn in_view(eye: Vec3, forward: Vec3, point: Vec3, angle: f32) -> bool {
    let direction = point - eye;
    direction.length() > 0.02 && forward.angle_between(direction) <= angle
}

/// Whether two pinching hands hold the pose that starts and ends editing (see the constants): the
/// pinch points near each other, at a comfortable distance in front of the eyes, both palms turned
/// away from the eyes
pub fn edit_gesture_pose(eye: Vec3, forward: Vec3, hands: [&EditorHand; 2]) -> bool {
    let [a, b] = hands;
    if a.cursor().distance(b.cursor()) > GESTURE_MAX_SPAN {
        return false;
    }
    hands.iter().all(|hand| {
        let distance = hand.cursor().distance(eye);
        let away = (hand.palm - eye).normalize_or_zero();
        (GESTURE_MIN_DISTANCE..=GESTURE_MAX_DISTANCE).contains(&distance)
            && in_view(eye, forward, hand.cursor(), GESTURE_CONE)
            && hand.palm_normal.dot(away) >= PALM_AWAY_COS
    })
}

/// What a pinch did in an update
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PinchEvent {
    None,
    Closed,
    Opened,
    /// Opened because the hand was not usable for `HAND_LOST_HOLD_S`
    Lost,
}

/// The pinch of one hand, with hysteresis (see `DEFAULT_PINCH_ON`)
#[derive(Clone, Copy, Debug, Default)]
pub struct Pinch {
    closed: bool,
    below: u32,
    above_since: Option<f64>,
    lost_since: Option<f64>,
}

impl Pinch {
    pub fn is_closed(&self) -> bool {
        self.closed
    }

    /// `gap`: of the hand (`EditorHand::gap`), None: the hand is not usable. `now`: seconds. `on`,
    /// `off`: the thresholds of the gap (meters). `lost_hold`: how long a closed pinch of a hand that is
    /// not usable stays closed (`HAND_LOST_HOLD_S`, `GRAB_LOST_HOLD_S`; seconds)
    pub fn update(
        &mut self,
        gap: Option<f32>,
        now: f64,
        on: f32,
        off: f32,
        lost_hold: f64,
    ) -> PinchEvent {
        let Some(gap) = gap.filter(|gap| gap.is_finite()) else {
            self.below = 0;
            self.above_since = None;
            if self.closed {
                let since = *self.lost_since.get_or_insert(now);
                if now - since >= lost_hold {
                    self.closed = false;
                    self.lost_since = None;
                    return PinchEvent::Lost;
                }
            }
            return PinchEvent::None;
        };
        self.lost_since = None;

        if self.closed {
            if gap > off {
                let since = *self.above_since.get_or_insert(now);
                if now - since >= PINCH_OFF_HOLD_S {
                    self.closed = false;
                    self.above_since = None;
                    return PinchEvent::Opened;
                }
            } else {
                self.above_since = None;
            }
        } else if gap < on {
            self.below += 1;
            if self.below >= PINCH_ON_UPDATES {
                self.closed = true;
                self.below = 0;
                return PinchEvent::Closed;
            }
        } else {
            self.below = 0;
        }
        PinchEvent::None
    }
}

/// A pose held for `GESTURE_HOLD_S`. Drops shorter than `GESTURE_TOLERANCE_S` don't start it again;
/// after it fired, the pose must be let go before it can fire again
#[derive(Clone, Copy, Debug, Default)]
pub struct HoldTimer {
    start: Option<f64>,
    last_held: Option<f64>,
    fired: bool,
}

impl HoldTimer {
    /// Starts it again at once, without the tolerance for short drops (the two-hand pinch became a new
    /// box: its square goes at once)
    pub fn cancel(&mut self) {
        self.start = None;
        self.last_held = None;
    }

    /// Returns how far it is (0..1) and whether it fired in this update
    pub fn update(&mut self, held: bool, now: f64) -> (f32, bool) {
        if held {
            self.last_held = Some(now);
            if self.fired {
                return (0.0, false);
            }
            let start = *self.start.get_or_insert(now);
            let progress = ((now - start) / GESTURE_HOLD_S).min(1.0) as f32;
            if progress >= 1.0 {
                self.fired = true;
                self.start = None;
                return (1.0, true);
            }
            return (progress, false);
        }
        if self
            .last_held
            .is_none_or(|last| now - last > GESTURE_TOLERANCE_S)
        {
            self.start = None;
            self.fired = false;
        }
        let progress = self.start.map_or(0.0, |start| {
            ((now - start) / GESTURE_HOLD_S).min(0.999) as f32
        });
        (progress, false)
    }
}

/// The last changes, newest last, at most `UNDO_DEPTH`
#[derive(Clone, Debug, Default)]
pub struct UndoStack {
    items: Vec<PlayArea>,
}

impl UndoStack {
    pub fn push(&mut self, area: PlayArea) {
        if self.items.len() >= UNDO_DEPTH {
            self.items.remove(0);
        }
        self.items.push(area);
    }

    pub fn pop(&mut self) -> Option<PlayArea> {
        self.items.pop()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }
}

// FNV-1a, 32 bits
fn fnv1a(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0x811c_9dc5, |hash, &byte| {
        (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193)
    })
}

/// Name of the layout file of a code: its payload with characters other than letters, digits, - and
/// _ replaced by _ (at most 40), and a hash of the whole payload, so that payloads that look alike get
/// their own files. None: a code without payload
pub fn area_file_name(payload: Option<&str>) -> String {
    let Some(payload) = payload else {
        return "area-no-payload.txt".into();
    };
    let name = payload
        .chars()
        .take(40)
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect::<String>();
    format!("area-{name}-{:08x}.txt", fnv1a(payload.as_bytes()))
}

/// The pose of the code a layout was saved with: stage position (meters) and heading yaw (radians, see
/// `heading_yaw`)
pub type SavedAnchor = (Vec3, f32);

/// The text of a layout file: the layout (`PlayArea::to_text`) and the pose of the code measured last,
/// as `anchor x y z heading_deg` (stage space)
pub fn area_file_text(
    area: &PlayArea,
    payload: Option<&str>,
    anchor: Option<SavedAnchor>,
) -> String {
    let mut text = area.to_text(payload);
    if let Some((position, yaw)) = anchor {
        text.push_str(
            "# The code when it was last measured: x y z in stage space (meters), heading (degrees)\n",
        );
        let _ = writeln!(
            text,
            "anchor {:.4} {:.4} {:.4} {:.3}",
            position.x,
            position.y,
            position.z,
            yaw.to_degrees()
        );
    }
    text
}

/// A layout file as read
#[derive(Clone, Debug, PartialEq)]
pub struct AreaFile {
    /// `anchor` lines are not in its `ignored`
    pub parsed: ParsedArea,
    pub anchor: Option<SavedAnchor>,
}

/// Reads the text of `area_file_text`. An anchor line that does not read is left out with a warning
pub fn parse_area_file(text: &str) -> Result<AreaFile, String> {
    let mut parsed = PlayArea::parse(text)?;
    let mut anchor = None;
    let mut ignored = Vec::new();
    for entry in std::mem::take(&mut parsed.ignored) {
        // "line <n>: <text>"
        let line = entry.split_once(": ").map_or("", |(_, line)| line);
        let Some(values) = line.strip_prefix("anchor ") else {
            ignored.push(entry);
            continue;
        };
        let values = values
            .split_whitespace()
            .map(|word| word.parse::<f32>().ok().filter(|value| value.is_finite()))
            .collect::<Option<Vec<_>>>();
        match values.as_deref() {
            Some(&[x, y, z, heading]) => {
                anchor = Some((Vec3::new(x, y, z), heading.to_radians()));
            }
            _ => parsed.warnings.push(format!(
                "{}: an anchor is x y z heading_deg, left out",
                entry.split_once(':').map_or("", |(number, _)| number)
            )),
        }
    }
    parsed.ignored = ignored;

    Ok(AreaFile { parsed, anchor })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MarkerBoxParams;
    use alvr_common::glam::{Mat4, Quat};

    fn params() -> MarkerBoxParams {
        MarkerBoxParams {
            offset: Vec3::new(0.0, 0.15, 0.0),
            size: Vec3::new(1.2, 0.9, 1.0),
            extra_yaw: 0.0,
        }
    }

    fn placed(area: PlayArea) -> PlacedArea {
        PlacedArea {
            area_to_world: Mat4::from_rotation_translation(
                Quat::from_rotation_y(0.6),
                Vec3::new(0.4, 0.9, -0.5),
            ),
            area,
        }
    }

    fn hand(cursor: Vec3, palm_normal: Vec3, gap: f32) -> EditorHand {
        EditorHand {
            thumb_tip: cursor - Vec3::X * (0.008 + 0.5 * gap),
            index_tip: cursor + Vec3::X * (0.008 + 0.5 * gap),
            tip_radii: 0.016,
            palm: cursor + Vec3::new(0.0, -0.03, 0.04),
            palm_normal,
            tracked: true,
            aim: None,
        }
    }

    #[test]
    fn hand_cursor_and_gap() {
        let hand = hand(Vec3::new(0.1, 1.0, -0.4), Vec3::NEG_Z, 0.012);
        assert!(hand.cursor().distance(Vec3::new(0.1, 1.0, -0.4)) < 1e-6);
        assert!((hand.gap() - 0.012).abs() < 1e-6);
        let eye = Vec3::new(0.0, 1.2, 0.0);
        assert!(hand.usable(eye, Vec3::NEG_Z));
        // Behind the head, or not tracked
        assert!(!hand.usable(eye, Vec3::Z));
        assert!(
            !EditorHand {
                tracked: false,
                ..hand
            }
            .usable(eye, Vec3::NEG_Z)
        );
    }

    #[test]
    fn pinch_has_hysteresis() {
        // The thresholds of the first build, the steps below are made for them
        let (on, off) = (0.010, 0.025);
        let mut pinch = Pinch::default();
        let mut t = 0.0;
        let mut step = |pinch: &mut Pinch, gap: Option<f32>| {
            t += 1.0 / 72.0;
            pinch.update(gap, t, on, off, HAND_LOST_HOLD_S)
        };
        // One update below is noise
        assert_eq!(step(&mut pinch, Some(0.005)), PinchEvent::None);
        assert_eq!(step(&mut pinch, Some(0.03)), PinchEvent::None);
        assert!(!pinch.is_closed());
        // Two in a row close it
        assert_eq!(step(&mut pinch, Some(0.005)), PinchEvent::None);
        assert_eq!(step(&mut pinch, Some(0.004)), PinchEvent::Closed);
        assert!(pinch.is_closed());
        // Between the thresholds and short trips above off: still closed
        for _ in 0..50 {
            assert_eq!(step(&mut pinch, Some(0.02)), PinchEvent::None);
        }
        for _ in 0..4 {
            assert_eq!(step(&mut pinch, Some(0.04)), PinchEvent::None);
        }
        assert_eq!(step(&mut pinch, Some(0.015)), PinchEvent::None);
        assert!(pinch.is_closed());
        // Above off for 80 ms opens it
        let mut opened = None;
        for i in 0..10 {
            if step(&mut pinch, Some(0.04)) == PinchEvent::Opened {
                opened = Some(i);
                break;
            }
        }
        // 80 ms at 72 Hz: the 7th update after the first one above
        assert_eq!(opened, Some(6));
        assert!(!pinch.is_closed());

        // A lost hand keeps a closed pinch for 0.3 s, then it is lost
        step(&mut pinch, Some(0.0));
        assert_eq!(step(&mut pinch, Some(0.0)), PinchEvent::Closed);
        let mut events = vec![];
        for _ in 0..30 {
            events.push(step(&mut pinch, None));
        }
        let lost = events.iter().position(|e| *e == PinchEvent::Lost).unwrap();
        assert!((20..=23).contains(&lost), "{lost}");
        assert!(!pinch.is_closed());
        // The hand back within the hold: still closed, no event
        step(&mut pinch, Some(0.0));
        step(&mut pinch, Some(0.0));
        for _ in 0..10 {
            assert_eq!(step(&mut pinch, None), PinchEvent::None);
        }
        assert_eq!(step(&mut pinch, Some(0.0)), PinchEvent::None);
        assert!(pinch.is_closed());
    }

    // The defaults: closes when the tips press into each other (gap below 0), opens 5 mm apart; and
    // with the opening gap equal to the closing one it still does not flicker
    #[test]
    fn pinch_at_contact_and_equal_thresholds() {
        let dt = 1.0 / 72.0;
        for (on, off) in [(DEFAULT_PINCH_ON, DEFAULT_PINCH_OFF), (-0.005, -0.005)] {
            let mut pinch = Pinch::default();
            let mut t = 0.0;
            let mut step = |pinch: &mut Pinch, gap: f32| {
                t += dt;
                pinch.update(Some(gap), t, on, off, HAND_LOST_HOLD_S)
            };
            // Fingers 1-2 cm apart and closing to just touching: still open
            for gap in [0.02, 0.015, 0.01, 0.005, 0.001] {
                assert_eq!(step(&mut pinch, gap + on.max(0.0)), PinchEvent::None);
            }
            assert!(!pinch.is_closed());
            // Pressed together (measured: -2 to -16 mm)
            step(&mut pinch, on - 0.004);
            assert_eq!(step(&mut pinch, on - 0.008), PinchEvent::Closed);
            // Noise around the thresholds shorter than 80 ms: closed
            for i in 0..40 {
                let gap = if i % 4 == 0 { off + 0.002 } else { on - 0.003 };
                assert_eq!(step(&mut pinch, gap), PinchEvent::None, "{i}");
            }
            assert!(pinch.is_closed());
            // Opened
            let events = (0..8)
                .map(|_| step(&mut pinch, off + 0.01))
                .collect::<Vec<_>>();
            assert!(events.contains(&PinchEvent::Opened), "{events:?}");
        }
    }

    // The hand that grabs keeps its pinch longer when it is not usable
    #[test]
    fn grab_keeps_the_pinch_longer() {
        let (on, off) = (DEFAULT_PINCH_ON, DEFAULT_PINCH_OFF);
        let dt = 1.0 / 72.0;
        let mut pinch = Pinch::default();
        pinch.update(Some(-0.01), 0.0, on, off, GRAB_LOST_HOLD_S);
        assert_eq!(
            pinch.update(Some(-0.01), dt, on, off, GRAB_LOST_HOLD_S),
            PinchEvent::Closed
        );
        let mut t = dt;
        let mut lost_at = None;
        while t < 2.0 {
            t += dt;
            if pinch.update(None, t, on, off, GRAB_LOST_HOLD_S) == PinchEvent::Lost {
                lost_at = Some(t);
                break;
            }
        }
        let lost_at = lost_at.unwrap() - 2.0 * dt;
        assert!(
            (GRAB_LOST_HOLD_S..GRAB_LOST_HOLD_S + 0.03).contains(&lost_at),
            "{lost_at}"
        );
    }

    #[test]
    fn why_a_hand_is_not_usable() {
        let eye = Vec3::new(0.0, 1.2, 0.0);
        let forward = Vec3::NEG_Z;
        // 60 deg to the side: out for picking, in for a grab
        let cursor =
            eye + Vec3::new(60_f32.to_radians().sin(), 0.0, -60_f32.to_radians().cos()) * 0.5;
        let side = hand(cursor, Vec3::NEG_Z, 0.0);
        assert!(!side.usable(eye, forward));
        assert_eq!(
            side.unusable_reason(eye, forward, IN_VIEW_ANGLE).as_deref(),
            Some("60 deg from the view direction")
        );
        assert_eq!(side.unusable_reason(eye, forward, GRAB_IN_VIEW_ANGLE), None);
        let estimated = EditorHand {
            tracked: false,
            ..side
        };
        assert_eq!(
            estimated
                .unusable_reason(eye, forward, GRAB_IN_VIEW_ANGLE)
                .as_deref(),
            Some("tips not tracked")
        );
    }

    #[test]
    fn hold_timer_fires_once_per_hold() {
        let mut timer = HoldTimer::default();
        let dt = 1.0 / 72.0;
        let mut t = 0.0;
        let mut fired_at = None;
        let mut last_progress = 0.0;
        while t < 3.0 {
            // A drop of 0.1 s at 0.5 s is ignored
            let held = !(0.5..0.6).contains(&t);
            let (progress, fired) = timer.update(held, t);
            if fired {
                assert!(fired_at.is_none(), "fired twice");
                fired_at = Some(t);
            }
            if held && fired_at.is_none() {
                assert!(progress >= last_progress);
                last_progress = progress;
            }
            t += dt;
        }
        let fired_at = fired_at.unwrap();
        assert!((1.49..1.53).contains(&fired_at), "{fired_at}");

        // Let go, then held again: fires again
        let (progress, _) = timer.update(false, 3.5);
        assert_eq!(progress, 0.0);
        let mut fired = false;
        let mut t = 3.6;
        while t < 5.2 {
            fired |= timer.update(true, t).1;
            t += dt;
        }
        assert!(fired);

        // A longer drop starts it again
        let mut timer = HoldTimer::default();
        timer.update(true, 0.0);
        timer.update(true, 1.0);
        timer.update(false, 1.1);
        assert_eq!(timer.update(false, 1.3).0, 0.0);
        assert!(!timer.update(true, 1.4).1);
        assert!(!timer.update(true, 2.8).1);
        assert!(timer.update(true, 2.95).1);
    }

    #[test]
    fn gesture_pose_and_the_wheel() {
        let eye = Vec3::new(0.0, 1.2, 0.0);
        let forward = Vec3::NEG_Z;
        // Both hands in front of the face, 10 cm apart, palms turned forward (away from the eyes)
        let left = hand(Vec3::new(-0.05, 1.1, -0.35), Vec3::NEG_Z, 0.0);
        let right = hand(Vec3::new(0.05, 1.1, -0.35), Vec3::NEG_Z, 0.0);
        assert!(edit_gesture_pose(eye, forward, [&left, &right]));

        // Palms towards the face (the system menu gesture of the headset)
        let towards = |hand: EditorHand| EditorHand {
            palm_normal: Vec3::Z,
            ..hand
        };
        assert!(!edit_gesture_pose(eye, forward, [&towards(left), &right]));

        // Hands on a wheel: at 9 and 3 o'clock, 32 cm apart, palms facing each other (the rim)
        let wheel_left = hand(Vec3::new(-0.16, 0.85, -0.45), Vec3::X, 0.0);
        let wheel_right = hand(Vec3::new(0.16, 0.85, -0.45), Vec3::NEG_X, 0.0);
        assert!(!edit_gesture_pose(
            eye,
            forward,
            [&wheel_left, &wheel_right]
        ));
        // Even turned away, they are too far apart
        let turned = |hand: EditorHand| EditorHand {
            palm_normal: Vec3::NEG_Z,
            ..hand
        };
        assert!(!edit_gesture_pose(
            eye,
            forward,
            [&turned(wheel_left), &turned(wheel_right)]
        ));

        // Hands together on the lap: out of the middle of the view
        let lap_left = hand(Vec3::new(-0.05, 0.6, -0.25), Vec3::NEG_Z, 0.0);
        let lap_right = hand(Vec3::new(0.05, 0.6, -0.25), Vec3::NEG_Z, 0.0);
        assert!(!edit_gesture_pose(eye, forward, [&lap_left, &lap_right]));

        // Too near the eyes, and arms stretched too far
        let near = |hand: EditorHand| EditorHand {
            thumb_tip: hand.thumb_tip + Vec3::new(0.0, 0.08, 0.27),
            index_tip: hand.index_tip + Vec3::new(0.0, 0.08, 0.27),
            ..hand
        };
        assert!(!edit_gesture_pose(
            eye,
            forward,
            [&near(left), &near(right)]
        ));
        let far = |hand: EditorHand| EditorHand {
            thumb_tip: hand.thumb_tip + Vec3::new(0.0, 0.0, -0.4),
            index_tip: hand.index_tip + Vec3::new(0.0, 0.0, -0.4),
            palm: hand.palm + Vec3::new(0.0, 0.0, -0.4),
            ..hand
        };
        assert!(!edit_gesture_pose(eye, forward, [&far(left), &far(right)]));
    }

    #[test]
    fn handles_of_the_selected_box() {
        let mut area = PlayArea::test_layout(&params());
        area.add_box(0, Side::new(2, true)).unwrap();
        let placed = placed(area);
        let handles = edit_handles(&placed, 0, false);
        // 6 sides, 6 domes, center, turn, remove; the centers of the 2 other boxes; done
        assert_eq!(handles.len(), 6 + 6 + 3 + 2 + 1);
        let to_area = placed.area_to_world.inverse();
        let area_box = placed.area.boxes()[0];
        for side in Side::ALL {
            let find = |kind| {
                handles
                    .iter()
                    .find(|handle| handle.id == HandleId { kind, box_index: 0 })
                    .unwrap()
            };
            let side_handle = find(HandleKind::Side(side));
            // On top of the dome of the side
            let local = to_area.transform_point3(side_handle.position);
            assert!(local.distance(area_box.side_handle(side)) < 1e-5);
            assert!(
                side_handle
                    .normal
                    .distance(placed.area_to_world.transform_vector3(side.normal()))
                    < 1e-5
            );
            // The dome handle beside it, apart from it
            let dome = find(HandleKind::Dome(side));
            let gap = dome.position.distance(side_handle.position);
            assert!((DOME_HANDLE_MIN - 1e-5..=DOME_HANDLE_MAX + 1e-5).contains(&gap));
            let offset = to_area.transform_vector3(dome.position - side_handle.position);
            assert!(offset.dot(side.normal()).abs() < 1e-5);
        }
        // The others: their center only
        for index in 1..3 {
            let of_box = handles
                .iter()
                .filter(|handle| handle.id.box_index == index && handle.id.kind.of_box())
                .collect::<Vec<_>>();
            assert_eq!(of_box.len(), 1);
            assert_eq!(of_box[0].id.kind, HandleKind::Center);
        }
        // Undo only with something to take back
        assert!(
            !handles
                .iter()
                .any(|handle| handle.id.kind == HandleKind::Undo)
        );
        assert!(
            edit_handles(&placed, 0, true)
                .iter()
                .any(|handle| handle.id.kind == HandleKind::Undo)
        );
        // No remove on the last box
        let single = self::placed(PlayArea::from_marker_box(&params()));
        let handles = edit_handles(&single, 0, false);
        assert!(
            !handles
                .iter()
                .any(|handle| handle.id.kind == HandleKind::Remove)
        );
        assert_eq!(handles.len(), 6 + 6 + 2 + 1);
    }

    fn at(kind: HandleKind, position: Vec3) -> Handle {
        Handle {
            id: HandleId { kind, box_index: 0 },
            position,
            normal: Vec3::ZERO,
        }
    }

    #[test]
    fn picking_by_nearness_and_the_hand_ray() {
        let eye = Vec3::new(0.0, 1.2, 0.0);
        let a = at(HandleKind::Center, Vec3::new(0.0, 1.0, -0.4));
        let b = at(HandleKind::Turn, Vec3::new(0.05, 1.0, -0.4));
        let far = at(HandleKind::Done, Vec3::new(0.3, 1.0, -1.5));
        let handles = [a, b, far];
        let ray_to = |from: Vec3, to: Vec3| HandRay::new(from, to - from).unwrap();
        let pick = |handles: &[Handle], cursor: Vec3, ray: &HandRay, current| {
            pick_handle(handles, eye, cursor, ray, current)
        };
        let near = |handle: &Handle| {
            Some(Pick {
                id: handle.id,
                far: false,
            })
        };
        let by_ray = |handle: &Handle| {
            Some(Pick {
                id: handle.id,
                far: true,
            })
        };

        // The nearest in reach, also with the ray on the far handle: near handles first
        let cursor = Vec3::new(0.035, 1.0, -0.4);
        assert_eq!(
            pick(&handles, cursor, &ray_to(cursor, far.position), None),
            near(&b)
        );
        // Nothing near and nothing on the ray
        let cursor = Vec3::new(-0.3, 1.3, -0.4);
        assert_eq!(
            pick(&handles, cursor, &ray_to(cursor, cursor + Vec3::Y), None),
            None
        );
        // Out of reach: along the ray of the hand, wherever the eyes look
        let cursor = Vec3::new(-0.2, 0.9, -0.3);
        let ray = ray_to(cursor, far.position);
        assert_eq!(pick(&handles, cursor, &ray, None), by_ray(&far));
        // The line of sight through the cursor no longer picks
        let on_sight = eye + (far.position - eye).normalize() * 0.4;
        assert_eq!(
            pick(
                &handles,
                on_sight,
                &ray_to(on_sight, on_sight + Vec3::NEG_Y),
                None
            ),
            None
        );
        // Off by 6 degrees: not picked, but kept when it was lit; off by 9: gone
        let turned = |degrees: f32| {
            HandRay::new(
                cursor,
                Quat::from_rotation_y(degrees.to_radians()) * ray.direction,
            )
            .unwrap()
        };
        assert_eq!(pick(&handles, cursor, &turned(6.0), None), None);
        assert_eq!(
            pick(&handles, cursor, &turned(6.0), Some(far.id)),
            by_ray(&far)
        );
        assert_eq!(pick(&handles, cursor, &turned(9.0), Some(far.id)), None);
        // Two handles on the ray: the one nearer to the hand
        let behind = at(HandleKind::Undo, far.position + ray.direction * 0.5);
        assert_eq!(pick(&[behind, far], cursor, &ray, None), by_ray(&far));

        // About as near: the one on the line of sight wins. One handle 2.5 cm straight behind the
        // cursor, the other 2 cm beside it
        let cursor = Vec3::new(0.0, 1.0, -0.4);
        let sight = (cursor - eye).normalize();
        let ray = ray_to(cursor, cursor + Vec3::NEG_Z);
        let behind = at(HandleKind::Center, cursor + sight * 0.025);
        let beside = at(HandleKind::Turn, cursor + Vec3::X * 0.02);
        assert_eq!(pick(&[beside, behind], cursor, &ray, None), near(&behind));
        // Beside it by more than the tie: the nearer one
        let farther = at(HandleKind::Center, cursor + sight * 0.04);
        assert_eq!(pick(&[beside, farther], cursor, &ray, None), near(&beside));

        // Hysteresis: the lit one stays unless another is nearer by a centimeter
        let handles = [a, b];
        let middle = (a.position + b.position) * 0.5 + Vec3::new(0.003, 0.0, 0.0);
        assert_eq!(pick(&handles, middle, &ray, Some(a.id)), near(&a));
        let nearer_b = b.position + Vec3::new(0.0, 0.0, 0.005);
        assert_eq!(pick(&handles, nearer_b, &ray, Some(a.id)), near(&b));
    }

    #[test]
    fn hand_ray_from_the_runtime_or_the_shoulder() {
        let eye = Vec3::new(0.0, 1.2, 0.0);
        let cursor = Vec3::new(0.25, 1.0, -0.35);
        let mut right = hand(cursor, Vec3::NEG_Z, 0.0);
        // Estimated: from the right shoulder (below, right of and behind the eyes) through the cursor
        let ray = right.ray(eye, Vec3::NEG_Z, false).unwrap();
        assert!(ray.origin.distance(cursor) < 1e-6);
        let shoulder = Vec3::new(SHOULDER_SIDE, 1.2 - SHOULDER_DROP, SHOULDER_BACK);
        assert!(ray.direction.distance((cursor - shoulder).normalize()) < 1e-5);
        // Forward, a little to the right and down
        assert!(ray.direction.z < -0.9 && ray.direction.x > 0.0 && ray.direction.y < 0.0);
        // The left hand from the left shoulder; looking down does not tilt the shoulders
        let left = hand(Vec3::new(-0.25, 1.0, -0.35), Vec3::NEG_Z, 0.0);
        let down = Vec3::new(0.0, -0.8, -0.6).normalize();
        let ray = left.ray(eye, down, true).unwrap();
        let shoulder = Vec3::new(-SHOULDER_SIDE, 1.2 - SHOULDER_DROP, SHOULDER_BACK);
        assert!(
            ray.direction
                .distance((left.cursor() - shoulder).normalize())
                < 1e-5
        );
        // The aim pose of the runtime when there is one
        let aim = HandRay::new(Vec3::new(0.2, 1.0, -0.3), Vec3::new(0.0, 0.5, -1.0)).unwrap();
        right.aim = Some(aim);
        assert_eq!(right.ray(eye, Vec3::NEG_Z, false), Some(aim));
        assert!(HandRay::new(Vec3::ZERO, Vec3::ZERO).is_none());
        assert!(HandRay::new(Vec3::ZERO, Vec3::NAN).is_none());
        // Nearest point, not behind the origin
        assert!(
            aim.nearest_point(aim.origin - aim.direction)
                .distance(aim.origin)
                < 1e-6
        );
        assert!(
            aim.nearest_point(aim.at(2.0) + Vec3::X * 0.1)
                .distance(aim.at(2.0))
                < 1e-5
        );
    }

    #[test]
    fn drag_has_a_dead_zone_and_catches_up() {
        assert_eq!(drag_displacement(Vec3::new(0.005, 0.0, 0.0)), Vec3::ZERO);
        assert_eq!(drag_displacement(Vec3::ZERO), Vec3::ZERO);
        // Continuous, monotonic, the same as the hand once caught up
        let mut last = 0.0;
        for i in 0..100 {
            let x = i as f32 * 0.001;
            let applied = drag_displacement(Vec3::new(0.0, x, 0.0));
            assert!(applied.x == 0.0 && applied.z == 0.0);
            assert!(applied.y >= last && applied.y - last < 0.0015, "{x}");
            last = applied.y;
        }
        let caught_up = DRAG_DEAD_ZONE + DRAG_RAMP;
        let hand = Vec3::new(0.03, -0.04, 0.02).normalize() * (caught_up + 0.01);
        assert!(drag_displacement(hand).distance(hand) < 1e-6);
    }

    #[test]
    fn drags_change_the_layout() {
        let area = PlayArea::from_marker_box(&params());
        let start = area.boxes()[0];
        let side = Side::new(0, true);

        // A side: along its normal only, the opposite side stays
        let id = HandleId {
            kind: HandleKind::Side(side),
            box_index: 0,
        };
        let (dragged, moved) = apply_drag(&area, id, Vec3::new(0.1, 0.3, -0.2));
        assert!((moved - 0.1).abs() < 1e-6);
        let after = dragged.boxes()[0];
        assert!((after.half.x - start.half.x - 0.05).abs() < 1e-6);
        let opposite = |b: &AreaBox| b.side_center(side.opposite()).x;
        assert!((opposite(&after) - opposite(&start)).abs() < 1e-6);
        assert_eq!(after.half.y, start.half.y);

        // A dome: up by the displacement along the normal, flat when pushed back near the side
        let dome = HandleId {
            kind: HandleKind::Dome(Side::new(1, true)),
            box_index: 0,
        };
        let (domed, height) = apply_drag(&area, dome, Vec3::new(0.0, 0.12, 0.05));
        assert!((height - 0.12).abs() < 1e-6);
        assert!((domed.boxes()[0].dome_height(Side::new(1, true)) - 0.12).abs() < 1e-6);
        let (flat, height) = apply_drag(&domed, dome, Vec3::new(0.0, -0.11, 0.0));
        assert_eq!(height, 0.0);
        assert!(!flat.boxes()[0].has_domes());
        // Never higher than the rim
        let (_, height) = apply_drag(&area, dome, Vec3::new(0.0, 3.0, 0.0));
        assert!((height - start.dome_rim(Side::new(1, true))).abs() < 1e-6);

        // The center: the whole box
        let center = HandleId {
            kind: HandleKind::Center,
            box_index: 0,
        };
        let delta = Vec3::new(0.1, -0.05, 0.2);
        let (moved_area, distance) = apply_drag(&area, center, delta);
        assert!((distance - delta.length()).abs() < 1e-6);
        assert!(moved_area.boxes()[0].center.distance(start.center + delta) < 1e-6);
        assert_eq!(moved_area.boxes()[0].half, start.half);

        // Buttons change nothing
        let undo = HandleId {
            kind: HandleKind::Undo,
            box_index: 0,
        };
        assert_eq!(apply_drag(&area, undo, delta).0, area);
    }

    #[test]
    fn first_pinch_on_another_box_only_selects_it() {
        let id = |kind, box_index| HandleId { kind, box_index };
        // Box 2 selected: the center of box 1 (all the other boxes show) selects it
        assert_eq!(
            pinch_action(id(HandleKind::Center, 1), 2),
            PinchAction::Select(1)
        );
        // Any handle of another box (a hand that kept a handle lit while the selection changed)
        assert_eq!(
            pinch_action(id(HandleKind::Side(Side::new(0, true)), 0), 2),
            PinchAction::Select(0)
        );
        // The selected box: taken
        for kind in [
            HandleKind::Center,
            HandleKind::Turn,
            HandleKind::Remove,
            HandleKind::Dome(Side::new(1, true)),
        ] {
            assert_eq!(pinch_action(id(kind, 2), 2), PinchAction::Grab);
        }
        // Undo and done belong to no box (index 0): always taken
        assert_eq!(pinch_action(id(HandleKind::Done, 0), 2), PinchAction::Grab);
        assert_eq!(pinch_action(id(HandleKind::Undo, 0), 1), PinchAction::Grab);
    }

    #[test]
    fn new_box_spans_the_pinch_points() {
        // Opposite corners
        let (a, b) = (Vec3::new(-0.2, 0.1, -0.5), Vec3::new(0.3, 0.6, -0.9));
        let new_box = new_box_between(a, b);
        assert!(new_box.center.distance((a + b) * 0.5) < 1e-6);
        assert!(new_box.half.distance(Vec3::new(0.25, 0.25, 0.2)) < 1e-6);
        assert_eq!((new_box.yaw, new_box.domes), (0.0, [0.0; 6]));
        // The same whichever hand is where
        assert_eq!(new_box_between(b, a), new_box);
        // A flat pull (both hands at one height and depth): the other sides at the minimum
        let flat = new_box_between(Vec3::new(-0.3, 1.0, -0.5), Vec3::new(0.3, 1.0, -0.5));
        assert!((flat.half.x - 0.3).abs() < 1e-6);
        assert_eq!(
            (flat.half.y, flat.half.z),
            (0.5 * NEW_BOX_MIN_SIZE, 0.5 * NEW_BOX_MIN_SIZE)
        );

        let area = PlayArea::from_marker_box(&params());
        assert_eq!(new_box_refusal(&area, a, b), None);
        // A pinch that never opened
        let near = a + Vec3::new(0.05, 0.05, 0.0);
        let refusal = new_box_refusal(&area, a, near).unwrap();
        assert!(
            refusal.starts_with("the pinch points were 71 mm apart"),
            "{refusal}"
        );
        // Full, too far from the code
        let full = PlayArea::stress_layout(&params());
        let refusal = new_box_refusal(&full, a, b).unwrap();
        assert!(refusal.starts_with("8 boxes already"), "{refusal}");
        let far = Vec3::new(0.0, 0.0, -MAX_BOX_CENTER - 0.5);
        let refusal = new_box_refusal(&area, far, far + Vec3::X * 0.3).unwrap();
        assert!(refusal.contains("more than 3 m"), "{refusal}");
        // Made: the box as drawn, selected as the last one
        let mut made = area;
        assert_eq!(made.push_box(new_box), Some(1));
        assert_eq!(made.boxes()[1], new_box);
    }

    // Recorded-like hands at 72 Hz, run as the client does: the timer of the gesture that ends editing
    // runs while the pose holds and the two-hand pinch is not a new box
    struct TwoHands {
        two: TwoHandPinch,
        timer: HoldTimer,
        t: f64,
    }

    impl TwoHands {
        fn new() -> Self {
            Self {
                two: TwoHandPinch::default(),
                timer: HoldTimer::default(),
                t: 0.0,
            }
        }

        // Both pinched on empty space, palms turned away, the pinch points `span` apart in front of the
        // face; returns the state, the progress of the gesture and whether it fired
        fn step(&mut self, pinched: bool, span: f32) -> (TwoHand, f32, bool) {
            self.t += 1.0 / 72.0;
            let eye = Vec3::new(0.0, 1.2, 0.0);
            let left = hand(Vec3::new(-0.5 * span, 1.1, -0.35), Vec3::NEG_Z, 0.0);
            let right = hand(Vec3::new(0.5 * span, 1.1, -0.35), Vec3::NEG_Z, 0.0);
            let state = self
                .two
                .update(pinched, left.cursor().distance(right.cursor()));
            let pose = pinched && edit_gesture_pose(eye, Vec3::NEG_Z, [&left, &right]);
            if state == TwoHand::Adding {
                self.timer.cancel();
            }
            let (progress, fired) = self.timer.update(pose && state != TwoHand::Adding, self.t);
            (state, progress, fired)
        }
    }

    #[test]
    fn two_hand_pinch_ends_editing_or_makes_a_box() {
        // The gesture: 10 cm apart, drifting by 1.5 cm while held: fires after 1.5 s, never a box
        let mut hands = TwoHands::new();
        let mut fired_at = None;
        for i in 0..130 {
            let drift = 0.015 * (i as f32 * 0.3).sin();
            let (state, _, fired) = hands.step(true, 0.10 + drift);
            assert_eq!(state, TwoHand::Undecided, "{i}");
            if fired {
                fired_at = Some(hands.t);
            }
        }
        let fired_at = fired_at.unwrap();
        assert!((1.49..1.53).contains(&fired_at), "{fired_at}");

        // A new box: pinched 8 cm apart (the amber square starts), pulled to 50 cm over 0.4 s: a box
        // from the frame the points are more than 20 cm apart, the square gone; back together and held
        // with the palms away for 2 s: still a box, the gesture does not run
        let mut hands = TwoHands::new();
        for i in 0..20 {
            let (state, progress, _) = hands.step(true, 0.08);
            assert_eq!(state, TwoHand::Undecided);
            assert!(i == 0 || progress > 0.0);
        }
        let mut adding_from = None;
        for i in 0..=29 {
            let span = 0.08 + 0.42 * i as f32 / 29.0;
            let (state, progress, fired) = hands.step(true, span);
            assert!(!fired);
            if span > GESTURE_MAX_SPAN + 1e-4 {
                assert_eq!(state, TwoHand::Adding, "{span}");
                adding_from.get_or_insert(span);
                assert_eq!(progress, 0.0, "{span}");
            } else if span < GESTURE_MAX_SPAN - 1e-4 {
                assert_eq!(state, TwoHand::Undecided, "{span}");
            }
        }
        assert!(adding_from.unwrap() < GESTURE_MAX_SPAN + 0.02);
        for _ in 0..144 {
            let (state, progress, fired) = hands.step(true, 0.10);
            assert_eq!((state, progress, fired), (TwoHand::Adding, 0.0, false));
        }
        // A hand lets go: over; a new pinch close together is the gesture again
        assert_eq!(hands.step(false, 0.10).0, TwoHand::None);
        for _ in 0..12 {
            hands.step(false, 0.10);
        }
        let mut fired = false;
        for _ in 0..115 {
            let (state, _, now) = hands.step(true, 0.10);
            assert_eq!(state, TwoHand::Undecided);
            fired |= now;
        }
        assert!(fired);

        // Pinched at two corners 35 cm apart: a box at once, the timer never runs
        let mut hands = TwoHands::new();
        for _ in 0..150 {
            assert_eq!(hands.step(true, 0.35), (TwoHand::Adding, 0.0, false));
        }

        // One frame beyond 20 cm is enough: it stays a box (the ghost shows it)
        let mut hands = TwoHands::new();
        hands.step(true, 0.19);
        assert_eq!(hands.step(true, 0.21).0, TwoHand::Adding);
        assert_eq!(hands.step(true, 0.15).0, TwoHand::Adding);
        // Not both pinched: nothing
        assert_eq!(TwoHandPinch::default().update(false, 0.5), TwoHand::None);
        // A span that is not a number decides nothing
        assert_eq!(
            TwoHandPinch::default().update(true, f32::NAN),
            TwoHand::Undecided
        );
    }

    // What a hand does in one update of `Session`
    #[derive(Clone, Copy)]
    struct HandStep {
        pinched: bool,
        // What the pinch closes on (the lit handle; None: empty space)
        on: Option<Pick>,
        // How far the cursor (of a far grab: what the ray gives over the gain) moved since the grab
        // (meters)
        moved: f32,
    }

    impl HandStep {
        fn moved(self, moved: f32) -> Self {
            Self { moved, ..self }
        }
    }

    const OPEN: HandStep = HandStep {
        pinched: false,
        on: None,
        moved: 0.0,
    };
    const EMPTY: HandStep = HandStep {
        pinched: true,
        on: None,
        moved: 0.0,
    };

    fn on(kind: HandleKind, box_index: usize, far: bool) -> HandStep {
        HandStep {
            pinched: true,
            on: Some(Pick {
                id: HandleId { kind, box_index },
                far,
            }),
            moved: 0.0,
        }
    }

    struct TestGrab {
        hand: usize,
        id: HandleId,
        start: f64,
        max_move: f32,
        removed: bool,
    }

    // Both hands at 72 Hz, run as the client does (area_editor.rs, `update` and `edit`): the pinches
    // and their roles, one grab, the two-hand pinch and the timer of the gesture that starts and ends
    // editing, a new box when a hand lets go of the two-hand pinch pulled open
    struct Session {
        editing: bool,
        pinches: [Pinch; 2],
        roles: PinchRoles,
        grab: Option<TestGrab>,
        selected: usize,
        two: TwoHandPinch,
        state: TwoHand,
        timer: HoldTimer,
        t: f64,
        // What happened
        starts: Vec<(usize, PinchStart)>,
        grabs: Vec<HandleId>,
        // Drags released out of their dead zone, buttons that acted
        released: Vec<HandleId>,
        // Grabs that gave way
        dropped: Vec<HandleId>,
        removed: u32,
        boxes: u32,
        ended: bool,
    }

    impl Session {
        fn new() -> Self {
            Self {
                editing: true,
                pinches: [Pinch::default(); 2],
                roles: PinchRoles::default(),
                grab: None,
                selected: 0,
                two: TwoHandPinch::default(),
                state: TwoHand::None,
                timer: HoldTimer::default(),
                t: 0.0,
                starts: Vec::new(),
                grabs: Vec::new(),
                released: Vec::new(),
                dropped: Vec::new(),
                removed: 0,
                boxes: 0,
                ended: false,
            }
        }

        // The pinch points `span` apart in front of the face, palms turned away (the pose of the
        // gesture while within `GESTURE_MAX_SPAN`)
        fn step(&mut self, hands: [HandStep; 2], span: f32) {
            self.t += 1.0 / 72.0;
            let mut events = [PinchEvent::None; 2];
            for i in 0..2 {
                let gap = if hands[i].pinched { -0.005 } else { 0.02 };
                events[i] = self.pinches[i].update(
                    Some(gap),
                    self.t,
                    DEFAULT_PINCH_ON,
                    DEFAULT_PINCH_OFF,
                    HAND_LOST_HOLD_S,
                );
                self.roles.follow(i, self.pinches[i].is_closed());
            }

            let closed = self.pinches.iter().all(Pinch::is_closed);
            let both_free = self.editing && self.grab.is_none() && closed && self.roles.two_hand();
            let state = self.two.update(both_free, span);
            if self.state == TwoHand::Adding && state != TwoHand::Adding {
                self.boxes += 1;
            }
            self.state = state;
            if state == TwoHand::Adding {
                self.timer.cancel();
            }
            let eye = Vec3::new(0.0, 1.2, 0.0);
            let left = hand(Vec3::new(-0.5 * span, 1.1, -0.35), Vec3::NEG_Z, 0.0);
            let right = hand(Vec3::new(0.5 * span, 1.1, -0.35), Vec3::NEG_Z, 0.0);
            let pose = closed
                && (!self.editing || both_free)
                && edit_gesture_pose(eye, Vec3::NEG_Z, [&left, &right]);
            let (_, fired) = self.timer.update(pose && state != TwoHand::Adding, self.t);
            if fired {
                if self.editing {
                    self.editing = false;
                    self.ended = true;
                    self.grab = None;
                } else {
                    self.editing = true;
                    self.roles.reset([true, true]);
                    self.two = TwoHandPinch::default();
                }
            }
            if !self.editing {
                return;
            }

            for i in 0..2 {
                if events[i] != PinchEvent::Closed {
                    continue;
                }
                let other = 1 - i;
                let other_acted = self.grab.as_ref().is_some_and(|grab| {
                    grab.hand == other && (grab.removed || grab.max_move > DRAG_DEAD_ZONE)
                });
                let start = self
                    .roles
                    .close(i, hands[i].on, self.selected, other_acted, self.t);
                self.starts.push((i, start));
                match start {
                    PinchStart::Handle(PinchAction::Select(index)) => self.selected = index,
                    PinchStart::Handle(PinchAction::Grab) if self.grab.is_none() => {
                        let id = hands[i].on.unwrap().id;
                        self.grabs.push(id);
                        self.grab = Some(TestGrab {
                            hand: i,
                            id,
                            start: self.t,
                            max_move: 0.0,
                            removed: false,
                        });
                    }
                    PinchStart::TwoHand { gives_way: true }
                        if self.grab.as_ref().is_some_and(|grab| grab.hand == other) =>
                    {
                        self.dropped.push(self.grab.take().unwrap().id);
                    }
                    _ => (),
                }
            }

            let Some(grab) = self.grab.as_mut() else {
                return;
            };
            grab.max_move = grab.max_move.max(hands[grab.hand].moved);
            if grab.id.kind == HandleKind::Remove
                && !grab.removed
                && grab.max_move < REMOVE_HOLD_MOVE
                && self.t - grab.start >= REMOVE_HOLD_S
            {
                grab.removed = true;
                self.removed += 1;
            }
            if events[grab.hand] == PinchEvent::Opened {
                let grab = self.grab.take().unwrap();
                if grab.id.kind.is_button()
                    || (grab.id.kind.is_drag() && grab.max_move > DRAG_DEAD_ZONE)
                {
                    self.released.push(grab.id);
                }
            }
        }

        fn run(&mut self, updates: usize, hands: [HandStep; 2], span: f32) {
            for _ in 0..updates {
                self.step(hands, span);
            }
        }

        fn last_start(&self) -> (usize, PinchStart) {
            *self.starts.last().unwrap()
        }
    }

    fn side_handle() -> HandleId {
        HandleId {
            kind: HandleKind::Side(Side::new(0, true)),
            box_index: 0,
        }
    }

    fn near_side() -> HandStep {
        on(HandleKind::Side(Side::new(0, true)), 0, false)
    }

    // The rule of the user: "pinch to resize is always with one hand. adding a new box or exiting edit
    // mode is always with 2"
    #[test]
    fn one_hand_works_handles_alone() {
        // A drag with one hand: taken, moved 3 cm, released
        let mut s = Session::new();
        for i in 0..30 {
            s.step([near_side().moved(0.001 * i as f32), OPEN], 0.4);
        }
        s.run(10, [OPEN, OPEN], 0.4);
        assert_eq!(
            (s.grabs.as_slice(), s.released.as_slice()),
            (&[side_handle()][..], &[side_handle()][..])
        );
        // A pinch on empty space alone, held 3 s, also moved about: free, nothing happens
        let mut s = Session::new();
        s.run(216, [EMPTY, OPEN], 0.1);
        s.run(10, [OPEN, OPEN], 0.1);
        s.run(216, [OPEN, EMPTY], 0.4);
        assert_eq!(s.starts, [(0, PinchStart::Free), (1, PinchStart::Free)]);
        assert_eq!((s.boxes, s.ended, s.state), (0, false, TwoHand::None));
        // Done with one hand acts on release, a handle of another box only selects it
        let mut s = Session::new();
        s.run(20, [OPEN, on(HandleKind::Center, 1, true)], 0.4);
        s.run(10, [OPEN, OPEN], 0.4);
        assert_eq!(s.selected, 1);
        s.run(20, [on(HandleKind::Done, 0, false), OPEN], 0.4);
        s.run(10, [OPEN, OPEN], 0.4);
        assert_eq!(
            s.released,
            [HandleId {
                kind: HandleKind::Done,
                box_index: 0
            }]
        );
    }

    // 3a: the other pinch is free: the two-hand pinch, whatever the new one closed on
    #[test]
    fn second_pinch_after_a_free_one_is_the_two_hand_pinch() {
        // On a near side of the selected box, the pinch points 10 cm apart, palms away: no grab, the
        // gesture ends editing after 1.5 s
        let mut s = Session::new();
        s.run(22, [EMPTY, OPEN], 0.10);
        s.run(150, [EMPTY, near_side()], 0.10);
        assert_eq!(
            s.last_start(),
            (1, PinchStart::TwoHand { gives_way: false })
        );
        assert!(s.grabs.is_empty() && s.ended);

        // On done by the ray, pulled 40 cm apart: a new box, done does nothing, editing goes on
        let mut s = Session::new();
        s.run(22, [EMPTY, OPEN], 0.10);
        s.run(10, [EMPTY, on(HandleKind::Done, 0, true)], 0.10);
        s.run(30, [EMPTY, on(HandleKind::Done, 0, true)], 0.40);
        assert_eq!(s.state, TwoHand::Adding);
        s.run(10, [EMPTY, OPEN], 0.40);
        assert_eq!(s.boxes, 1);
        assert!(s.grabs.is_empty() && s.released.is_empty() && !s.ended);

        // On another box: not selected
        let mut s = Session::new();
        s.run(22, [OPEN, EMPTY], 0.10);
        s.run(10, [on(HandleKind::Center, 1, false), EMPTY], 0.10);
        assert_eq!(
            s.last_start(),
            (0, PinchStart::TwoHand { gives_way: false })
        );
        assert_eq!((s.selected, s.state), (0, TwoHand::Undecided));

        // Both closing in the same update, either hand on a handle: the two-hand pinch
        for hands in [[near_side(), EMPTY], [EMPTY, near_side()]] {
            let mut s = Session::new();
            s.run(10, hands, 0.10);
            assert!(s.grab.is_none() && s.roles.two_hand());
            assert_eq!(s.state, TwoHand::Undecided);
        }
    }

    // 3b: the other pinch on a handle since at most `GRAB_YIELD_S`, its grab not acted: it gives way
    #[test]
    fn a_young_grab_gives_way_to_the_second_pinch() {
        // Near drag, 5 mm of drift (in the dead zone), the other hand pinches empty space 0.3 s later:
        // dropped, pulled apart: a new box, no release of the side
        let mut s = Session::new();
        s.run(22, [OPEN, near_side().moved(0.005)], 0.40);
        s.run(10, [EMPTY, near_side().moved(0.005)], 0.40);
        assert_eq!(s.last_start(), (0, PinchStart::TwoHand { gives_way: true }));
        assert_eq!(
            (s.grabs.as_slice(), s.dropped.as_slice()),
            (&[side_handle()][..], &[side_handle()][..])
        );
        s.run(10, [EMPTY, near_side()], 0.40);
        assert_eq!(s.state, TwoHand::Adding);
        s.run(10, [OPEN, near_side()], 0.40);
        assert_eq!(s.boxes, 1);
        assert!(s.released.is_empty());

        // The center by the ray, the other hand closes on another box 0.3 s later: dropped, nothing
        // selected; the gesture ends editing
        let mut s = Session::new();
        s.run(22, [OPEN, on(HandleKind::Center, 0, true)], 0.10);
        s.run(
            140,
            [
                on(HandleKind::Center, 1, false),
                on(HandleKind::Center, 0, true),
            ],
            0.10,
        );
        assert_eq!(s.dropped.len(), 1);
        assert_eq!(s.selected, 0);
        assert!(s.ended);

        // Done, 0.4 s, then the other hand: done does nothing on release, editing goes on
        let mut s = Session::new();
        s.run(29, [OPEN, on(HandleKind::Done, 0, false)], 0.10);
        s.run(10, [EMPTY, on(HandleKind::Done, 0, false)], 0.10);
        s.run(10, [EMPTY, OPEN], 0.10);
        s.run(10, [OPEN, OPEN], 0.10);
        assert_eq!(
            s.dropped,
            [HandleId {
                kind: HandleKind::Done,
                box_index: 0
            }]
        );
        assert!(s.released.is_empty() && s.editing);

        // The hold of "x": gives way before it removed, nothing removed however long both are held
        let mut s = Session::new();
        s.run(22, [OPEN, on(HandleKind::Remove, 0, false)], 0.30);
        s.run(100, [EMPTY, on(HandleKind::Remove, 0, false)], 0.30);
        assert_eq!((s.dropped.len(), s.removed), (1, 0));

        // A selection (box 1), the other hand 0.2 s later: box 1 stays selected, both are the two-hand
        // pinch, the gesture ends editing
        let mut s = Session::new();
        s.run(15, [on(HandleKind::Center, 1, false), OPEN], 0.10);
        assert_eq!(s.selected, 1);
        s.run(140, [on(HandleKind::Center, 1, false), near_side()], 0.10);
        assert_eq!(s.last_start(), (1, PinchStart::TwoHand { gives_way: true }));
        assert!(s.grabs.is_empty() && s.dropped.is_empty());
        assert_eq!(s.selected, 1);
        assert!(s.ended);
    }

    // 3c: the other hand is resizing (its grab older than `GRAB_YIELD_S`, or out of its dead zone):
    // the new pinch does nothing while it stays closed and does not disturb the grab
    #[test]
    fn a_running_resize_is_not_disturbed_by_the_other_hand() {
        // The right hand drags 3 cm in 0.2 s, the left pinches empty space at 0.3 s and holds it with
        // the palms away 10 cm from the right for 2 s: no gesture, no box, the drag goes on and is
        // released with its change
        let mut s = Session::new();
        for i in 0..22 {
            s.step(
                [OPEN, near_side().moved((0.0015 * i as f32).min(0.03))],
                0.10,
            );
        }
        s.run(150, [EMPTY, near_side().moved(0.05)], 0.10);
        assert_eq!(s.last_start(), (0, PinchStart::Idle));
        assert_eq!(s.roles.role(0), PinchRole::Idle);
        assert!(s.grab.as_ref().is_some_and(|grab| grab.max_move == 0.05));
        assert!(s.dropped.is_empty() && s.editing && s.state == TwoHand::None);
        s.run(10, [EMPTY, OPEN], 0.10);
        assert_eq!(s.released, [side_handle()]);
        // The left still pinched (it does nothing): a new pinch of the right on the side does nothing
        // either
        s.run(10, [EMPTY, near_side()], 0.10);
        assert_eq!(s.last_start(), (1, PinchStart::Idle));
        assert_eq!(s.grabs.len(), 1);
        // Both open: a new two-hand pinch works again
        s.run(10, [OPEN, OPEN], 0.10);
        s.run(130, [EMPTY, EMPTY], 0.10);
        assert!(s.ended);

        // Held 0.6 s without moving (older than `GRAB_YIELD_S`), the other hand on another box: not
        // selected, nothing dropped
        let mut s = Session::new();
        s.run(44, [OPEN, near_side()], 0.40);
        s.run(10, [on(HandleKind::Center, 1, false), near_side()], 0.40);
        assert_eq!(s.last_start(), (0, PinchStart::Idle));
        assert_eq!((s.selected, s.dropped.len()), (0, 0));
        assert!(s.grab.is_some());

        // Done held 0.7 s, the other hand pinches: nothing; done acts when its pinch opens (one hand)
        let mut s = Session::new();
        s.run(50, [OPEN, on(HandleKind::Done, 0, false)], 0.10);
        s.run(10, [EMPTY, on(HandleKind::Done, 0, false)], 0.10);
        s.run(10, [EMPTY, OPEN], 0.10);
        assert_eq!(
            s.released,
            [HandleId {
                kind: HandleKind::Done,
                box_index: 0
            }]
        );
    }

    // 4: editing started by the gesture: the hands still pinched start no box and don't end editing
    #[test]
    fn pinches_that_started_editing_do_nothing() {
        let mut s = Session::new();
        s.editing = false;
        s.run(130, [EMPTY, EMPTY], 0.10);
        assert!(s.editing);
        assert_eq!(s.roles.role(0), PinchRole::Idle);
        s.run(220, [EMPTY, EMPTY], 0.10);
        s.run(30, [EMPTY, EMPTY], 0.40);
        assert!(s.editing && !s.ended);
        assert_eq!((s.boxes, s.state), (0, TwoHand::None));
        // One hand opens and pinches on a handle while the other stays pinched: nothing
        s.run(10, [EMPTY, OPEN], 0.40);
        s.run(10, [EMPTY, near_side()], 0.40);
        assert_eq!(s.last_start(), (1, PinchStart::Idle));
        assert!(s.grabs.is_empty());
        // Both open: a new two-hand pinch ends editing
        s.run(10, [OPEN, OPEN], 0.10);
        s.run(130, [EMPTY, EMPTY], 0.10);
        assert!(s.ended);
    }

    #[test]
    fn glyphs_face_the_eyes_and_stay_small() {
        let eye = Vec3::new(0.0, 1.2, 0.0);
        let position = Vec3::new(0.3, 1.0, -0.8);
        let view = (position - eye).normalize();
        for kind in [
            HandleKind::Side(Side::new(0, true)),
            HandleKind::Dome(Side::new(1, true)),
            HandleKind::Center,
            HandleKind::Turn,
            HandleKind::Remove,
            HandleKind::Undo,
            HandleKind::Done,
        ] {
            let handle = Handle {
                id: HandleId { kind, box_index: 0 },
                position,
                normal: Vec3::Y,
            };
            let lines = handle_glyph(&handle, eye, 1.0, OVERLAY_CYAN, 0.002);
            assert!((2..=4).contains(&lines.len()), "{kind:?}");
            let radius = handle_radius(position, eye);
            for line in &lines {
                for point in [line.start, line.end] {
                    // In the plane facing the eyes, within 1.3 times the radius (the diamond)
                    assert!((point - position).dot(view).abs() < 1e-5);
                    assert!(point.distance(position) <= radius * 1.3 * 1.42 + 1e-5);
                }
                assert_eq!(line.color, OVERLAY_CYAN);
            }
        }
        // The dome triangle points along the normal as seen: up here
        let dome = Handle {
            id: HandleId {
                kind: HandleKind::Dome(Side::new(1, true)),
                box_index: 0,
            },
            position,
            normal: Vec3::Y,
        };
        let lines = handle_glyph(&dome, eye, 1.0, OVERLAY_CYAN, 0.002);
        let top = lines
            .iter()
            .map(|line| line.start.y)
            .fold(f32::MIN, f32::max);
        assert!((lines[0].start.y - top).abs() < 1e-6);
        // Far handles are larger
        assert!(handle_radius(Vec3::new(0.0, 1.2, -3.0), eye) > handle_radius(position, eye));
        assert_eq!(
            cursor_glyph(position, eye, false, OVERLAY_DIM, 0.002).len(),
            2
        );
        assert_eq!(
            cursor_glyph(position, eye, true, OVERLAY_DIM, 0.002).len(),
            4
        );
        // A ray: to the handle it meets with a dot there, else a line of the drawn length
        let ray = HandRay::new(Vec3::new(0.2, 1.0, -0.3), Vec3::NEG_Z).unwrap();
        let lines = ray_lines(&ray, Some(position), eye, OVERLAY_DIM, 0.002);
        assert_eq!(lines.len(), 5);
        assert_eq!((lines[0].start, lines[0].end), (ray.origin, position));
        for line in &lines[1..] {
            assert!(line.start.distance(position) <= handle_radius(position, eye) * 0.5);
        }
        let lines = ray_lines(&ray, None, eye, OVERLAY_DIM, 0.002);
        assert_eq!(lines.len(), 1);
        assert!((lines[0].end.distance(ray.origin) - RAY_DRAW_LENGTH).abs() < 1e-6);
    }

    #[test]
    fn handles_of_eight_boxes_leave_room_for_the_outline() {
        // The client draws the handles, the 12 edges of the selected box and 2 cursors, and in the
        // worst frame of each kind: a grab (the guide of a turn, the longest, or the square of a held
        // remove, and 2 rays), the gesture (its square and 2 rays: a grab is dropped then) or a new box
        // (its 12 edges; no rays, no grab). The outline of the area gets what is left of the 249
        // segments of an eye
        let placed = placed(PlayArea::stress_layout(&params()));
        let eye = Vec3::new(0.0, 1.2, 0.0);
        let handles = edit_handles(&placed, 3, true);
        let glyphs = handles
            .iter()
            .map(|handle| handle_glyph(handle, eye, 1.3, OVERLAY_CYAN, 0.002).len())
            .sum::<usize>();
        let ray = HandRay::new(eye, Vec3::NEG_Z).unwrap();
        let ray_segments = ray_lines(&ray, Some(Vec3::NEG_Z), eye, OVERLAY_DIM, 0.002).len();
        let grab = (2 + TURN_GUIDE_SEGMENTS).max(4) + 2 * ray_segments;
        let gesture = 4 + 2 * ray_segments;
        let new_box = 12;
        let editor = glyphs + 12 + 2 * 4 + grab.max(gesture).max(new_box);
        assert!(editor <= 123, "{editor}");
        // All 96 edges of the 8 boxes still fit
        assert!(crate::MAX_OVERLAY_SEGMENTS - 7 - editor >= 96 + 30);
    }

    #[test]
    fn progress_square_grows_with_progress() {
        let eye = Vec3::ZERO;
        let center = Vec3::new(0.0, 0.0, -0.5);
        assert!(progress_square(center, eye, 0.03, 0.0, OVERLAY_DIM, 0.002).is_empty());
        let length = |progress: f32| {
            progress_square(center, eye, 0.03, progress, OVERLAY_DIM, 0.002)
                .iter()
                .map(|line| line.start.distance(line.end))
                .sum::<f32>()
        };
        assert!((length(1.0) - 0.24).abs() < 1e-5);
        assert!((length(0.375) - 0.09).abs() < 1e-5);
        assert_eq!(
            progress_square(center, eye, 0.03, 0.6, OVERLAY_DIM, 0.002).len(),
            3
        );
    }

    #[test]
    fn undo_keeps_the_last_changes() {
        let mut undo = UndoStack::default();
        let mut area = PlayArea::from_marker_box(&params());
        for i in 0..25 {
            area.move_box(0, Vec3::new(0.01, 0.0, 0.0));
            undo.push(area);
            assert_eq!(undo.len(), (i + 1).min(UNDO_DEPTH));
        }
        let newest = undo.pop().unwrap();
        assert_eq!(newest, area);
        let mut count = 1;
        while undo.pop().is_some() {
            count += 1;
        }
        assert_eq!(count, UNDO_DEPTH);
        assert!(undo.is_empty());
    }

    #[test]
    fn file_names_are_safe_and_distinct() {
        let name = area_file_name(Some("RIG1"));
        assert!(
            name.starts_with("area-RIG1-") && name.ends_with(".txt"),
            "{name}"
        );
        assert_eq!(name, area_file_name(Some("RIG1")));
        // Path characters go, payloads that look alike keep their own files
        let a = area_file_name(Some("../a b/c"));
        let b = area_file_name(Some("__a_b_c"));
        assert!(a.starts_with("area-___a_b_c-"), "{a}");
        assert!(!a.contains('/') && !a.contains(' ') && !a.contains(".."));
        assert_ne!(a, b);
        let long = area_file_name(Some(&"x".repeat(300)));
        assert!(long.len() < 64);
        assert_eq!(area_file_name(None), "area-no-payload.txt");
        // FNV-1a of "a"
        assert_eq!(fnv1a(b"a"), 0xe40c_292c);
    }

    #[test]
    fn layout_file_round_trip_with_the_anchor() {
        let area = PlayArea::test_layout(&params());
        let anchor = (Vec3::new(-0.3412, 0.9634, -0.4301), 204.13_f32.to_radians());
        let text = area_file_text(&area, Some("SCR1"), Some(anchor));
        let file = parse_area_file(&text).unwrap();
        assert_eq!(file.parsed.payload.as_deref(), Some("SCR1"));
        assert!(file.parsed.ignored.is_empty(), "{:?}", file.parsed.ignored);
        assert!(
            file.parsed.warnings.is_empty(),
            "{:?}",
            file.parsed.warnings
        );
        let (position, yaw) = file.anchor.unwrap();
        assert!(position.distance(anchor.0) < 1e-4);
        assert!((yaw - anchor.1).abs() < 1e-4);
        assert_eq!(file.parsed.area.len(), 2);
        // The same text again
        let again = area_file_text(&file.parsed.area, Some("SCR1"), file.anchor);
        assert_eq!(again, text);

        // Without an anchor, an unknown line, a bad anchor
        let text = area_file_text(&area, None, None);
        assert_eq!(parse_area_file(&text).unwrap().anchor, None);
        let text = format!("{text}color blue\nanchor 1 2\n");
        let file = parse_area_file(&text).unwrap();
        assert_eq!(file.anchor, None);
        assert_eq!(file.parsed.ignored.len(), 1);
        assert!(file.parsed.ignored[0].ends_with("color blue"));
        assert_eq!(file.parsed.warnings.len(), 1);
        assert!(file.parsed.warnings[0].contains("anchor"));
        // Errors of the layout stay errors
        assert!(parse_area_file("anchor 1 2 3 4").is_err());
    }

    fn turned_area(yaw: f32) -> PlayArea {
        let mut area = PlayArea::from_marker_box(&params());
        area.turn_box(0, yaw);
        area
    }

    #[test]
    fn handles_of_a_turned_box_turn_with_it() {
        let yaw = 0.7;
        let straight = placed(turned_area(0.0));
        let turned = placed(turned_area(yaw));
        let to_area = turned.area_to_world.inverse();
        let area_box = turned.area.boxes()[0];
        let handles = edit_handles(&turned, 0, false);
        for (handle, unturned) in handles.iter().zip(edit_handles(&straight, 0, false)) {
            assert_eq!(handle.id, unturned.id);
            if handle.id.kind.of_box() {
                // The same place in the frame of the box
                let local = area_box.to_local(to_area.transform_point3(handle.position));
                let local_unturned = to_area.transform_point3(unturned.position) - area_box.center;
                assert!(local.distance(local_unturned) < 1e-5, "{:?}", handle.id);
                assert!(
                    area_box
                        .turn_back(to_area.transform_vector3(handle.normal))
                        .distance(to_area.transform_vector3(unturned.normal))
                        < 1e-5
                );
            }
        }
        // The side handles are on the surface, the turn handle away from the center and the sides
        for handle in &handles {
            let local = to_area.transform_point3(handle.position);
            match handle.id.kind {
                HandleKind::Side(_) => {
                    assert!(area_box.signed_distance(local, f32::INFINITY).abs() < 1e-5);
                }
                HandleKind::Turn => {
                    let offset = local - area_box.center;
                    assert!(offset.y.abs() < 1e-6);
                    let radius = offset.length();
                    assert!((TURN_HANDLE_MIN..=TURN_HANDLE_MAX + 1e-6).contains(&radius));
                    for other in &handles {
                        if other.id != handle.id {
                            assert!(other.position.distance(handle.position) > 0.04, "{other:?}");
                        }
                    }
                }
                _ => (),
            }
        }
    }

    #[test]
    fn turn_handle_of_a_small_box_stays_apart() {
        let area = PlayArea::single(AreaBox::new(Vec3::ZERO, Vec3::splat(0.1)));
        let handles = edit_handles(&placed(area), 0, false);
        let turn = handles
            .iter()
            .find(|handle| handle.id.kind == HandleKind::Turn)
            .unwrap();
        for other in &handles {
            if other.id != turn.id {
                assert!(other.position.distance(turn.position) > 0.04, "{other:?}");
            }
        }
    }

    #[test]
    fn sweep_angle_around_the_axis() {
        let center = Vec3::new(0.3, 1.0, -0.5);
        let at = |degrees: f32, radius: f32, height: f32| {
            // An angle like a yaw: +x is 0, -z is 90
            let (sin, cos) = degrees.to_radians().sin_cos();
            center + Vec3::new(cos * radius, height, -sin * radius)
        };
        let swept = sweep_angle(center, at(10.0, 0.2, 0.0), at(40.0, 0.25, 0.1)).unwrap();
        assert!((swept - 30_f32.to_radians()).abs() < 1e-5, "{swept}");
        // As a yaw turns: the box turned by the angle swept keeps the point where it was
        let mut area_box = AreaBox::new(center, Vec3::splat(0.3));
        let point = at(10.0, 0.2, 0.0);
        let local = area_box.to_local(point);
        area_box.yaw = swept;
        assert!(area_box.to_area(local).distance(at(40.0, 0.2, 0.0)) < 1e-5);
        // Across -180 / 180: the short way
        let swept = sweep_angle(center, at(170.0, 0.2, 0.0), at(-170.0, 0.2, 0.0)).unwrap();
        assert!((swept - 20_f32.to_radians()).abs() < 1e-5, "{swept}");
        // Near the axis: none
        assert_eq!(
            sweep_angle(center, at(0.0, 0.01, 0.0), at(90.0, 0.2, 0.0)),
            None
        );
    }

    #[test]
    fn turning_snaps_to_detents() {
        let deg = f32::to_radians;
        let (yaw, snapped) = turned_yaw(0.0, deg(13.5));
        assert!(snapped && (yaw - deg(15.0)).abs() < 1e-6);
        let (yaw, snapped) = turned_yaw(deg(5.0), deg(7.0));
        assert!(!snapped && (yaw - deg(12.0)).abs() < 1e-6);
        let (yaw, snapped) = turned_yaw(deg(-44.0), deg(-2.5));
        assert!(snapped && (yaw - deg(-45.0)).abs() < 1e-6);
        // Wrapped
        let (yaw, snapped) = turned_yaw(deg(170.0), deg(44.0));
        assert!(
            !snapped && (yaw - deg(-146.0)).abs() < 1e-5,
            "{}",
            yaw.to_degrees()
        );
        let (yaw, snapped) = turned_yaw(deg(179.0), deg(2.0));
        assert!(snapped && (yaw.abs() - deg(180.0)).abs() < 1e-5);

        // The layout: only that box, nothing else changes
        let mut area = PlayArea::from_marker_box(&params());
        area.add_box(0, Side::new(0, true)).unwrap();
        let (turned, yaw, snapped) = apply_turn(&area, 1, deg(31.0));
        assert!(snapped && (yaw - deg(30.0)).abs() < 1e-6);
        assert_eq!(turned.boxes()[0], area.boxes()[0]);
        let (before, after) = (area.boxes()[1], turned.boxes()[1]);
        assert_eq!((after.center, after.half), (before.center, before.half));
        assert_eq!(apply_turn(&area, 5, 1.0).0, area);
    }

    #[test]
    fn drags_in_the_frame_of_a_turned_box() {
        let yaw = 0.6;
        let area = turned_area(yaw);
        let start = area.boxes()[0];
        let side = Side::new(0, true);
        let normal = start.side_normal(side);
        let id = |kind| HandleId { kind, box_index: 0 };
        // Along the normal of the turned side: that side moves by it, the opposite one stays
        let across = start.turn(Vec3::new(0.0, 0.1, 0.2));
        let (dragged, moved) = apply_drag(&area, id(HandleKind::Side(side)), normal * 0.1 + across);
        assert!((moved - 0.1).abs() < 1e-5, "{moved}");
        let after = dragged.boxes()[0];
        assert!((after.half.x - start.half.x - 0.05).abs() < 1e-5);
        assert_eq!(
            (after.half.y, after.half.z, after.yaw),
            (start.half.y, start.half.z, yaw)
        );
        let opposite = side.opposite();
        assert!(
            after
                .side_center(opposite)
                .distance(start.side_center(opposite))
                < 1e-5
        );
        assert!(
            after
                .side_center(side)
                .distance(start.side_center(side) + normal * 0.1)
                < 1e-5
        );
        // The +x axis of the area frame moves the side by the cosine of the yaw only
        let (_, moved) = apply_drag(&area, id(HandleKind::Side(side)), Vec3::X * 0.1);
        assert!((moved - 0.1 * yaw.cos()).abs() < 1e-5, "{moved}");

        // A dome along its normal
        let front = Side::new(2, false);
        let (domed, height) = apply_drag(
            &area,
            id(HandleKind::Dome(front)),
            start.side_normal(front) * 0.12 + start.turn(Vec3::new(0.1, 0.1, 0.0)),
        );
        assert!((height - 0.12).abs() < 1e-5, "{height}");
        assert!((domed.boxes()[0].dome_height(front) - 0.12).abs() < 1e-5);

        // The center: in the area frame, the box keeps its yaw
        let delta = Vec3::new(0.1, -0.05, 0.2);
        let (moved_area, _) = apply_drag(&area, id(HandleKind::Center), delta);
        assert!(moved_area.boxes()[0].center.distance(start.center + delta) < 1e-6);
        assert_eq!(moved_area.boxes()[0].yaw, yaw);
    }

    fn far_handle(kind: HandleKind, position: Vec3, normal: Vec3) -> Handle {
        Handle {
            id: HandleId { kind, box_index: 0 },
            position,
            normal,
        }
    }

    #[test]
    fn ray_passes_nearest_to_an_axis() {
        // The axis along +x through (0, 1, -2); a ray from the origin straight at a point of it
        let point = Vec3::new(0.0, 1.0, -2.0);
        let ray = |to: Vec3| HandRay::new(Vec3::new(0.0, 1.0, 0.0), to - Vec3::Y).unwrap();
        let param = ray_axis_param(&ray(point + Vec3::X * 0.3), point, Vec3::X).unwrap();
        assert!((param - 0.3).abs() < 1e-5, "{param}");
        // Passing beside the axis (10 cm above): the point of the axis below where it passes
        let param =
            ray_axis_param(&ray(point + Vec3::new(-0.4, 0.1, 0.0)), point, Vec3::X).unwrap();
        assert!((param + 0.4).abs() < 0.02, "{param}");
        // Near parallel (the ray along the axis), and behind the origin: none
        let along = HandRay::new(point - Vec3::X, Vec3::new(1.0, 0.0, 0.1)).unwrap();
        assert_eq!(ray_axis_param(&along, point, Vec3::X), None);
        let away = HandRay::new(Vec3::new(0.0, 1.0, 0.0), Vec3::Z).unwrap();
        assert_eq!(ray_axis_param(&away, point, Vec3::X), None);
    }

    #[test]
    fn far_side_follows_the_ray_along_its_axis() {
        // The +x side of a box 2 m ahead, its handle at (0.4, 1, -2); the hand at (0.2, 1, -0.3)
        let handle = far_handle(
            HandleKind::Side(Side::new(0, true)),
            Vec3::new(0.4, 1.0, -2.0),
            Vec3::X,
        );
        let hand = Vec3::new(0.2, 1.0, -0.3);
        let ray_to = |from: Vec3, to: Vec3| HandRay::new(from, to - from).unwrap();
        let start = ray_to(hand, handle.position + Vec3::new(0.02, 0.03, 0.0));
        let mut drag = FarDrag::new(&handle, &start, hand).unwrap();
        // The gain: 1.7 m over 0.45 m
        assert!((drag.gain() - handle.position.distance(hand) / FAR_HAND_REACH).abs() < 1e-5);
        // No jump: the same ray moves nothing (the grab was 2 cm beside the handle)
        assert_eq!(drag.follow(&start, hand).moved, Vec3::ZERO);
        // The ray turned to a point 30 cm farther out along the axis: the side follows by 30 cm, the
        // small motion of the hand does not count
        let step = drag.follow(
            &ray_to(hand, handle.position + Vec3::new(0.32, 0.03, 0.0)),
            hand + Vec3::new(0.01, 0.0, 0.0),
        );
        assert!((step.moved.x - 0.3).abs() < 0.01, "{}", step.moved);
        assert!(step.moved.y.abs() < 1e-6 && step.moved.z.abs() < 1e-6);
        assert!(step.point.distance(handle.position + step.moved) < 1e-6);
        // The ray nearly parallel to the axis: the hand along the axis times the gain
        let before = step.moved;
        let along = HandRay::new(hand + Vec3::X * 0.02, Vec3::new(1.0, 0.0, -0.1)).unwrap();
        let step = drag.follow(&along, hand + Vec3::X * 0.02);
        assert!(
            (step.moved.x - before.x - 0.01 * drag.gain()).abs() < 1e-5,
            "{} {}",
            step.moved,
            before
        );
        // Out of parallel again: no jump, then it follows again
        let before = step.moved;
        let back = ray_to(hand, handle.position + Vec3::new(0.5, 0.0, 0.0));
        assert_eq!(drag.follow(&back, hand).moved, before);
        let step = drag.follow(
            &ray_to(hand, handle.position + Vec3::new(0.4, 0.0, 0.0)),
            hand,
        );
        assert!(
            (step.moved.x - before.x + 0.1).abs() < 0.01,
            "{}",
            step.moved
        );
        // Buttons and remove are not dragged
        let undo = far_handle(HandleKind::Undo, handle.position, Vec3::ZERO);
        assert!(FarDrag::new(&undo, &start, hand).is_none());
    }

    #[test]
    fn far_center_moves_on_the_sphere_of_the_grab() {
        let handle = far_handle(HandleKind::Center, Vec3::new(0.0, 1.0, -2.0), Vec3::ZERO);
        let hand = Vec3::new(0.0, 1.0, -0.5);
        let ray = HandRay::new(hand, Vec3::new(0.01, 0.0, -1.0)).unwrap();
        let mut drag = FarDrag::new(&handle, &ray, hand).unwrap();
        assert_eq!(drag.follow(&ray, hand).moved, Vec3::ZERO);
        // Turned by 10 deg to the left: on the sphere of 1.5 m around the hand
        let turned = Quat::from_rotation_y(10_f32.to_radians()) * ray.direction;
        let step = drag.follow(&HandRay::new(hand, turned).unwrap(), hand);
        let distance = handle.position.distance(hand);
        assert!(((step.point - hand).length() - distance).abs() < 1e-5);
        assert!(step.moved.x < -0.2 && step.moved.x > -0.3, "{}", step.moved);
        // The hand moving along: the box with it
        let step = drag.follow(&HandRay::new(hand + Vec3::Y * 0.1, turned).unwrap(), hand);
        assert!(
            step.point
                .distance(hand + Vec3::Y * 0.1 + turned * distance)
                < 1e-5
        );
    }

    #[test]
    fn far_turn_follows_the_ray_around_the_axis() {
        // The turn handle 10 cm from the axis of a box 1.5 m ahead, 0.5 m below the hand
        let center = Vec3::new(0.0, 0.7, -1.5);
        let handle = far_handle(
            HandleKind::Turn,
            center + Vec3::new(0.07, 0.0, 0.07),
            Vec3::ZERO,
        );
        let hand = Vec3::new(0.0, 1.2, -0.3);
        let ray_to = |to: Vec3| HandRay::new(hand, to - hand).unwrap();
        let mut drag = FarDrag::new(&handle, &ray_to(handle.position), hand).unwrap();
        // Steep enough: where the ray meets the level plane of the handle
        let target = center + Vec3::new(-0.1, 0.0, 0.05);
        let step = drag.follow(&ray_to(target), hand);
        assert!(step.continuous);
        assert!(step.moved.distance(target - handle.position) < 1e-5);
        assert!(step.point.distance(target) < 1e-5, "{}", step.point);
        // From -45 deg to -153 deg around the axis
        let swept = sweep_angle(center, handle.position, step.point).unwrap();
        assert!(
            (swept.to_degrees() + 108.4).abs() < 0.5,
            "{}",
            swept.to_degrees()
        );
        // Level: the point of the ray at the distance of the grab, not continuous at the change
        let level = HandRay::new(hand, Vec3::new(0.0, -0.05, -1.0)).unwrap();
        let before = step.moved;
        let step = drag.follow(&level, hand);
        assert!(!step.continuous);
        assert!((step.point.distance(hand) - handle.position.distance(hand)).abs() < 1e-5);
        // The displacement does not jump at the change, and goes on from there
        assert!(
            step.moved.distance(before) < 1e-6,
            "{} {before}",
            step.moved
        );
        let lower = HandRay::new(hand, Vec3::new(0.1, -0.05, -1.0)).unwrap();
        let next = drag.follow(&lower, hand);
        assert!(next.continuous);
        assert!(next.moved.distance(before + next.point - step.point) < 1e-5);
    }

    #[test]
    fn far_turn_moves_nothing_at_the_grab() {
        // The ray at the grab 5 deg beside the turn handle (a pick allows that): the dead zone is
        // measured from where the ray points, not from the handle
        let center = Vec3::new(0.0, 0.7, -1.5);
        let handle = far_handle(
            HandleKind::Turn,
            center + Vec3::new(0.07, 0.0, 0.07),
            Vec3::ZERO,
        );
        let hand = Vec3::new(0.0, 1.2, -0.3);
        let to_handle = handle.position - hand;
        for (axis, steep) in [(Vec3::Y, true), (Vec3::X, true), (Vec3::Y, false)] {
            let direction = Quat::from_axis_angle(axis, 5_f32.to_radians()) * to_handle;
            // Level: the handle at the height of the hand, the point at the distance of the grab
            let (from, direction) = if steep {
                (hand, direction)
            } else {
                let from = Vec3::new(hand.x, handle.position.y + 0.02, hand.z);
                let to = handle.position - from;
                (from, Quat::from_axis_angle(axis, 5_f32.to_radians()) * to)
            };
            let ray = HandRay::new(from, direction).unwrap();
            let mut drag = FarDrag::new(&handle, &ray, hand).unwrap();
            let step = drag.follow(&ray, hand);
            assert!(step.continuous);
            assert!(
                step.point.distance(handle.position) > 0.05,
                "{}",
                step.point
            );
            assert_eq!(step.moved, Vec3::ZERO);
        }
    }

    #[test]
    fn far_drag_dead_zone_grows_with_the_gain() {
        let gain = 4.0;
        // 2 cm: inside 4 times the dead zone
        assert_eq!(scaled_drag_displacement(Vec3::X * 0.02, gain), Vec3::ZERO);
        let caught_up = (DRAG_DEAD_ZONE + DRAG_RAMP) * gain;
        let moved = Vec3::new(0.0, 0.0, caught_up + 0.05);
        assert!(scaled_drag_displacement(moved, gain).distance(moved) < 1e-5);
        // Gain 1 (or below, or not a number): the near drag
        let moved = Vec3::X * 0.02;
        for gain in [1.0, 0.3, f32::NAN] {
            assert_eq!(
                scaled_drag_displacement(moved, gain),
                drag_displacement(moved)
            );
        }
    }

    #[test]
    fn turn_guide_shows_the_angle() {
        let area = turned_area(0.0);
        let start = area.boxes()[0];
        let (turned, _, snapped) = apply_turn(&area, 0, 50_f32.to_radians());
        let now = turned.boxes()[0];
        let lines = turn_guide(&Mat4::IDENTITY, &start, &now, snapped, 0.002);
        // The two radial lines and the arc of 50 deg in 3 segments
        assert_eq!(lines.len(), 2 + 3);
        assert!(lines[0].dashed && !lines[1].dashed);
        assert_eq!(lines[1].color, OVERLAY_AMBER);
        // The line through the turned handle
        let handle = now.center + turn_handle_offset(&now);
        let direction = (lines[1].end - lines[1].start).normalize();
        assert!(direction.dot((handle - now.center).normalize()) > 1.0 - 1e-5);
        // The arc at the radius of the handle, from the handle at the grab to it now
        let radius = turn_handle_offset(&start).length();
        for line in &lines[2..] {
            assert!((line.start.distance(now.center) - radius).abs() < 1e-5);
        }
        assert!(
            lines[2]
                .start
                .distance(start.center + turn_handle_offset(&start))
                < 1e-5
        );
        assert!(lines[4].end.distance(handle) < 1e-5);
        // On a detent: green; without a turn: no arc
        let (turned, _, snapped) = apply_turn(&area, 0, 44_f32.to_radians());
        let lines = turn_guide(&Mat4::IDENTITY, &start, &turned.boxes()[0], snapped, 0.002);
        assert!(snapped && lines[1].color == OVERLAY_GREEN);
        assert_eq!(
            turn_guide(&Mat4::IDENTITY, &start, &start, false, 0.002).len(),
            2
        );
        // A whole half turn: at most the longest arc
        let (turned, _, _) = apply_turn(&area, 0, 3.1);
        let lines = turn_guide(&Mat4::IDENTITY, &start, &turned.boxes()[0], false, 0.002);
        assert_eq!(lines.len(), 2 + TURN_GUIDE_SEGMENTS);
    }

    #[test]
    fn buttons_have_no_drag() {
        for kind in [HandleKind::Undo, HandleKind::Done] {
            assert!(kind.is_button() && !kind.is_drag());
        }
        for kind in [
            HandleKind::Remove,
            HandleKind::Turn,
            HandleKind::Center,
            HandleKind::Side(Side::new(0, true)),
        ] {
            assert!(!kind.is_button());
        }
        assert!(HandleKind::Turn.is_drag());
    }
}
