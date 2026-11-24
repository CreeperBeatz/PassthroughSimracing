//! The play area: up to 8 boxes in the frame of the anchor (the QR code on the rig), where the depth
//! mask shows the real room. Pure geometry with unit tests. The depth mask (`area_signed_distance` in
//! depth_mask.wgsl is a line-by-line copy of `PlayArea::signed_distance`), the hands limited to the
//! area and the outline all use the shape defined here.
//!
//! Area frame: the level frame of the code that `marker_box` builds (origin at the center of the code,
//! +Y stage up, -Z along the heading, +X to the right of the heading), turned about +Y by the yaw of
//! the layout. Every box is level: its own frame is the area frame moved to its center and turned about
//! +Y by its yaw (`AreaBox::yaw`, 0: axis aligned). Its sides, domes, handles and drags are those of its
//! own frame; `Side::normal` is the normal there, `AreaBox::side_normal` the one in the area frame.
//!
//! Domes ("balloons"): each of the 6 sides of a box can bulge out by a height h (0: flat). The dome is
//! the part beyond the side of a capsule that cuts the side along the largest stadium that fits in it
//! (a circle on a square side, so the dome is then a spherical cap). The corners of the side stay
//! flat. With a and b half the longer and the shorter edge of the side, the rim is the stadium of half
//! width b around a segment of half length a - b through the middle of the side; the capsule has the
//! radius R = (b^2 + h^2) / (2 h) and its axis lies d = R - h below the side. h is at most b: then the
//! dome is a half ball (square side) or a half cylinder with round ends. Below `MIN_DOME` a side is
//! flat, which also keeps R finite.
//!
//! The signed distance is exact outside the shape (meters, the feather of the mask keeps its meaning)
//! and has the right sign inside; inside, its size can be too small near a dome, which the mask does
//! not see (it saturates there). Under a dome it is never deeper than the opposite side of the box, so
//! that a box thinner than the capsule has no jump at its middle.
//!
//! The layout is saved as text, see `PlayArea::to_text`.

use crate::{MarkerBoxParams, RegionOverride};
use alvr_common::glam::{Mat4, Quat, Vec2, Vec3};
use std::fmt::Write;

/// Most boxes in a layout (the uniform block of the mask holds this many)
pub const MAX_AREA_BOXES: usize = 8;
/// Smallest and largest half extent a drag leaves (meters). The properties allow 5 mm..2.5 m
pub const MIN_BOX_HALF: f32 = 0.025;
pub const MAX_BOX_HALF: f32 = 2.5;
/// Half extents read from a file are kept within this and `MAX_BOX_HALF`
const MIN_FILE_HALF: f32 = 0.005;
/// Largest distance of the center of a box from the code, per axis (meters)
pub const MAX_BOX_CENTER: f32 = 3.0;
/// A dome lower than this is flat (meters)
pub const MIN_DOME: f32 = 0.005;
/// A box added next to another reaches this far beyond the side it is added to, at most as far as
/// the other box is thick (meters)
pub const NEW_BOX_DEPTH: f32 = 0.3;
/// ... and reaches this far into the other box (at most its half thickness). Boxes that only touch
/// have a distance of 0 on the side they share (the minimum of the two), a coverage of 0.5: the mask
/// still shows the real room there (it is full from 0.35 on), but a hand limited to the area would be
/// thinned along that side
pub const ADDED_BOX_OVERLAP: f32 = 0.05;
/// First line of the text of a layout, followed by the version
pub const AREA_FILE_HEADER: &str = "alvr_play_area";
pub const AREA_FILE_VERSION: u32 = 1;
/// Key of the line after a box line that turns that box (degrees). A reader without it leaves the line
/// out as unknown and reads the box unturned
pub const BOX_YAW_KEY: &str = "box_yaw_deg";

/// An angle in -pi..pi (radians); 0 for one that is not finite
pub fn wrap_angle(angle: f32) -> f32 {
    if !angle.is_finite() {
        return 0.0;
    }
    let tau = std::f32::consts::TAU;
    (angle - tau * (angle / tau).round()).clamp(-std::f32::consts::PI, std::f32::consts::PI)
}

/// A vector turned about +Y by the angle of this sine and cosine (counter-clockwise seen from above,
/// as `Quat::from_rotation_y`). Same as in `area_box_distance` of depth_mask.wgsl
fn turn_y(v: Vec3, sin: f32, cos: f32) -> Vec3 {
    Vec3::new(cos * v.x + sin * v.z, v.y, cos * v.z - sin * v.x)
}

/// The two other axes of each axis, in order
pub(crate) const OTHER_AREA_AXES: [(usize, usize); 3] = [(1, 2), (0, 2), (0, 1)];

/// One of the 6 sides of a box
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Side {
    /// 0: X, 1: Y, 2: Z
    pub axis: usize,
    /// The side towards +axis
    pub positive: bool,
}

impl Side {
    /// In the order of `AreaBox::domes`: +X, -X, +Y, -Y, +Z, -Z
    pub const ALL: [Side; 6] = [
        Side::new(0, true),
        Side::new(0, false),
        Side::new(1, true),
        Side::new(1, false),
        Side::new(2, true),
        Side::new(2, false),
    ];

    pub const fn new(axis: usize, positive: bool) -> Self {
        Self { axis, positive }
    }

    /// Index in `Side::ALL` and `AreaBox::domes`
    pub fn index(self) -> usize {
        self.axis * 2 + usize::from(!self.positive)
    }

    pub fn sign(self) -> f32 {
        if self.positive { 1.0 } else { -1.0 }
    }

    /// Outward unit normal in the frame of its box (`AreaBox::side_normal`: in the area frame)
    pub fn normal(self) -> Vec3 {
        let mut normal = Vec3::ZERO;
        normal[self.axis] = self.sign();
        normal
    }

    pub fn opposite(self) -> Self {
        Self::new(self.axis, !self.positive)
    }

    /// "+x" .. "-z", as in the text of a layout
    pub fn name(self) -> &'static str {
        ["+x", "-x", "+y", "-y", "+z", "-z"][self.index()]
    }
}

/// A level box of the area, in its own frame (the area frame moved to its center, turned by its yaw)
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AreaBox {
    /// Area frame (meters)
    pub center: Vec3,
    /// Along the axes of its own frame (meters)
    pub half: Vec3,
    /// Height of the dome of each side, in the order of `Side::ALL` (meters, 0: flat). As set: the dome
    /// shown is at most the rim radius (`dome_height`), so that a box made smaller and larger again
    /// keeps its domes
    pub domes: [f32; 6],
    /// Turns the box about the vertical axis through its center (radians, counter-clockwise seen from
    /// above, -pi..pi; 0: axis aligned in the area frame)
    pub yaw: f32,
}

impl AreaBox {
    pub fn new(center: Vec3, half: Vec3) -> Self {
        Self {
            center,
            half,
            domes: [0.0; 6],
            yaw: 0.0,
        }
    }

    /// A vector of the frame of the box in the area frame
    pub fn turn(&self, v: Vec3) -> Vec3 {
        let (sin, cos) = self.yaw.sin_cos();
        turn_y(v, sin, cos)
    }

    /// A vector of the area frame in the frame of the box
    pub fn turn_back(&self, v: Vec3) -> Vec3 {
        let (sin, cos) = self.yaw.sin_cos();
        turn_y(v, -sin, cos)
    }

    /// A point of the area frame in the frame of the box (origin at its center)
    pub fn to_local(&self, point: Vec3) -> Vec3 {
        self.turn_back(point - self.center)
    }

    /// A point of the frame of the box in the area frame
    pub fn to_area(&self, local: Vec3) -> Vec3 {
        self.center + self.turn(local)
    }

    /// Outward unit normal of a side, area frame
    pub fn side_normal(&self, side: Side) -> Vec3 {
        self.turn(side.normal())
    }

    /// Radius of the rim of the dome of a side: half its shorter edge
    pub fn dome_rim(&self, side: Side) -> f32 {
        let (u, v) = OTHER_AREA_AXES[side.axis];
        self.half[u].min(self.half[v])
    }

    /// Height of the dome of a side as shown: at most the rim radius, 0 below `MIN_DOME`
    pub fn dome_height(&self, side: Side) -> f32 {
        let height = self.domes[side.index()].min(self.dome_rim(side));
        // Also for NaN
        if height >= MIN_DOME { height } else { 0.0 }
    }

    /// The capsule of the dome of a side: its radius and the depth of its axis below the side. (0, 0):
    /// flat
    pub fn dome_sphere(&self, side: Side) -> (f32, f32) {
        let height = self.dome_height(side);
        if height <= 0.0 {
            return (0.0, 0.0);
        }
        let rim = self.dome_rim(side);
        let radius = (rim * rim + height * height) / (2.0 * height);
        (radius, radius - height)
    }

    /// Height of the tallest dome (0: none)
    pub fn tallest_dome(&self) -> f32 {
        Side::ALL
            .iter()
            .map(|&side| self.dome_height(side))
            .fold(0.0, f32::max)
    }

    pub fn has_domes(&self) -> bool {
        self.tallest_dome() > 0.0
    }

    /// Signed distance from the surface of the box with its domes (meters, negative inside), area
    /// frame. `feathering`: the soft edge of the mask. Values beyond it on either side are only bounds,
    /// which give the same mask: below -feathering the distance can be too large, above +feathering
    /// too small. `f32::INFINITY`: exact. Same as `area_box_distance` in depth_mask.wgsl
    pub fn signed_distance(&self, point: Vec3, feathering: f32) -> f32 {
        let local = self.to_local(point);
        let e = local.abs() - self.half;
        let plain = e.max(Vec3::ZERO).length() + e.max_element().min(0.0);
        let tallest = self.tallest_dome();
        // Deep inside, a dome only makes it more negative. Far outside no dome is within the soft
        // edge, and every dome is within its height of the box: minus that is still a lower bound
        if tallest <= 0.0 || plain <= -feathering {
            return plain;
        }
        if plain >= tallest + feathering {
            return plain - tallest;
        }

        // Of each axis only the side the point is on: the other one is farther than the box
        let mut distance = plain;
        for axis in 0..3 {
            let side = Side::new(axis, local[axis] >= 0.0);
            let (radius, depth) = self.dome_sphere(side);
            let (u, v) = OTHER_AREA_AXES[axis];
            distance = distance.min(dome_distance(
                e[axis],
                Vec2::new(e[u], e[v]),
                self.dome_rim(side),
                radius,
                depth,
                2.0 * self.half[axis],
            ));
        }
        distance
    }

    /// Whether a point (area frame) is in the box or one of its domes. Exact, for the tests and the
    /// outline
    pub fn contains(&self, point: Vec3) -> bool {
        let local = self.to_local(point);
        let e = local.abs() - self.half;
        if e.max_element() <= 0.0 {
            return true;
        }
        (0..3).any(|axis| {
            let side = Side::new(axis, local[axis] >= 0.0);
            let (radius, depth) = self.dome_sphere(side);
            let (u, v) = OTHER_AREA_AXES[axis];
            let rim = self.dome_rim(side);
            let rho = (Vec2::new(e[u], e[v]) + rim).max(Vec2::ZERO).length();
            radius > 0.0 && e[axis] >= 0.0 && Vec2::new(rho, e[axis] + depth).length() <= radius
        })
    }

    /// Smallest and largest corner of the box with its domes in its own frame (around its center)
    pub fn local_bounds(&self) -> (Vec3, Vec3) {
        let mut min = -self.half;
        let mut max = self.half;
        for side in Side::ALL {
            let height = self.dome_height(side);
            if side.positive {
                max[side.axis] += height;
            } else {
                min[side.axis] -= height;
            }
        }
        (min, max)
    }

    /// Smallest and largest corner of the box with its domes, area frame. Of a turned box those of its
    /// local bounds turned, which hold it
    pub fn bounds(&self) -> (Vec3, Vec3) {
        let (min, max) = self.local_bounds();
        if self.yaw == 0.0 {
            return (self.center + min, self.center + max);
        }
        let corners = [
            (min.x, min.z),
            (max.x, min.z),
            (min.x, max.z),
            (max.x, max.z),
        ]
        .map(|(x, z)| self.turn(Vec3::new(x, 0.0, z)));
        let low = corners.iter().fold(Vec3::INFINITY, |a, &b| a.min(b));
        let high = corners.iter().fold(Vec3::NEG_INFINITY, |a, &b| a.max(b));
        (
            self.center + Vec3::new(low.x, min.y, low.z),
            self.center + Vec3::new(high.x, max.y, high.z),
        )
    }

    /// The center of a side (on the flat side, below a dome), area frame
    pub fn side_center(&self, side: Side) -> Vec3 {
        self.center + self.side_normal(side) * self.half[side.axis]
    }

    /// Where the handle of a side is: its center, on top of its dome, area frame
    pub fn side_handle(&self, side: Side) -> Vec3 {
        self.side_center(side) + self.side_normal(side) * self.dome_height(side)
    }
}

/// Distance from a dome (see the module): `height` above the plane of the side, `aside` how far the
/// point is outside the edges of the side in its plane (negative inside, per in-plane axis), `rim` the
/// rim radius, `radius` and `depth` the capsule (radius 0: flat, no dome), `thickness` the edge of the
/// box across the side. Infinite where the box itself is the answer. Same as `area_dome_distance` in
/// depth_mask.wgsl
fn dome_distance(
    height: f32,
    aside: Vec2,
    rim: f32,
    radius: f32,
    depth: f32,
    thickness: f32,
) -> f32 {
    if radius <= 0.0 {
        return f32::INFINITY;
    }
    // Distance from the axis in the plane of the side: on the short axis the distance from the
    // middle, on the long one from the end of the axis (0 along it)
    let rho = (aside + rim).max(Vec2::ZERO).length();
    let y = height + depth;
    // Inside the cone from the axis through the rim the nearest point of the capsule is on the dome.
    // Under the side, inside the box, the capsule counts too, so that there is no seam at the side;
    // never deeper than the opposite side (thickness + height below the point)
    if depth * rho < rim * y || (height <= 0.0 && aside.max_element() < 0.0) {
        (Vec2::new(rho, y).length() - radius)
            .max(aside.max_element())
            .max(-(thickness + height))
    } else {
        f32::INFINITY
    }
}

/// The boxes of the play area, in the area frame. Never empty
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlayArea {
    /// Turns the whole layout about the code (radians, counter-clockwise seen from above): the area
    /// frame is the frame of the code turned by this. The box of the properties takes it from
    /// `debug.alvr.marker_box_yaw`
    pub yaw: f32,
    boxes: [AreaBox; MAX_AREA_BOXES],
    count: usize,
}

impl PlayArea {
    pub fn single(area_box: AreaBox) -> Self {
        let mut boxes = [AreaBox::new(Vec3::ZERO, Vec3::ZERO); MAX_AREA_BOXES];
        boxes[0] = area_box;
        Self {
            yaw: 0.0,
            boxes,
            count: 1,
        }
    }

    /// The one box of the `debug.alvr.marker_box_*` properties: the same box as `marker_box`
    pub fn from_marker_box(params: &MarkerBoxParams) -> Self {
        let center = Vec3::new(params.offset.x, params.offset.y, -params.offset.z);
        Self {
            yaw: params.extra_yaw,
            ..Self::single(AreaBox::new(center, params.size.abs() * 0.5))
        }
    }

    /// A test layout from the box of the properties (`debug.alvr.area_test 1`): that box with a dome
    /// on its top (25 cm, at most the rim), and a second box on its +X side, reaching 40 cm beyond it
    /// (with a box at least that wide), half as high, standing on the same floor
    pub fn test_layout(params: &MarkerBoxParams) -> Self {
        let mut area = Self::from_marker_box(params);
        area.set_dome(0, Side::new(1, true), 0.25);
        if let Some(added) = area.add_box(0, Side::new(0, true)) {
            area.drag_side(added, Side::new(0, true), 0.1);
            let height = 2.0 * area.boxes[added].half.y;
            area.drag_side(added, Side::new(1, true), -0.5 * height);
        }
        area
    }

    /// The worst case for the mask (`debug.alvr.area_test 8`): the box of the properties cut into 2 x
    /// 2 x 2 boxes, each with domes on its 3 outer sides (15 cm, at most the rim)
    pub fn stress_layout(params: &MarkerBoxParams) -> Self {
        let whole = Self::from_marker_box(params);
        let outer = whole.boxes[0];
        let mut area = whole;
        area.count = 0;
        for i in 0..MAX_AREA_BOXES {
            let sign = Vec3::new(
                if i & 1 == 0 { -1.0 } else { 1.0 },
                if i & 2 == 0 { -1.0 } else { 1.0 },
                if i & 4 == 0 { -1.0 } else { 1.0 },
            );
            let half = outer.half * 0.5;
            let mut part = AreaBox::new(outer.center + sign * half, half);
            for axis in 0..3 {
                part.domes[Side::new(axis, sign[axis] > 0.0).index()] = 0.15;
            }
            area.boxes[i] = part;
            area.count += 1;
        }
        area
    }

    pub fn boxes(&self) -> &[AreaBox] {
        &self.boxes[..self.count]
    }

    pub fn len(&self) -> usize {
        self.count
    }

    /// Never true: a layout has at least one box
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    pub fn dome_count(&self) -> usize {
        self.boxes()
            .iter()
            .map(|area_box| {
                Side::ALL
                    .iter()
                    .filter(|&&side| area_box.dome_height(side) > 0.0)
                    .count()
            })
            .sum()
    }

    /// Signed distance from the surface of the union of the boxes (meters, negative inside), area
    /// frame, with the bounds of `AreaBox::signed_distance`. Same as `area_signed_distance` in
    /// depth_mask.wgsl
    pub fn signed_distance(&self, point: Vec3, feathering: f32) -> f32 {
        // Outside the bounds of all boxes by the soft edge: that distance is a lower bound
        let (min, max) = self.bounds();
        let center = (min + max) * 0.5;
        let e = (point - center).abs() - (max - min) * 0.5;
        let outside = e.max(Vec3::ZERO).length();
        if outside >= feathering {
            return outside;
        }

        let mut distance = f32::INFINITY;
        for area_box in self.boxes() {
            distance = distance.min(area_box.signed_distance(point, feathering));
            // Inside by the soft edge: the mask is 1 whatever the other boxes give
            if distance <= -feathering {
                break;
            }
        }
        distance
    }

    pub fn contains(&self, point: Vec3) -> bool {
        self.boxes().iter().any(|area_box| area_box.contains(point))
    }

    /// Smallest and largest corner of all boxes with their domes, area frame
    pub fn bounds(&self) -> (Vec3, Vec3) {
        self.boxes().iter().fold(
            (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY)),
            |(min, max), area_box| {
                let (box_min, box_max) = area_box.bounds();
                (min.min(box_min), max.max(box_max))
            },
        )
    }

    /// From the area frame to stage space, for the code at `anchor_position` with the level heading
    /// `anchor_yaw` (see `heading_yaw`): the code turned by the yaw of the layout
    pub fn area_to_world(&self, anchor_position: Vec3, anchor_yaw: f32) -> Mat4 {
        Mat4::from_rotation_translation(
            Quat::from_rotation_y(anchor_yaw + self.yaw),
            anchor_position,
        )
    }

    /// All handles: box index, side, position (area frame)
    pub fn handles(&self) -> impl Iterator<Item = (usize, Side, Vec3)> + '_ {
        self.boxes()
            .iter()
            .enumerate()
            .flat_map(|(index, area_box)| {
                Side::ALL.map(|side| (index, side, area_box.side_handle(side)))
            })
    }

    /// Moves one side of a box by `amount` along its outward normal (meters, negative: inwards; of a
    /// turned box the normal in its own frame); the opposite side stays. Within `MIN_BOX_HALF`,
    /// `MAX_BOX_HALF` and `MAX_BOX_CENTER` (per axis of the area frame). Returns how far it moved
    pub fn drag_side(&mut self, index: usize, side: Side, amount: f32) -> f32 {
        if index >= self.count || !amount.is_finite() {
            return 0.0;
        }
        let area_box = &mut self.boxes[index];
        let half = area_box.half[side.axis];
        let normal = area_box.side_normal(side);
        // The size grows by the amount, the center moves by half of it along the normal
        let mut low = 2.0 * (MIN_BOX_HALF.min(half) - half);
        let mut high = 2.0 * (MAX_BOX_HALF.max(half) - half);
        for axis in 0..3 {
            let step = 0.5 * normal[axis];
            if step.abs() < 1e-6 {
                continue;
            }
            let center = area_box.center[axis];
            let (a, b) = (
                (-MAX_BOX_CENTER - center) / step,
                (MAX_BOX_CENTER - center) / step,
            );
            // A center already beyond the limit may stay where it is
            low = low.max(a.min(b).min(0.0));
            high = high.min(a.max(b).max(0.0));
        }
        let applied = amount.clamp(low.min(0.0), high.max(0.0));
        area_box.half[side.axis] = half + 0.5 * applied;
        area_box.center += normal * (0.5 * applied);
        applied
    }

    /// Turns a box about its vertical axis to `yaw` (radians, wrapped to -pi..pi). Returns the yaw set
    pub fn turn_box(&mut self, index: usize, yaw: f32) -> f32 {
        if index >= self.count {
            return 0.0;
        }
        let yaw = wrap_angle(yaw);
        self.boxes[index].yaw = yaw;
        yaw
    }

    /// Sets the dome of a side, within 0 and its rim radius (below `MIN_DOME`: flat). Returns the
    /// height set
    pub fn set_dome(&mut self, index: usize, side: Side, height: f32) -> f32 {
        if index >= self.count {
            return 0.0;
        }
        let area_box = &mut self.boxes[index];
        let height = if height.is_finite() {
            height.clamp(0.0, area_box.dome_rim(side))
        } else {
            0.0
        };
        let height = if height >= MIN_DOME { height } else { 0.0 };
        area_box.domes[side.index()] = height;
        height
    }

    /// Moves a whole box (area frame), its center within `MAX_BOX_CENTER`. Returns how far it moved
    pub fn move_box(&mut self, index: usize, delta: Vec3) -> Vec3 {
        if index >= self.count || !delta.is_finite() {
            return Vec3::ZERO;
        }
        let area_box = &mut self.boxes[index];
        let center = (area_box.center + delta).clamp(
            Vec3::splat(-MAX_BOX_CENTER).min(area_box.center),
            Vec3::splat(MAX_BOX_CENTER).max(area_box.center),
        );
        let moved = center - area_box.center;
        area_box.center = center;
        moved
    }

    /// Adds a box on a side of another: as large as that side, reaching `NEW_BOX_DEPTH` beyond it (at
    /// most as far as the other box is thick) and `ADDED_BOX_OVERLAP` into the other box, turned like
    /// it, no domes. Returns its index. None: already `MAX_AREA_BOXES`, or it would be farther than
    /// `MAX_BOX_CENTER`
    pub fn add_box(&mut self, from: usize, side: Side) -> Option<usize> {
        if from >= self.count || self.count >= MAX_AREA_BOXES {
            return None;
        }
        let source = self.boxes[from];
        let source_half = source.half[side.axis];
        let overlap = ADDED_BOX_OVERLAP.min(source_half);
        let beyond = NEW_BOX_DEPTH.min(2.0 * source_half);
        let mut half = source.half;
        half[side.axis] = 0.5 * (beyond + overlap);
        let center =
            source.center + source.side_normal(side) * (source_half - overlap + half[side.axis]);
        if center.abs().max_element() > MAX_BOX_CENTER {
            return None;
        }
        self.boxes[self.count] = AreaBox {
            yaw: source.yaw,
            ..AreaBox::new(center, half)
        };
        self.count += 1;
        Some(self.count - 1)
    }

    /// Removes a box. The last one is never removed (false)
    pub fn remove_box(&mut self, index: usize) -> bool {
        if index >= self.count || self.count <= 1 {
            return false;
        }
        self.boxes.copy_within(index + 1..self.count, index);
        self.count -= 1;
        self.boxes[self.count] = AreaBox::new(Vec3::ZERO, Vec3::ZERO);
        true
    }

    /// One line for the log
    pub fn describe(&self) -> String {
        let mut text = format!(
            "{} box{}, yaw {:.1} deg:",
            self.count,
            if self.count == 1 { "" } else { "es" },
            self.yaw.to_degrees()
        );
        for index in 0..self.count {
            let _ = write!(text, " {};", self.describe_box(index));
        }
        text.pop();
        text
    }

    /// One box for the log: "#<index> at (x, y, z) m, w x h x d m, yaw 30.0 deg, dome +y 0.250 m" (the
    /// yaw only when the box is turned)
    pub fn describe_box(&self, index: usize) -> String {
        let Some(area_box) = self.boxes().get(index) else {
            return format!("#{index} (no such box)");
        };
        let size = area_box.half * 2.0;
        let mut text = format!(
            "#{index} at ({:.3}, {:.3}, {:.3}) m, {:.3} x {:.3} x {:.3} m",
            area_box.center.x, area_box.center.y, area_box.center.z, size.x, size.y, size.z
        );
        if area_box.yaw != 0.0 {
            let _ = write!(text, ", yaw {:.1} deg", area_box.yaw.to_degrees());
        }
        for side in Side::ALL {
            let height = area_box.dome_height(side);
            if height > 0.0 {
                let _ = write!(text, ", dome {} {height:.3} m", side.name());
            }
        }
        text
    }

    /// The layout as text, one item per line (meters, area frame):
    /// ```text
    /// alvr_play_area 1
    /// payload RIG1
    /// yaw_deg 0.00
    /// box cx cy cz hx hy hz [dome+x dome-x dome+y dome-y dome+z dome-z]
    /// box_yaw_deg 30.00
    /// ```
    /// `payload`: the text of the code the layout belongs to (written when given, the rest of its
    /// line). `box_yaw_deg`: turns the box of the line before (written only for a turned box; a reader
    /// of before it leaves the line out as unknown and reads the box unturned, and this one reads its
    /// files: the version stays 1). Lines starting with # are comments
    pub fn to_text(&self, payload: Option<&str>) -> String {
        let mut text = format!("{AREA_FILE_HEADER} {AREA_FILE_VERSION}\n");
        text.push_str(
            "# Play area of the depth mask: boxes in the frame of the code, meters. +x to the right of \
             the heading, +y up, -z along the heading\n",
        );
        text.push_str(
            "# box center_x center_y center_z half_x half_y half_z [domes +x -x +y -y +z -z]; half \
             extents and domes in the frame of the box, turned about +y by the box_yaw_deg line after \
             it (counter-clockwise seen from above; none: 0)\n",
        );
        if let Some(payload) = payload {
            // One line
            let payload = payload.replace(['\n', '\r'], " ");
            let _ = writeln!(text, "payload {payload}");
        }
        let _ = writeln!(text, "yaw_deg {:.2}", self.yaw.to_degrees());
        for area_box in self.boxes() {
            let c = area_box.center;
            let h = area_box.half;
            let _ = write!(
                text,
                "box {:.4} {:.4} {:.4} {:.4} {:.4} {:.4}",
                c.x, c.y, c.z, h.x, h.y, h.z
            );
            if area_box.domes.iter().any(|&dome| dome > 0.0) {
                for dome in area_box.domes {
                    let _ = write!(text, " {dome:.4}");
                }
            }
            text.push('\n');
            if area_box.yaw != 0.0 {
                let _ = writeln!(text, "{BOX_YAW_KEY} {:.2}", area_box.yaw.to_degrees());
            }
        }
        text
    }

    /// Reads the text of `to_text`. Unknown lines are left out (listed in `ignored`), values out of
    /// the limits are clamped and boxes after the 8th left out (listed in `warnings`). Err: no header,
    /// another version, a box line that does not read, or no box
    pub fn parse(text: &str) -> Result<ParsedArea, String> {
        let mut lines = text
            .lines()
            .enumerate()
            .map(|(number, line)| (number + 1, line.trim()))
            .filter(|(_, line)| !line.is_empty() && !line.starts_with('#'));

        let (_, header) = lines.next().ok_or("empty")?;
        let mut words = header.split_whitespace();
        if words.next() != Some(AREA_FILE_HEADER) {
            return Err(format!(
                "the first line is not \"{AREA_FILE_HEADER} <version>\""
            ));
        }
        let version = words
            .next()
            .and_then(|word| word.parse::<u32>().ok())
            .ok_or("no version on the first line")?;
        if version != AREA_FILE_VERSION {
            return Err(format!(
                "version {version}, only {AREA_FILE_VERSION} is known"
            ));
        }

        let mut area = Self::single(AreaBox::new(Vec3::ZERO, Vec3::ZERO));
        area.count = 0;
        let mut parsed = ParsedArea {
            area,
            payload: None,
            ignored: Vec::new(),
            warnings: Vec::new(),
        };
        // Whether the last box line made a box (a `box_yaw_deg` line turns it)
        let mut last_box_kept = false;
        for (number, line) in lines {
            let (key, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
            let rest = rest.trim();
            match key {
                "payload" => parsed.payload = Some(rest.to_owned()),
                "yaw_deg" => {
                    let yaw = rest
                        .parse::<f32>()
                        .ok()
                        .filter(|yaw| yaw.is_finite())
                        .ok_or(format!("line {number}: yaw_deg is not a number"))?;
                    parsed.area.yaw = yaw.clamp(-360.0, 360.0).to_radians();
                }
                "box" => {
                    let values = rest
                        .split_whitespace()
                        .map(|word| word.parse::<f32>().ok().filter(|value| value.is_finite()))
                        .collect::<Option<Vec<_>>>()
                        .ok_or(format!("line {number}: a box value is not a number"))?;
                    if values.len() != 6 && values.len() != 12 {
                        return Err(format!(
                            "line {number}: a box has 6 or 12 numbers, not {}",
                            values.len()
                        ));
                    }
                    if parsed.area.count >= MAX_AREA_BOXES {
                        parsed.warnings.push(format!(
                            "line {number}: more than {MAX_AREA_BOXES} boxes, left out"
                        ));
                        last_box_kept = false;
                        continue;
                    }
                    let given = AreaBox {
                        center: Vec3::from_slice(&values[0..3]),
                        half: Vec3::from_slice(&values[3..6]),
                        domes: std::array::from_fn(|i| values.get(6 + i).copied().unwrap_or(0.0)),
                        yaw: 0.0,
                    };
                    let mut area_box = AreaBox {
                        center: given
                            .center
                            .clamp(Vec3::splat(-MAX_BOX_CENTER), Vec3::splat(MAX_BOX_CENTER)),
                        half: given
                            .half
                            .clamp(Vec3::splat(MIN_FILE_HALF), Vec3::splat(MAX_BOX_HALF)),
                        domes: given.domes.map(|dome| dome.max(0.0)),
                        yaw: 0.0,
                    };
                    for side in Side::ALL {
                        let rim = area_box.dome_rim(side);
                        area_box.domes[side.index()] = area_box.domes[side.index()].min(rim);
                    }
                    if area_box != given {
                        parsed.warnings.push(format!(
                            "line {number}: box values out of the limits, clamped"
                        ));
                    }
                    parsed.area.boxes[parsed.area.count] = area_box;
                    parsed.area.count += 1;
                    last_box_kept = true;
                }
                BOX_YAW_KEY => {
                    let yaw = rest
                        .parse::<f32>()
                        .ok()
                        .filter(|yaw| yaw.is_finite())
                        .ok_or(format!("line {number}: {BOX_YAW_KEY} is not a number"))?;
                    if last_box_kept {
                        let index = parsed.area.count - 1;
                        parsed.area.boxes[index].yaw = wrap_angle(yaw.to_radians());
                    } else {
                        parsed.warnings.push(format!(
                            "line {number}: {BOX_YAW_KEY} without a box before it, left out"
                        ));
                    }
                    // One per box
                    last_box_kept = false;
                }
                _ => parsed.ignored.push(format!("line {number}: {line}")),
            }
        }
        if parsed.area.count == 0 {
            return Err("no box".into());
        }

        Ok(parsed)
    }
}

/// A layout read from text
#[derive(Clone, Debug, PartialEq)]
pub struct ParsedArea {
    pub area: PlayArea,
    pub payload: Option<String>,
    /// Lines with an unknown key, "line <n>: <text>"
    pub ignored: Vec<String>,
    /// Values clamped, boxes left out
    pub warnings: Vec<String>,
}

/// A play area placed in stage space: the depth mask uses it instead of the region of the settings
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlacedArea {
    /// Rigid transform (rotation about +Y and translation) from the area frame to stage space
    pub area_to_world: Mat4,
    pub area: PlayArea,
}

impl PlacedArea {
    /// Signed distance of a point in stage space, exact (see `PlayArea::signed_distance`)
    pub fn signed_distance(&self, world_point: Vec3) -> f32 {
        let point = self.area_to_world.inverse().transform_point3(world_point);
        self.area.signed_distance(point, f32::INFINITY)
    }

    /// Box `index` as an oriented box in stage space (no domes; turned by its yaw)
    pub fn box_region(&self, index: usize) -> RegionOverride {
        let area_box = self.area.boxes()[index.min(self.area.len() - 1)];
        let box_to_area = if area_box.yaw == 0.0 {
            Mat4::from_translation(area_box.center)
        } else {
            Mat4::from_rotation_translation(Quat::from_rotation_y(area_box.yaw), area_box.center)
        };
        RegionOverride {
            volume_to_world: self.area_to_world * box_to_area,
            half_extents: area_box.half,
        }
    }
}

impl From<RegionOverride> for PlacedArea {
    /// One box without domes, the area frame at its center
    fn from(region: RegionOverride) -> Self {
        Self {
            area_to_world: region.volume_to_world,
            area: PlayArea::single(AreaBox::new(Vec3::ZERO, region.half_extents)),
        }
    }
}

/// Floats of one box in the uniform block of the mask (`AreaBox` in depth_common.wgsl)
pub(crate) const AREA_BOX_FLOAT_COUNT: usize = 7 * 4;
/// Floats of the area at the end of the uniform block: the bounds (center, half extents) and the boxes
pub(crate) const AREA_FLOAT_COUNT: usize = 2 * 4 + MAX_AREA_BOXES * AREA_BOX_FLOAT_COUNT;

/// The area at the end of the uniform block, see `AreaBox` in depth_common.wgsl. Unused boxes are
/// zeros. Pass `None` without an area (the distance region)
pub(crate) fn area_uniforms(area: Option<&PlayArea>) -> [f32; AREA_FLOAT_COUNT] {
    let mut data = [0.0; AREA_FLOAT_COUNT];
    let Some(area) = area else {
        return data;
    };
    let (min, max) = area.bounds();
    data[0..3].copy_from_slice(&((min + max) * 0.5).to_array());
    data[4..7].copy_from_slice(&((max - min) * 0.5).to_array());
    for (index, area_box) in area.boxes().iter().enumerate() {
        let spheres = Side::ALL.map(|side| area_box.dome_sphere(side));
        let pick = |positive: bool, depth: bool| {
            [0, 1, 2].map(|axis| {
                let (radius, below) = spheres[Side::new(axis, positive).index()];
                if depth { below } else { radius }
            })
        };
        let base = 8 + index * AREA_BOX_FLOAT_COUNT;
        let out = &mut data[base..base + AREA_BOX_FLOAT_COUNT];
        out[0..3].copy_from_slice(&area_box.center.to_array());
        out[3] = area_box.tallest_dome();
        out[4..7].copy_from_slice(&area_box.half.to_array());
        out[8..11].copy_from_slice(&pick(true, false));
        out[12..15].copy_from_slice(&pick(false, false));
        out[16..19].copy_from_slice(&pick(true, true));
        out[20..23].copy_from_slice(&pick(false, true));
        let (sin, cos) = area_box.yaw.sin_cos();
        out[24] = cos;
        out[25] = sin;
    }
    data
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Random(u32);

    impl Random {
        // -1..1
        fn next(&mut self) -> f32 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 17;
            self.0 ^= self.0 << 5;
            (self.0 as f32 / u32::MAX as f32) * 2.0 - 1.0
        }

        fn vec3(&mut self) -> Vec3 {
            Vec3::new(self.next(), self.next(), self.next())
        }
    }

    // A box with domes of every kind: square side (spherical cap), long sides, a half ball/cylinder
    fn domed_box() -> AreaBox {
        let mut area_box = AreaBox::new(Vec3::new(0.1, 0.2, -0.3), Vec3::new(0.3, 0.2, 0.2));
        area_box.domes = [0.15, 0.0, 0.08, 0.0, 0.2, 0.03];
        area_box
    }

    // Points on the surface of the solid (box faces, also under the domes, and the domes), about
    // `step` apart. All of them are in the closed solid: the smallest distance to them is the distance
    // from the solid, up to the step
    fn surface_points(area_box: &AreaBox, step: f32) -> Vec<Vec3> {
        let mut points = vec![];
        for side in Side::ALL {
            let (u, v) = OTHER_AREA_AXES[side.axis];
            let (hu, hv) = (area_box.half[u], area_box.half[v]);
            let nu = (2.0 * hu / step).ceil() as usize;
            let nv = (2.0 * hv / step).ceil() as usize;
            for i in 0..=nu {
                for j in 0..=nv {
                    let mut point = area_box.side_center(side);
                    point[u] += -hu + 2.0 * hu * i as f32 / nu as f32;
                    point[v] += -hv + 2.0 * hv * j as f32 / nv as f32;
                    points.push(point);
                }
            }
            let (radius, depth) = area_box.dome_sphere(side);
            if radius <= 0.0 {
                continue;
            }
            // The capsule around the axis along the longer in-plane axis, by angle (even spacing
            // also where the dome is steep), only the part beyond the side
            let rim = area_box.dome_rim(side);
            let (long, short) = if hu >= hv { (u, v) } else { (v, u) };
            let axis_half = area_box.half[long] - rim;
            let normal = side.normal();
            let mut long_dir = Vec3::ZERO;
            long_dir[long] = 1.0;
            let mut short_dir = Vec3::ZERO;
            short_dir[short] = 1.0;
            let middle = area_box.side_center(side) - normal * depth;
            let max_angle = (depth / radius).clamp(-1.0, 1.0).acos();
            // Balls at both ends of the axis
            let rings = ((max_angle * radius / step).ceil() as usize).max(1);
            for end in [-axis_half, axis_half] {
                let center = middle + long_dir * end;
                for i in 0..=rings {
                    let polar = max_angle * i as f32 / rings as f32;
                    let around = ((std::f32::consts::TAU * radius * polar.sin() / step).ceil()
                        as usize)
                        .max(1);
                    for j in 0..around {
                        let angle = std::f32::consts::TAU * j as f32 / around as f32;
                        let direction = normal * polar.cos()
                            + (long_dir * angle.cos() + short_dir * angle.sin()) * polar.sin();
                        points.push(center + direction * radius);
                    }
                }
            }
            // The cylinder between them
            let lengths = (2.0 * axis_half / step).ceil() as usize;
            let arcs = ((2.0 * max_angle * radius / step).ceil() as usize).max(1);
            for i in 0..=lengths {
                let along = if lengths == 0 {
                    0.0
                } else {
                    -axis_half + 2.0 * axis_half * i as f32 / lengths as f32
                };
                for j in 0..=arcs {
                    let angle = -max_angle + 2.0 * max_angle * j as f32 / arcs as f32;
                    let direction = normal * angle.cos() + short_dir * angle.sin();
                    points.push(middle + long_dir * along + direction * radius);
                }
            }
        }
        points
    }

    fn brute_force(points: &[Vec3], point: Vec3) -> f32 {
        points
            .iter()
            .map(|p| p.distance(point))
            .fold(f32::INFINITY, f32::min)
    }

    #[test]
    fn sides_in_the_order_of_the_domes() {
        for (index, side) in Side::ALL.iter().enumerate() {
            assert_eq!(side.index(), index);
            assert_eq!(side.opposite().opposite(), *side);
            assert_eq!(side.normal(), -side.opposite().normal());
            assert_eq!(side.normal()[side.axis], side.sign());
        }
        assert_eq!(
            Side::ALL.map(Side::name),
            ["+x", "-x", "+y", "-y", "+z", "-z"]
        );
    }

    #[test]
    fn distance_matches_brute_force_outside() {
        let area_box = domed_box();
        let step = 0.005;
        let points = surface_points(&area_box, step);
        let mut random = Random(12345);
        let mut checked = 0;
        for _ in 0..800 {
            let point = area_box.center + random.vec3() * Vec3::new(0.7, 0.6, 0.6);
            if area_box.contains(point) {
                continue;
            }
            let exact = area_box.signed_distance(point, f32::INFINITY);
            let expected = brute_force(&points, point);
            // The samples are in the solid: never nearer than the true distance
            assert!(exact <= expected + 1e-4, "{point}: {exact} > {expected}");
            assert!(expected - exact < step, "{point}: {exact} vs {expected}");
            checked += 1;
        }
        assert!(checked > 400, "{checked}");
    }

    #[test]
    fn sign_is_right_inside_and_outside() {
        let area_box = domed_box();
        let mut random = Random(777);
        for _ in 0..5000 {
            let point = area_box.center + random.vec3() * Vec3::new(0.6, 0.5, 0.5);
            let distance = area_box.signed_distance(point, f32::INFINITY);
            if distance.abs() > 1e-4 {
                assert_eq!(
                    distance < 0.0,
                    area_box.contains(point),
                    "{point}: {distance}"
                );
            }
        }
    }

    #[test]
    fn thin_box_has_no_jump_under_a_dome() {
        // 5 cm thick, a 10 cm dome on top: the capsule reaches far below the bottom of the box
        let mut area_box = AreaBox::new(Vec3::ZERO, Vec3::new(0.5, 0.025, 0.5));
        area_box.domes[Side::new(1, true).index()] = 0.1;
        let above = area_box.signed_distance(Vec3::new(0.0, 1e-6, 0.0), 0.03);
        let below = area_box.signed_distance(Vec3::new(0.0, -1e-6, 0.0), 0.03);
        assert!((above - below).abs() < 1e-4, "{above} vs {below}");
        assert!((above + 0.025).abs() < 1e-4, "{above}");
    }

    #[test]
    fn union_matches_brute_force_outside() {
        let mut area = PlayArea::single(domed_box());
        let added = area.add_box(0, Side::new(0, false)).unwrap();
        area.set_dome(added, Side::new(2, true), 0.1);
        area.drag_side(added, Side::new(1, true), 0.15);
        let step = 0.005;
        let points = area
            .boxes()
            .iter()
            .flat_map(|area_box| surface_points(area_box, step))
            .collect::<Vec<_>>();
        let (min, max) = area.bounds();
        let mut random = Random(4242);
        for _ in 0..400 {
            let point = (min + max) * 0.5 + random.vec3() * ((max - min) * 0.5 + 0.1);
            let exact = area.signed_distance(point, f32::INFINITY);
            if area.contains(point) {
                assert!(exact <= 1e-4, "{point}: {exact}");
                continue;
            }
            let expected = brute_force(&points, point);
            assert!(exact <= expected + 1e-4, "{point}: {exact} > {expected}");
            assert!(expected - exact < step, "{point}: {exact} vs {expected}");
        }
    }

    // The bounds the shader takes (with the feather) give the same mask as the exact distance
    #[test]
    fn shortcuts_keep_the_mask() {
        let mask = |distance: f32, feathering: f32| {
            let t = ((distance + feathering) / (2.0 * feathering)).clamp(0.0, 1.0);
            1.0 - t * t * (3.0 - 2.0 * t)
        };
        let mut area = PlayArea::stress_layout(&MarkerBoxParams {
            offset: Vec3::new(0.0, 0.15, 0.0),
            size: Vec3::new(1.2, 0.9, 1.0),
            extra_yaw: 0.0,
        });
        area.set_dome(0, Side::new(0, false), 0.3);
        let mut random = Random(99);
        for feathering in [0.0005, 0.01, 0.03] {
            for _ in 0..4000 {
                let point = Vec3::new(0.0, 0.15, 0.0) + random.vec3() * Vec3::new(1.0, 0.8, 0.9);
                let fast = area.signed_distance(point, feathering);
                let exact = area.signed_distance(point, f32::INFINITY);
                assert!(
                    (mask(fast, feathering) - mask(exact, feathering)).abs() < 1e-6,
                    "{point}: {fast} vs {exact}"
                );
                if exact >= 0.0 {
                    // Outside the fast value is a lower bound
                    assert!(fast <= exact + 1e-6, "{point}: {fast} vs {exact}");
                }
            }
        }
    }

    #[test]
    fn flat_domes_give_the_plain_box() {
        let mut area_box = domed_box();
        area_box.domes = [0.0, 0.0, 0.004, 0.0, 0.0, 0.0];
        assert!(!area_box.has_domes());
        let mut random = Random(5);
        for _ in 0..1000 {
            let point = area_box.center + random.vec3();
            let e = (point - area_box.center).abs() - area_box.half;
            let plain = e.max(Vec3::ZERO).length() + e.max_element().min(0.0);
            assert_eq!(area_box.signed_distance(point, f32::INFINITY), plain);
            assert_eq!(area_box.signed_distance(point, 0.01), plain);
        }
    }

    #[test]
    fn dome_limits() {
        let mut area = PlayArea::single(AreaBox::new(Vec3::ZERO, Vec3::new(0.5, 0.4, 0.3)));
        let top = Side::new(1, true);
        // The rim of the top is half its shorter edge, the dome at most that high
        assert_eq!(area.boxes()[0].dome_rim(top), 0.3);
        assert_eq!(area.set_dome(0, top, 10.0), 0.3);
        assert_eq!(area.set_dome(0, top, 0.004), 0.0);
        assert_eq!(area.set_dome(0, top, f32::NAN), 0.0);
        assert_eq!(area.set_dome(0, top, 0.2), 0.2);
        let area_box = area.boxes()[0];
        assert_eq!(area_box.tallest_dome(), 0.2);

        // The top of the dome is at the handle
        let handle = area_box.side_handle(top);
        assert!((handle - Vec3::new(0.0, 0.6, 0.0)).length() < 1e-6);
        assert!(area_box.signed_distance(handle, f32::INFINITY).abs() < 1e-5);
        let above = handle + Vec3::Y * 0.1;
        assert!((area_box.signed_distance(above, f32::INFINITY) - 0.1).abs() < 1e-5);

        // Along the long axis the top is straight
        let along = Vec3::new(0.15, 0.6, 0.0);
        assert!(area_box.signed_distance(along, f32::INFINITY).abs() < 1e-5);

        // The corners of the side stay flat, also with the tallest dome
        area.set_dome(0, top, 1.0);
        let area_box = area.boxes()[0];
        for x in [-1.0, 1.0] {
            for z in [-1.0, 1.0] {
                let near_corner = Vec3::new(0.49 * x, 0.401, 0.29 * z);
                assert!(!area_box.contains(near_corner), "{near_corner}");
                let distance = area_box.signed_distance(near_corner, f32::INFINITY);
                assert!((distance - 0.001).abs() < 1e-5, "{distance}");
            }
        }
        // The rim lies in the side: just inside it the dome starts, just outside the side is flat
        let (radius, depth) = area_box.dome_sphere(top);
        assert!(
            (radius - 0.3).abs() < 1e-6 && depth.abs() < 1e-6,
            "half cylinder"
        );
        assert!(area_box.contains(Vec3::new(0.0, 0.401, 0.29)));
        assert!(area_box.contains(Vec3::new(0.49, 0.401, 0.0)));
        assert!(!area_box.contains(Vec3::new(0.49, 0.401, 0.1)));
        // The bounds hold the dome
        let (_, max) = area_box.bounds();
        assert!((max.y - 0.7).abs() < 1e-6);
    }

    #[test]
    fn spherical_cap_on_a_square_side() {
        let mut area_box = AreaBox::new(Vec3::ZERO, Vec3::splat(0.2));
        area_box.domes[Side::new(2, false).index()] = 0.05;
        let (radius, depth) = area_box.dome_sphere(Side::new(2, false));
        // R = (b^2 + h^2) / 2h
        assert!((radius - (0.04 + 0.0025) / 0.1).abs() < 1e-6);
        assert!((depth - (radius - 0.05)).abs() < 1e-6);
        // A sphere: the same at every angle about the axis of the side
        for angle in [0.0_f32, 0.4, 0.8, 1.2] {
            let rho = 0.15;
            let point = Vec3::new(rho * angle.cos(), rho * angle.sin(), -0.2);
            let height = (radius * radius - rho * rho).sqrt() - depth;
            let on_dome = point - Vec3::Z * height;
            assert!(
                area_box.signed_distance(on_dome, f32::INFINITY).abs() < 1e-5,
                "{angle}"
            );
        }
    }

    // Before the play area the mask had one box: the same distance for the box of the properties
    #[test]
    fn single_box_matches_the_marker_box() {
        let mut random = Random(31337);
        for _ in 0..50 {
            let params = MarkerBoxParams {
                offset: random.vec3() * 0.5,
                size: Vec3::new(1.2, 0.9, 1.0) + random.vec3() * 0.3,
                extra_yaw: random.next(),
            };
            let position = random.vec3() * 2.0;
            let yaw = random.next() * 3.0;
            let old = crate::marker_box(position, yaw, &params);
            let area = PlayArea::from_marker_box(&params);
            let placed = PlacedArea {
                area_to_world: area.area_to_world(position, yaw),
                area,
            };
            assert!(
                placed
                    .box_region(0)
                    .volume_to_world
                    .abs_diff_eq(old.volume_to_world, 1e-5)
            );
            let world_to_volume = old.volume_to_world.inverse();
            let world_to_area = placed.area_to_world.inverse();
            for _ in 0..50 {
                let point = position + random.vec3() * 1.5;
                let e = world_to_volume.transform_point3(point).abs() - old.half_extents;
                let before = e.max(Vec3::ZERO).length() + e.max_element().min(0.0);
                let local = world_to_area.transform_point3(point);
                for feathering in [0.0005, 0.03, f32::INFINITY] {
                    let now = area.signed_distance(local, feathering);
                    assert!((now - before).abs() < 2e-5, "{now} vs {before}");
                }
            }
        }
    }

    #[test]
    fn region_converts_to_one_box() {
        let region = RegionOverride {
            volume_to_world: Mat4::from_rotation_translation(
                Quat::from_rotation_y(0.3),
                Vec3::new(0.2, 1.0, -0.5),
            ),
            half_extents: Vec3::new(0.6, 0.45, 0.5),
        };
        let placed = PlacedArea::from(region);
        assert_eq!(placed.area.len(), 1);
        assert_eq!(placed.box_region(0), region);
        let inside = region
            .volume_to_world
            .transform_point3(Vec3::new(0.5, 0.0, 0.0));
        assert!((placed.signed_distance(inside) + 0.1).abs() < 1e-5);
    }

    #[test]
    fn dragging_a_side_keeps_the_opposite_one() {
        let mut area = PlayArea::single(AreaBox::new(Vec3::ZERO, Vec3::new(0.5, 0.4, 0.3)));
        let side = Side::new(0, true);
        let opposite = area.boxes()[0].side_center(side.opposite());
        assert!((area.drag_side(0, side, 0.2) - 0.2).abs() < 1e-6);
        let area_box = area.boxes()[0];
        assert!((area_box.side_center(side).x - 0.7).abs() < 1e-6);
        assert!(area_box.side_center(side.opposite()).distance(opposite) < 1e-6);

        // Not smaller than the minimum
        let applied = area.drag_side(0, Side::new(2, false), -5.0);
        assert!(
            (applied + (0.6 - 2.0 * MIN_BOX_HALF)).abs() < 1e-5,
            "{applied}"
        );
        assert!((area.boxes()[0].half.z - MIN_BOX_HALF).abs() < 1e-6);
        assert!((area.boxes()[0].side_center(Side::new(2, true)).z - 0.3).abs() < 1e-6);
        // Not larger than the maximum
        area.drag_side(0, Side::new(1, true), 100.0);
        assert!(area.boxes()[0].half.y <= MAX_BOX_HALF + 1e-6);
        assert!(area.boxes()[0].center.abs().max_element() <= MAX_BOX_CENTER + 1e-6);
        // A box out of reach
        assert_eq!(area.drag_side(3, side, 0.1), 0.0);
    }

    #[test]
    fn dome_follows_a_smaller_side() {
        let mut area = PlayArea::single(AreaBox::new(Vec3::ZERO, Vec3::splat(0.3)));
        let top = Side::new(1, true);
        area.set_dome(0, top, 0.3);
        area.drag_side(0, Side::new(0, true), -0.3);
        // Now 0.15 wide in x: the dome shown is at most the new rim, the one set is kept
        assert!((area.boxes()[0].dome_height(top) - 0.15).abs() < 1e-6);
        area.drag_side(0, Side::new(0, true), 0.3);
        assert!((area.boxes()[0].dome_height(top) - 0.3).abs() < 1e-6);
    }

    #[test]
    fn boxes_are_added_next_to_a_side_and_removed() {
        let mut area = PlayArea::single(AreaBox::new(Vec3::ZERO, Vec3::new(0.5, 0.4, 0.3)));
        let side = Side::new(2, false);
        let added = area.add_box(0, side).unwrap();
        let (first, second) = (area.boxes()[0], area.boxes()[added]);
        // On that side, as large as it, NEW_BOX_DEPTH beyond it and ADDED_BOX_OVERLAP into the first
        let shared = first.side_center(side);
        assert!((second.side_center(side.opposite()) - (shared + Vec3::Z * 0.05)).length() < 1e-6);
        assert!((second.side_center(side) - (shared - Vec3::Z * 0.3)).length() < 1e-6);
        assert_eq!(second.half.x, 0.5);
        assert_eq!(second.half.y, 0.4);
        // Without a seam: the middle of the side they share is inside by the overlap
        let distance = area.signed_distance(shared, f32::INFINITY);
        assert!((distance + 0.05).abs() < 1e-5, "{distance}");

        while area.add_box(0, Side::new(1, true)).is_some() {}
        assert_eq!(area.len(), MAX_AREA_BOXES);
        assert!(area.remove_box(1));
        assert_eq!(area.len(), MAX_AREA_BOXES - 1);
        while area.remove_box(0) {}
        // The last box stays
        assert_eq!(area.len(), 1);
        assert!(!area.remove_box(0));

        // Moving a box
        let moved = area.move_box(0, Vec3::new(10.0, 0.1, 0.0));
        assert!((area.boxes()[0].center.x - MAX_BOX_CENTER).abs() < 1e-6);
        assert!((moved.y - 0.1).abs() < 1e-6);
    }

    #[test]
    fn handles_are_on_the_sides() {
        let area = PlayArea::test_layout(&MarkerBoxParams {
            offset: Vec3::new(0.0, 0.15, 0.0),
            size: Vec3::new(1.2, 0.9, 1.0),
            extra_yaw: 0.0,
        });
        let handles = area.handles().collect::<Vec<_>>();
        assert_eq!(handles.len(), 6 * area.len());
        for (index, _, position) in handles {
            // On the surface of its box
            let distance = area.boxes()[index].signed_distance(position, f32::INFINITY);
            assert!(distance.abs() < 1e-5, "{distance}");
        }
    }

    #[test]
    fn test_layouts() {
        let params = MarkerBoxParams {
            offset: Vec3::new(0.0, 0.15, 0.0),
            size: Vec3::new(1.2, 0.9, 1.0),
            extra_yaw: 0.0,
        };
        let area = PlayArea::test_layout(&params);
        assert_eq!(area.len(), 2);
        assert_eq!(area.dome_count(), 1);
        let [first, second] = [area.boxes()[0], area.boxes()[1]];
        assert_eq!(first.half, Vec3::new(0.6, 0.45, 0.5));
        assert!((first.dome_height(Side::new(1, true)) - 0.25).abs() < 1e-6);
        // On the same floor, half as high, 40 cm beyond the +X side and 5 cm into the first box
        assert!((first.center.y - first.half.y - (second.center.y - second.half.y)).abs() < 1e-6);
        assert!((second.half.y - 0.5 * first.half.y).abs() < 1e-6);
        let side = first.center.x + first.half.x;
        assert!((second.center.x - second.half.x - (side - ADDED_BOX_OVERLAP)).abs() < 1e-6);
        assert!((second.center.x + second.half.x - (side + 0.4)).abs() < 1e-6);

        let stress = PlayArea::stress_layout(&params);
        assert_eq!(stress.len(), MAX_AREA_BOXES);
        assert_eq!(stress.dome_count(), 24);
        // Fills the box of the properties
        let (min, max) = stress
            .boxes()
            .iter()
            .fold((Vec3::splat(9.0), Vec3::splat(-9.0)), |(min, max), b| {
                (min.min(b.center - b.half), max.max(b.center + b.half))
            });
        assert!((min - Vec3::new(-0.6, -0.3, -0.5)).length() < 1e-6);
        assert!((max - Vec3::new(0.6, 0.6, 0.5)).length() < 1e-6);
    }

    #[test]
    fn text_round_trip() {
        let params = MarkerBoxParams {
            offset: Vec3::new(0.05, 0.15, -0.1),
            size: Vec3::new(1.2, 0.9, 1.0),
            extra_yaw: 10_f32.to_radians(),
        };
        for area in [
            PlayArea::from_marker_box(&params),
            PlayArea::test_layout(&params),
            PlayArea::stress_layout(&params),
        ] {
            let text = area.to_text(Some("RIG 1"));
            let parsed = PlayArea::parse(&text).unwrap();
            assert_eq!(parsed.payload.as_deref(), Some("RIG 1"));
            assert!(
                parsed.ignored.is_empty() && parsed.warnings.is_empty(),
                "{parsed:?}"
            );
            assert_eq!(parsed.area.len(), area.len());
            assert!((parsed.area.yaw - area.yaw).abs() < 1e-4);
            for (a, b) in parsed.area.boxes().iter().zip(area.boxes()) {
                assert!((a.center - b.center).abs().max_element() < 1e-4);
                assert!((a.half - b.half).abs().max_element() < 1e-4);
                for (x, y) in a.domes.iter().zip(b.domes) {
                    assert!((x - y).abs() < 1e-4);
                }
            }
            // And the same text again
            assert_eq!(parsed.area.to_text(Some("RIG 1")), text);
        }
    }

    #[test]
    fn text_tolerates_unknown_lines() {
        let text = "\n# a comment\nalvr_play_area 1\npayload  my rig \nmarker_mm 144.2\n\
                    box 0 0.15 0 0.6 0.45 0.5\nmount standing\n\
                    box 0.8 0.0 0 0.2 0.2 0.5 0 0 0.1 0 0 0\n";
        let parsed = PlayArea::parse(text).unwrap();
        assert_eq!(parsed.payload.as_deref(), Some("my rig"));
        assert_eq!(parsed.area.len(), 2);
        assert_eq!(parsed.area.yaw, 0.0);
        assert_eq!(
            parsed.ignored,
            ["line 5: marker_mm 144.2", "line 7: mount standing"]
        );
        assert!((parsed.area.boxes()[1].dome_height(Side::new(1, true)) - 0.1).abs() < 1e-6);
        // Out of the limits: clamped, with a warning
        let parsed =
            PlayArea::parse("alvr_play_area 1\nbox 9 0 0 0.001 0.2 0.2 0 0 5 0 0 0").unwrap();
        let area_box = parsed.area.boxes()[0];
        assert_eq!(area_box.center.x, MAX_BOX_CENTER);
        assert_eq!(area_box.half.x, MIN_FILE_HALF);
        assert_eq!(area_box.domes[2], MIN_FILE_HALF);
        assert_eq!(parsed.warnings.len(), 1);
        // More than 8 boxes: the rest left out
        let many = format!("alvr_play_area 1\n{}", "box 0 0 0 0.1 0.1 0.1\n".repeat(10));
        let parsed = PlayArea::parse(&many).unwrap();
        assert_eq!(parsed.area.len(), MAX_AREA_BOXES);
        assert_eq!(parsed.warnings.len(), 2);
    }

    #[test]
    fn text_errors() {
        for (text, error) in [
            ("", "empty"),
            ("box 0 0 0 1 1 1", "first line"),
            ("alvr_play_area", "no version"),
            ("alvr_play_area 2\nbox 0 0 0 1 1 1", "version 2"),
            ("alvr_play_area 1\nbox 0 0 0 1 1", "6 or 12"),
            ("alvr_play_area 1\nbox 0 0 0 1 1 x", "not a number"),
            ("alvr_play_area 1\nbox 0 0 0 1 1 inf", "not a number"),
            ("alvr_play_area 1\nyaw_deg 3", "no box"),
            ("alvr_play_area 1\nyaw_deg a\nbox 0 0 0 1 1 1", "yaw_deg"),
        ] {
            let result = PlayArea::parse(text);
            assert!(
                result.as_ref().is_err_and(|e| e.contains(error)),
                "{text:?}: {result:?}"
            );
        }
    }

    #[test]
    fn turning_matches_quat_and_wraps() {
        let mut area_box = domed_box();
        area_box.yaw = 0.8;
        let v = Vec3::new(0.3, -0.2, 0.7);
        assert!(area_box.turn(v).distance(Quat::from_rotation_y(0.8) * v) < 1e-6);
        assert!(area_box.turn_back(area_box.turn(v)).distance(v) < 1e-6);
        let point = Vec3::new(-0.4, 0.5, 0.2);
        assert!(area_box.to_area(area_box.to_local(point)).distance(point) < 1e-6);
        assert!((wrap_angle(3.5) - (3.5 - std::f32::consts::TAU)).abs() < 1e-6);
        assert!((wrap_angle(-7.0) - (-7.0 + std::f32::consts::TAU)).abs() < 1e-5);
        assert_eq!(wrap_angle(0.25), 0.25);
        assert_eq!(wrap_angle(f32::NAN), 0.0);
        assert!(wrap_angle(std::f32::consts::PI).abs() <= std::f32::consts::PI);
    }

    // A turned box is the box without its yaw in its own frame: distance, inside, handles
    #[test]
    fn turned_box_is_the_unturned_box_in_its_own_frame() {
        let unturned = domed_box();
        let mut random = Random(2718);
        for yaw in [0.3_f32, -1.2, 2.9] {
            let turned = AreaBox { yaw, ..unturned };
            for _ in 0..500 {
                let local = random.vec3() * Vec3::new(0.7, 0.6, 0.6);
                let in_area = turned.to_area(local);
                let plain = unturned.center + local;
                for feathering in [0.0005, 0.03, f32::INFINITY] {
                    let a = turned.signed_distance(in_area, feathering);
                    let b = unturned.signed_distance(plain, feathering);
                    assert!((a - b).abs() < 1e-5, "{yaw} {local}: {a} vs {b}");
                }
                assert_eq!(turned.contains(in_area), unturned.contains(plain));
            }
            for side in Side::ALL {
                let local = unturned.side_handle(side) - unturned.center;
                assert!(turned.side_handle(side).distance(turned.to_area(local)) < 1e-5);
                assert!(
                    turned
                        .side_normal(side)
                        .distance(Quat::from_rotation_y(yaw) * side.normal())
                        < 1e-6
                );
            }
        }
    }

    #[test]
    fn turned_box_matches_brute_force_outside() {
        let mut area_box = domed_box();
        area_box.yaw = 0.65;
        let step = 0.005;
        let unturned = AreaBox {
            yaw: 0.0,
            ..area_box
        };
        // The surface of the box without its yaw, turned about its center
        let points = surface_points(&unturned, step)
            .into_iter()
            .map(|point| area_box.to_area(point - area_box.center))
            .collect::<Vec<_>>();
        let (min, max) = area_box.bounds();
        let mut random = Random(16180);
        let mut checked = 0;
        for _ in 0..800 {
            let point = area_box.center + random.vec3() * Vec3::new(0.75, 0.6, 0.75);
            assert!(
                area_box.contains(point) == (area_box.signed_distance(point, f32::INFINITY) < 0.0)
                    || area_box.signed_distance(point, f32::INFINITY).abs() < 1e-4
            );
            // The bounds hold the box
            if area_box.contains(point) {
                assert!(point.cmpge(min - 1e-5).all() && point.cmple(max + 1e-5).all());
                continue;
            }
            let exact = area_box.signed_distance(point, f32::INFINITY);
            let expected = brute_force(&points, point);
            assert!(exact <= expected + 1e-4, "{point}: {exact} > {expected}");
            assert!(expected - exact < step, "{point}: {exact} vs {expected}");
            checked += 1;
        }
        assert!(checked > 400, "{checked}");
        // The bounds are those of the turned corners: a box turned by 45 deg is wider
        let square = AreaBox {
            yaw: std::f32::consts::FRAC_PI_4,
            ..AreaBox::new(Vec3::ZERO, Vec3::new(0.5, 0.2, 0.5))
        };
        let (min, max) = square.bounds();
        assert!((max.x - 0.5 * std::f32::consts::SQRT_2).abs() < 1e-5);
        assert!((min.z + 0.5 * std::f32::consts::SQRT_2).abs() < 1e-5);
        assert!((max.y - 0.2).abs() < 1e-6);
    }

    #[test]
    fn union_with_a_turned_box_keeps_the_mask() {
        let mask = |distance: f32, feathering: f32| {
            let t = ((distance + feathering) / (2.0 * feathering)).clamp(0.0, 1.0);
            1.0 - t * t * (3.0 - 2.0 * t)
        };
        let mut area = PlayArea::single(domed_box());
        area.turn_box(0, -0.4);
        let added = area.add_box(0, Side::new(0, true)).unwrap();
        area.set_dome(added, Side::new(1, true), 0.1);
        area.turn_box(added, 0.9);
        let (min, max) = area.bounds();
        let mut random = Random(4711);
        for feathering in [0.0005, 0.01, 0.03] {
            for _ in 0..3000 {
                let point = (min + max) * 0.5 + random.vec3() * ((max - min) * 0.5 + 0.1);
                let fast = area.signed_distance(point, feathering);
                let exact = area.signed_distance(point, f32::INFINITY);
                assert!((mask(fast, feathering) - mask(exact, feathering)).abs() < 1e-6);
                // Every box is within the bounds of the area
                if area.contains(point) {
                    assert!(point.cmpge(min - 1e-5).all() && point.cmple(max + 1e-5).all());
                }
            }
        }
    }

    #[test]
    fn turned_boxes_are_added_and_dragged_in_their_frame() {
        let yaw = 0.5_f32;
        let mut area = PlayArea::single(AreaBox {
            yaw,
            ..AreaBox::new(Vec3::new(0.1, 0.0, -0.2), Vec3::new(0.5, 0.4, 0.3))
        });
        let first = area.boxes()[0];
        let side = Side::new(2, false);
        let added = area.add_box(0, side).unwrap();
        let second = area.boxes()[added];
        // Turned like the first, on its turned side, as large as it, overlapping it by 5 cm
        assert_eq!(second.yaw, yaw);
        assert_eq!((second.half.x, second.half.y), (0.5, 0.4));
        let shared = first.side_center(side);
        let normal = first.side_normal(side);
        assert!(
            second
                .side_center(side.opposite())
                .distance(shared - normal * 0.05)
                < 1e-5
        );
        assert!(second.side_center(side).distance(shared + normal * 0.3) < 1e-5);
        assert!((area.signed_distance(shared, f32::INFINITY) + 0.05).abs() < 1e-5);

        // A side moves along its turned normal, the opposite one stays
        let opposite = area.boxes()[0].side_center(Side::new(0, false));
        let moved = area.drag_side(0, Side::new(0, true), 0.2);
        assert!((moved - 0.2).abs() < 1e-6);
        let dragged = area.boxes()[0];
        assert!(dragged.side_center(Side::new(0, false)).distance(opposite) < 1e-5);
        assert!((dragged.half.x - 0.6).abs() < 1e-6);
        // The center stays within the limit per axis of the area frame
        let moved = area.drag_side(0, Side::new(0, true), 10.0);
        let center = area.boxes()[0].center;
        assert!(
            center.abs().max_element() <= MAX_BOX_CENTER + 1e-5,
            "{center}"
        );
        assert!(moved > 0.0 && area.boxes()[0].half.x <= MAX_BOX_HALF + 1e-6);

        // Turned and described
        assert!((area.turn_box(1, 4.0) - (4.0 - std::f32::consts::TAU)).abs() < 1e-6);
        assert_eq!(area.turn_box(9, 1.0), 0.0);
        assert!(
            area.describe_box(0).contains(", yaw 28.6 deg"),
            "{}",
            area.describe_box(0)
        );
        assert!(
            !PlayArea::single(domed_box())
                .describe_box(0)
                .contains("yaw")
        );
    }

    #[test]
    fn turned_box_region_and_uniforms() {
        let mut area = PlayArea::single(AreaBox::new(
            Vec3::new(0.2, 0.1, -0.3),
            Vec3::new(0.4, 0.3, 0.2),
        ));
        area.turn_box(0, 0.7);
        let placed = PlacedArea {
            area_to_world: Mat4::from_rotation_translation(
                Quat::from_rotation_y(0.4),
                Vec3::new(1.0, 0.5, -1.0),
            ),
            area,
        };
        let region = placed.box_region(0);
        // A point inside near a corner of the region is inside the area by the same distance
        let local = Vec3::new(0.35, 0.25, 0.15);
        let world = region.volume_to_world.transform_point3(local);
        let e = local.abs() - region.half_extents;
        let expected = e.max_element();
        assert!((placed.signed_distance(world) - expected).abs() < 1e-5);
        let data = area_uniforms(Some(&area));
        let base = 8;
        assert!((data[base + 24] - 0.7_f32.cos()).abs() < 1e-6);
        assert!((data[base + 25] - 0.7_f32.sin()).abs() < 1e-6);
    }

    #[test]
    fn text_keeps_the_yaw_of_a_box() {
        let mut area = PlayArea::test_layout(&MarkerBoxParams {
            offset: Vec3::new(0.0, 0.15, 0.0),
            size: Vec3::new(1.2, 0.9, 1.0),
            extra_yaw: 0.0,
        });
        area.turn_box(1, 30_f32.to_radians());
        let text = area.to_text(Some("RIG1"));
        // Only the turned box has the line, right after its box line
        assert_eq!(text.matches(BOX_YAW_KEY).count(), 2, "{text}");
        let lines = text.lines().collect::<Vec<_>>();
        let at = lines
            .iter()
            .position(|line| line.starts_with("box_yaw_deg 30.00"))
            .unwrap();
        assert!(lines[at - 1].starts_with("box "));
        assert_eq!(
            lines.iter().filter(|line| line.starts_with("box ")).count(),
            2
        );
        let parsed = PlayArea::parse(&text).unwrap();
        assert!(
            parsed.ignored.is_empty() && parsed.warnings.is_empty(),
            "{parsed:?}"
        );
        assert_eq!(parsed.area.boxes()[0].yaw, 0.0);
        assert!((parsed.area.boxes()[1].yaw - 30_f32.to_radians()).abs() < 1e-5);
        assert_eq!(parsed.area.to_text(Some("RIG1")), text);

        // A file of before (no yaw lines) reads unturned; a reader of before leaves the yaw line out as
        // unknown and keeps the boxes: what this parser does with an unknown key
        let old = "alvr_play_area 1\nbox 0 0 0 0.5 0.5 0.5\n";
        assert_eq!(PlayArea::parse(old).unwrap().area.boxes()[0].yaw, 0.0);
        let unknown = text.replace(BOX_YAW_KEY, "box_tilt_deg");
        let parsed = PlayArea::parse(&unknown).unwrap();
        assert_eq!(parsed.area.len(), 2);
        assert_eq!(parsed.ignored.len(), 1);

        // Wrapped, a line without a box before it, one that is not a number
        let parsed =
            PlayArea::parse("alvr_play_area 1\nbox 0 0 0 1 1 1\nbox_yaw_deg 370\nbox_yaw_deg 5")
                .unwrap();
        assert!((parsed.area.boxes()[0].yaw - 10_f32.to_radians()).abs() < 1e-5);
        assert_eq!(parsed.warnings.len(), 1);
        assert!(parsed.warnings[0].contains("without a box"));
        let parsed = PlayArea::parse("alvr_play_area 1\nbox_yaw_deg 5\nbox 0 0 0 1 1 1").unwrap();
        assert_eq!(parsed.area.boxes()[0].yaw, 0.0);
        assert_eq!(parsed.warnings.len(), 1);
        assert!(PlayArea::parse("alvr_play_area 1\nbox 0 0 0 1 1 1\nbox_yaw_deg x").is_err());
    }

    #[test]
    fn uniforms_of_the_boxes() {
        let params = MarkerBoxParams {
            offset: Vec3::new(0.0, 0.15, 0.0),
            size: Vec3::new(1.2, 0.9, 1.0),
            extra_yaw: 0.0,
        };
        let area = PlayArea::test_layout(&params);
        let data = area_uniforms(Some(&area));
        let (min, max) = area.bounds();
        assert_eq!(&data[0..3], &((min + max) * 0.5).to_array());
        assert_eq!(&data[4..7], &((max - min) * 0.5).to_array());
        // First box: the dome on +Y
        let base = 8;
        assert_eq!(&data[base..base + 3], &[0.0, 0.15, 0.0]);
        assert!((data[base + 3] - 0.25).abs() < 1e-6);
        let (radius, depth) = area.boxes()[0].dome_sphere(Side::new(1, true));
        assert_eq!(data[base + 8 + 1], radius);
        assert_eq!(data[base + 16 + 1], depth);
        assert_eq!(data[base + 8], 0.0);
        assert_eq!(data[base + 12 + 1], 0.0);
        // Second box: no domes. Unused boxes are zeros
        assert_eq!(data[base + AREA_BOX_FLOAT_COUNT + 3], 0.0);
        assert!(
            data[base + 2 * AREA_BOX_FLOAT_COUNT..]
                .iter()
                .all(|&v| v == 0.0)
        );
        assert!(area_uniforms(None).iter().all(|&v| v == 0.0));
    }
}
