//! Outline overlay drawn over the stream: thin lines in stage space (the anchor and the box of the
//! play area), drawn into the main pass of the stream renderer, see overlay.wgsl. Pure geometry here:
//! the segments, the camera of an eye, the clipping against its near plane and the packing for the
//! uniform buffer.
//!
//! Each segment becomes a screen-facing quad of a constant width on screen (an angle), opaque.

use crate::{PROJECTION_NEAR, RegionOverride};
use alvr_common::{
    Fov,
    glam::{Mat4, Quat, Vec2, Vec3, Vec4},
};

/// Segments of a frame. The uniform buffer holds them once per eye (`OVERLAY_SLOTS`)
pub const MAX_OVERLAY_SEGMENTS: usize = 128;
pub const OVERLAY_SLOTS: usize = 2 * MAX_OVERLAY_SEGMENTS;
/// start (xyz, width), end (xyz, unused), color
pub(crate) const SEGMENT_FLOAT_COUNT: usize = 12;
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

/// A line in stage space
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OverlaySegment {
    pub start: Vec3,
    pub end: Vec3,
    /// rgb as in the stream image, alpha is ignored (the lines are opaque)
    pub color: Vec4,
    /// On screen (radians)
    pub width: f32,
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
    })
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
        out.extend(segment.start.extend(segment.width).to_array());
        out.extend(segment.end.extend(0.0).to_array());
        out.extend(segment.color.to_array());
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

        let many = vec![segments[0]; MAX_OVERLAY_SEGMENTS + 5];
        out.clear();
        assert_eq!(
            pack_segments(&eye, &many, &mut out) as usize,
            MAX_OVERLAY_SEGMENTS
        );
        assert!(out.len() * 4 * 2 <= OVERLAY_UNIFORMS_SIZE);
    }

    #[test]
    fn width_in_hundredths_of_a_degree() {
        assert!((overlay_width(12.0) - 0.12_f32.to_radians()).abs() < 1e-9);
    }
}
