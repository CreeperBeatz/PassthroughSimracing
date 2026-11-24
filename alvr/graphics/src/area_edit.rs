//! The play area editor, its pure parts: the handles and how a hand picks and drags them, the pinch of
//! a hand, the two-hand gesture that starts and ends editing, the shapes drawn for the handles, the undo
//! stack and the layout file. The client (area_editor.rs) keeps the state, reads the hands and writes
//! the files. Positions are in stage space unless the name says area frame.
//!
//! Handles of the selected box: one on each side, on top of its dome (drag along the normal of the
//! side: that side moves, the opposite one stays), a smaller one next to it for the dome (drag out:
//! higher, back to the side: flat, snapped below `DOME_SNAP`), one at its center (moves the box), "+"
//! (a tap adds a box next to the side the eyes look at) and "x" (held still for `REMOVE_HOLD_S`: removes
//! the box; not on the last one). The other boxes show their center handle only; pinching any handle
//! of a box selects it. Above the code: undo (a tap takes the last change back) and done (a tap ends
//! editing).
//!
//! Picking: the cursor of a hand is the midpoint of its thumb and index tips. The handle nearest to the
//! cursor within `HANDLE_REACH` is lit; handles about as near as that one (`HANDLE_TIE`) go by the line
//! of sight from the eyes through the cursor. With none in reach, the handle nearest to that line of
//! sight within `RAY_PICK_ANGLE` is lit (handles out of reach, like the far side of the box). The lit
//! handle stays lit against small changes, and the client keeps it while the fingers close: what is lit
//! before the pinch is what is taken. A grab moves the handle by the displacement of the cursor since
//! the pinch (no jump), after a small dead zone that is caught up over `DRAG_RAMP`.

use crate::{
    AreaBox, OverlaySegment, ParsedArea, PlacedArea, PlayArea, Side, play_area::OTHER_AREA_AXES,
};
use alvr_common::glam::{Vec2, Vec3, Vec4};
use std::fmt::Write;

/// A handle within this of the cursor is picked by nearness (meters)
pub const HANDLE_REACH: f32 = 0.08;
/// Handles this much farther than the nearest one go by the line of sight too (meters)
pub const HANDLE_TIE: f32 = 0.015;
/// Without a handle in reach: the handle nearest to the line of sight within this (radians)
pub const RAY_PICK_ANGLE: f32 = 4.0 * std::f32::consts::PI / 180.0;
/// A lit handle stays lit up to this angle from the line of sight (radians)
pub const RAY_KEEP_ANGLE: f32 = 6.0 * std::f32::consts::PI / 180.0;
/// Handles within this of the best angle: the one nearer to the eyes (radians)
pub const RAY_TIE_ANGLE: f32 = 1.5 * std::f32::consts::PI / 180.0;
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
/// A pinch released within this of where it started is a tap (meters)
pub const TAP_MOVE: f32 = 0.02;
/// The remove handle is held still this long (seconds)
pub const REMOVE_HOLD_S: f64 = 0.8;
/// Changes that can be taken back
pub const UNDO_DEPTH: usize = 20;

/// The dome handle sits beside the side handle, this share of the half extent of the side along it,
/// within the two distances (meters)
const DOME_HANDLE_SHARE: f32 = 0.35;
const DOME_HANDLE_MIN: f32 = 0.04;
const DOME_HANDLE_MAX: f32 = 0.12;
/// "+" and "x": above the center of the box, to its left and right (meters, area frame)
const BUTTON_HEIGHT: f32 = 0.09;
const BUTTON_GAP: f32 = 0.05;
/// Undo and done: above the code, to its left and right (meters, area frame)
const TOOLBAR_HEIGHT: f32 = 0.12;
const TOOLBAR_GAP: f32 = 0.05;

/// Half the size of a handle as drawn: this angle seen from the eyes, at least the minimum (meters)
const HANDLE_ANGULAR_SIZE: f32 = 0.012;
const HANDLE_MIN_RADIUS: f32 = 0.015;
/// Of the cursor of a hand (meters)
const CURSOR_RADIUS: f32 = 0.008;

/// Pinch: the gap between the surfaces of the thumb and index tips (center distance minus the two
/// radii) below `on` for this many updates closes it, above `off` for this long opens it (seconds)
pub const DEFAULT_PINCH_ON: f32 = 0.010;
pub const DEFAULT_PINCH_OFF: f32 = 0.025;
pub const PINCH_ON_UPDATES: u32 = 2;
pub const PINCH_OFF_HOLD_S: f64 = 0.08;
/// A closed pinch of a hand that is not usable (not tracked, out of view) opens after this (seconds)
pub const HAND_LOST_HOLD_S: f64 = 0.3;
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
/// reports hands as tracked also out of view (radians)
pub const IN_VIEW_ANGLE: f32 = 50.0 * std::f32::consts::PI / 180.0;

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
    /// A tap adds a box next to this one
    Add,
    /// Held still: removes this box
    Remove,
    /// A tap takes the last change back
    Undo,
    /// A tap ends editing
    Done,
}

impl HandleKind {
    /// For the log
    pub fn name(self) -> String {
        match self {
            Self::Side(side) => format!("side {}", side.name()),
            Self::Dome(side) => format!("dome {}", side.name()),
            Self::Center => "center".into(),
            Self::Add => "add".into(),
            Self::Remove => "remove".into(),
            Self::Undo => "undo".into(),
            Self::Done => "done".into(),
        }
    }

    /// Dragging it changes the layout
    pub fn is_drag(self) -> bool {
        matches!(self, Self::Side(_) | Self::Dome(_) | Self::Center)
    }

    /// It belongs to a box (pinching it selects that box)
    pub fn of_box(self) -> bool {
        !matches!(self, Self::Undo | Self::Done)
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
    offset
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
            handles.push(handle(HandleKind::Side(side), index, top, side.normal()));
            handles.push(handle(
                HandleKind::Dome(side),
                index,
                top + dome_handle_offset(area_box, side),
                side.normal(),
            ));
        }
        handles.push(handle(
            HandleKind::Center,
            index,
            area_box.center,
            Vec3::ZERO,
        ));
        let button = |x: f32| area_box.center + Vec3::new(x, BUTTON_HEIGHT, 0.0);
        handles.push(handle(
            HandleKind::Add,
            index,
            button(-BUTTON_GAP),
            Vec3::ZERO,
        ));
        if boxes.len() > 1 {
            handles.push(handle(
                HandleKind::Remove,
                index,
                button(BUTTON_GAP),
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

/// The handle a hand points at (see the module), `current`: the one lit until now. None: none
pub fn pick_handle(
    handles: &[Handle],
    eye: Vec3,
    cursor: Vec3,
    current: Option<HandleId>,
) -> Option<HandleId> {
    let ray = (cursor - eye).normalize_or_zero();
    if ray == Vec3::ZERO || !ray.is_finite() {
        return None;
    }
    let angle = |handle: &Handle| ray.angle_between((handle.position - eye).normalize_or(ray));
    let distance = |handle: &Handle| handle.position.distance(cursor);
    let current = current.and_then(|id| handles.iter().find(|handle| handle.id == id));

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
            .min_by(|a, b| angle(a).total_cmp(&angle(b)))?;
        if let Some(current) = current
            && distance(current) < HANDLE_REACH * KEEP_REACH_FACTOR
            && distance(current) <= distance(chosen) + KEEP_DISTANCE
        {
            return Some(current.id);
        }
        return Some(chosen.id);
    }
    // The lit one a little out of reach stays
    if let Some(current) = current
        && distance(current) < HANDLE_REACH * KEEP_REACH_FACTOR
    {
        return Some(current.id);
    }

    // Out of reach: by the line of sight, about as good ones by the distance from the eyes
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
                    .distance(eye)
                    .total_cmp(&b.position.distance(eye))
            })
    });
    if let Some(current) = current
        && angle(current) < RAY_KEEP_ANGLE
        && chosen.is_none_or(|chosen| angle(current) <= angle(chosen) + RAY_TIE_ANGLE)
    {
        return Some(current.id);
    }

    chosen.map(|handle| handle.id)
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

/// The layout `start` with handle `id` dragged by `displacement` (area frame, after
/// `drag_displacement`), and the value it took for the log: how far the side moved, the height of the
/// dome, how far the box moved (meters). Other handles change nothing
pub fn apply_drag(start: &PlayArea, id: HandleId, displacement: Vec3) -> (PlayArea, f32) {
    let mut area = *start;
    let index = id.box_index;
    let value = match id.kind {
        HandleKind::Side(side) => area.drag_side(index, side, displacement.dot(side.normal())),
        HandleKind::Dome(side) => {
            let Some(area_box) = start.boxes().get(index) else {
                return (area, 0.0);
            };
            let height = area_box.dome_height(side) + displacement.dot(side.normal());
            area.set_dome(index, side, if height < DOME_SNAP { 0.0 } else { height })
        }
        HandleKind::Center => area.move_box(index, displacement).length(),
        _ => 0.0,
    };

    (area, value)
}

/// The side of a box a ray goes through first (area frame): where it leaves the box when it starts
/// inside, where it enters when it starts outside. None: it misses
fn ray_box_side(area_box: &AreaBox, origin: Vec3, direction: Vec3) -> Option<Side> {
    let mut best: Option<(f32, Side)> = None;
    for side in Side::ALL {
        let axis = side.axis;
        if direction[axis].abs() < 1e-6 {
            continue;
        }
        let plane = area_box.center[axis] + side.sign() * area_box.half[axis];
        let t = (plane - origin[axis]) / direction[axis];
        if t.is_nan() || t <= 0.0 {
            continue;
        }
        let point = origin + direction * t;
        let (u, v) = OTHER_AREA_AXES[axis];
        let within = |k: usize| (point[k] - area_box.center[k]).abs() <= area_box.half[k] + 1e-4;
        if within(u) && within(v) && best.is_none_or(|(best_t, _)| t < best_t) {
            best = Some((t, side));
        }
    }
    best.map(|(_, side)| side)
}

/// Adds a box next to box `from` (see `PlayArea::add_box`) on the side the eyes look at: the side the
/// line of sight goes through first, then the others in the order they face the eyes (from inside the
/// box: the direction of the view, from outside: the eyes). A side is skipped where the new box would
/// have its center in another box. `eye` and `gaze` in the area frame. Returns the new box and its side.
/// None: 8 boxes already, or no side has room
pub fn add_box_facing(
    area: &mut PlayArea,
    from: usize,
    eye: Vec3,
    gaze: Vec3,
) -> Option<(usize, Side)> {
    let source = *area.boxes().get(from)?;
    let gaze = gaze.normalize_or_zero();
    let inside = ((eye - source.center).abs() - source.half).max_element() < 0.0;
    let facing = |side: Side| {
        if inside {
            gaze.dot(side.normal())
        } else {
            side.normal()
                .dot((eye - source.side_center(side)).normalize_or_zero())
        }
    };
    let mut order = Side::ALL.to_vec();
    order.sort_by(|a, b| facing(*b).total_cmp(&facing(*a)));
    if let Some(hit) = ray_box_side(&source, eye, gaze) {
        order.retain(|&side| side != hit);
        order.insert(0, hit);
    }

    for side in order {
        let mut trial = *area;
        let Some(index) = trial.add_box(from, side) else {
            continue;
        };
        let center = trial.boxes()[index].center;
        let taken = area
            .boxes()
            .iter()
            .enumerate()
            .any(|(other, area_box)| other != from && area_box.contains(center));
        if !taken {
            *area = trial;
            return Some((index, side));
        }
    }
    None
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
/// pointing out of the side (dome, smaller), a diamond (center), a plus (add), a cross (remove), an
/// arrow to the left (undo), a check mark (done). At most 4 segments
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
        HandleKind::Add => glyph(
            handle.position,
            eye,
            radius,
            &[&[p(-1.0, 0.0), p(1.0, 0.0)], &[p(0.0, -1.0), p(0.0, 1.0)]],
            color,
            width,
        ),
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

    /// Tracked and in view
    pub fn usable(&self, eye: Vec3, forward: Vec3) -> bool {
        self.tracked && in_view(eye, forward, self.cursor(), IN_VIEW_ANGLE)
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
    /// `off`: the thresholds of the gap (meters)
    pub fn update(&mut self, gap: Option<f32>, now: f64, on: f32, off: f32) -> PinchEvent {
        let Some(gap) = gap.filter(|gap| gap.is_finite()) else {
            self.below = 0;
            self.above_since = None;
            if self.closed {
                let since = *self.lost_since.get_or_insert(now);
                if now - since >= HAND_LOST_HOLD_S {
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
        let (on, off) = (DEFAULT_PINCH_ON, DEFAULT_PINCH_OFF);
        let mut pinch = Pinch::default();
        let mut t = 0.0;
        let mut step = |pinch: &mut Pinch, gap: Option<f32>| {
            t += 1.0 / 72.0;
            pinch.update(gap, t, on, off)
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
        // 6 sides, 6 domes, center, add, remove; the centers of the 2 other boxes; done
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
    fn picking_by_nearness_and_line_of_sight() {
        let eye = Vec3::new(0.0, 1.2, 0.0);
        let a = at(HandleKind::Center, Vec3::new(0.0, 1.0, -0.4));
        let b = at(HandleKind::Add, Vec3::new(0.05, 1.0, -0.4));
        let far = at(HandleKind::Done, Vec3::new(0.3, 1.0, -1.5));
        let handles = [a, b, far];

        // The nearest in reach
        let cursor = Vec3::new(0.035, 1.0, -0.4);
        assert_eq!(pick_handle(&handles, eye, cursor, None), Some(b.id));
        // Nothing near and nothing on the line of sight
        assert_eq!(
            pick_handle(&handles, eye, Vec3::new(-0.3, 1.3, -0.4), None),
            None
        );
        // Out of reach: along the line of sight through the cursor
        let towards_far = eye + (far.position - eye).normalize() * 0.4;
        assert_eq!(pick_handle(&handles, eye, towards_far, None), Some(far.id));
        // Off by 5 degrees: not picked, but kept when it was lit
        let off = Quat::from_rotation_y(5.0_f32.to_radians()) * (far.position - eye).normalize();
        let cursor = eye + off * 0.4;
        assert_eq!(pick_handle(&handles, eye, cursor, None), None);
        assert_eq!(
            pick_handle(&handles, eye, cursor, Some(far.id)),
            Some(far.id)
        );

        // About as near: the one on the line of sight wins. One handle 2.5 cm straight behind the
        // cursor, the other 2 cm beside it
        let cursor = Vec3::new(0.0, 1.0, -0.4);
        let ray = (cursor - eye).normalize();
        let behind = at(HandleKind::Center, cursor + ray * 0.025);
        let beside = at(HandleKind::Add, cursor + Vec3::X * 0.02);
        let handles = [beside, behind];
        assert_eq!(pick_handle(&handles, eye, cursor, None), Some(behind.id));
        // Beside it by more than the tie: the nearer one
        let farther = at(HandleKind::Center, cursor + ray * 0.04);
        assert_eq!(
            pick_handle(&[beside, farther], eye, cursor, None),
            Some(beside.id)
        );

        // Hysteresis: the lit one stays unless another is nearer by a centimeter
        let handles = [a, b];
        let middle = (a.position + b.position) * 0.5 + Vec3::new(0.003, 0.0, 0.0);
        assert_eq!(pick_handle(&handles, eye, middle, Some(a.id)), Some(a.id));
        let nearer_b = b.position + Vec3::new(0.0, 0.0, 0.005);
        assert_eq!(pick_handle(&handles, eye, nearer_b, Some(a.id)), Some(b.id));
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
        let add = HandleId {
            kind: HandleKind::Add,
            box_index: 0,
        };
        assert_eq!(apply_drag(&area, add, delta).0, area);
    }

    #[test]
    fn added_box_goes_where_the_eyes_look() {
        let area = PlayArea::from_marker_box(&params());
        let area_box = area.boxes()[0];
        // From inside the box, looking along +x: the +x side
        let mut inside = area;
        let (index, side) = add_box_facing(&mut inside, 0, area_box.center, Vec3::X).unwrap();
        assert_eq!((index, side), (1, Side::new(0, true)));
        assert!(inside.boxes()[1].center.x > area_box.center.x + area_box.half.x);
        // Looking up and a little forward from inside: the top
        let mut up = area;
        let (_, side) =
            add_box_facing(&mut up, 0, area_box.center, Vec3::new(0.0, 1.0, -0.2)).unwrap();
        assert_eq!(side, Side::new(1, true));

        // From outside in front of the -z side, looking at it: the -z side
        let eye = area_box.center + Vec3::new(0.1, 0.2, -area_box.half.z - 0.8);
        let mut outside = area;
        let (_, side) = add_box_facing(&mut outside, 0, eye, area_box.center - eye).unwrap();
        assert_eq!(side, Side::new(2, false));

        // That side taken: the next one facing the eyes
        let (_, side) = add_box_facing(&mut outside, 0, eye, area_box.center - eye).unwrap();
        assert_ne!(side, Side::new(2, false));
        assert_eq!(outside.len(), 3);

        // Full: none
        let mut full = PlayArea::stress_layout(&params());
        assert_eq!(add_box_facing(&mut full, 0, Vec3::ZERO, Vec3::X), None);
        assert_eq!(full.len(), 8);
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
            HandleKind::Add,
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
    }

    #[test]
    fn handles_of_eight_boxes_leave_room_for_the_outline() {
        // The client draws the handles, the 12 edges of the selected box, 2 cursors, a guide line and
        // 2 progress squares; the outline of the area gets what is left of the 249 segments of an eye
        let placed = placed(PlayArea::stress_layout(&params()));
        let eye = Vec3::new(0.0, 1.2, 0.0);
        let handles = edit_handles(&placed, 3, true);
        let glyphs = handles
            .iter()
            .map(|handle| handle_glyph(handle, eye, 1.3, OVERLAY_CYAN, 0.002).len())
            .sum::<usize>();
        let editor = glyphs + 12 + 2 * 4 + 1 + 2 * 4;
        assert!(editor <= 115, "{editor}");
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
}
