//! The green-screen key (`debug.alvr.green_boxes`, docs/green-screen.md; the client side is
//! `green_screen.rs` of alvr_client_openxr). The rig PC shows a pure green rectangle on its monitor,
//! behind the top of the steering wheel's rim. A play-area box drawn as a thin slab on it is the zone:
//! there the depth mask shows the room whatever the depth says (`green_coverage` in depth_common.wgsl),
//! the bottom passthrough layer is keyed by a colour LUT that makes "monitor green" transparent
//! (`green_key_lut`), and a projection layer under it shows the game, cropped to the zone's rectangle in
//! each view (`zone_view_rect`). So the rim, hands and arms in front of the green are cut by the
//! compositor's own per-pixel colour key, at display rate.
//!
//! Pure maths, tested on the PC: the LUT rule, the zone's rectangle in a view, and what the passthrough
//! camera sees inside the zone (`green_camera_stats`, for the `green:` line).

use crate::{
    COLOR_LUT_RESOLUTION, CameraView, RegionOverride,
    depth_mask::{MAX_GREEN_WINDOWS, crop_view, ray_crosses_box},
};
use alvr_common::{
    Fov, ViewParams,
    glam::{Mat4, UVec2, Vec2, Vec3},
};
use std::fmt::Write;

/// Bytes of a colour LUT: RGBA, `COLOR_LUT_RESOLUTION` texels per channel, indexed `[b][g][r]`
pub const GREEN_LUT_SIZE: usize = COLOR_LUT_RESOLUTION.pow(3) * 4;
/// Pixels of the camera image looked at per learner pass, at most about this many
pub const GREEN_CAMERA_SAMPLES: u32 = 300;
/// A corner of a zone box nearer to the eye plane than this (or behind it): the whole view (m)
const ZONE_NEAR: f32 = 0.05;

/// `debug.alvr.green_floor`, `green_ratio`, `green_delta`, `green_soft`: which camera colours are
/// "monitor green". For a colour r, g, b (0..255) and m = max(r, b):
/// s = min((g - floor) / soft, (g - ratio / 100 x m - delta) / soft); keyed (transparent) by
/// 1 - clamp(s, 0, 1). So green must be bright enough (floor) and clearly above red and blue (ratio,
/// delta); soft is the width of the ramp
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GreenKeyParams {
    /// 0..255, default 70
    pub floor: i32,
    /// Hundredths, 0..1000, default 135
    pub ratio: i32,
    /// -255..255, default 25
    pub delta: i32,
    /// 1..255, default 24
    pub soft: i32,
}

impl Default for GreenKeyParams {
    fn default() -> Self {
        Self::from_values(None, None, None, None)
    }
}

impl GreenKeyParams {
    /// From the property values, None: unset. Clamped to the ranges above
    pub fn from_values(
        floor: Option<i64>,
        ratio: Option<i64>,
        delta: Option<i64>,
        soft: Option<i64>,
    ) -> Self {
        Self {
            floor: floor.unwrap_or(70).clamp(0, 255) as i32,
            ratio: ratio.unwrap_or(135).clamp(0, 1000) as i32,
            delta: delta.unwrap_or(25).clamp(-255, 255) as i32,
            soft: soft.unwrap_or(24).clamp(1, 255) as i32,
        }
    }

    /// s of the rule for a colour (0..255 per channel): 1 or more is green, 0 or less is not
    pub fn strength(&self, [r, g, b]: [f32; 3]) -> f32 {
        let m = r.max(b);
        let soft = self.soft.max(1) as f32;
        let by_floor = (g - self.floor as f32) / soft;
        let by_ratio = (g - self.ratio as f32 / 100.0 * m - self.delta as f32) / soft;

        by_floor.min(by_ratio)
    }

    /// The LUT's alpha for a colour: 0 keyed (green), 1 kept
    pub fn alpha(&self, rgb: [f32; 3]) -> f32 {
        1.0 - self.strength(rgb).clamp(0.0, 1.0)
    }

    /// For the `green:` line
    pub fn text(&self) -> String {
        format!(
            "floor {} ratio {:.2} delta {} soft {}",
            self.floor,
            self.ratio as f32 / 100.0,
            self.delta,
            self.soft
        )
    }
}

/// The colour a LUT texel stands for (0..255), as the identity grid of `ColorModel::update_lut`
fn texel_channel(index: usize) -> u8 {
    (index * 255 / (COLOR_LUT_RESOLUTION - 1)) as u8
}

/// Fill `lut` (`GREEN_LUT_SIZE` bytes, `[b][g][r]`) for the base passthrough layer: RGB the identity,
/// alpha 0 for monitor green and 255 for everything else, by the rule of `GreenKeyParams` at the colour
/// of each texel. Nothing is allocated
pub fn fill_green_key_lut(params: &GreenKeyParams, lut: &mut [u8]) {
    for (index, texel) in lut.chunks_exact_mut(4).enumerate() {
        let r = texel_channel(index % COLOR_LUT_RESOLUTION);
        let g = texel_channel(index / COLOR_LUT_RESOLUTION % COLOR_LUT_RESOLUTION);
        let b = texel_channel(index / (COLOR_LUT_RESOLUTION * COLOR_LUT_RESOLUTION));
        let alpha = params.alpha([f32::from(r), f32::from(g), f32::from(b)]);
        texel.copy_from_slice(&[r, g, b, (alpha * 255.0).round() as u8]);
    }
}

/// `fill_green_key_lut` into a new table
pub fn green_key_lut(params: &GreenKeyParams) -> Vec<u8> {
    let mut lut = vec![0; GREEN_LUT_SIZE];
    fill_green_key_lut(params, &mut lut);
    lut
}

/// Where the zone's boxes are in a view
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ZoneRect {
    /// Entirely beside the view
    Outside,
    /// A corner is behind the eye or too close to its plane: the whole view, to be safe
    Full,
    /// Bounding rectangle in view uv (origin top-left), clamped to the view: min, max
    Part(Vec2, Vec2),
}

impl ZoneRect {
    pub fn is_visible(&self) -> bool {
        !matches!(self, Self::Outside)
    }

    /// For the `green:` line
    pub fn text(&self) -> String {
        match self {
            Self::Outside => "outside".into(),
            Self::Full => "full (a corner near or behind the eye)".into(),
            Self::Part(min, max) => {
                format!("u {:.3}-{:.3} v {:.3}-{:.3}", min.x, max.x, min.y, max.y)
            }
        }
    }
}

/// The bounding rectangle of the boxes' 8 corners each, projected with the pose and fov of a view (the
/// one the game layers are submitted with), clamped to the view
pub fn zone_view_rect(view: &ViewParams, boxes: &[RegionOverride]) -> ZoneRect {
    let tan_min = Vec2::new(view.fov.left.tan(), view.fov.down.tan());
    let tan_max = Vec2::new(view.fov.right.tan(), view.fov.up.tan());
    let world_to_view = view.pose.orientation.inverse();
    let mut min = Vec2::MAX;
    let mut max = Vec2::MIN;
    for region in boxes {
        for corner in region.corners() {
            let local = world_to_view * (corner - view.pose.position);
            let depth = -local.z;
            if depth.is_nan() || depth < ZONE_NEAR {
                return ZoneRect::Full;
            }
            let uv = (local.truncate() / depth - tan_min) / (tan_max - tan_min);
            let uv = Vec2::new(uv.x, 1.0 - uv.y);
            min = min.min(uv);
            max = max.max(uv);
        }
    }
    if !(min.cmplt(Vec2::ONE).all() && max.cmpgt(Vec2::ZERO).all()) {
        return ZoneRect::Outside;
    }

    ZoneRect::Part(
        min.clamp(Vec2::ZERO, Vec2::ONE),
        max.clamp(Vec2::ZERO, Vec2::ONE),
    )
}

/// The part of a view's image the under layer shows (pixel offset, size, and the fov of exactly these
/// pixels, as `crop_view`): the zone's rectangle grown by `pad` (view uv) each side; the whole view for
/// `Full`; one pixel in a corner for `Outside` (a projection layer needs both views)
pub fn green_under_crop(
    fov: Fov,
    resolution: UVec2,
    rect: ZoneRect,
    pad: f32,
    top_origin: bool,
) -> (UVec2, UVec2, Fov) {
    match rect {
        ZoneRect::Full => (UVec2::ZERO, resolution, fov),
        ZoneRect::Outside => crop_view(fov, resolution, (Vec2::ZERO, Vec2::ZERO), 0.0, top_origin),
        ZoneRect::Part(min, max) => crop_view(fov, resolution, (min, max), pad, top_origin),
    }
}

/// What the passthrough camera sees inside the zone in one learner pass, for the `green:` line
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct GreenCameraStats {
    /// Some corner of a box is in front of the camera and the boxes' rectangle overlaps the image
    pub in_image: bool,
    /// Pixels looked at whose ray crosses a zone box, and of them those the rule calls green (s >= 1)
    /// and those in between (0 < s < 1)
    pub pixels: u32,
    pub green: u32,
    pub between: u32,
    /// 10th percentile, median and 90th percentile of r, g, b (`[channel][percentile]`) of the green
    /// pixels and of the rest, None: no such pixel
    pub green_rgb: Option<[[u8; 3]; 3]>,
    pub rest_rgb: Option<[[u8; 3]; 3]>,
}

impl GreenCameraStats {
    /// For the `green:` line
    pub fn text(&self) -> String {
        if !self.in_image {
            return "the zone is not in the camera image".into();
        }
        if self.pixels == 0 {
            return "no camera pixel sees through the zone".into();
        }
        let share = |count: u32| count as f32 / self.pixels as f32;
        let mut text = format!(
            "{} px, green {:.2}, between {:.2}",
            self.pixels,
            share(self.green),
            share(self.between)
        );
        for (name, rgb) in [("green", self.green_rgb), ("rest", self.rest_rgb)] {
            if let Some(rgb) = rgb {
                let _ = write!(text, ", {name} p10/50/90");
                for (channel, [p10, p50, p90]) in ["r", "g", "b"].iter().zip(rgb) {
                    let _ = write!(text, " {channel} {p10}/{p50}/{p90}");
                }
            }
        }
        text
    }
}

/// 10th percentile, median, 90th percentile of a histogram of `count` values
fn percentiles(histogram: &[u32; 256], count: u32) -> [u8; 3] {
    [0.1, 0.5, 0.9].map(|p| {
        let rank = ((p * count as f32).ceil() as u32).clamp(1, count.max(1));
        let mut seen = 0;
        for (value, &n) in histogram.iter().enumerate() {
            seen += n;
            if seen >= rank {
                return value as u8;
            }
        }
        255
    })
}

/// What the camera sees inside the zone: the pixels whose ray (from the camera) crosses a box, on a
/// grid over the boxes' bounding rectangle in the image so that at most about `max_samples` are looked
/// at, classified by the rule of `params` on their own colour. `rgb`: the colour of a pixel. Only the
/// first `MAX_GREEN_WINDOWS` boxes count. Nothing is allocated
pub fn green_camera_stats(
    view: &CameraView,
    width: u32,
    height: u32,
    boxes: &[RegionOverride],
    params: &GreenKeyParams,
    max_samples: u32,
    rgb: impl Fn(u32, u32) -> [u8; 3],
) -> GreenCameraStats {
    let mut stats = GreenCameraStats::default();
    if width == 0 || height == 0 {
        return stats;
    }
    let boxes = &boxes[..boxes.len().min(MAX_GREEN_WINDOWS)];
    let mut frames = [(Mat4::IDENTITY, Vec3::ZERO); MAX_GREEN_WINDOWS];
    let mut min = Vec2::MAX;
    let mut max = Vec2::MIN;
    for (frame, region) in frames.iter_mut().zip(boxes) {
        *frame = (region.volume_to_world.inverse(), region.half_extents);
        for corner in region.corners() {
            match view.project(corner) {
                Some((pixel, _)) => {
                    min = min.min(pixel);
                    max = max.max(pixel);
                }
                // Behind the camera: anywhere in the image
                None => {
                    min = Vec2::ZERO;
                    max = Vec2::new(width as f32, height as f32);
                }
            }
        }
    }
    let size = Vec2::new(width as f32, height as f32);
    let min = min.max(Vec2::ZERO).min(size);
    let max = max.min(size).max(Vec2::ZERO);
    if boxes.is_empty() || !min.cmplt(max).all() || !min.is_finite() || !max.is_finite() {
        return stats;
    }
    stats.in_image = true;

    let x0 = min.x.floor() as u32;
    let y0 = min.y.floor() as u32;
    let x1 = (max.x.ceil() as u32).clamp(x0 + 1, width);
    let y1 = (max.y.ceil() as u32).clamp(y0 + 1, height);
    let mut step = 1;
    while (x1 - x0).div_ceil(step) * (y1 - y0).div_ceil(step) > max_samples.max(1) {
        step += 1;
    }

    // [green, rest][channel][value]
    let mut histograms = [[[0_u32; 256]; 3]; 2];
    let mut counts = [0_u32; 2];
    for y in (y0..y1).step_by(step as usize) {
        for x in (x0..x1).step_by(step as usize) {
            let direction = view.ray(Vec2::new(x as f32 + 0.5, y as f32 + 0.5));
            let crosses = frames[..boxes.len()].iter().any(|(world_to_box, half)| {
                ray_crosses_box(
                    world_to_box.transform_point3(view.position),
                    world_to_box.transform_vector3(direction),
                    *half,
                )
            });
            if !crosses {
                continue;
            }
            let color = rgb(x, y);
            let s = params.strength(color.map(f32::from));
            stats.pixels += 1;
            let class = if s >= 1.0 {
                stats.green += 1;
                0
            } else {
                if s > 0.0 {
                    stats.between += 1;
                }
                1
            };
            counts[class] += 1;
            for (histogram, value) in histograms[class].iter_mut().zip(color) {
                histogram[value as usize] += 1;
            }
        }
    }
    let summary = |class: usize| {
        (counts[class] > 0).then(|| {
            histograms[class]
                .each_ref()
                .map(|h| percentiles(h, counts[class]))
        })
    };
    stats.green_rgb = summary(0);
    stats.rest_rgb = summary(1);

    stats
}

#[cfg(test)]
mod tests {
    use super::*;
    use alvr_common::{
        Pose,
        glam::{Quat, Vec3},
    };

    fn keyed(rgb: [f32; 3]) -> f32 {
        GreenKeyParams::default().alpha(rgb)
    }

    #[test]
    fn params_defaults_and_clamps() {
        assert_eq!(
            GreenKeyParams::default(),
            GreenKeyParams {
                floor: 70,
                ratio: 135,
                delta: 25,
                soft: 24
            }
        );
        assert_eq!(
            GreenKeyParams::from_values(Some(-5), Some(5000), Some(-900), Some(0)),
            GreenKeyParams {
                floor: 0,
                ratio: 1000,
                delta: -255,
                soft: 1
            }
        );
        assert_eq!(
            GreenKeyParams::from_values(Some(300), Some(-1), Some(900), Some(999)),
            GreenKeyParams {
                floor: 255,
                ratio: 0,
                delta: 255,
                soft: 255
            }
        );
        assert_eq!(
            GreenKeyParams::default().text(),
            "floor 70 ratio 1.35 delta 25 soft 24"
        );
    }

    // Pure and camera-like greens are keyed (alpha 0); black, greys, white, skin, red, blue, yellow
    // and a washed-out bloomed green are kept (alpha 1)
    #[test]
    fn rule_keys_monitor_green_only() {
        for green in [
            [0.0, 255.0, 0.0],
            [40.0, 230.0, 60.0],
            [60.0, 200.0, 70.0],
            [90.0, 250.0, 110.0],
            [20.0, 140.0, 30.0],
        ] {
            assert_eq!(keyed(green), 0.0, "{green:?}");
        }
        for kept in [
            [0.0, 0.0, 0.0],
            [30.0, 30.0, 30.0],
            [60.0, 60.0, 60.0],
            [128.0, 128.0, 128.0],
            [255.0, 255.0, 255.0],
            // Skin tones
            [224.0, 172.0, 138.0],
            [180.0, 120.0, 90.0],
            [110.0, 70.0, 50.0],
            [255.0, 0.0, 0.0],
            [0.0, 0.0, 255.0],
            [230.0, 230.0, 40.0],
            [255.0, 255.0, 0.0],
            // Bloomed: the camera's green clipped, red and blue leaking in
            [170.0, 245.0, 170.0],
            [200.0, 255.0, 190.0],
            // Dark green under the floor
            [10.0, 60.0, 10.0],
        ] {
            assert_eq!(keyed(kept), 1.0, "{kept:?}");
        }
        // In between on the ramp
        let partial = keyed([30.0, 80.0, 30.0]);
        assert!(partial > 0.0 && partial < 1.0, "{partial}");
    }

    // Along any line towards green the alpha only falls: more green, less red / blue
    #[test]
    fn soft_ramp_is_monotonic() {
        let params = GreenKeyParams::default();
        for base in [[0.0, 0.0, 0.0], [80.0, 60.0, 100.0], [200.0, 100.0, 50.0]] {
            let mut last = params.alpha(base);
            for g in (0..=255).map(|g| g as f32) {
                let alpha = params.alpha([base[0], g, base[2]]);
                assert!(alpha <= last + 1e-6, "{base:?} g {g}");
                last = alpha;
            }
        }
        let mut last = params.alpha([255.0, 220.0, 0.0]);
        for r in (0..=255).rev().map(|r| r as f32) {
            let alpha = params.alpha([r, 220.0, 0.0]);
            assert!(alpha <= last + 1e-6, "r {r}");
            last = alpha;
        }
        // A wider ramp is never steeper
        let soft = GreenKeyParams { soft: 60, ..params };
        for g in 70..200 {
            let rgb = [30.0, g as f32, 30.0];
            assert!(soft.alpha(rgb) >= params.alpha(rgb) - 1e-6);
        }
    }

    #[test]
    fn lut_layout_is_bgr_with_identity_rgb() {
        let params = GreenKeyParams::default();
        let lut = green_key_lut(&params);
        assert_eq!(lut.len(), GREEN_LUT_SIZE);
        assert_eq!(GREEN_LUT_SIZE, 131072);
        let texel = |r: usize, g: usize, b: usize| {
            let index = 4 * ((b * COLOR_LUT_RESOLUTION + g) * COLOR_LUT_RESOLUTION + r);
            [lut[index], lut[index + 1], lut[index + 2], lut[index + 3]]
        };
        // Identity RGB, as the learned table
        assert_eq!(texel(0, 0, 0), [0, 0, 0, 255]);
        assert_eq!(texel(31, 31, 31), [255, 255, 255, 255]);
        assert_eq!(texel(31, 0, 0), [255, 0, 0, 255]);
        assert_eq!(texel(0, 0, 31), [0, 0, 255, 255]);
        assert_eq!(texel(3, 10, 17), [24, 82, 139, texel(3, 10, 17)[3]]);
        // Green keyed: r and b low, g high
        assert_eq!(texel(0, 31, 0), [0, 255, 0, 0]);
        assert_eq!(texel(4, 28, 5), [32, 230, 41, 0]);
        // The order is [b][g][r]: swapping g and b moves the key
        assert_eq!(texel(0, 0, 31)[3], 255);
        // Every texel's alpha is the rule at its colour
        for (index, texel) in lut.chunks_exact(4).enumerate() {
            let expected = params.alpha([
                f32::from(texel_channel(index % 32)),
                f32::from(texel_channel(index / 32 % 32)),
                f32::from(texel_channel(index / 1024)),
            ]);
            assert_eq!(texel[3], (expected * 255.0).round() as u8);
        }
        // The same into a reused buffer
        let mut reused = vec![7; GREEN_LUT_SIZE];
        fill_green_key_lut(&params, &mut reused);
        assert_eq!(reused, lut);
    }

    // A view at the origin looking along -Z, 90 degrees wide and high
    fn view(position: Vec3, orientation: Quat) -> ViewParams {
        ViewParams {
            pose: Pose {
                orientation,
                position,
            },
            fov: Fov {
                left: -45_f32.to_radians(),
                right: 45_f32.to_radians(),
                up: 45_f32.to_radians(),
                down: -45_f32.to_radians(),
            },
        }
    }

    // A slab 40 x 20 x 2 cm, its centre at `center`
    fn slab(center: Vec3) -> RegionOverride {
        RegionOverride {
            volume_to_world: Mat4::from_translation(center),
            half_extents: Vec3::new(0.2, 0.1, 0.01),
        }
    }

    #[test]
    fn zone_rect_of_a_box_in_front() {
        let eye = view(Vec3::ZERO, Quat::IDENTITY);
        // 1 m ahead, centred: x spans tangents +-0.2 (/ 0.99 or 1.01), y +-0.1
        let ZoneRect::Part(min, max) = zone_view_rect(&eye, &[slab(Vec3::new(0.0, 0.0, -1.0))])
        else {
            panic!();
        };
        let near = 0.2 / 0.99;
        assert!((min.x - (0.5 - 0.5 * near)).abs() < 1e-5, "{min}");
        assert!((max.x - (0.5 + 0.5 * near)).abs() < 1e-5, "{max}");
        assert!((min.y - (0.5 - 0.5 * 0.1 / 0.99)).abs() < 1e-5, "{min}");
        // Up is the top of the view (v down)
        let ZoneRect::Part(high_min, _) = zone_view_rect(&eye, &[slab(Vec3::new(0.0, 0.3, -1.0))])
        else {
            panic!();
        };
        assert!(high_min.y < min.y);
        // Two boxes: the union
        let ZoneRect::Part(both_min, both_max) = zone_view_rect(
            &eye,
            &[
                slab(Vec3::new(-0.3, 0.0, -1.0)),
                slab(Vec3::new(0.3, 0.0, -1.0)),
            ],
        ) else {
            panic!();
        };
        assert!(both_min.x < min.x && both_max.x > max.x);
        // The crop pads it and gives a matching fov
        let (offset, size, fov) = green_under_crop(
            eye.fov,
            UVec2::new(1000, 1000),
            ZoneRect::Part(min, max),
            0.03,
            false,
        );
        assert_eq!(offset.x, ((min.x - 0.03) * 1000.0).floor() as u32);
        assert_eq!(offset.x + size.x, ((max.x + 0.03) * 1000.0).ceil() as u32);
        assert!(fov.left > eye.fov.left && fov.right < eye.fov.right);
    }

    #[test]
    fn zone_rect_partly_off_the_view_is_clamped() {
        let eye = view(Vec3::ZERO, Quat::IDENTITY);
        // Its right half beyond the right edge (tangent 1)
        let ZoneRect::Part(min, max) = zone_view_rect(&eye, &[slab(Vec3::new(1.0, 0.0, -1.0))])
        else {
            panic!();
        };
        assert_eq!(max.x, 1.0);
        assert!(min.x > 0.8 && min.x < 1.0, "{min}");
        // Entirely beside it
        assert_eq!(
            zone_view_rect(&eye, &[slab(Vec3::new(3.0, 0.0, -1.0))]),
            ZoneRect::Outside
        );
        // Outside: a one-pixel crop in a corner, valid for a projection layer
        let (offset, size, _) = green_under_crop(
            eye.fov,
            UVec2::new(800, 600),
            ZoneRect::Outside,
            0.03,
            false,
        );
        assert_eq!(size, UVec2::ONE);
        assert!(offset.x < 800 && offset.y < 600);
        // Full: the whole view with its own fov
        assert_eq!(
            green_under_crop(eye.fov, UVec2::new(800, 600), ZoneRect::Full, 0.03, false),
            (UVec2::ZERO, UVec2::new(800, 600), eye.fov)
        );
    }

    #[test]
    fn zone_rect_behind_or_at_the_eye_is_the_whole_view() {
        let eye = view(Vec3::ZERO, Quat::IDENTITY);
        // Behind
        assert_eq!(
            zone_view_rect(&eye, &[slab(Vec3::new(0.0, 0.0, 1.0))]),
            ZoneRect::Full
        );
        // Through the eye plane
        assert_eq!(
            zone_view_rect(&eye, &[slab(Vec3::new(0.5, 0.0, -0.005))]),
            ZoneRect::Full
        );
        // Not finite: the whole view too
        let mut broken = slab(Vec3::new(0.0, 0.0, -1.0));
        broken.volume_to_world.w_axis.x = f32::NAN;
        assert_eq!(zone_view_rect(&eye, &[broken]), ZoneRect::Full);
        // No box: nothing to show
        assert_eq!(zone_view_rect(&eye, &[]), ZoneRect::Outside);
    }

    // Each eye from its own pose: the left eye sees the box further right
    #[test]
    fn zone_rect_of_both_eyes() {
        let zone = [slab(Vec3::new(0.0, 0.0, -0.8))];
        let left = view(Vec3::new(-0.032, 0.0, 0.0), Quat::IDENTITY);
        let right = view(Vec3::new(0.032, 0.0, 0.0), Quat::IDENTITY);
        let (ZoneRect::Part(left_min, left_max), ZoneRect::Part(right_min, right_max)) =
            (zone_view_rect(&left, &zone), zone_view_rect(&right, &zone))
        else {
            panic!();
        };
        assert!(left_min.x > right_min.x && left_max.x > right_max.x);
        assert!((left_min.y - right_min.y).abs() < 1e-6);
        // A head turned 60 degrees to the left: the box is beside the view (45 degrees each side)
        let turned = Quat::from_rotation_y(60_f32.to_radians());
        assert_eq!(
            zone_view_rect(&view(Vec3::ZERO, turned), &zone),
            ZoneRect::Outside
        );
    }

    #[test]
    fn camera_stats_of_the_zone() {
        // 320 x 240 camera at the origin looking along +Z (CameraView: z forward, y down)
        let camera = CameraView {
            position: Vec3::ZERO,
            orientation: Quat::IDENTITY,
            focal_length: Vec2::splat(200.0),
            center: Vec2::new(160.0, 120.0),
        };
        // 40 x 20 cm at 1 m: 80 x 40 pixels around the centre
        let zone = RegionOverride {
            volume_to_world: Mat4::from_translation(Vec3::new(0.0, 0.0, 1.0)),
            half_extents: Vec3::new(0.2, 0.1, 0.01),
        };
        let params = GreenKeyParams::default();
        // Green inside, a skin-coloured rim across its upper quarter, grey elsewhere
        let image = |x: u32, y: u32| {
            if (120..200).contains(&x) && (100..140).contains(&y) {
                if y < 110 {
                    [220, 170, 140]
                } else {
                    [40, 210, 60]
                }
            } else {
                [90, 90, 90]
            }
        };
        let stats = green_camera_stats(&camera, 320, 240, &[zone], &params, 300, image);
        assert!(stats.in_image);
        assert!(
            stats.pixels > 150 && stats.pixels <= 300,
            "{}",
            stats.pixels
        );
        let share = stats.green as f32 / stats.pixels as f32;
        assert!((share - 0.75).abs() < 0.1, "{share}");
        assert_eq!(stats.between, 0);
        assert_eq!(stats.green_rgb, Some([[40; 3], [210; 3], [60; 3]]));
        assert_eq!(stats.rest_rgb.unwrap()[0][1], 220);
        assert!(
            stats
                .text()
                .contains("green p10/50/90 r 40/40/40 g 210/210/210")
        );

        // Behind the camera: not in the image. Beside it too
        let behind = RegionOverride {
            volume_to_world: Mat4::from_translation(Vec3::new(0.0, 0.0, -1.0)),
            ..zone
        };
        let stats = green_camera_stats(&camera, 320, 240, &[behind], &params, 300, image);
        assert_eq!(stats.pixels, 0);
        let beside = RegionOverride {
            volume_to_world: Mat4::from_translation(Vec3::new(5.0, 0.0, 1.0)),
            ..zone
        };
        let stats = green_camera_stats(&camera, 320, 240, &[beside], &params, 300, image);
        assert!(!stats.in_image);
        assert_eq!(stats.text(), "the zone is not in the camera image");
        // No box
        assert_eq!(
            green_camera_stats(&camera, 320, 240, &[], &params, 300, image),
            GreenCameraStats::default()
        );
    }

    #[test]
    fn percentiles_of_a_histogram() {
        let mut histogram = [0; 256];
        for value in 1..=100 {
            histogram[value] += 1;
        }
        assert_eq!(percentiles(&histogram, 100), [10, 50, 90]);
        let mut one = [0; 256];
        one[7] = 1;
        assert_eq!(percentiles(&one, 1), [7, 7, 7]);
    }
}
