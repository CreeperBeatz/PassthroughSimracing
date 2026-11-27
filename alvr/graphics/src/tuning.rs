//! The tuning menu, its pure parts: the praying-hands gesture that opens and closes it, the pages and
//! rows of settings with their ranges, the floating panel (where it is placed, which row a hand's ray
//! points at), how a pinch and a pinch-drag change a value, the green cap on the rig monitor (moved and
//! sized by hand, the UDP messages to `green_patch.py`), the profile file and what the panel shows. The
//! client (tuning_mode.rs) keeps the state, reads the hands, applies the values as overrides of the
//! `debug.alvr.*` properties, renders the panel (`render_menu`) as a quad layer and sends the messages.
//! Positions are in stage space (+Y up).
//!
//! Praying hands (`praying_pose`): both hands tracked, the palms facing each other (`PRAY_FACING_DOT`),
//! the palm centres within `PRAY_MAX_PALM_GAP`, the fingers pointing up (`PRAY_MIN_FINGER_ELEVATION`
//! above horizontal), both palms `PRAY_MIN_DISTANCE`..`PRAY_MAX_DISTANCE` from the eyes. Held for
//! `PRAY_HOLD_S` it fires (`PrayingGesture`); it must be let go before it fires again. The finger
//! direction is `EditorHand::finger_direction` (the client fills it from the palm joint), else the
//! direction from the palm to the index tip.
//!
//! The panel (`PanelPose`): `PANEL_WIDTH` x `PANEL_HEIGHT`, placed once on entry `PANEL_AHEAD` ahead of
//! the eyes along the yaw of the head and `PANEL_ABOVE` above them, turned to face the eyes. Panel
//! coordinates (u, v) run 0..1 from its left edge to its right one and from its top to its bottom, as
//! the texture: the tab strip at the top (`MENU_TAB_HEIGHT`), the rows (`MENU_ROWS_TOP`, at most
//! `MENU_MAX_ROWS` of `menu_row_height`), the stats footer (`MENU_FOOTER_TOP`). `render_menu` draws
//! with the same constants.
//!
//! Input (`TuneMenu::update`): the ray of each hand (`EditorHand::ray`) on the panel is its cursor; a
//! pinch on a tab, a switch or a button acts when the pinch opens; a pinch on a value starts a drag
//! along the panel's x axis: the full width of the panel is the full range of the row, snapped to its
//! step and clamped (after a dead zone of `DRAG_DEAD_U`). Pinches that were closed when the menu opened
//! do nothing until they open. On the green page, with "Move / size patch" on, a pinch off the panel
//! moves the cap (one hand: `patch_moved`) or sizes it (both hands: `patch_resized`).
//!
//! Values: properties are integers (as `debug_property` reads them), named without `debug.alvr.`
//! (`PROPERTY_PREFIX`). The value in effect is the menu's override, else the property as set with
//! setprop, else the client's default (`property_default`). "Room strictness / Skin preference" is one
//! row: `key_auto_weight` while `key_auto` is 1, else `key_bias`.

use crate::{
    DEFAULT_PINCH_OFF, DEFAULT_PINCH_ON, EditorHand, EditorInput, GRAB_LOST_HOLD_S,
    HAND_LOST_HOLD_S, HandRay, HoldTimer, Pinch, PinchEvent,
    area_edit::{file_safe_payload, fnv1a},
};
use alvr_common::glam::{Mat3, Quat, Vec2, Vec3};
use std::{collections::BTreeMap, fmt::Write};

// --- Praying hands --------------------------------------------------------------------------------

/// The palm normals face each other: their dot product is below this
pub const PRAY_FACING_DOT: f32 = -0.7;
/// Between the palm centres at most (meters)
pub const PRAY_MAX_PALM_GAP: f32 = 0.06;
/// The fingers of both hands point more than this above horizontal (radians)
pub const PRAY_MIN_FINGER_ELEVATION: f32 = 45.0 * std::f32::consts::PI / 180.0;
/// Both palms this far from the eyes (meters)
pub const PRAY_MIN_DISTANCE: f32 = 0.20;
pub const PRAY_MAX_DISTANCE: f32 = 0.60;
/// Held this long it fires (seconds)
pub const PRAY_HOLD_S: f64 = 1.0;

/// Along the fingers of a hand, unit: `EditorHand::finger_direction`, else from the palm to the index
/// tip. Zero: unknown
pub fn finger_direction(hand: &EditorHand) -> Vec3 {
    let given = hand.finger_direction.normalize_or_zero();
    if given != Vec3::ZERO {
        given
    } else {
        (hand.index_tip - hand.palm).normalize_or_zero()
    }
}

/// Whether the hands hold the praying pose (see the module)
pub fn praying_pose(input: &EditorInput) -> bool {
    let [Some(left), Some(right)] = &input.hands else {
        return false;
    };
    let hands = [left, right];
    if !hands.iter().all(|hand| hand.tracked) {
        return false;
    }
    let (a, b) = (
        left.palm_normal.normalize_or_zero(),
        right.palm_normal.normalize_or_zero(),
    );
    if a == Vec3::ZERO || b == Vec3::ZERO || a.dot(b) >= PRAY_FACING_DOT {
        return false;
    }
    if left.palm.distance(right.palm) >= PRAY_MAX_PALM_GAP {
        return false;
    }
    let min_up = PRAY_MIN_FINGER_ELEVATION.sin();
    hands.iter().all(|hand| {
        let distance = hand.palm.distance(input.eye);
        finger_direction(hand).y > min_up
            && (PRAY_MIN_DISTANCE..=PRAY_MAX_DISTANCE).contains(&distance)
    })
}

/// The praying hands held for `PRAY_HOLD_S` (short drops tolerated as `HoldTimer` does); after it
/// fired the hands must let go of the pose before it fires again
#[derive(Clone, Copy, Debug, Default)]
pub struct PrayingGesture {
    timer: HoldTimer,
}

impl PrayingGesture {
    /// `input`: None, no head pose. Returns how far it is (0..1) and whether it fired in this update
    pub fn update(&mut self, input: Option<&EditorInput>, now: f64) -> (f32, bool) {
        let held = input.is_some_and(praying_pose);
        self.timer.update_for(held, now, PRAY_HOLD_S)
    }
}

// --- Pages and rows -------------------------------------------------------------------------------

/// The properties are `debug.alvr.<name>`
pub const PROPERTY_PREFIX: &str = "debug.alvr.";

/// `debug.alvr.<property>`
pub fn full_property(property: &str) -> String {
    format!("{PROPERTY_PREFIX}{property}")
}

/// The range of a value row: min, max, step, the client's default, the unit shown after it
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ValueRange {
    pub min: f32,
    pub max: f32,
    pub step: f32,
    pub default: f32,
    pub unit: &'static str,
}

impl ValueRange {
    const fn new(min: f32, max: f32, step: f32, default: f32, unit: &'static str) -> Self {
        Self {
            min,
            max,
            step,
            default,
            unit,
        }
    }

    /// Snapped to the step from `min`, clamped to min..max
    pub fn snap(&self, value: f32) -> f32 {
        if !value.is_finite() {
            return self.default;
        }
        let step = if self.step > 0.0 { self.step } else { 1.0 };
        let snapped = self.min + ((value - self.min) / step).round() * step;
        // No 0.15000001 from the float steps
        ((snapped.clamp(self.min, self.max)) * 10_000.0).round() / 10_000.0
    }

    /// Where `value` is in the range, 0..1
    pub fn fraction(&self, value: f32) -> f32 {
        if self.max > self.min {
            ((value - self.min) / (self.max - self.min)).clamp(0.0, 1.0)
        } else {
            0.0
        }
    }
}

/// What a row does that is not a value
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TuneAction {
    /// Opens the area editor (the boxes decide what is real at all)
    EditBoxes,
    /// Switches the moving / sizing of the green cap by hand (green page)
    PatchMove,
    /// Removes the overrides of the properties of the page
    ResetPage,
    /// Removes all overrides
    ResetAll,
    /// Ends tuning
    Done,
}

/// What a row changes
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RowKind {
    /// An integer property
    Value {
        property: &'static str,
        range: ValueRange,
    },
    /// A property that is 0 (off) or 1 (on)
    Toggle {
        property: &'static str,
        default: bool,
    },
    /// `key_auto_weight` (`KEY_AUTO_WEIGHT`) while `key_auto` is on, else `key_bias` (`KEY_BIAS`)
    KeyStrictness,
    /// The cut line of the green cap (`PatchCap::cut`), not a property
    CapCut,
    Action(TuneAction),
}

/// A row of a page
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TuneRow {
    pub label: &'static str,
    pub kind: RowKind,
}

/// The property, label and range of a value row as it is now (`KeyStrictness` resolved)
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ValueSpec {
    pub label: &'static str,
    /// None: the cap cut
    pub property: Option<&'static str>,
    pub range: ValueRange,
}

/// "Room strictness" while `key_auto` is on: how much a leak costs against the skin kept
/// (passthrough.rs `KeyAutoParams`, 0..1000 %, default 300)
pub const KEY_AUTO_WEIGHT: ValueSpec = ValueSpec {
    label: "Room strictness",
    property: Some("key_auto_weight"),
    range: ValueRange::new(0.0, 1000.0, 10.0, 300.0, "%"),
};
/// "Skin preference" while `key_auto` is off (auto_color_key.rs, 0..1000 %, default 150)
pub const KEY_BIAS: ValueSpec = ValueSpec {
    label: "Skin preference",
    property: Some("key_bias"),
    range: ValueRange::new(0.0, 1000.0, 10.0, 150.0, "%"),
};
/// The cut line of the cap: -1 nothing, 0 the upper half disc, 1 the full disc
pub const CAP_CUT: ValueSpec = ValueSpec {
    label: "Cap cut line",
    property: None,
    range: ValueRange::new(-1.0, 1.0, 0.05, 0.0, ""),
};

const fn value(label: &'static str, property: &'static str, range: ValueRange) -> TuneRow {
    TuneRow {
        label,
        kind: RowKind::Value { property, range },
    }
}

const fn action(label: &'static str, action: TuneAction) -> TuneRow {
    TuneRow {
        label,
        kind: RowKind::Action(action),
    }
}

const COMMON_ROWS: [TuneRow; 3] = [
    action("Reset page", TuneAction::ResetPage),
    action("Reset all", TuneAction::ResetAll),
    action("Done", TuneAction::Done),
];

// The defaults are the client's (the property unset): passthrough.rs, auto_color_key.rs,
// motion_cloud.rs (`CloudParams::default`), room_memory.rs (`DEFAULT_MARGIN_MM`), stream.rs
// (`MaskParams`), green_screen.rs (`GreenKeyParams::from_values`)
const HANDS_ROWS: [TuneRow; 7] = [
    TuneRow {
        label: "Auto key",
        kind: RowKind::Toggle {
            property: "key_auto",
            default: false,
        },
    },
    TuneRow {
        label: "Room strictness",
        kind: RowKind::KeyStrictness,
    },
    value(
        "Unknown colours kept",
        "key_unknown",
        ValueRange::new(0.0, 100.0, 5.0, 10.0, "%"),
    ),
    value(
        "Hand rim max",
        "key_rim_max",
        ValueRange::new(0.0, 30.0, 1.0, 8.0, "mm"),
    ),
    COMMON_ROWS[0],
    COMMON_ROWS[1],
    COMMON_ROWS[2],
];

const ARMS_ROWS: [TuneRow; 7] = [
    value(
        "Arm border to key",
        "cloud_trail",
        ValueRange::new(0.0, 48.0, 1.0, 0.0, "texels"),
    ),
    value(
        "Fast-arm reach",
        "cloud_cap",
        ValueRange::new(4.0, 48.0, 1.0, 14.0, "texels"),
    ),
    value(
        "Band gain",
        "cloud_gain",
        ValueRange::new(0.0, 400.0, 10.0, 150.0, "%"),
    ),
    value(
        "Hand ghost fix",
        "cloud_own",
        ValueRange::new(0.0, 40.0, 1.0, 0.0, "mm"),
    ),
    COMMON_ROWS[0],
    COMMON_ROWS[1],
    COMMON_ROWS[2],
];

const DEPTH_ROWS: [TuneRow; 6] = [
    value(
        "Depth edge position",
        "edge_cut",
        ValueRange::new(17.0, 84.0, 1.0, 20.0, "%"),
    ),
    value(
        "Memory margin",
        "mem_margin_mm",
        ValueRange::new(20.0, 150.0, 5.0, 60.0, "mm"),
    ),
    action("Edit boxes", TuneAction::EditBoxes),
    COMMON_ROWS[0],
    COMMON_ROWS[1],
    COMMON_ROWS[2],
];

const GREEN_ROWS: [TuneRow; 8] = [
    value(
        "Green strength",
        "green_ratio",
        ValueRange::new(0.0, 300.0, 5.0, 135.0, "%"),
    ),
    value(
        "Dark green floor",
        "green_floor",
        ValueRange::new(0.0, 255.0, 1.0, 70.0, ""),
    ),
    value(
        "Edge softness",
        "green_soft",
        ValueRange::new(1.0, 255.0, 1.0, 24.0, ""),
    ),
    TuneRow {
        label: "Cap cut line",
        kind: RowKind::CapCut,
    },
    action("Move / size patch", TuneAction::PatchMove),
    COMMON_ROWS[0],
    COMMON_ROWS[1],
    COMMON_ROWS[2],
];

/// A page of the menu, a tab
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TunePage {
    #[default]
    HandsVsRoom,
    ArmsInMotion,
    DepthEdge,
    GreenScreen,
}

impl TunePage {
    /// In the order of the tabs
    pub const ALL: [TunePage; 4] = [
        TunePage::HandsVsRoom,
        TunePage::ArmsInMotion,
        TunePage::DepthEdge,
        TunePage::GreenScreen,
    ];

    pub fn title(self) -> &'static str {
        match self {
            TunePage::HandsVsRoom => "Hands vs room",
            TunePage::ArmsInMotion => "Arms in motion",
            TunePage::DepthEdge => "Depth edge",
            TunePage::GreenScreen => "Green screen",
        }
    }

    pub fn rows(self) -> &'static [TuneRow] {
        match self {
            TunePage::HandsVsRoom => &HANDS_ROWS,
            TunePage::ArmsInMotion => &ARMS_ROWS,
            TunePage::DepthEdge => &DEPTH_ROWS,
            TunePage::GreenScreen => &GREEN_ROWS,
        }
    }

    /// Its index among the tabs
    pub fn index(self) -> usize {
        Self::ALL.iter().position(|page| *page == self).unwrap_or(0)
    }

    /// The properties its rows change (both of "Room strictness")
    pub fn properties(self) -> Vec<&'static str> {
        let mut properties = Vec::new();
        for row in self.rows() {
            match row.kind {
                RowKind::Value { property, .. } | RowKind::Toggle { property, .. } => {
                    properties.push(property);
                }
                RowKind::KeyStrictness => {
                    properties.extend(KEY_AUTO_WEIGHT.property);
                    properties.extend(KEY_BIAS.property);
                }
                RowKind::CapCut | RowKind::Action(_) => (),
            }
        }
        properties
    }
}

/// Every property the menu changes, page by page
pub fn tuned_properties() -> Vec<&'static str> {
    TunePage::ALL
        .iter()
        .flat_map(|page| page.properties())
        .collect()
}

/// The range of a property of the menu (a toggle: 0..1). None: not one of the menu
pub fn property_range(property: &str) -> Option<ValueRange> {
    for spec in [KEY_AUTO_WEIGHT, KEY_BIAS] {
        if spec.property == Some(property) {
            return Some(spec.range);
        }
    }
    TunePage::ALL
        .iter()
        .flat_map(|page| page.rows())
        .find_map(|row| match row.kind {
            RowKind::Value {
                property: name,
                range,
            } if name == property => Some(range),
            RowKind::Toggle {
                property: name,
                default,
            } if name == property => Some(ValueRange::new(
                0.0,
                1.0,
                1.0,
                if default { 1.0 } else { 0.0 },
                "",
            )),
            _ => None,
        })
}

/// The client's default of a property of the menu
pub fn property_default(property: &str) -> Option<i64> {
    property_range(property).map(|range| range.default.round() as i64)
}

/// The values of the properties of the menu
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TuneValues {
    /// As set with setprop (without the menu), read by the client; missing: unset. Names without
    /// `PROPERTY_PREFIX`
    pub props: BTreeMap<String, i64>,
    /// The menu's values, which win over the properties
    pub overrides: BTreeMap<String, i64>,
}

impl TuneValues {
    /// The value without the menu: the property, else the default
    pub fn base(&self, property: &str) -> i64 {
        self.props
            .get(property)
            .copied()
            .or_else(|| property_default(property))
            .unwrap_or(0)
    }

    /// The value in effect: the override, else `base`
    pub fn value(&self, property: &str) -> i64 {
        self.overrides
            .get(property)
            .copied()
            .unwrap_or_else(|| self.base(property))
    }

    pub fn is_overridden(&self, property: &str) -> bool {
        self.overrides.contains_key(property)
    }

    pub fn key_auto_on(&self) -> bool {
        self.value("key_auto") > 0
    }

    /// The value row as it is now: "Room strictness" is `key_auto_weight` while `key_auto` is on, else
    /// "Skin preference", `key_bias`. None: no value row
    pub fn value_spec(&self, row: &TuneRow) -> Option<ValueSpec> {
        match row.kind {
            RowKind::Value { property, range } => Some(ValueSpec {
                label: row.label,
                property: Some(property),
                range,
            }),
            RowKind::KeyStrictness => Some(if self.key_auto_on() {
                KEY_AUTO_WEIGHT
            } else {
                KEY_BIAS
            }),
            RowKind::CapCut => Some(CAP_CUT),
            RowKind::Toggle { .. } | RowKind::Action(_) => None,
        }
    }
}

// --- The panel ------------------------------------------------------------------------------------

pub const PANEL_WIDTH: f32 = 0.40;
pub const PANEL_HEIGHT: f32 = 0.30;
/// From the eyes along the yaw of the head, and above them (meters)
pub const PANEL_AHEAD: f32 = 0.55;
pub const PANEL_ABOVE: f32 = 0.15;
/// The texture of the panel (pixels), the aspect of the panel
pub const MENU_TEXTURE_WIDTH: u32 = 1024;
pub const MENU_TEXTURE_HEIGHT: u32 = 768;

/// Layout, fractions of the panel's height (v) from its top: the tab strip, the rows, the footer
pub const MENU_TAB_HEIGHT: f32 = 0.12;
pub const MENU_ROWS_TOP: f32 = 0.14;
pub const MENU_FOOTER_TOP: f32 = 0.88;
pub const MENU_MAX_ROWS: usize = 8;
/// Layout of a row, fractions of the panel's width (u): the override mark, the label (left aligned),
/// the value (right aligned at), the bar
pub const ROW_MARK_LEFT: f32 = 0.008;
pub const ROW_LABEL_LEFT: f32 = 0.04;
pub const ROW_VALUE_RIGHT: f32 = 0.62;
pub const ROW_BAR_LEFT: f32 = 0.65;
pub const ROW_BAR_RIGHT: f32 = 0.97;

/// Height of a row, fraction of the panel's height
pub fn menu_row_height() -> f32 {
    (MENU_FOOTER_TOP - 0.01 - MENU_ROWS_TOP) / MENU_MAX_ROWS as f32
}

/// The panel in the room, stage space
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PanelPose {
    pub center: Vec3,
    /// Along its width (u grows), horizontal, unit
    pub right: Vec3,
    /// Along its height upwards (v falls), unit
    pub up: Vec3,
    /// Out of its front, towards the eyes, unit
    pub normal: Vec3,
}

impl PanelPose {
    /// `PANEL_AHEAD` ahead of the eyes along the yaw of `forward`, `PANEL_ABOVE` above them, facing the
    /// eyes
    pub fn from_head(eye: Vec3, forward: Vec3) -> Self {
        let ahead = Vec3::new(forward.x, 0.0, forward.z).normalize_or(Vec3::NEG_Z);
        let center = eye + ahead * PANEL_AHEAD + Vec3::Y * PANEL_ABOVE;
        let normal = (eye - center).normalize_or(-ahead);
        let right = Vec3::Y.cross(normal).normalize_or(ahead.cross(Vec3::Y));
        let up = normal.cross(right);
        Self {
            center,
            right,
            up,
            normal,
        }
    }

    /// For the quad layer (OpenXR quads lie in their x-y plane and face +Z)
    pub fn orientation(&self) -> Quat {
        Quat::from_mat3(&Mat3::from_cols(self.right, self.up, self.normal)).normalize()
    }

    /// Where the ray meets the plane of the panel from its front, (u, v) not limited to 0..1. None:
    /// along the plane, from behind or away from it
    pub fn plane_uv(&self, ray: &HandRay) -> Option<Vec2> {
        let towards = ray.direction.dot(self.normal);
        if towards > -1e-4 {
            return None;
        }
        let distance = (self.center - ray.origin).dot(self.normal) / towards;
        if distance <= 0.0 || !distance.is_finite() {
            return None;
        }
        let offset = ray.at(distance) - self.center;
        Some(Vec2::new(
            offset.dot(self.right) / PANEL_WIDTH + 0.5,
            0.5 - offset.dot(self.up) / PANEL_HEIGHT,
        ))
    }

    /// `plane_uv` on the panel only
    pub fn hit(&self, ray: &HandRay) -> Option<Vec2> {
        self.plane_uv(ray)
            .filter(|uv| (0.0..=1.0).contains(&uv.x) && (0.0..=1.0).contains(&uv.y))
    }

    /// Stage position of (u, v)
    pub fn point(&self, uv: Vec2) -> Vec3 {
        self.center
            + self.right * (uv.x - 0.5) * PANEL_WIDTH
            + self.up * (0.5 - uv.y) * PANEL_HEIGHT
    }
}

/// What is at a spot of the panel
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelSpot {
    /// Index of the page
    Tab(usize),
    /// Index of the row on its page
    Row(usize),
    Footer,
    /// Between them
    Empty,
}

/// What is at (u, v) of a panel with `rows` rows
pub fn spot_at(uv: Vec2, rows: usize) -> PanelSpot {
    if uv.y < MENU_TAB_HEIGHT {
        let tabs = TunePage::ALL.len();
        return PanelSpot::Tab(((uv.x.clamp(0.0, 1.0) * tabs as f32) as usize).min(tabs - 1));
    }
    if uv.y >= MENU_FOOTER_TOP {
        return PanelSpot::Footer;
    }
    if uv.y >= MENU_ROWS_TOP {
        let row = ((uv.y - MENU_ROWS_TOP) / menu_row_height()) as usize;
        if row < rows.min(MENU_MAX_ROWS) {
            return PanelSpot::Row(row);
        }
    }
    PanelSpot::Empty
}

// --- Dragging a value -----------------------------------------------------------------------------

/// Along the panel's x axis a pinch moves this much (fraction of its width) before a value changes
pub const DRAG_DEAD_U: f32 = 0.01;

/// The value `start` dragged from u `start_u` to `u`: the full width of the panel is the full range,
/// after `DRAG_DEAD_U`, snapped and clamped
pub fn dragged_value(range: &ValueRange, start: f32, start_u: f32, u: f32) -> f32 {
    let moved = u - start_u;
    let moved = moved.signum() * (moved.abs() - DRAG_DEAD_U).max(0.0);
    range.snap(start + moved * (range.max - range.min))
}

// --- The green cap --------------------------------------------------------------------------------

/// The green patch on the rig monitor as a cap: the part of a disc above a cut line. Fractions of the
/// monitor, (0, 0) its top left corner (as `green_patch.py`'s rect)
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PatchCap {
    /// Centre of the disc, of the monitor's width, 0..1
    pub cx: f32,
    /// Centre of the disc, of the monitor's height from its top, 0..1
    pub cy: f32,
    /// Radius, of the monitor's height (the disc stays round), `PATCH_MIN_RADIUS`..1
    pub r: f32,
    /// The cut line, of the radius below the centre: -1 nothing shows, 0 the upper half disc, 1 the
    /// full disc
    pub cut: f32,
}

pub const PATCH_MIN_RADIUS: f32 = 0.02;
pub const PATCH_MAX_RADIUS: f32 = 1.0;

impl Default for PatchCap {
    fn default() -> Self {
        Self {
            cx: 0.5,
            cy: 0.6,
            r: 0.2,
            cut: 0.0,
        }
    }
}

impl PatchCap {
    /// Within the ranges; a value that is not finite is the default's
    pub fn clamped(self) -> Self {
        let defaults = Self::default();
        let fix = |value: f32, default: f32, min: f32, max: f32| {
            if value.is_finite() {
                value.clamp(min, max)
            } else {
                default
            }
        };
        Self {
            cx: fix(self.cx, defaults.cx, 0.0, 1.0),
            cy: fix(self.cy, defaults.cy, 0.0, 1.0),
            r: fix(self.r, defaults.r, PATCH_MIN_RADIUS, PATCH_MAX_RADIUS),
            cut: fix(self.cut, defaults.cut, -1.0, 1.0),
        }
    }
}

/// The cap moves this much (fraction of the monitor) per meter the hand moves: 1 cm, 1 %
pub const PATCH_MOVE_PER_M: f32 = 1.0;
/// The radius grows this much (fraction of the monitor's height) per meter the two pinches move apart
pub const PATCH_RADIUS_PER_M: f32 = 1.0;

/// The cap `start` after a one-hand drag by `hand_moved` (stage space): the hand to the right (along
/// `right`, the panel's right) moves the cap to the right, up (+Y) moves it up. Clamped
pub fn patch_moved(start: PatchCap, hand_moved: Vec3, right: Vec3) -> PatchCap {
    let right = Vec3::new(right.x, 0.0, right.z).normalize_or(Vec3::X);
    PatchCap {
        cx: start.cx + hand_moved.dot(right) * PATCH_MOVE_PER_M,
        cy: start.cy - hand_moved.y * PATCH_MOVE_PER_M,
        ..start
    }
    .clamped()
}

/// The cap `start` after the two pinches went from `span_start` to `span` apart (meters). Clamped
pub fn patch_resized(start: PatchCap, span_start: f32, span: f32) -> PatchCap {
    PatchCap {
        r: start.r + (span - span_start) * PATCH_RADIUS_PER_M,
        ..start
    }
    .clamped()
}

/// The UDP port of `green_patch.py`
pub const PATCH_UDP_PORT: u16 = 9945;
/// At most this often a cap is sent while it is dragged (seconds)
pub const PATCH_SEND_INTERVAL_S: f64 = 0.1;

/// `{"seq":n,"shape":"cap","cap":[cx,cy,r,cut],"visible":true}`: absolute, a lost one does no harm
pub fn patch_state_message(seq: u64, cap: &PatchCap, visible: bool) -> String {
    let cap = cap.clamped();
    format!(
        "{{\"seq\":{seq},\"shape\":\"cap\",\"cap\":[{:.4},{:.4},{:.4},{:.4}],\"visible\":{visible}}}",
        cap.cx, cap.cy, cap.r, cap.cut
    )
}

/// Asks `green_patch.py` for its state (the reply: `parse_patch_reply`)
pub const PATCH_GET_MESSAGE: &str = "{\"get\":1}";

/// Shows (or hides) the QR code with `payload` on the rig monitor
pub fn qr_message(payload: &str, show: bool) -> String {
    format!(
        "{{\"qr\":{},\"show\":{}}}",
        json_string(payload),
        u8::from(show)
    )
}

/// The caps to send while one is dragged: a changed cap at most every `PATCH_SEND_INTERVAL_S`, and the
/// last one when it is let go (`force`)
#[derive(Clone, Copy, Debug, Default)]
pub struct PatchLink {
    seq: u64,
    sent_at: Option<f64>,
    sent: Option<PatchCap>,
}

impl PatchLink {
    /// The message to send now, if any. `force`: whenever `cap` is not the one sent last
    pub fn message(&mut self, cap: &PatchCap, now: f64, force: bool) -> Option<String> {
        if self.sent == Some(*cap) {
            return None;
        }
        if !force
            && self
                .sent_at
                .is_some_and(|at| now - at < PATCH_SEND_INTERVAL_S)
        {
            return None;
        }
        self.seq += 1;
        self.sent_at = Some(now);
        self.sent = Some(*cap);
        Some(patch_state_message(self.seq, cap, true))
    }

    /// The number of the last message
    pub fn seq(&self) -> u64 {
        self.seq
    }
}

/// The state `green_patch.py` replies with. Missing or unreadable keys are None
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PatchReply {
    pub monitor: Option<i64>,
    /// x, y, w, h, fractions of the monitor
    pub rect: Option<[f32; 4]>,
    pub color: Option<[u8; 3]>,
    pub visible: Option<bool>,
    /// "rect" or "cap"
    pub shape: Option<String>,
    pub cap: Option<PatchCap>,
    pub seq: Option<u64>,
}

/// Reads a reply of `green_patch.py` (a JSON object; other keys are ignored)
pub fn parse_patch_reply(text: &str) -> Result<PatchReply, String> {
    let Json::Object(entries) = parse_json(text)? else {
        return Err("not a JSON object".into());
    };
    let get = |key: &str| entries.iter().find(|(name, _)| name == key).map(|(_, v)| v);
    let numbers = |key: &str| -> Option<Vec<f64>> {
        match get(key)? {
            Json::Array(items) => items
                .iter()
                .map(|item| match item {
                    Json::Number(number) if number.is_finite() => Some(*number),
                    _ => None,
                })
                .collect(),
            _ => None,
        }
    };
    let integer = |key: &str| match get(key)? {
        Json::Number(number) if number.is_finite() && number.fract() == 0.0 => Some(*number as i64),
        _ => None,
    };

    Ok(PatchReply {
        monitor: integer("monitor"),
        rect: numbers("rect")
            .and_then(|values| <[f64; 4]>::try_from(values).ok())
            .map(|values| values.map(|value| value as f32)),
        color: numbers("color")
            .and_then(|values| <[f64; 3]>::try_from(values).ok())
            .map(|values| values.map(|value| value.clamp(0.0, 255.0) as u8)),
        visible: match get("visible") {
            Some(Json::Bool(visible)) => Some(*visible),
            Some(Json::Number(number)) => Some(*number != 0.0),
            _ => None,
        },
        shape: match get("shape") {
            Some(Json::String(shape)) => Some(shape.clone()),
            _ => None,
        },
        cap: numbers("cap")
            .and_then(|values| <[f64; 4]>::try_from(values).ok())
            .map(|[cx, cy, r, cut]| {
                PatchCap {
                    cx: cx as f32,
                    cy: cy as f32,
                    r: r as f32,
                    cut: cut as f32,
                }
                .clamped()
            }),
        seq: integer("seq").and_then(|seq| u64::try_from(seq).ok()),
    })
}

// A small JSON reader for the replies (alvr_graphics has no serde_json)
#[derive(Clone, Debug, PartialEq)]
enum Json {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

fn parse_json(text: &str) -> Result<Json, String> {
    let mut reader = JsonReader {
        chars: text.chars().collect(),
        at: 0,
    };
    let value = reader.value(0)?;
    reader.skip_space();
    if reader.at < reader.chars.len() {
        return Err(format!("unexpected text at {}", reader.at));
    }
    Ok(value)
}

struct JsonReader {
    chars: Vec<char>,
    at: usize,
}

impl JsonReader {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.at).copied()
    }

    fn skip_space(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.at += 1;
        }
    }

    fn expect(&mut self, c: char) -> Result<(), String> {
        self.skip_space();
        if self.peek() == Some(c) {
            self.at += 1;
            Ok(())
        } else {
            Err(format!("expected '{c}' at {}", self.at))
        }
    }

    fn word(&mut self, word: &str, value: Json) -> Result<Json, String> {
        let end = self.at + word.chars().count();
        if self
            .chars
            .get(self.at..end)
            .is_some_and(|chars| chars.iter().copied().eq(word.chars()))
        {
            self.at = end;
            Ok(value)
        } else {
            Err(format!("unexpected text at {}", self.at))
        }
    }

    fn value(&mut self, depth: usize) -> Result<Json, String> {
        if depth > 16 {
            return Err("nested too deep".into());
        }
        self.skip_space();
        match self.peek() {
            Some('{') => {
                self.at += 1;
                let mut entries = Vec::new();
                self.skip_space();
                if self.peek() == Some('}') {
                    self.at += 1;
                    return Ok(Json::Object(entries));
                }
                loop {
                    self.skip_space();
                    let key = self.string()?;
                    self.expect(':')?;
                    entries.push((key, self.value(depth + 1)?));
                    self.skip_space();
                    match self.peek() {
                        Some(',') => self.at += 1,
                        Some('}') => {
                            self.at += 1;
                            return Ok(Json::Object(entries));
                        }
                        _ => return Err(format!("expected ',' or '}}' at {}", self.at)),
                    }
                }
            }
            Some('[') => {
                self.at += 1;
                let mut items = Vec::new();
                self.skip_space();
                if self.peek() == Some(']') {
                    self.at += 1;
                    return Ok(Json::Array(items));
                }
                loop {
                    items.push(self.value(depth + 1)?);
                    self.skip_space();
                    match self.peek() {
                        Some(',') => self.at += 1,
                        Some(']') => {
                            self.at += 1;
                            return Ok(Json::Array(items));
                        }
                        _ => return Err(format!("expected ',' or ']' at {}", self.at)),
                    }
                }
            }
            Some('"') => Ok(Json::String(self.string()?)),
            Some('t') => self.word("true", Json::Bool(true)),
            Some('f') => self.word("false", Json::Bool(false)),
            Some('n') => self.word("null", Json::Null),
            Some(c) if c == '-' || c.is_ascii_digit() => {
                let start = self.at;
                while self
                    .peek()
                    .is_some_and(|c| c.is_ascii_digit() || "+-.eE".contains(c))
                {
                    self.at += 1;
                }
                let number = self.chars[start..self.at].iter().collect::<String>();
                number
                    .parse::<f64>()
                    .map(Json::Number)
                    .map_err(|_| format!("bad number {number}"))
            }
            _ => Err(format!("unexpected text at {}", self.at)),
        }
    }

    fn string(&mut self) -> Result<String, String> {
        if self.peek() != Some('"') {
            return Err(format!("expected a string at {}", self.at));
        }
        self.at += 1;
        let mut text = String::new();
        loop {
            let Some(c) = self.peek() else {
                return Err("unterminated string".into());
            };
            self.at += 1;
            match c {
                '"' => return Ok(text),
                '\\' => {
                    let Some(escaped) = self.peek() else {
                        return Err("unterminated string".into());
                    };
                    self.at += 1;
                    match escaped {
                        'n' => text.push('\n'),
                        't' => text.push('\t'),
                        'r' => text.push('\r'),
                        'b' => text.push('\u{8}'),
                        'f' => text.push('\u{c}'),
                        'u' => {
                            let hex = self
                                .chars
                                .get(self.at..self.at + 4)
                                .map(|chars| chars.iter().collect::<String>())
                                .ok_or("bad \\u escape")?;
                            self.at += 4;
                            let code = u32::from_str_radix(&hex, 16)
                                .map_err(|_| "bad \\u escape".to_string())?;
                            text.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                        }
                        other => text.push(other),
                    }
                }
                c => text.push(c),
            }
        }
    }
}

/// `text` as a JSON string, quoted
fn json_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

// --- Profiles -------------------------------------------------------------------------------------

/// The profile used while no marker payload is known
pub const DEFAULT_PROFILE_FILE: &str = "tune-default.txt";
/// The key of the cap in a profile: `patch_cap=cx,cy,r,cut`
pub const PROFILE_CAP_KEY: &str = "patch_cap";

/// Name of the profile of a place: `tune-<payload>.txt`, the payload with characters other than letters,
/// digits, - and _ replaced by _ (at most 40, as `area_file_name`); when that changed the payload, a hash
/// of the whole payload follows, so that payloads that look alike get their own files. None:
/// `DEFAULT_PROFILE_FILE`
pub fn profile_file_name(payload: Option<&str>) -> String {
    let Some(payload) = payload.filter(|payload| !payload.is_empty()) else {
        return DEFAULT_PROFILE_FILE.into();
    };
    let name = file_safe_payload(payload);
    if name == payload {
        format!("tune-{name}.txt")
    } else {
        format!("tune-{name}-{:08x}.txt", fnv1a(payload.as_bytes()))
    }
}

/// The menu's values of a place
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TuneProfile {
    /// Property (without `PROPERTY_PREFIX`) to value, only properties of the menu
    pub overrides: BTreeMap<String, i64>,
    pub cap: Option<PatchCap>,
}

/// The text of a profile: a header comment, then `key=value` lines (properties without `debug.alvr.`,
/// sorted, then `patch_cap=cx,cy,r,cut`)
pub fn profile_text(profile: &TuneProfile, payload: Option<&str>) -> String {
    let mut text = String::from(
        "# ALVR tuning profile: the values of the tuning menu (praying hands), which win over the \
         debug.alvr.* properties\n",
    );
    let _ = writeln!(
        text,
        "# Place: {}. One key=value per line; unknown keys are ignored",
        payload.unwrap_or("(no marker)")
    );
    for (property, value) in &profile.overrides {
        let _ = writeln!(text, "{property}={value}");
    }
    if let Some(cap) = profile.cap {
        let cap = cap.clamped();
        let _ = writeln!(
            text,
            "{PROFILE_CAP_KEY}={:.4},{:.4},{:.4},{:.4}",
            cap.cx, cap.cy, cap.r, cap.cut
        );
    }
    text
}

/// Reads the text of `profile_text`. Comments, empty lines, unknown keys and values that do not read
/// are left out; values are clamped to the ranges of the menu
pub fn parse_profile(text: &str) -> TuneProfile {
    let mut profile = TuneProfile::default();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let (key, value) = (key.trim(), value.trim());
        let key = key.strip_prefix(PROPERTY_PREFIX).unwrap_or(key);
        if key == PROFILE_CAP_KEY {
            let values = value
                .split(',')
                .map(|value| value.trim().parse::<f32>().ok())
                .collect::<Option<Vec<_>>>();
            if let Some(&[cx, cy, r, cut]) = values.as_deref() {
                profile.cap = Some(PatchCap { cx, cy, r, cut }.clamped());
            }
            continue;
        }
        let (Some(range), Ok(value)) = (property_range(key), value.parse::<i64>()) else {
            continue;
        };
        let value = range.snap(value as f32).round() as i64;
        profile.overrides.insert(key.to_owned(), value);
    }
    profile
}

// --- What the panel shows -------------------------------------------------------------------------

/// The live numbers in the footer. Fractions 0..1 except the bias; None: not known
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TuneStats {
    /// Of the skin colours, the share the key keeps
    pub skin_kept: Option<f32>,
    /// Of the colours around the hands (room)
    pub background_kept: Option<f32>,
    /// Of the scene
    pub scene_kept: Option<f32>,
    /// The skin bias `key_auto` chose
    pub auto_bias: Option<f32>,
    /// Of the camera image, the share that is green
    pub green_share: Option<f32>,
}

impl TuneStats {
    /// The footer line
    pub fn text(&self) -> String {
        let percent = |value: Option<f32>| {
            value.map_or("-".to_string(), |value| format!("{:.0} %", value * 100.0))
        };
        format!(
            "skin {}   room {}   scene {}   bias {}   green {}",
            percent(self.skin_kept),
            percent(self.background_kept),
            percent(self.scene_kept),
            self.auto_bias
                .map_or("-".to_string(), |bias| format!("{bias:.2}")),
            percent(self.green_share),
        )
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct MenuTab {
    pub label: String,
    pub active: bool,
    pub hovered: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MenuRow {
    pub label: String,
    pub value: String,
    /// Where the value is in its range, 0..1; None: no bar
    pub bar: Option<f32>,
    /// The menu's value wins over the property
    pub overridden: bool,
    pub hovered: bool,
    /// Being dragged or pressed, or "Move / size patch" while it is on
    pub selected: bool,
}

/// What `render_menu` draws
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MenuContent {
    pub tabs: Vec<MenuTab>,
    pub rows: Vec<MenuRow>,
    /// The stats line
    pub footer: String,
    /// The cursors of the hands on the panel, (u, v)
    pub cursors: Vec<Vec2>,
}

// Integers as they are, values with fractional steps (the cut) signed with two decimals
fn value_text(value: f32, range: &ValueRange) -> String {
    let number = if range.step.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        format!("{value:+.2}")
    };
    if range.unit.is_empty() {
        number
    } else {
        format!("{number} {}", range.unit)
    }
}

// --- The menu -------------------------------------------------------------------------------------

/// What the menu did in an update, for the client to apply and log
#[derive(Clone, Debug, PartialEq)]
pub enum TuneEvent {
    /// The menu's value of `property` is `new` now (an override); `old` was in effect before. Many
    /// while a value is dragged: apply each
    Set {
        property: &'static str,
        old: i64,
        new: i64,
    },
    /// The override of `property` is removed: `new` (the property or its default) is in effect again
    Reset {
        property: &'static str,
        old: i64,
        new: i64,
    },
    /// A drag of a value ended: from the value at its start to the last one (for the log)
    Committed {
        property: &'static str,
        from: i64,
        to: i64,
    },
    /// The cap changed (a drag of it or of its cut): send it (`PatchLink`)
    Cap { old: PatchCap, new: PatchCap },
    /// A drag of the cap or its cut ended (send the last one with `force`, log it)
    CapCommitted { from: PatchCap, to: PatchCap },
    /// Another page is shown
    Page(TunePage),
    /// Moving / sizing the cap by hand is on or off
    PatchMode(bool),
    /// A button: `EditBoxes`, `Done` for the client; `ResetPage` / `ResetAll` after their `Reset`s, for
    /// the log; `PatchMove` comes as `PatchMode`
    Action(TuneAction),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum HandRole {
    /// Open
    Open,
    /// Closed when the menu opened, or on nothing: does nothing until it opens
    Ignored,
    /// On a tab or a button: acts when it opens
    Press(PanelSpot),
    /// Drags a value (`TuneMenu::drag`)
    Drag,
    /// Moves / sizes the cap
    Patch,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ValueDrag {
    hand: usize,
    row: usize,
    spec: ValueSpec,
    start_u: f32,
    start: f32,
    last: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum PatchGrab {
    None,
    Move {
        hand: usize,
        start_cursor: Vec3,
        start: PatchCap,
    },
    Resize {
        span_start: f32,
        start: PatchCap,
    },
}

/// The menu while it is open (see the module). The client creates it on entry with the panel placed
/// from the head (`PanelPose::from_head`), the values (properties and the profile's overrides) and the
/// cap (from `green_patch.py`'s reply or the profile)
#[derive(Clone, Debug)]
pub struct TuneMenu {
    pub panel: PanelPose,
    page: TunePage,
    values: TuneValues,
    cap: PatchCap,
    patch_mode: bool,
    pinches: [Pinch; 2],
    roles: [HandRole; 2],
    spots: [Option<PanelSpot>; 2],
    cursors: [Option<Vec2>; 2],
    drag: Option<ValueDrag>,
    patch: PatchGrab,
    // The cap when the hands started to move / size it
    patch_from: Option<PatchCap>,
    last_activity: Option<f64>,
}

impl TuneMenu {
    pub fn new(panel: PanelPose, values: TuneValues, cap: PatchCap) -> Self {
        Self {
            panel,
            page: TunePage::default(),
            values,
            cap: cap.clamped(),
            patch_mode: false,
            pinches: [Pinch::default(); 2],
            roles: [HandRole::Ignored; 2],
            spots: [None; 2],
            cursors: [None; 2],
            drag: None,
            patch: PatchGrab::None,
            patch_from: None,
            last_activity: None,
        }
    }

    pub fn page(&self) -> TunePage {
        self.page
    }

    pub fn values(&self) -> &TuneValues {
        &self.values
    }

    /// The property values as set with setprop (the client reads them again from time to time)
    pub fn set_prop(&mut self, property: &str, value: Option<i64>) {
        match value {
            Some(value) => self.values.props.insert(property.to_owned(), value),
            None => self.values.props.remove(property),
        };
    }

    pub fn cap(&self) -> PatchCap {
        self.cap
    }

    /// A cap from `green_patch.py`. Ignored while the hands move it or its cut is dragged
    pub fn set_cap(&mut self, cap: PatchCap) {
        let cut_dragged = self.drag.is_some_and(|drag| drag.spec.property.is_none());
        if self.patch == PatchGrab::None && !cut_dragged {
            self.cap = cap.clamped();
        }
    }

    pub fn patch_mode(&self) -> bool {
        self.patch_mode
    }

    /// The time of the last pinch or drag (None: none yet), for the idle exit
    pub fn last_activity(&self) -> Option<f64> {
        self.last_activity
    }

    /// Shows another page (also from the client: adb)
    pub fn set_page(&mut self, page: TunePage) -> Vec<TuneEvent> {
        let mut events = Vec::new();
        if page == self.page {
            return events;
        }
        self.end_drag(&mut events);
        self.page = page;
        events.push(TuneEvent::Page(page));
        if page != TunePage::GreenScreen && self.patch_mode {
            self.end_patch(&mut events);
            self.patch_mode = false;
            events.push(TuneEvent::PatchMode(false));
        }
        events
    }

    /// One frame. `input`: None, no head pose. `now`: seconds. `pinch_on` / `pinch_off`: the thresholds
    /// of the pinch gap as in the area editor (`DEFAULT_PINCH_ON` / `DEFAULT_PINCH_OFF`)
    pub fn update(
        &mut self,
        input: Option<&EditorInput>,
        now: f64,
        pinch_on: f32,
        pinch_off: f32,
    ) -> Vec<TuneEvent> {
        let mut events = Vec::new();
        let rows = self.page.rows().len();

        for index in 0..2 {
            let hand = input.and_then(|input| input.hands[index].map(|hand| (input, hand)));
            let usable = hand.filter(|(input, hand)| hand.usable(input.eye, input.forward));
            let ray =
                usable.and_then(|(input, hand)| hand.ray(input.eye, input.forward, index == 0));
            let uv = ray.and_then(|ray| self.panel.hit(&ray));
            self.spots[index] = uv.map(|uv| spot_at(uv, rows));
            self.cursors[index] = uv;
            let gap = usable.map(|(_, hand)| hand.gap());

            let lost_hold = if matches!(self.roles[index], HandRole::Drag | HandRole::Patch) {
                GRAB_LOST_HOLD_S
            } else {
                HAND_LOST_HOLD_S
            };
            let event = self.pinches[index].update(gap, now, pinch_on, pinch_off, lost_hold);
            match event {
                PinchEvent::Closed => {
                    self.last_activity = Some(now);
                    if self.roles[index] != HandRole::Ignored {
                        self.roles[index] = self.close(index, uv);
                    }
                }
                PinchEvent::Opened | PinchEvent::Lost => {
                    self.last_activity = Some(now);
                    let role = std::mem::replace(&mut self.roles[index], HandRole::Open);
                    self.open(index, role, event == PinchEvent::Opened, &mut events);
                }
                PinchEvent::None => {
                    if self.roles[index] == HandRole::Ignored
                        && !self.pinches[index].is_closed()
                        && gap.is_none_or(|gap| gap >= pinch_on)
                    {
                        self.roles[index] = HandRole::Open;
                    }
                }
            }

            // A value follows the ray along the plane of the panel, also beyond its edges
            if let Some(mut drag) = self.drag.filter(|drag| drag.hand == index)
                && let Some(uv) = ray.and_then(|ray| self.panel.plane_uv(&ray))
            {
                let value = dragged_value(&drag.spec.range, drag.start, drag.start_u, uv.x);
                if value != drag.last {
                    self.last_activity = Some(now);
                    self.apply_value(&drag.spec, value, &mut events);
                    drag.last = value;
                    self.drag = Some(drag);
                }
            }
        }

        self.update_patch(input, now, &mut events);
        events
    }

    // A pinch closed: what it is for
    fn close(&mut self, hand: usize, uv: Option<Vec2>) -> HandRole {
        let Some(uv) = uv else {
            return if self.page == TunePage::GreenScreen && self.patch_mode {
                HandRole::Patch
            } else {
                HandRole::Ignored
            };
        };
        let spot = spot_at(uv, self.page.rows().len());
        match spot {
            PanelSpot::Tab(_) => HandRole::Press(spot),
            PanelSpot::Row(row) => {
                let row_def = self.page.rows()[row];
                match self.values.value_spec(&row_def) {
                    Some(spec) if self.drag.is_none() => {
                        let start = self.current(&spec);
                        self.drag = Some(ValueDrag {
                            hand,
                            row,
                            spec,
                            start_u: uv.x,
                            start,
                            last: start,
                        });
                        if spec.property.is_none() {
                            self.patch_from.get_or_insert(self.cap);
                        }
                        HandRole::Drag
                    }
                    Some(_) => HandRole::Ignored,
                    None => HandRole::Press(spot),
                }
            }
            PanelSpot::Footer | PanelSpot::Empty => HandRole::Ignored,
        }
    }

    // A pinch opened (`released`: not lost)
    fn open(&mut self, hand: usize, role: HandRole, released: bool, events: &mut Vec<TuneEvent>) {
        match role {
            HandRole::Open | HandRole::Ignored | HandRole::Patch => (),
            HandRole::Drag => {
                if self.drag.is_some_and(|drag| drag.hand == hand) {
                    self.end_drag(events);
                }
            }
            HandRole::Press(spot) => {
                if released {
                    self.press(spot, events);
                }
            }
        }
    }

    fn end_drag(&mut self, events: &mut Vec<TuneEvent>) {
        let Some(drag) = self.drag.take() else {
            return;
        };
        // Ended by another page while the pinch is closed: nothing until it opens
        if self.roles[drag.hand] == HandRole::Drag {
            self.roles[drag.hand] = HandRole::Ignored;
        }
        match drag.spec.property {
            Some(property) => {
                if drag.last != drag.start {
                    events.push(TuneEvent::Committed {
                        property,
                        from: drag.start.round() as i64,
                        to: drag.last.round() as i64,
                    });
                }
            }
            None => {
                if self.patch == PatchGrab::None
                    && let Some(from) = self.patch_from.take()
                    && from != self.cap
                {
                    events.push(TuneEvent::CapCommitted { from, to: self.cap });
                }
            }
        }
    }

    // The value of a value row now
    fn current(&self, spec: &ValueSpec) -> f32 {
        match spec.property {
            Some(property) => self.values.value(property) as f32,
            None => self.cap.cut,
        }
    }

    fn apply_value(&mut self, spec: &ValueSpec, new: f32, events: &mut Vec<TuneEvent>) {
        match spec.property {
            Some(property) => {
                let old = self.values.value(property);
                let new = new.round() as i64;
                self.values.overrides.insert(property.to_owned(), new);
                if old != new {
                    events.push(TuneEvent::Set { property, old, new });
                }
            }
            None => {
                let before = self.cap;
                self.cap = PatchCap {
                    cut: new,
                    ..self.cap
                }
                .clamped();
                if before != self.cap {
                    events.push(TuneEvent::Cap {
                        old: before,
                        new: self.cap,
                    });
                }
            }
        }
    }

    fn set_property(&mut self, property: &'static str, new: i64, events: &mut Vec<TuneEvent>) {
        let old = self.values.value(property);
        self.values.overrides.insert(property.to_owned(), new);
        if old != new {
            events.push(TuneEvent::Set { property, old, new });
        }
    }

    fn reset(&mut self, properties: &[&'static str], events: &mut Vec<TuneEvent>) {
        for &property in properties {
            if let Some(old) = self.values.overrides.remove(property) {
                events.push(TuneEvent::Reset {
                    property,
                    old,
                    new: self.values.value(property),
                });
            }
        }
    }

    // A tab or a button was pinched and let go
    fn press(&mut self, spot: PanelSpot, events: &mut Vec<TuneEvent>) {
        match spot {
            PanelSpot::Tab(index) => {
                if let Some(&page) = TunePage::ALL.get(index) {
                    events.extend(self.set_page(page));
                }
            }
            PanelSpot::Row(row) => {
                let Some(row) = self.page.rows().get(row).copied() else {
                    return;
                };
                match row.kind {
                    RowKind::Toggle { property, .. } => {
                        let new = i64::from(self.values.value(property) <= 0);
                        self.set_property(property, new, events);
                    }
                    RowKind::Action(TuneAction::PatchMove) => {
                        if self.patch_mode {
                            self.end_patch(events);
                        }
                        self.patch_mode = !self.patch_mode;
                        events.push(TuneEvent::PatchMode(self.patch_mode));
                    }
                    RowKind::Action(TuneAction::ResetPage) => {
                        self.reset(&self.page.properties(), events);
                        events.push(TuneEvent::Action(TuneAction::ResetPage));
                    }
                    RowKind::Action(TuneAction::ResetAll) => {
                        let all = tuned_properties();
                        self.reset(&all, events);
                        events.push(TuneEvent::Action(TuneAction::ResetAll));
                    }
                    RowKind::Action(action) => events.push(TuneEvent::Action(action)),
                    RowKind::Value { .. } | RowKind::KeyStrictness | RowKind::CapCut => (),
                }
            }
            PanelSpot::Footer | PanelSpot::Empty => (),
        }
    }

    fn end_patch(&mut self, events: &mut Vec<TuneEvent>) {
        self.patch = PatchGrab::None;
        for role in &mut self.roles {
            if *role == HandRole::Patch {
                *role = HandRole::Ignored;
            }
        }
        if let Some(from) = self.patch_from.take()
            && from != self.cap
        {
            events.push(TuneEvent::CapCommitted { from, to: self.cap });
        }
    }

    // The hands that move / size the cap
    fn update_patch(&mut self, input: Option<&EditorInput>, now: f64, events: &mut Vec<TuneEvent>) {
        let holding = [0, 1]
            .map(|hand| self.roles[hand] == HandRole::Patch && self.pinches[hand].is_closed());
        let cursor = |hand: usize| {
            input
                .and_then(|input| input.hands[hand])
                .map(|h| h.cursor())
        };

        let grab = match holding {
            [true, true] => {
                let (Some(a), Some(b)) = (cursor(0), cursor(1)) else {
                    return; // Hold the cap while a hand is not seen
                };
                let span = a.distance(b);
                match self.patch {
                    PatchGrab::Resize { span_start, start } => {
                        Some((patch_resized(start, span_start, span), self.patch))
                    }
                    _ => Some((
                        self.cap,
                        PatchGrab::Resize {
                            span_start: span,
                            start: self.cap,
                        },
                    )),
                }
            }
            [true, false] | [false, true] => {
                let hand = usize::from(holding[1]);
                let Some(position) = cursor(hand) else {
                    return;
                };
                match self.patch {
                    PatchGrab::Move {
                        hand: moving,
                        start_cursor,
                        start,
                    } if moving == hand => Some((
                        patch_moved(start, position - start_cursor, self.panel.right),
                        self.patch,
                    )),
                    _ => Some((
                        self.cap,
                        PatchGrab::Move {
                            hand,
                            start_cursor: position,
                            start: self.cap,
                        },
                    )),
                }
            }
            [false, false] => None,
        };

        match grab {
            Some((cap, patch)) => {
                self.patch_from.get_or_insert(self.cap);
                self.patch = patch;
                if cap != self.cap {
                    self.last_activity = Some(now);
                    events.push(TuneEvent::Cap {
                        old: self.cap,
                        new: cap,
                    });
                    self.cap = cap;
                }
            }
            None => {
                if self.patch != PatchGrab::None {
                    self.patch = PatchGrab::None;
                    let cut_dragged = self.drag.is_some_and(|drag| drag.spec.property.is_none());
                    if !cut_dragged
                        && let Some(from) = self.patch_from.take()
                        && from != self.cap
                    {
                        events.push(TuneEvent::CapCommitted { from, to: self.cap });
                    }
                }
            }
        }
    }

    /// What the panel shows now
    pub fn content(&self, stats: &TuneStats) -> MenuContent {
        let hovered = self
            .drag
            .map(|drag| PanelSpot::Row(drag.row))
            .or(self.spots[1])
            .or(self.spots[0]);
        let pressed = self.roles.iter().find_map(|role| match role {
            HandRole::Press(spot) => Some(*spot),
            _ => None,
        });

        let tabs = TunePage::ALL
            .iter()
            .enumerate()
            .map(|(index, page)| MenuTab {
                label: page.title().into(),
                active: *page == self.page,
                hovered: hovered == Some(PanelSpot::Tab(index)),
            })
            .collect();

        let rows = self
            .page
            .rows()
            .iter()
            .enumerate()
            .map(|(index, row)| {
                let spot = PanelSpot::Row(index);
                let dragged = self.drag.is_some_and(|drag| drag.row == index);
                let (label, value, bar, overridden, on) = match row.kind {
                    RowKind::Toggle { property, .. } => {
                        let on = self.values.value(property) > 0;
                        (
                            row.label,
                            if on { "on" } else { "off" }.to_string(),
                            None,
                            self.values.is_overridden(property),
                            false,
                        )
                    }
                    RowKind::Action(TuneAction::PatchMove) => (
                        row.label,
                        if self.patch_mode { "on" } else { "off" }.to_string(),
                        None,
                        false,
                        self.patch_mode,
                    ),
                    RowKind::Action(TuneAction::EditBoxes) => {
                        (row.label, ">".to_string(), None, false, false)
                    }
                    RowKind::Action(_) => (row.label, String::new(), None, false, false),
                    RowKind::Value { .. } | RowKind::KeyStrictness | RowKind::CapCut => {
                        // A drag keeps the row it started with (`key_auto` does not change meanwhile)
                        let spec = self
                            .drag
                            .filter(|drag| drag.row == index)
                            .map(|drag| drag.spec)
                            .or_else(|| self.values.value_spec(row))
                            .unwrap_or(CAP_CUT);
                        let value = self.current(&spec);
                        (
                            spec.label,
                            value_text(value, &spec.range),
                            Some(spec.range.fraction(value)),
                            spec.property
                                .is_some_and(|property| self.values.is_overridden(property)),
                            false,
                        )
                    }
                };
                MenuRow {
                    label: label.into(),
                    value,
                    bar,
                    overridden,
                    hovered: hovered == Some(spot),
                    selected: dragged || on || pressed == Some(spot),
                }
            })
            .collect();

        MenuContent {
            tabs,
            rows,
            footer: stats.text(),
            cursors: self.cursors.iter().flatten().copied().collect(),
        }
    }
}

/// The pinch thresholds of the area editor, for callers without their own
pub const DEFAULT_PINCH: (f32, f32) = (DEFAULT_PINCH_ON, DEFAULT_PINCH_OFF);

#[cfg(test)]
mod tests {
    use super::*;

    const EYE: Vec3 = Vec3::new(0.0, 1.2, 0.0);

    fn hand(palm: Vec3, palm_normal: Vec3, fingers: Vec3) -> EditorHand {
        EditorHand {
            thumb_tip: palm + Vec3::new(0.0, 0.05, 0.02),
            index_tip: palm + fingers.normalize() * 0.09,
            tip_radii: 0.016,
            palm,
            palm_normal,
            tracked: true,
            finger_direction: fingers.normalize(),
            aim: None,
        }
    }

    fn praying() -> EditorInput {
        let up = Vec3::new(0.0, 0.9, -0.3);
        EditorInput {
            eye: EYE,
            forward: Vec3::NEG_Z,
            hands: [
                Some(hand(Vec3::new(-0.012, 1.1, -0.35), Vec3::X, up)),
                Some(hand(Vec3::new(0.012, 1.1, -0.35), Vec3::NEG_X, up)),
            ],
        }
    }

    fn with_hands(input: &EditorInput, f: impl Fn(EditorHand) -> EditorHand) -> EditorInput {
        EditorInput {
            hands: input.hands.map(|hand| hand.map(&f)),
            ..*input
        }
    }

    #[test]
    fn praying_pose_conditions() {
        let pose = praying();
        assert!(praying_pose(&pose));

        // Palms apart
        let mut apart = pose;
        for (hand, side) in apart.hands.iter_mut().zip([-1.0, 1.0]) {
            hand.as_mut().unwrap().palm.x = side * 0.05;
        }
        assert!(!praying_pose(&apart));

        // Both palms facing the same way
        let mut same = pose;
        same.hands[1].as_mut().unwrap().palm_normal = Vec3::X;
        assert!(!praying_pose(&same));

        // Fingers down, or level ahead
        let down = with_hands(&pose, |hand| EditorHand {
            finger_direction: Vec3::new(0.0, -0.9, -0.3).normalize(),
            ..hand
        });
        assert!(!praying_pose(&down));
        let level = with_hands(&pose, |hand| EditorHand {
            finger_direction: Vec3::new(0.0, 0.6, -0.8),
            ..hand
        });
        // 37 degrees up: not enough
        assert!(!praying_pose(&level));

        // Too near, too far
        let near = with_hands(&pose, |hand| EditorHand {
            palm: hand.palm + Vec3::new(0.0, 0.05, 0.25),
            ..hand
        });
        assert!(!praying_pose(&near));
        let far = with_hands(&pose, |hand| EditorHand {
            palm: hand.palm + Vec3::new(0.0, 0.0, -0.4),
            ..hand
        });
        assert!(!praying_pose(&far));

        // One hand missing or not tracked
        let mut one = pose;
        one.hands[0] = None;
        assert!(!praying_pose(&one));
        let untracked = with_hands(&pose, |hand| EditorHand {
            tracked: false,
            ..hand
        });
        assert!(!praying_pose(&untracked));

        // Without the finger direction: from the palm to the index tip
        let estimated = with_hands(&pose, |hand| EditorHand {
            finger_direction: Vec3::ZERO,
            ..hand
        });
        assert!(praying_pose(&estimated));
    }

    #[test]
    fn praying_gesture_fires_after_a_second_and_rearms() {
        let pose = praying();
        let mut apart = pose;
        apart.hands[1].as_mut().unwrap().palm.x = 0.2;
        let mut gesture = PrayingGesture::default();
        let dt = 1.0 / 72.0;
        let mut t = 0.0;
        let mut fired_at = None;
        while t < 2.5 {
            let (_, fired) = gesture.update(Some(&pose), t);
            if fired {
                assert!(fired_at.is_none(), "fired twice in one hold");
                fired_at = Some(t);
            }
            t += dt;
        }
        let fired_at = fired_at.unwrap();
        // (the progress is an f32: within 1e-6 of a second is a second)
        assert!(
            (1.0 - 1e-6..1.0 + 2.0 * dt).contains(&fired_at),
            "{fired_at}"
        );

        // Not with the palms apart
        let mut other = PrayingGesture::default();
        for step in 0..200 {
            assert!(!other.update(Some(&apart), f64::from(step) * dt).1);
        }

        // Let go, then held again: fires again
        for _ in 0..30 {
            assert!(!gesture.update(Some(&apart), t).1);
            t += dt;
        }
        let mut again = false;
        for _ in 0..80 {
            again |= gesture.update(Some(&pose), t).1;
            t += dt;
        }
        assert!(again);

        // No input is not the pose
        assert_eq!(gesture.update(None, t + 5.0), (0.0, false));
    }

    #[test]
    fn panel_placement_and_ray_hit() {
        // Looking ahead and down: the panel is level ahead along the yaw, above the eyes
        let forward = Vec3::new(0.0, -0.5, -1.0).normalize();
        let panel = PanelPose::from_head(EYE, forward);
        assert!(panel.center.distance(Vec3::new(0.0, 1.35, -0.55)) < 1e-5);
        assert!(panel.right.distance(Vec3::X) < 1e-5);
        // Facing the eyes, its top tilted towards them
        assert!((panel.normal - (EYE - panel.center).normalize()).length() < 1e-5);
        assert!(panel.up.y > 0.9 && panel.up.z > 0.0);
        let rotated = panel.orientation() * Vec3::Z;
        assert!(rotated.distance(panel.normal) < 1e-4);

        // A ray from the eyes to a point of the panel hits it there
        for uv in [
            Vec2::new(0.5, 0.5),
            Vec2::new(0.1, 0.2),
            Vec2::new(0.9, 0.95),
        ] {
            let ray = HandRay::new(EYE, panel.point(uv) - EYE).unwrap();
            let hit = panel.hit(&ray).unwrap();
            assert!(hit.distance(uv) < 1e-4, "{hit} {uv}");
        }
        // Beyond its edge: only on the plane
        let beyond = HandRay::new(EYE, panel.point(Vec2::new(1.3, 0.5)) - EYE).unwrap();
        assert_eq!(panel.hit(&beyond), None);
        assert!(
            panel
                .plane_uv(&beyond)
                .unwrap()
                .distance(Vec2::new(1.3, 0.5))
                < 1e-4
        );
        // Away from it, from behind
        assert_eq!(panel.hit(&HandRay::new(EYE, Vec3::Z).unwrap()), None);
        let behind = panel.center - panel.normal * 0.3;
        assert_eq!(
            panel.hit(&HandRay::new(behind, panel.center - behind).unwrap()),
            None
        );

        // The spots
        assert_eq!(spot_at(Vec2::new(0.1, 0.05), 7), PanelSpot::Tab(0));
        assert_eq!(spot_at(Vec2::new(0.99, 0.05), 7), PanelSpot::Tab(3));
        assert_eq!(
            spot_at(Vec2::new(0.5, MENU_ROWS_TOP + 0.001), 7),
            PanelSpot::Row(0)
        );
        assert_eq!(
            spot_at(Vec2::new(0.5, MENU_ROWS_TOP + 2.5 * menu_row_height()), 7),
            PanelSpot::Row(2)
        );
        // Below the last row of a short page
        assert_eq!(
            spot_at(Vec2::new(0.5, MENU_ROWS_TOP + 6.5 * menu_row_height()), 6),
            PanelSpot::Empty
        );
        assert_eq!(spot_at(Vec2::new(0.5, 0.95), 7), PanelSpot::Footer);
    }

    #[test]
    fn drag_maps_snaps_and_clamps() {
        let range = KEY_BIAS.range;
        // Within the dead zone: unchanged
        assert_eq!(dragged_value(&range, 150.0, 0.5, 0.505), 150.0);
        // A tenth of the width beyond the dead zone: a tenth of 0..1000
        assert_eq!(dragged_value(&range, 150.0, 0.5, 0.61), 250.0);
        assert_eq!(dragged_value(&range, 150.0, 0.5, 0.39), 50.0);
        // Snapped to the step of 10
        assert_eq!(dragged_value(&range, 150.0, 0.5, 0.5434), 180.0);
        // Clamped
        assert_eq!(dragged_value(&range, 150.0, 0.5, 2.0), 1000.0);
        assert_eq!(dragged_value(&range, 150.0, 0.5, -1.0), 0.0);
        // Steps from the min, float steps
        let edge = property_range("edge_cut").unwrap();
        assert_eq!(edge.snap(16.0), 17.0);
        assert_eq!(edge.snap(50.4), 50.0);
        assert_eq!(CAP_CUT.range.snap(0.137), 0.15);
        assert_eq!(CAP_CUT.range.snap(-3.0), -1.0);
        assert_eq!(CAP_CUT.range.snap(f32::NAN), 0.0);
    }

    #[test]
    fn rows_have_the_client_defaults() {
        for (property, default) in [
            ("key_auto", 0),
            ("key_auto_weight", 300),
            ("key_bias", 150),
            ("key_unknown", 10),
            ("key_rim_max", 8),
            ("cloud_trail", 0),
            ("cloud_cap", 14),
            ("cloud_gain", 150),
            ("cloud_own", 0),
            ("edge_cut", 20),
            ("mem_margin_mm", 60),
            ("green_ratio", 135),
            ("green_floor", 70),
            ("green_soft", 24),
        ] {
            assert_eq!(property_default(property), Some(default), "{property}");
            let range = property_range(property).unwrap();
            assert!(range.min <= range.default && range.default <= range.max);
        }
        assert_eq!(property_default("key_tight"), None);
        assert_eq!(tuned_properties().len(), 14);
        for page in TunePage::ALL {
            assert!(page.rows().len() <= MENU_MAX_ROWS);
            assert_eq!(
                page.rows()[page.rows().len() - 1].kind,
                RowKind::Action(TuneAction::Done)
            );
        }
    }

    // A hand that points from the eyes at (u, v) of the panel, pinched or not
    fn pointing(panel: &PanelPose, uv: Vec2, pinched: bool) -> EditorHand {
        let target = panel.point(uv);
        let cursor = EYE + (target - EYE).normalize() * 0.35;
        let gap = if pinched { -0.01 } else { 0.03 };
        EditorHand {
            thumb_tip: cursor - Vec3::X * (0.008 + 0.5 * gap),
            index_tip: cursor + Vec3::X * (0.008 + 0.5 * gap),
            tip_radii: 0.016,
            palm: cursor + Vec3::new(0.0, -0.03, 0.04),
            palm_normal: Vec3::NEG_Z,
            tracked: true,
            finger_direction: Vec3::ZERO,
            aim: HandRay::new(EYE, target - EYE),
        }
    }

    struct Driver {
        menu: TuneMenu,
        t: f64,
    }

    impl Driver {
        fn new(values: TuneValues) -> Self {
            let panel = PanelPose::from_head(EYE, Vec3::NEG_Z);
            Self {
                menu: TuneMenu::new(panel, values, PatchCap::default()),
                t: 0.0,
            }
        }

        fn frame(&mut self, hands: [Option<EditorHand>; 2]) -> Vec<TuneEvent> {
            self.t += 1.0 / 72.0;
            let input = EditorInput {
                eye: EYE,
                forward: Vec3::NEG_Z,
                hands,
            };
            self.menu
                .update(Some(&input), self.t, DEFAULT_PINCH.0, DEFAULT_PINCH.1)
        }

        // The right hand at `uv`, held for some frames
        fn right(&mut self, uv: Vec2, pinched: bool, frames: usize) -> Vec<TuneEvent> {
            let hand = pointing(&self.menu.panel, uv, pinched);
            (0..frames)
                .flat_map(|_| self.frame([None, Some(hand)]))
                .collect()
        }

        fn tap(&mut self, uv: Vec2) -> Vec<TuneEvent> {
            let mut events = self.right(uv, false, 2);
            events.extend(self.right(uv, true, 4));
            events.extend(self.right(uv, false, 12));
            events
        }

        fn drag(&mut self, from: Vec2, to: Vec2) -> Vec<TuneEvent> {
            let mut events = self.right(from, false, 2);
            events.extend(self.right(from, true, 4));
            for step in 1..=10 {
                events.extend(self.right(from.lerp(to, step as f32 / 10.0), true, 1));
            }
            events.extend(self.right(to, false, 12));
            events
        }
    }

    fn row_uv(row: usize, u: f32) -> Vec2 {
        Vec2::new(u, MENU_ROWS_TOP + (row as f32 + 0.5) * menu_row_height())
    }

    fn tab_uv(page: TunePage) -> Vec2 {
        Vec2::new((page.index() as f32 + 0.5) / 4.0, MENU_TAB_HEIGHT / 2.0)
    }

    #[test]
    fn key_auto_switches_the_strictness_row() {
        let mut driver = Driver::new(TuneValues::default());
        let stats = TuneStats::default();
        let content = driver.menu.content(&stats);
        assert_eq!(content.rows[1].label, "Skin preference");
        assert_eq!(content.rows[1].value, "150 %");
        assert!(!content.rows[1].overridden);

        // Dragging it changes key_bias: 0.2 of the width (after the dead zone) is 200
        let events = driver.drag(row_uv(1, 0.4), row_uv(1, 0.61));
        assert!(events.contains(&TuneEvent::Committed {
            property: "key_bias",
            from: 150,
            to: 350,
        }));
        assert!(events.iter().all(
            |event| !matches!(event, TuneEvent::Set { property, .. } if *property != "key_bias")
        ));
        assert_eq!(driver.menu.values().value("key_bias"), 350);
        assert!(driver.menu.content(&stats).rows[1].overridden);

        // Auto key on: the row is the room strictness
        let events = driver.tap(row_uv(0, 0.5));
        assert_eq!(
            events,
            vec![TuneEvent::Set {
                property: "key_auto",
                old: 0,
                new: 1,
            }]
        );
        let content = driver.menu.content(&stats);
        assert_eq!(content.rows[0].value, "on");
        assert_eq!(content.rows[1].label, "Room strictness");
        assert_eq!(content.rows[1].value, "300 %");
        assert!(!content.rows[1].overridden);
        let events = driver.drag(row_uv(1, 0.5), row_uv(1, 0.4));
        assert!(events.contains(&TuneEvent::Committed {
            property: "key_auto_weight",
            from: 300,
            to: 210,
        }));
        assert_eq!(driver.menu.values().value("key_bias"), 350);

        // The property set with setprop is the start when there is no override
        driver.menu.set_prop("key_auto", Some(1));
        driver.menu.set_prop("key_unknown", Some(40));
        assert_eq!(driver.menu.content(&stats).rows[2].value, "40 %");

        // Reset page: all overrides of the page go, the properties are in effect again
        let events = driver.tap(row_uv(4, 0.5));
        assert!(events.contains(&TuneEvent::Reset {
            property: "key_bias",
            old: 350,
            new: 150,
        }));
        assert!(events.contains(&TuneEvent::Reset {
            property: "key_auto",
            old: 1,
            new: 1,
        }));
        assert_eq!(
            events.last(),
            Some(&TuneEvent::Action(TuneAction::ResetPage))
        );
        assert!(driver.menu.values().overrides.is_empty());
    }

    #[test]
    fn tabs_buttons_and_pinches_from_before() {
        // A pinch closed when the menu opened does nothing until it opens
        let mut driver = Driver::new(TuneValues::default());
        let events = driver.right(tab_uv(TunePage::GreenScreen), true, 10);
        assert!(events.is_empty());
        let events = driver.right(tab_uv(TunePage::GreenScreen), false, 12);
        assert!(events.is_empty());
        assert_eq!(driver.menu.page(), TunePage::HandsVsRoom);

        // Now a tap on the tab
        let events = driver.tap(tab_uv(TunePage::DepthEdge));
        assert_eq!(events, vec![TuneEvent::Page(TunePage::DepthEdge)]);
        assert_eq!(driver.menu.page(), TunePage::DepthEdge);
        let content = driver.menu.content(&TuneStats::default());
        assert!(content.tabs[2].active);
        assert_eq!(content.rows.len(), 6);
        assert_eq!(content.rows[0].value, "20 %");

        // Hovered row and cursor
        driver.right(row_uv(1, 0.3), false, 1);
        let content = driver.menu.content(&TuneStats::default());
        assert!(content.rows[1].hovered && !content.rows[0].hovered);
        assert_eq!(content.cursors.len(), 1);
        assert!(content.cursors[0].distance(row_uv(1, 0.3)) < 1e-3);

        // Edit boxes, Done
        let events = driver.tap(row_uv(2, 0.5));
        assert_eq!(events, vec![TuneEvent::Action(TuneAction::EditBoxes)]);
        let events = driver.tap(row_uv(5, 0.5));
        assert_eq!(events, vec![TuneEvent::Action(TuneAction::Done)]);
        assert!(driver.menu.last_activity().is_some());

        // Reset all from another page removes the overrides of every page
        driver.drag(row_uv(0, 0.5), row_uv(0, 0.7));
        assert!(driver.menu.values().is_overridden("edge_cut"));
        driver.tap(tab_uv(TunePage::ArmsInMotion));
        let events = driver.tap(row_uv(5, 0.5));
        assert!(events.iter().any(|event| matches!(
            event,
            TuneEvent::Reset {
                property: "edge_cut",
                ..
            }
        )));
        assert!(driver.menu.values().overrides.is_empty());
    }

    #[test]
    fn patch_drag_and_clamps() {
        // One hand: 1 cm right / up is 1 % right / up (cy grows downwards)
        let start = PatchCap::default();
        let moved = patch_moved(start, Vec3::new(0.05, 0.02, 0.3), Vec3::X);
        assert!((moved.cx - 0.55).abs() < 1e-5);
        assert!((moved.cy - 0.58).abs() < 1e-5);
        assert_eq!((moved.r, moved.cut), (start.r, start.cut));
        // Facing another way: right is along the panel
        let turned = patch_moved(start, Vec3::new(0.0, 0.0, 0.1), Vec3::Z);
        assert!((turned.cx - 0.6).abs() < 1e-5);
        // Clamped to the monitor
        let far = patch_moved(start, Vec3::new(2.0, -3.0, 0.0), Vec3::X);
        assert_eq!((far.cx, far.cy), (1.0, 1.0));

        // Two hands apart: a larger radius, clamped
        let sized = patch_resized(start, 0.2, 0.25);
        assert!((sized.r - 0.25).abs() < 1e-5);
        assert_eq!(patch_resized(start, 0.2, 0.0).r, PATCH_MIN_RADIUS);
        assert_eq!(patch_resized(start, 0.0, 5.0).r, PATCH_MAX_RADIUS);
        let wild = PatchCap {
            cx: -1.0,
            cy: f32::NAN,
            r: 0.0,
            cut: 7.0,
        }
        .clamped();
        assert_eq!(
            wild,
            PatchCap {
                cx: 0.0,
                cy: 0.6,
                r: PATCH_MIN_RADIUS,
                cut: 1.0
            }
        );
    }

    #[test]
    fn patch_mode_moves_and_sizes_the_cap() {
        let mut driver = Driver::new(TuneValues::default());
        driver.tap(tab_uv(TunePage::GreenScreen));
        // A pinch off the panel does nothing without the patch mode
        let below = Vec2::new(0.5, 2.0);
        let events = driver.drag(below, below + Vec2::new(0.3, 0.0));
        assert!(events.is_empty());

        let events = driver.tap(row_uv(4, 0.5));
        assert_eq!(events, vec![TuneEvent::PatchMode(true)]);
        assert!(driver.menu.content(&TuneStats::default()).rows[4].selected);

        // One hand off the panel moves the cap with the cursor of the hand
        let a = pointing(&driver.menu.panel, below, false);
        let shifted = |hand: EditorHand, by: Vec3, pinched: bool| {
            let gap = if pinched { -0.01 } else { 0.03 };
            let cursor = hand.cursor() + by;
            EditorHand {
                thumb_tip: cursor - Vec3::X * (0.008 + 0.5 * gap),
                index_tip: cursor + Vec3::X * (0.008 + 0.5 * gap),
                palm: hand.palm + by,
                aim: hand
                    .aim
                    .map(|aim| HandRay::new(aim.origin + by, aim.direction).unwrap()),
                ..hand
            }
        };
        let before = driver.menu.cap();
        for _ in 0..4 {
            driver.frame([None, Some(shifted(a, Vec3::ZERO, true))]);
        }
        let mut events = Vec::new();
        for step in 1..=10 {
            let by = Vec3::new(0.01, 0.005, 0.0) * step as f32;
            events.extend(driver.frame([None, Some(shifted(a, by, true))]));
        }
        let moved = driver.menu.cap();
        assert!((moved.cx - (before.cx + 0.1)).abs() < 1e-4, "{moved:?}");
        assert!((moved.cy - (before.cy - 0.05)).abs() < 1e-4, "{moved:?}");
        assert!(
            events
                .iter()
                .all(|event| matches!(event, TuneEvent::Cap { .. }))
        );
        let mut events = Vec::new();
        for _ in 0..12 {
            events.extend(driver.frame([None, Some(shifted(a, Vec3::new(0.1, 0.05, 0.0), false))]));
        }
        assert_eq!(
            events,
            vec![TuneEvent::CapCommitted {
                from: before,
                to: moved,
            }]
        );

        // Two hands: their spread sizes it
        let left = shifted(a, Vec3::new(-0.05, 0.0, 0.0), false);
        let right = shifted(a, Vec3::new(0.05, 0.0, 0.0), false);
        driver.frame([Some(left), Some(right)]);
        for _ in 0..4 {
            driver.frame([
                Some(shifted(left, Vec3::ZERO, true)),
                Some(shifted(right, Vec3::ZERO, true)),
            ]);
        }
        let r = driver.menu.cap().r;
        driver.frame([
            Some(shifted(left, Vec3::new(-0.02, 0.0, 0.0), true)),
            Some(shifted(right, Vec3::new(0.02, 0.0, 0.0), true)),
        ]);
        assert!((driver.menu.cap().r - (r + 0.04)).abs() < 1e-4);
        assert!((driver.menu.cap().cx - moved.cx).abs() < 1e-6);

        // Both let go where they are; the cut is a row
        let mut events = Vec::new();
        for _ in 0..12 {
            events.extend(driver.frame([
                Some(shifted(left, Vec3::new(-0.02, 0.0, 0.0), false)),
                Some(shifted(right, Vec3::new(0.02, 0.0, 0.0), false)),
            ]));
        }
        assert!(
            matches!(events[..], [TuneEvent::CapCommitted { .. }]),
            "{events:?}"
        );
        let events = driver.drag(row_uv(3, 0.5), row_uv(3, 0.56));
        assert!(
            events
                .iter()
                .any(|event| matches!(event, TuneEvent::CapCommitted { to, .. } if to.cut == 0.1))
        );
        assert_eq!(
            driver.menu.content(&TuneStats::default()).rows[3].value,
            "+0.10"
        );

        // Leaving the page ends the patch mode
        let events = driver.tap(tab_uv(TunePage::HandsVsRoom));
        assert_eq!(
            events,
            vec![
                TuneEvent::Page(TunePage::HandsVsRoom),
                TuneEvent::PatchMode(false)
            ]
        );
    }

    #[test]
    fn messages_encode_and_parse() {
        let cap = PatchCap {
            cx: 0.5,
            cy: 0.62,
            r: 0.18,
            cut: -0.25,
        };
        assert_eq!(
            patch_state_message(7, &cap, true),
            r#"{"seq":7,"shape":"cap","cap":[0.5000,0.6200,0.1800,-0.2500],"visible":true}"#
        );
        assert_eq!(PATCH_GET_MESSAGE, r#"{"get":1}"#);
        assert_eq!(qr_message("SCR1", true), r#"{"qr":"SCR1","show":1}"#);
        assert_eq!(qr_message("a\"b\\c", false), r#"{"qr":"a\"b\\c","show":0}"#);

        // The message reads back as a reply
        let reply = parse_patch_reply(&patch_state_message(7, &cap, true)).unwrap();
        assert_eq!(reply.seq, Some(7));
        assert_eq!(reply.cap, Some(cap));
        assert_eq!(reply.shape.as_deref(), Some("cap"));

        let reply = parse_patch_reply(
            r#"{
  "monitor": 1,
  "rect": [0.3, 0.55, 0.4, 0.45],
  "color": [0, 200, 0],
  "visible": false,
  "shape": "rect",
  "cap": [0.5, 0.6, 0.2, 0.0],
  "seq": 12,
  "qr": {"monitor": 0, "px": 12},
  "note": "café \"x\""
}"#,
        )
        .unwrap();
        assert_eq!(
            reply,
            PatchReply {
                monitor: Some(1),
                rect: Some([0.3, 0.55, 0.4, 0.45]),
                color: Some([0, 200, 0]),
                visible: Some(false),
                shape: Some("rect".into()),
                cap: Some(PatchCap {
                    cx: 0.5,
                    cy: 0.6,
                    r: 0.2,
                    cut: 0.0
                }),
                seq: Some(12),
            }
        );
        // Missing keys, wrong types
        let reply = parse_patch_reply(r#"{"rect":[1,2],"cap":"x","seq":-1}"#).unwrap();
        assert_eq!(reply, PatchReply::default());
        assert!(parse_patch_reply("[1,2]").is_err());
        assert!(parse_patch_reply(r#"{"a":1"#).is_err());
        assert!(parse_patch_reply("").is_err());
        assert!(parse_patch_reply(r#"{"a":1} x"#).is_err());

        // Sent at most every 0.1 s, the last one when forced, never twice the same
        let mut link = PatchLink::default();
        assert!(link.message(&cap, 0.0, false).is_some());
        assert!(link.message(&cap, 1.0, false).is_none());
        let next = PatchCap { cx: 0.6, ..cap };
        assert!(link.message(&next, 1.05, false).is_some());
        let last = PatchCap { cx: 0.7, ..cap };
        assert!(link.message(&last, 1.08, false).is_none());
        let forced = link.message(&last, 1.09, true).unwrap();
        assert!(forced.starts_with(r#"{"seq":3,"#));
        assert_eq!(link.seq(), 3);
    }

    #[test]
    fn profile_round_trip() {
        let mut profile = TuneProfile::default();
        profile.overrides.insert("key_auto".into(), 1);
        profile.overrides.insert("key_auto_weight".into(), 420);
        profile.overrides.insert("green_floor".into(), 30);
        profile.cap = Some(PatchCap {
            cx: 0.45,
            cy: 0.7,
            r: 0.25,
            cut: 0.1,
        });
        let text = profile_text(&profile, Some("SCR1"));
        assert!(text.starts_with('#'));
        assert!(text.contains("key_auto_weight=420\n"));
        assert_eq!(parse_profile(&text), profile);

        // Unknown keys, comments, bad values, the prefix, out of range values
        let parsed = parse_profile(
            "# x\n\nkey_tight=40\nfoo\ngreen_soft=abc\ndebug.alvr.edge_cut=99\n cloud_gain = 155 \n\
             patch_cap=1,2\n",
        );
        assert_eq!(parsed.overrides.len(), 2);
        assert_eq!(parsed.overrides["edge_cut"], 84);
        assert_eq!(parsed.overrides["cloud_gain"], 160);
        assert_eq!(parsed.cap, None);
        assert_eq!(parse_profile(""), TuneProfile::default());

        // File names
        assert_eq!(profile_file_name(Some("SCR1")), "tune-SCR1.txt");
        assert_eq!(profile_file_name(None), DEFAULT_PROFILE_FILE);
        assert_eq!(profile_file_name(Some("")), "tune-default.txt");
        let a = profile_file_name(Some("../a b"));
        assert!(a.starts_with("tune-___a_b-") && a.ends_with(".txt"), "{a}");
        assert_ne!(a, profile_file_name(Some("___a_b")));
        assert!(profile_file_name(Some(&"x".repeat(300))).len() < 60);
    }

    #[test]
    fn stats_footer() {
        assert_eq!(
            TuneStats::default().text(),
            "skin -   room -   scene -   bias -   green -"
        );
        let stats = TuneStats {
            skin_kept: Some(0.923),
            background_kept: Some(0.04),
            scene_kept: Some(0.3),
            auto_bias: Some(1.5),
            green_share: Some(0.12),
        };
        assert_eq!(
            stats.text(),
            "skin 92 %   room 4 %   scene 30 %   bias 1.50   green 12 %"
        );
        let content = Driver::new(TuneValues::default()).menu.content(&stats);
        assert_eq!(content.footer, stats.text());
        assert_eq!(content.tabs.len(), 4);
        assert_eq!(content.rows.len(), 7);
        let buffer = crate::render_menu(&content, MENU_TEXTURE_WIDTH, MENU_TEXTURE_HEIGHT);
        assert_eq!(
            buffer.len(),
            (MENU_TEXTURE_WIDTH * MENU_TEXTURE_HEIGHT * 4) as usize
        );
    }
}
