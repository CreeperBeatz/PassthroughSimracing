//! Outline overlay drawn over the stream: thin lines in stage space (the anchor and the box of the
//! play area), drawn into the main pass of the stream renderer, see overlay.wgsl. Pure geometry here:
//! the segments, the camera of an eye, the clipping against its near plane and the packing for the
//! uniform buffer.
//!
//! Each segment becomes a screen-facing quad of a constant width on screen (an angle), opaque, solid or
//! dashed (dashes of a fixed length in the room, so they don't crawl when the head moves).

use crate::{
    PROJECTION_NEAR, PlacedArea, RegionOverride, Side,
    play_area::{AreaBox, OTHER_AREA_AXES},
};
use alvr_common::{
    Fov,
    glam::{Mat4, Quat, Vec2, Vec3, Vec4},
};

/// Segments of a frame. The uniform buffer holds them once per eye (`OVERLAY_SLOTS`). 256: the edges
/// of 8 boxes (96) and about 10 domes drawn with arcs of 8 segments. The buffer is then 16 KB, the
/// size of a uniform block every GLES 3 device takes
pub const MAX_OVERLAY_SEGMENTS: usize = 256;
pub const OVERLAY_SLOTS: usize = 2 * MAX_OVERLAY_SEGMENTS;
/// start (xyz, width, negative: dashed), end (xyz, color packed in a float, see `pack_color`)
pub(crate) const SEGMENT_FLOAT_COUNT: usize = 8;
pub(crate) const OVERLAY_UNIFORMS_SIZE: usize = OVERLAY_SLOTS * SEGMENT_FLOAT_COUNT * 4;

/// Segments end this far in front of the near plane of the projection (meters)
const NEAR_MARGIN: f32 = 0.005;

// Colors as in the stream image (sRGB values): the shader corrects them like the stream
pub const OVERLAY_AMBER: Vec4 = Vec4::new(1.0, 0.7, 0.1, 1.0);
pub const OVERLAY_GREEN: Vec4 = Vec4::new(0.3, 1.0, 0.4, 1.0);
pub const OVERLAY_WHITE: Vec4 = Vec4::new(1.0, 1.0, 1.0, 1.0);
pub const OVERLAY_HEADING: Vec4 = Vec4::new(0.2, 0.5, 1.0, 1.0);
pub const OVERLAY_UP: Vec4 = Vec4::new(1.0, 0.3, 1.0, 1.0);
pub const OVERLAY_RIGHT: Vec4 = Vec4::new(1.0, 0.25, 0.2, 1.0);

/// Lengths of the axes of the anchor (meters)
const HEADING_LENGTH: f32 = 0.2;
const AXIS_LENGTH: f32 = 0.1;

/// Most segments of an arc of a dome (`area_outline`); fewer when the layout has many domes
const DOME_ARC_SEGMENTS: usize = 8;
/// Edges of a box inside another box by more than this are left out of the outline (meters)
const HIDDEN_EDGE_MARGIN: f32 = 0.001;

/// A line in stage space
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OverlaySegment {
    pub start: Vec3,
    pub end: Vec3,
    /// rgb as in the stream image, alpha is ignored (the lines are opaque)
    pub color: Vec4,
    /// On screen (radians)
    pub width: f32,
    /// Dashes of `DASH_PERIOD` meters along the line, see overlay.wgsl
    pub dashed: bool,
}

/// Of a dashed line: a dash and a gap, measured from its start (meters). As `DASH_PERIOD` and
/// `DASH_ON` in overlay.wgsl
pub const DASH_PERIOD: f32 = 0.04;
pub const DASH_ON: f32 = 0.6;

/// A color as one float, 8 bits per channel: r * 65536 + g * 256 + b, an integer below 2^24, which a
/// float holds exactly (see overlay.wgsl)
pub(crate) fn pack_color(color: Vec4) -> f32 {
    let [r, g, b] = (color.truncate().clamp(Vec3::ZERO, Vec3::ONE) * 255.0)
        .round()
        .to_array();
    r * 65536.0 + g * 256.0 + b
}

/// The 12 edges of a box
pub fn box_edges(region: &RegionOverride, color: Vec4, width: f32) -> [OverlaySegment; 12] {
    let corners = region.corners();
    // Corner i has bit 0 set for +X, bit 1 for +Y, bit 2 for +Z: an edge joins corners that differ in
    // one bit
    let mut edges = [(0, 0); 12];
    let mut count = 0;
    for i in 0..8 {
        for bit in [1, 2, 4] {
            if i & bit == 0 {
                edges[count] = (i, i | bit);
                count += 1;
            }
        }
    }

    edges.map(|(a, b)| OverlaySegment {
        start: corners[a],
        end: corners[b],
        color,
        width,
        dashed: false,
    })
}

/// The part of the segment from `a` to `b` (area frame) inside a box, as parameters along it (0: `a`,
/// 1: `b`). Across a face the segment runs along, it must be inside by `HIDDEN_EDGE_MARGIN`; a face it
/// crosses cuts it where it crosses. None: not inside
fn segment_inside(a: Vec3, b: Vec3, other: &AreaBox) -> Option<(f32, f32)> {
    let start = other.to_local(a);
    let direction = other.turn_back(b - a);
    let length = direction.length();
    if length <= 0.0 {
        return None;
    }
    let (mut low, mut high) = (0.0_f32, 1.0_f32);
    for k in 0..3 {
        let half = other.half[k];
        if (direction[k] / length).abs() < 1e-4 {
            if start[k].abs() >= half - HIDDEN_EDGE_MARGIN {
                return None;
            }
            continue;
        }
        let (t0, t1) = (
            (-half - start[k]) / direction[k],
            (half - start[k]) / direction[k],
        );
        low = low.max(t0.min(t1));
        high = high.min(t0.max(t1));
    }
    (low < high).then_some((low, high))
}

/// The outline of a play area, stage space: the edges of its boxes without the parts inside another
/// box, and of each dome its rim and two arcs over its top (across the side and along it). At most
/// `budget` segments: with many domes their arcs get fewer segments (down to 2), the edges come
/// first. Returns the segments and how many were left out to stay within the budget
pub fn area_outline(
    placed: &PlacedArea,
    color: Vec4,
    width: f32,
    dashed: bool,
    budget: usize,
) -> (Vec<OverlaySegment>, usize) {
    let boxes = placed.area.boxes();
    let mut lines = Vec::<(Vec3, Vec3)>::new();

    for (index, area_box) in boxes.iter().enumerate() {
        let half = area_box.half;
        for axis in 0..3 {
            let (u, v) = OTHER_AREA_AXES[axis];
            for corner in 0..4 {
                // The edge along `axis` in the frame of the box, then in the area frame
                let mut local = Vec3::ZERO;
                local[u] = if corner & 1 == 0 { -half[u] } else { half[u] };
                local[v] = if corner & 2 == 0 { -half[v] } else { half[v] };
                let (mut a, mut b) = (local, local);
                a[axis] = -half[axis];
                b[axis] = half[axis];
                let (a, b) = (area_box.to_area(a), area_box.to_area(b));
                let length = a.distance(b);
                // The parts of the edge that are not inside another box (parameters along it)
                let mut pieces = vec![(0.0, 1.0)];
                for (other_index, other) in boxes.iter().enumerate() {
                    if other_index == index {
                        continue;
                    }
                    let Some((low, high)) = segment_inside(a, b, other) else {
                        continue;
                    };
                    pieces = pieces
                        .into_iter()
                        .flat_map(|(start, end): (f32, f32)| {
                            [(start, end.min(low)), (start.max(high), end)]
                        })
                        .filter(|(start, end)| (end - start) * length > HIDDEN_EDGE_MARGIN)
                        .collect();
                }
                for (start, end) in pieces {
                    lines.push((a.lerp(b, start), a.lerp(b, end)));
                }
            }
        }
    }

    let domes = boxes
        .iter()
        .flat_map(|area_box| {
            Side::ALL
                .into_iter()
                .filter(|&side| area_box.dome_height(side) > 0.0)
                .map(move |side| (*area_box, side))
        })
        .collect::<Vec<_>>();
    let edge_count = lines.len();
    let arc_segments = [DOME_ARC_SEGMENTS, 6, 4, 3, 2]
        .into_iter()
        .find(|&segments| {
            let total = domes
                .iter()
                .map(|(area_box, side)| dome_lines(area_box, *side, segments).len())
                .sum::<usize>();
            edge_count + total <= budget
        })
        .unwrap_or(2);
    for (area_box, side) in &domes {
        lines.extend(dome_lines(area_box, *side, arc_segments));
    }

    let left_out = lines.len().saturating_sub(budget);
    let segments = lines
        .into_iter()
        .take(budget)
        .map(|(start, end)| OverlaySegment {
            start: placed.area_to_world.transform_point3(start),
            end: placed.area_to_world.transform_point3(end),
            color,
            width,
            dashed,
        })
        .collect();

    (segments, left_out)
}

// The lines of the dome of a side (area frame): its rim, the arc across the side and the one along it
// (over the straight top of a long side), `segments` per half circle
fn dome_lines(area_box: &AreaBox, side: Side, segments: usize) -> Vec<(Vec3, Vec3)> {
    // Drawn in the frame of the box, then turned and moved into the area frame
    let local_box = AreaBox {
        center: Vec3::ZERO,
        yaw: 0.0,
        ..*area_box
    };
    local_dome_lines(&local_box, side, segments)
        .into_iter()
        .map(|(a, b)| (area_box.to_area(a), area_box.to_area(b)))
        .collect()
}

// `dome_lines` of a box that is not turned
fn local_dome_lines(area_box: &AreaBox, side: Side, segments: usize) -> Vec<(Vec3, Vec3)> {
    let (radius, depth) = area_box.dome_sphere(side);
    if radius <= 0.0 {
        return vec![];
    }
    let (u, v) = OTHER_AREA_AXES[side.axis];
    let (long, short) = if area_box.half[u] >= area_box.half[v] {
        (u, v)
    } else {
        (v, u)
    };
    let mut long_dir = Vec3::ZERO;
    long_dir[long] = 1.0;
    let mut short_dir = Vec3::ZERO;
    short_dir[short] = 1.0;
    let normal = side.normal();
    let rim = area_box.dome_rim(side);
    // A side square within 0.1 mm: no straight parts
    let axis_half = area_box.half[long] - rim;
    let axis_half = if axis_half < 1e-4 { 0.0 } else { axis_half };
    let center = area_box.side_center(side);
    // From the plane of the side up to the top, on the circle of the capsule
    let start_angle = (depth / radius).clamp(-1.0, 1.0).asin();
    let half_turn = std::f32::consts::PI;

    let mut lines = vec![];
    let mut polyline = |points: &[Vec3]| {
        lines.extend(points.windows(2).map(|pair| (pair[0], pair[1])));
    };
    let arc = |from: f32, to: f32, count: usize, point: &dyn Fn(f32) -> Vec3| {
        (0..=count)
            .map(|i| point(from + (to - from) * i as f32 / count as f32))
            .collect::<Vec<_>>()
    };

    // The rim: a half circle around each end of the axis, and the straight parts between them
    for (end, from) in [(axis_half, -0.5 * half_turn), (-axis_half, 0.5 * half_turn)] {
        let end_center = center + long_dir * end;
        polyline(&arc(from, from + half_turn, segments, &|angle| {
            end_center + (long_dir * angle.cos() + short_dir * angle.sin()) * rim
        }));
    }
    if axis_half > 0.0 {
        for sign in [-1.0, 1.0] {
            polyline(&[
                center + short_dir * sign * rim + long_dir * axis_half,
                center + short_dir * sign * rim - long_dir * axis_half,
            ]);
        }
    }

    // Across the side, through its middle
    let across = arc(start_angle, half_turn - start_angle, segments, &|angle| {
        center + short_dir * radius * angle.cos() + normal * (radius * angle.sin() - depth)
    });
    polyline(&across);

    // Along the side: a quarter at each end and the straight top between them
    let quarter = segments.div_ceil(2);
    let mut along = arc(start_angle, 0.5 * half_turn, quarter, &|angle| {
        center
            + long_dir * (axis_half + radius * angle.cos())
            + normal * (radius * angle.sin() - depth)
    });
    let mut back = arc(
        0.5 * half_turn,
        half_turn - start_angle,
        quarter,
        &|angle| {
            center
                + long_dir * (-axis_half + radius * angle.cos())
                + normal * (radius * angle.sin() - depth)
        },
    );
    // Joined by the straight top; on a square side both quarters end at the same top point
    if axis_half <= 0.0 {
        back.remove(0);
    }
    along.append(&mut back);
    polyline(&along);

    lines
}

/// The outline of a code: a rectangle of `size` (width, height) in its plane, centered at `position`.
/// Marker axes: +X to the right of the code, +Y to its top
pub fn code_outline(
    position: Vec3,
    orientation: Quat,
    size: Vec2,
    color: Vec4,
    width: f32,
) -> [OverlaySegment; 4] {
    let half = size * 0.5;
    let corner = |x: f32, y: f32| position + orientation * Vec3::new(x * half.x, y * half.y, 0.0);
    let corners = [
        corner(-1.0, -1.0),
        corner(1.0, -1.0),
        corner(1.0, 1.0),
        corner(-1.0, 1.0),
    ];

    std::array::from_fn(|i| OverlaySegment {
        start: corners[i],
        end: corners[(i + 1) % 4],
        color,
        width,
        dashed: false,
    })
}

/// The axes of the frame of a box at the anchor: the heading (-Z of `rotation`, 20 cm), up (+Y, 10
/// cm) and to the right of the heading (+X, 10 cm)
pub fn anchor_axes(position: Vec3, rotation: Quat, width: f32) -> [OverlaySegment; 3] {
    [
        (Vec3::NEG_Z * HEADING_LENGTH, OVERLAY_HEADING),
        (Vec3::Y * AXIS_LENGTH, OVERLAY_UP),
        (Vec3::X * AXIS_LENGTH, OVERLAY_RIGHT),
    ]
    .map(|(axis, color)| OverlaySegment {
        start: position,
        end: position + rotation * axis,
        color,
        width,
        dashed: false,
    })
}

/// Width of a line from hundredths of a degree
pub fn overlay_width(hundredths_deg: f32) -> f32 {
    (hundredths_deg / 100.0).to_radians()
}

/// The camera of one eye
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OverlayEye {
    /// Stage to view
    pub view: Mat4,
    pub view_proj: Mat4,
    /// Size of the target
    pub viewport: Vec2,
    /// Of the target at the center of the view, to turn the width of a line into pixels
    pub pixels_per_radian: f32,
}

impl OverlayEye {
    /// The camera at `eye_position` turned like `orientation`, with the projection of the stream
    /// (`projection_from_fov`)
    pub fn new(eye_position: Vec3, orientation: Quat, fov: Fov, viewport: Vec2) -> Self {
        let view = Mat4::from_rotation_translation(orientation, eye_position).inverse();

        Self {
            view,
            view_proj: crate::projection_from_fov(fov) * view,
            viewport,
            pixels_per_radian: viewport.x / (fov.right.tan() - fov.left.tan()),
        }
    }

    /// The part of a segment in front of the near plane of the projection (plus a few mm). None:
    /// all of it is behind
    pub fn clip(&self, segment: &OverlaySegment) -> Option<OverlaySegment> {
        let near = PROJECTION_NEAR + NEAR_MARGIN;
        // Distance in front of the eye
        let start = -self.view.transform_point3(segment.start).z;
        let end = -self.view.transform_point3(segment.end).z;
        if start < near && end < near {
            return None;
        }
        // The view is affine: the point at the near plane lies on the segment in stage space too
        let at_near = || {
            segment
                .start
                .lerp(segment.end, (near - start) / (end - start))
        };

        Some(OverlaySegment {
            start: if start < near {
                at_near()
            } else {
                segment.start
            },
            end: if end < near { at_near() } else { segment.end },
            ..*segment
        })
    }
}

/// Appends the segments of one eye to the content of the uniform buffer, see overlay.wgsl. Returns
/// how many were added (at most `MAX_OVERLAY_SEGMENTS`)
pub(crate) fn pack_segments(
    eye: &OverlayEye,
    segments: &[OverlaySegment],
    out: &mut Vec<f32>,
) -> u32 {
    let mut count = 0;
    for segment in segments
        .iter()
        .filter_map(|segment| eye.clip(segment))
        .take(MAX_OVERLAY_SEGMENTS)
    {
        let width = if segment.dashed {
            -segment.width
        } else {
            segment.width
        };
        out.extend(segment.start.extend(width).to_array());
        out.extend(segment.end.extend(pack_color(segment.color)).to_array());
        count += 1;
    }

    count
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fov() -> Fov {
        let a = std::f32::consts::FRAC_PI_4;
        Fov {
            left: -a,
            right: a,
            up: a,
            down: -a,
        }
    }

    fn eye() -> OverlayEye {
        OverlayEye::new(Vec3::ZERO, Quat::IDENTITY, fov(), Vec2::splat(100.0))
    }

    fn segment(start: Vec3, end: Vec3) -> OverlaySegment {
        OverlaySegment {
            start,
            end,
            color: OVERLAY_WHITE,
            width: 0.01,
            dashed: false,
        }
    }

    #[test]
    fn box_has_twelve_edges_along_its_axes() {
        let region = RegionOverride {
            volume_to_world: Mat4::from_rotation_translation(
                Quat::from_rotation_y(0.5),
                Vec3::new(1.0, 2.0, 3.0),
            ),
            half_extents: Vec3::new(0.6, 0.45, 0.5),
        };
        let edges = box_edges(&region, OVERLAY_WHITE, 0.01);
        let to_box = region.volume_to_world.inverse();
        let mut lengths = edges
            .iter()
            .map(|edge| {
                let a = to_box.transform_point3(edge.start);
                let b = to_box.transform_point3(edge.end);
                // Each edge runs along one axis of the box, its ends on the corners
                let changed = (b - a).abs().cmpgt(Vec3::splat(1e-4));
                assert_eq!(changed.bitmask().count_ones(), 1, "{a} {b}");
                assert!((a.abs() - region.half_extents).abs().max_element() < 1e-4);
                (b - a).length()
            })
            .collect::<Vec<_>>();
        lengths.sort_by(f32::total_cmp);
        let expected = [0.9, 0.9, 0.9, 0.9, 1.0, 1.0, 1.0, 1.0, 1.2, 1.2, 1.2, 1.2];
        for (length, expected) in lengths.iter().zip(expected) {
            assert!((length - expected).abs() < 1e-4, "{lengths:?}");
        }
        // No edge twice
        for (i, a) in edges.iter().enumerate() {
            for b in &edges[i + 1..] {
                assert!(a.start.distance(b.start) + a.end.distance(b.end) > 1e-4);
            }
        }
    }

    #[test]
    fn code_outline_lies_in_the_plane_of_the_code() {
        // A code standing up, facing +X
        let orientation = Quat::from_rotation_y(std::f32::consts::FRAC_PI_2);
        let center = Vec3::new(0.5, 1.0, -0.3);
        let outline = code_outline(center, orientation, Vec2::splat(0.144), OVERLAY_GREEN, 0.01);
        for side in &outline {
            assert!((side.start.x - center.x).abs() < 1e-6);
            assert!((side.start.distance(side.end) - 0.144).abs() < 1e-5);
        }
        // Closed
        for i in 0..4 {
            assert_eq!(outline[i].end, outline[(i + 1) % 4].start);
        }
    }

    #[test]
    fn anchor_axes_follow_the_frame_of_the_box() {
        // Heading towards +X: yaw -90 degrees
        let rotation = Quat::from_rotation_y(-std::f32::consts::FRAC_PI_2);
        let [heading, up, right] = anchor_axes(Vec3::ZERO, rotation, 0.01);
        assert!(heading.end.distance(Vec3::new(0.2, 0.0, 0.0)) < 1e-5);
        assert!(up.end.distance(Vec3::new(0.0, 0.1, 0.0)) < 1e-5);
        assert!(right.end.distance(Vec3::new(0.0, 0.0, 0.1)) < 1e-5);
        assert!(
            heading.color != up.color && up.color != right.color && heading.color != right.color
        );
    }

    #[test]
    fn segments_are_clipped_at_the_near_plane() {
        let eye = eye();
        let near = PROJECTION_NEAR + NEAR_MARGIN;

        // In front: unchanged
        let front = segment(Vec3::new(-1.0, 0.0, -1.0), Vec3::new(1.0, 0.0, -2.0));
        assert_eq!(eye.clip(&front), Some(front));

        // Behind: dropped
        let behind = segment(Vec3::new(-1.0, 0.0, 1.0), Vec3::new(1.0, 0.0, 0.05));
        assert_eq!(eye.clip(&behind), None);

        // Through the head (the eye sits inside the box): the end behind moves to the near plane
        let through = segment(Vec3::new(0.2, 0.0, -1.0), Vec3::new(0.2, 0.0, 1.0));
        let clipped = eye.clip(&through).unwrap();
        assert_eq!(clipped.start, through.start);
        assert!((clipped.end.z + near).abs() < 1e-5);
        assert!((clipped.end.x - 0.2).abs() < 1e-6);
        // Either way round
        let reversed = segment(through.end, through.start);
        let clipped = eye.clip(&reversed).unwrap();
        assert!((clipped.start.z + near).abs() < 1e-5);
        assert_eq!(clipped.end, through.start);

        // The clipped end projects in front of the near plane of the projection
        let clip_space = eye.view_proj * clipped.start.extend(1.0);
        assert!(clip_space.w > 0.0 && clip_space.z > 0.0 && clip_space.z < clip_space.w);
    }

    #[test]
    fn clipping_uses_the_pose_of_the_eye() {
        // Eye at 1 m height, turned to look along +X
        let eye = OverlayEye::new(
            Vec3::new(0.0, 1.0, 0.0),
            Quat::from_rotation_y(-std::f32::consts::FRAC_PI_2),
            fov(),
            Vec2::splat(100.0),
        );
        // In front of it
        let ahead = segment(Vec3::new(1.0, 1.0, -0.5), Vec3::new(1.0, 1.0, 0.5));
        assert_eq!(eye.clip(&ahead), Some(ahead));
        // Behind it (in front of an eye looking along -Z)
        let behind = segment(Vec3::new(-1.0, 1.0, -0.5), Vec3::new(-0.5, 1.0, -2.0));
        assert_eq!(eye.clip(&behind), None);
    }

    #[test]
    fn eye_projects_like_the_stream() {
        let eye = eye();
        // A point straight ahead is at the center
        let center = eye.view_proj * Vec4::new(0.0, 0.0, -2.0, 1.0);
        assert!((center.x / center.w).abs() < 1e-6 && (center.y / center.w).abs() < 1e-6);
        // A point on the right edge of the fov is at the right edge
        let right = eye.view_proj * Vec4::new(2.0, 0.0, -2.0, 1.0);
        assert!((right.x / right.w - 1.0).abs() < 1e-5);
        // 90 degrees over 100 pixels: tangents -1..1
        assert!((eye.pixels_per_radian - 50.0).abs() < 1e-4);
    }

    #[test]
    fn packing_holds_one_eye() {
        let eye = eye();
        let segments = [
            segment(Vec3::new(0.0, 0.0, -1.0), Vec3::new(1.0, 0.0, -1.0)),
            segment(Vec3::new(0.0, 0.0, 1.0), Vec3::new(1.0, 0.0, 1.0)),
        ];
        let mut out = vec![];
        assert_eq!(pack_segments(&eye, &segments, &mut out), 1);
        assert_eq!(out.len(), SEGMENT_FLOAT_COUNT);
        assert_eq!(out[3], 0.01);
        assert_eq!(out[7], pack_color(OVERLAY_WHITE));
        // Dashed: a negative width
        let dashed = OverlaySegment {
            dashed: true,
            ..segments[0]
        };
        out.clear();
        pack_segments(&eye, &[dashed], &mut out);
        assert_eq!(out[3], -0.01);

        let many = vec![segments[0]; MAX_OVERLAY_SEGMENTS + 5];
        out.clear();
        assert_eq!(
            pack_segments(&eye, &many, &mut out) as usize,
            MAX_OVERLAY_SEGMENTS
        );
        assert!(out.len() * 4 * 2 <= OVERLAY_UNIFORMS_SIZE);
    }

    fn placed(area: crate::PlayArea) -> PlacedArea {
        PlacedArea {
            area_to_world: Mat4::from_rotation_translation(
                Quat::from_rotation_y(0.7),
                Vec3::new(0.3, 1.0, -0.4),
            ),
            area,
        }
    }

    fn params() -> crate::MarkerBoxParams {
        crate::MarkerBoxParams {
            offset: Vec3::new(0.0, 0.15, 0.0),
            size: Vec3::new(1.2, 0.9, 1.0),
            extra_yaw: 0.0,
        }
    }

    #[test]
    fn outline_of_one_box_is_its_edges() {
        let placed = placed(crate::PlayArea::from_marker_box(&params()));
        let (outline, left_out) = area_outline(&placed, OVERLAY_WHITE, 0.01, false, 100);
        assert_eq!(left_out, 0);
        let edges = box_edges(&placed.box_region(0), OVERLAY_WHITE, 0.01);
        assert_eq!(outline.len(), 12);
        for edge in &edges {
            assert!(
                outline.iter().any(|line| {
                    let same = line.start.distance(edge.start) + line.end.distance(edge.end);
                    let reversed = line.start.distance(edge.end) + line.end.distance(edge.start);
                    same.min(reversed) < 1e-5
                }),
                "{edge:?}"
            );
        }
    }

    #[test]
    fn edges_inside_another_box_are_left_out() {
        // A box through the middle of the +X side of a cube: its 4 edges along X cross the cube
        let area = crate::PlayArea::parse(
            "alvr_play_area 1
box 0 0 0 0.5 0.5 0.5
box 0.6 0 0 0.4 0.1 0.1",
        )
        .unwrap()
        .area;
        let placed = placed(area);
        let to_area = placed.area_to_world.inverse();
        let (outline, _) = area_outline(&placed, OVERLAY_WHITE, 0.01, false, 100);
        let mut length = 0.0;
        for line in &outline {
            let a = to_area.transform_point3(line.start);
            let b = to_area.transform_point3(line.end);
            length += a.distance(b);
            // No part strictly inside another box
            for t in [0.01, 0.5, 0.99] {
                let point = a.lerp(b, t);
                let inside = placed
                    .area
                    .boxes()
                    .iter()
                    .filter(|area_box| {
                        ((point - area_box.center).abs() - area_box.half).max_element() < -0.002
                    })
                    .count();
                assert_eq!(inside, 0, "{point}");
            }
        }
        // All 12 edges of the cube (1 m each); of the other box the parts of its 4 edges along X
        // outside the cube (0.5 m) and the 4 edges of its outer end (0.2 m). The 4 edges of its inner
        // end are inside the cube
        let expected = 12.0 + 4.0 * 0.5 + 4.0 * 0.2;
        assert!((length - expected).abs() < 1e-4, "{length} vs {expected}");
    }

    // The whole layout of `edges_inside_another_box_are_left_out` turned about the code: the boxes
    // turned by their own yaw, the same outline
    #[test]
    fn edges_of_turned_boxes_inside_another_box_are_left_out() {
        let mut area = crate::PlayArea::parse(
            "alvr_play_area 1
box 0 0 0 0.5 0.5 0.5
box 0.6 0 0 0.4 0.1 0.1",
        )
        .unwrap()
        .area;
        let yaw = 0.5_f32;
        for index in 0..2 {
            let center = area.boxes()[index].center;
            area.move_box(index, Quat::from_rotation_y(yaw) * center - center);
            area.turn_box(index, yaw);
        }
        let placed = placed(area);
        let to_area = placed.area_to_world.inverse();
        let (outline, _) = area_outline(&placed, OVERLAY_WHITE, 0.01, false, 100);
        let mut length = 0.0;
        for line in &outline {
            let a = to_area.transform_point3(line.start);
            let b = to_area.transform_point3(line.end);
            length += a.distance(b);
            for t in [0.01, 0.5, 0.99] {
                let point = a.lerp(b, t);
                let inside = placed
                    .area
                    .boxes()
                    .iter()
                    .filter(|area_box| {
                        (area_box.to_local(point).abs() - area_box.half).max_element() < -0.002
                    })
                    .count();
                assert_eq!(inside, 0, "{point}");
                // On the surface of a box
                assert!(placed.area.signed_distance(point, f32::INFINITY).abs() < 1e-4);
            }
        }
        let expected = 12.0 + 4.0 * 0.5 + 4.0 * 0.2;
        assert!((length - expected).abs() < 1e-4, "{length} vs {expected}");

        // One turned box: its outline is the edges of its region
        let single = self::placed(crate::PlayArea::single(crate::AreaBox {
            yaw: -0.8,
            ..crate::AreaBox::new(Vec3::new(0.2, 0.1, -0.3), Vec3::new(0.4, 0.3, 0.2))
        }));
        let (outline, _) = area_outline(&single, OVERLAY_WHITE, 0.01, false, 100);
        assert_eq!(outline.len(), 12);
        for edge in &box_edges(&single.box_region(0), OVERLAY_WHITE, 0.01) {
            assert!(outline.iter().any(|line| {
                let same = line.start.distance(edge.start) + line.end.distance(edge.end);
                let reversed = line.start.distance(edge.end) + line.end.distance(edge.start);
                same.min(reversed) < 1e-5
            }));
        }
    }

    #[test]
    fn dome_lines_lie_on_the_dome() {
        let mut area = crate::PlayArea::test_layout(&params());
        // A square side too: a spherical cap
        let index = area.add_box(1, Side::new(2, false)).unwrap();
        let area_box = area.boxes()[index];
        assert!((area_box.half.x - area_box.half.y).abs() < 1e-6);
        area.set_dome(index, Side::new(2, false), 0.06);
        let placed = placed(area);
        let to_area = placed.area_to_world.inverse();
        let (outline, left_out) = area_outline(&placed, OVERLAY_GREEN, 0.01, true, 249);
        assert_eq!(left_out, 0);
        assert!(
            outline
                .iter()
                .all(|line| line.dashed && line.color == OVERLAY_GREEN)
        );
        let edges = outline
            .iter()
            .filter(|line| {
                let d = to_area.transform_vector3(line.end - line.start).abs();
                // Along one axis: the edges and the straight parts of a dome
                (d.cmpgt(Vec3::splat(1e-5))).bitmask().count_ones() == 1
            })
            .count();
        // Each dome has 32 lines that are not along an axis: the two half circles of its rim (16),
        // the arc across (8) and the two quarters along (8). The long side has 3 straight lines more
        // (2 of the rim, the top), counted with the edges
        assert_eq!(outline.len() - edges, 2 * 32);
        for line in &outline {
            for point in [line.start, line.end] {
                let local = to_area.transform_point3(point);
                // On the surface of the area, or on an edge hidden only by a dome
                let distance = placed.area.signed_distance(local, f32::INFINITY);
                assert!(distance.abs() < 1e-4, "{local}: {distance}");
            }
        }

        // A turned box with domes: the lines on its surface too
        let mut turned = crate::PlayArea::single(crate::AreaBox {
            yaw: 1.1,
            ..crate::AreaBox::new(Vec3::new(0.1, 0.0, -0.2), Vec3::new(0.4, 0.2, 0.3))
        });
        turned.set_dome(0, Side::new(0, true), 0.15);
        turned.set_dome(0, Side::new(1, true), 0.1);
        let placed = self::placed(turned);
        let (outline, left_out) = area_outline(&placed, OVERLAY_GREEN, 0.01, false, 249);
        assert_eq!(left_out, 0);
        // Both sides are long: 3 straight lines each
        assert_eq!(outline.len(), 12 + 2 * (32 + 3));
        for line in &outline {
            for point in [line.start, line.end] {
                let local = to_area.transform_point3(point);
                let distance = placed.area.signed_distance(local, f32::INFINITY);
                assert!(distance.abs() < 1e-4, "{local}: {distance}");
            }
        }
    }

    #[test]
    fn outline_stays_within_the_budget() {
        let placed = placed(crate::PlayArea::stress_layout(&params()));
        let (outline, left_out) = area_outline(&placed, OVERLAY_WHITE, 0.01, false, 249);
        assert_eq!(outline.len(), 249);
        assert!(left_out > 0);
        // The edges come first, all of them
        let to_area = placed.area_to_world.inverse();
        assert!(outline[..96].iter().all(|line| {
            let d = to_area.transform_vector3(line.end - line.start).abs();
            d.cmpgt(Vec3::splat(1e-5)).bitmask().count_ones() == 1
        }));
        // The test layout fits with full arcs
        let placed = placed_test();
        let (_, left_out) = area_outline(&placed, OVERLAY_WHITE, 0.01, false, 249);
        assert_eq!(left_out, 0);
    }

    fn placed_test() -> PlacedArea {
        placed(crate::PlayArea::test_layout(&params()))
    }

    #[test]
    fn width_in_hundredths_of_a_degree() {
        assert!((overlay_width(12.0) - 0.12_f32.to_radians()).abs() < 1e-9);
    }
}
