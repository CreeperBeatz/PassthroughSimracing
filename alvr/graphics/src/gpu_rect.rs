// Stage S3 of docs/gpu-optimization.md, `debug.alvr.gpu_rect`: the active rectangle. Per eye and per frame
// the CPU bounds the part of the stream view (view uv) outside which the depth mask is 0 in every channel
// the main pass reads and the hands' focus area is 0. The mask pass is scissored to it (the clear of the
// pass keeps the rest at 0), the main pass takes no mask or focus tap outside it, and with the colour key
// and `key_crop 0` the lower game layer is cropped to it. Off (unset or 0): nothing here is used, the
// uniform block's part is zeros and the renderer is today's.
//
// What can make a mask texel non-zero (depth_mask.wgsl `fragment_main`, every branch), and its bound:
// 1. The green-screen zone: a view ray through one of its boxes (slab test from the eye, whatever the
//    depth). Bound: the boxes seen from the eye.
// 2. No depth image: 0 (after 1).
// 3. `current_mask`, r: the coverage of the region by the surface points of a 3 x 3 neighbourhood. All of
//    them lie on ONE ray of the depth camera C, the one through Q = E + t dir - D (E the eye, t the
//    refined distance along the view ray, D the arm shift at E + t dir), each moved by D, and a point
//    counts only within the soft edge of a box: region_mask > 0 needs the signed distance below the
//    feathering f, which the shader only returns within tallest dome + f of a box (area_box_distance).
//    - D != 0 only where arm_displacement is non-zero: E + t dir inside the swept arm A (below). Then dir
//      is in the projection of A.
//    - D = 0: the ray of C through Q crosses the grown boxes B at a depth of at least `near` (linearized
//      depth), so the direction of Q - C lies in the cone of C over B. The angle between Q - E = t dir
//      and Q - C is at most asin(|C - E| / t) (triangle E, C, Q), t >= T (the shader clamps t to 0.05;
//      with the depth's near plane n, t >= n cos(asin(|C - E| / 0.05)) - |C - E| after a refinement step
//      that found a surface). So dir lies within that angle of the cone: the cone's rectangle grown by it.
//    g (proximity) and b (depth flag) are not 0 outside the region, but stream.wgsl reads them only with
//    the cloud (then they hold the zone and the trail, 4.) or in the Show depth view (a fallback).
// 4. The depth cloud's zone (g) and trail (b): looked up in the field at P = E + L dir (L = cloud_fetch,
//    moved by D with `cloud 2`: inside A as above). Non-zero needs |phi| < reach <= cap (or cloud_fixed)
//    and the field's phi saturates at the window's truncation 2W, so with cap <= 2W the lookup lies within
//    cap + 8 depth texels of a texel of M (in the region), whose ray from the field's camera C_F crosses
//    the boxes with domes. The angle between P - E and P - C_F is at most asin(|C_F - E| / L). Held
//    speeds anywhere do not matter. A field of another region or one that may not open: see the code.
//    `cloud_own` (the hand's own late depth, after the zone): it runs only where r of 3 is above 0, lowers r
//    and raises g there, and leaves b and a. So every texel it opens is inside the bound of 3 already.
// 5. The wrist gap: a ray within GAP_REACH of the gap segment (from 0.05 m on): the capsule.
// 6. Near the hands (focus area b, only with the key and a playzone): inside the focus pass's rectangle.
// 7. The lever windows: a ray through an open lever box. Bound: the boxes seen from the eye.
// The focus area is 0 outside the focus pass's scissor (the pass clears all of it), or all 0 without hands.
// Everything else falls back to the full view: listed in `RectFallback`.

use crate::{
    depth_mask::{self, ArmGap, DepthRenderInput, EffectiveRegion, LeverWindow, effective_region},
    motion_cloud::{CloudParams, cloud_truncation},
};
use alvr_common::{
    Fov, ViewParams,
    glam::{Quat, UVec2, Vec2, Vec3, Vec4},
};
use alvr_session::{DepthPassthroughConfig, DepthPassthroughDebugView};
use std::fmt::Write;

/// `debug.alvr.gpu_rect_pad` by default (thousandths of the view on each side)
pub const GPU_RECT_PAD_DEFAULT: i64 = 20;
/// Largest `debug.alvr.gpu_rect_pad`
pub const GPU_RECT_PAD_MAX: i64 = 250;

// Constants of depth_mask.wgsl (a test checks that they are still there)
/// The smallest distance along the view ray the mask looks up (`max(..., 0.05)`)
pub(crate) const SHADER_MIN_T: f32 = 0.05;
pub(crate) const SHADER_ARM_LENGTH: f32 = 0.45;
pub(crate) const SHADER_ARM_RADIUS: f32 = 0.14;
/// The lower end of `hand_edge` (`vec2f(-0.2, -0.1)` without the key; -0.03 with it)
pub(crate) const SHADER_HAND_EDGE: f32 = -0.2;
pub(crate) const SHADER_GAP_REACH: f32 = 0.055;

/// The cubic of `debug.alvr.fast_srgb` in stream.wgsl (`FAST_SRGB_C3` .. `FAST_SRGB_C0`)
pub const FAST_SRGB_COEFFICIENTS: [f32; 4] =
    [0.330_696, 0.656_133_81, 0.014_212_526, 0.001_549_237_5];

// Added to every box (m): rounding of the shader's slab tests and signed distances
const BOX_MARGIN: f32 = 0.002;
// Closer than this to a lever or green box the eye may see it at any angle (m)
const NEAR_BOX: f32 = 0.02;
// Larger angular growths fall back to the full view (radians): beyond it the azimuth bound below needs
// directions away from the poles and in front of the eye
const MAX_GROWTH: f32 = 25.0 * std::f32::consts::PI / 180.0;
// The cloud's lookup lies within cap + this of a texel of M (depth texels): 2 for the bilinear tap of the
// field, 2 for the cell's centre, 0.5 to the texel, 1 for the 8-bit phi, rounded up
const CLOUD_TEXEL_MARGIN: f32 = 8.0;
// Largest number of points of one convex source (four spheres of the swept arm as cubes)
const MAX_HULL_POINTS: usize = 32;

/// `debug.alvr.gpu_rect` 1 or 2 and `debug.alvr.gpu_rect_pad`
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GpuRectParams {
    /// `gpu_rect 2`: the borders are drawn in the Tint mask view
    pub border: bool,
    /// `gpu_rect_pad` as a share of the view (uv) on each side
    pub pad: f32,
}

/// The properties as read: None while `gpu_rect` is unset or 0 or below. The pad is clamped to
/// 0..`GPU_RECT_PAD_MAX` thousandths (unset: `GPU_RECT_PAD_DEFAULT`)
pub fn gpu_rect_params(value: Option<i64>, pad: Option<i64>) -> Option<GpuRectParams> {
    let value = value.filter(|&value| value > 0)?;
    let pad = pad
        .unwrap_or(GPU_RECT_PAD_DEFAULT)
        .clamp(0, GPU_RECT_PAD_MAX);

    Some(GpuRectParams {
        border: value >= 2,
        pad: pad as f32 / 1000.0,
    })
}

/// For the `stream:` line and a change: the values of `gpu_rect`, `gpu_rect_pad` and `fast_srgb`
pub fn gpu_work_text(params: Option<&GpuRectParams>, fast_srgb: bool) -> String {
    match params {
        Some(params) => format!(
            "gpu_rect {} (pad {}), fast_srgb {}",
            if params.border { 2 } else { 1 },
            (params.pad * 1000.0).round(),
            u8::from(fast_srgb)
        ),
        None => format!("gpu_rect 0, fast_srgb {}", u8::from(fast_srgb)),
    }
}

/// Why an eye's rectangle is the full view in a frame
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RectFallback {
    /// The distance region moves with the eye: every ray may see it
    Distance,
    /// The Show depth view reads the mask's proximity and depth flag everywhere
    ShowDepth,
    ProblemView,
    /// The area editor is open (the client)
    Editor,
    /// A depth dump reads the whole mask in this frame
    Dump,
    /// A burst records or reads the depth in this frame
    Burst,
    /// `cloud_ab` draws its mark
    CloudAb,
    /// `dbg_skip_mask`, `dbg_mask_every` or `dbg_skip_focus`: a texture keeps an older frame's content
    Ablation,
    /// The depth camera is too far from the eye for the angle bound (the head moved fast)
    DepthCamera,
    /// The eye is at or in a lever or green box
    NearBox,
    /// The cloud's reach (`cloud_cap` or `cloud_fixed`) is above the truncation of its window
    CloudReach,
    /// The field's camera is too far from the eye, or its cone reaches behind the view
    CloudCamera,
    /// The field was built with another region
    CloudRegion,
    /// A growth angle above `MAX_GROWTH`
    Angle,
}

impl RectFallback {
    pub const ALL: [Self; 14] = [
        Self::Distance,
        Self::ShowDepth,
        Self::ProblemView,
        Self::Editor,
        Self::Dump,
        Self::Burst,
        Self::CloudAb,
        Self::Ablation,
        Self::DepthCamera,
        Self::NearBox,
        Self::CloudReach,
        Self::CloudCamera,
        Self::CloudRegion,
        Self::Angle,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Distance => "distance region",
            Self::ShowDepth => "Show depth view",
            Self::ProblemView => "problem view",
            Self::Editor => "area editor",
            Self::Dump => "depth dump",
            Self::Burst => "burst",
            Self::CloudAb => "cloud_ab mark",
            Self::Ablation => "dbg_* ablation",
            Self::DepthCamera => "depth camera off the eye",
            Self::NearBox => "eye at a lever or green box",
            Self::CloudReach => "cloud reach above its window",
            Self::CloudCamera => "cloud field camera",
            Self::CloudRegion => "cloud field of another region",
            Self::Angle => "growth angle",
        }
    }

    fn index(self) -> usize {
        Self::ALL.iter().position(|&reason| reason == self).unwrap()
    }
}

/// The field of one eye of the depth cloud, as the mask reads it in this frame
#[derive(Clone, Copy)]
pub struct RectField {
    /// The camera of the depth image the field was built from
    pub camera: ViewParams,
    /// The field may open its zone (`pair.w` of the cloud uniforms)
    pub open: bool,
    /// The field was built with the region of this frame (`region_signature`)
    pub same_region: bool,
}

/// The depth cloud while it runs and the mask reads its field (None: the dummy, no zone)
#[derive(Clone, Copy)]
pub struct RectCloud {
    pub params: CloudParams,
    pub eyes: [Option<RectField>; 2],
    /// Of the depth images (the field's texels are counted in them)
    pub depth_size: UVec2,
}

/// What the bound of a frame needs
pub struct RectInput<'a> {
    pub config: &'a DepthPassthroughConfig,
    pub input: &'a DepthRenderInput,
    /// The views the stream was rendered with: their orientation and fov turn a uv into a ray
    pub stream_views: &'a [ViewParams; 2],
    /// The mask reads a playzone (playzone.x of the uniforms)
    pub playzone: bool,
    pub cloud: Option<RectCloud>,
    /// The focus pass's rectangle of each eye (`hands_view_rect_reach`), None: no hand, the focus area is 0
    pub hand_rects: [Option<(Vec2, Vec2)>; 2],
    pub mask_size: UVec2,
    pub focus_size: UVec2,
    pub pad: f32,
    /// A reason of the caller to use the full view
    pub hold: Option<RectFallback>,
}

/// The result for one eye
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EyeRect {
    /// Why it is the full view, None: bounded
    pub full: Option<RectFallback>,
    /// Texels of the mask pass, min (inclusive) and max (exclusive); empty unless min < max
    pub scissor: (UVec2, UVec2),
    /// The main pass takes the mask tap inside (min xy, max zw view uv)
    pub mask_taps: Vec4,
    /// The main pass takes the focus tap inside
    pub focus_taps: Vec4,
    /// Where the colour key can act: both rectangles, inside the view. None: nowhere
    pub active: Option<(Vec2, Vec2)>,
}

/// A rectangle no uv is in
pub const NO_TAPS: Vec4 = Vec4::new(2.0, 2.0, -1.0, -1.0);
/// A rectangle every uv is in
pub const ALL_TAPS: Vec4 = Vec4::new(-1.0, -1.0, 2.0, 2.0);

impl EyeRect {
    fn full(reason: RectFallback, mask_size: UVec2) -> Self {
        Self {
            full: Some(reason),
            scissor: (UVec2::ZERO, mask_size),
            mask_taps: ALL_TAPS,
            focus_taps: ALL_TAPS,
            active: Some((Vec2::ZERO, Vec2::ONE)),
        }
    }

    /// The share of the mask's texels the mask pass renders (0..1)
    pub fn share(&self, mask_size: UVec2) -> f32 {
        let (min, max) = self.scissor;
        if !min.cmplt(max).all() {
            return 0.0;
        }
        let size = (max - min).as_vec2();
        size.x * size.y / mask_size.max(UVec2::ONE).as_vec2().element_product()
    }

    /// The mask pass's texels in view uv (None: it renders nothing)
    pub fn scissor_uv(&self, mask_size: UVec2) -> Option<(Vec2, Vec2)> {
        let (min, max) = self.scissor;
        let size = mask_size.max(UVec2::ONE).as_vec2();
        min.cmplt(max)
            .all()
            .then(|| (min.as_vec2() / size, max.as_vec2() / size))
    }
}

/// The focus pass's scissor for a rectangle of the hands (view uv): texels min and max (exclusive), None:
/// nothing drawn. The one of `encode_focus_area`
pub(crate) fn focus_scissor(rect: (Vec2, Vec2), resolution: UVec2) -> Option<(UVec2, UVec2)> {
    let (min, max) = rect;
    let min = (min * resolution.as_vec2()).floor().as_uvec2();
    let max = (max * resolution.as_vec2())
        .ceil()
        .as_uvec2()
        .min(resolution);
    min.cmplt(max).all().then_some((min, max))
}

// A rectangle (in a tangent plane or in uv); empty unless min <= max
#[derive(Clone, Copy, Debug, PartialEq)]
struct Bounds {
    min: Vec2,
    max: Vec2,
}

impl Bounds {
    const EMPTY: Self = Self {
        min: Vec2::INFINITY,
        max: Vec2::NEG_INFINITY,
    };

    fn is_empty(&self) -> bool {
        !(self.min.x <= self.max.x && self.min.y <= self.max.y)
    }

    fn add(&mut self, point: Vec2) {
        self.min = self.min.min(point);
        self.max = self.max.max(point);
    }

    fn union(self, other: Self) -> Self {
        if self.is_empty() {
            return other;
        }
        if other.is_empty() {
            return self;
        }
        Self {
            min: self.min.min(other.min),
            max: self.max.max(other.max),
        }
    }
}

// The tangents (x right, y up; x / -z, y / -z) in a frame (`to_frame`: world to frame rotation) of the
// directions from `apex` to the convex hull of `points` (world), cut to the part at least `near` in front
// of the apex along the frame's axis (z <= -near). The hull cut by a plane is the hull of the points in
// front and of the crossings of every segment between two points (its edges are among them)
fn hull_tangents(apex: Vec3, to_frame: Quat, points: &[Vec3], near: f32) -> Bounds {
    debug_assert!(points.len() <= MAX_HULL_POINTS && near > 0.0);
    let mut local = [Vec3::ZERO; MAX_HULL_POINTS];
    let count = points.len().min(MAX_HULL_POINTS);
    for (local, point) in local.iter_mut().zip(&points[..count]) {
        *local = to_frame * (*point - apex);
    }
    let local = &local[..count];

    let mut bounds = Bounds::EMPTY;
    for point in local {
        if point.z <= -near {
            bounds.add(point.truncate() / -point.z);
        }
    }
    for (i, a) in local.iter().enumerate() {
        for b in &local[i + 1..] {
            let (da, db) = (a.z + near, b.z + near);
            if (da < 0.0) != (db < 0.0) && da != db {
                let crossing = a.lerp(*b, da / (da - db));
                bounds.add(crossing.truncate() / near);
            }
        }
    }
    bounds
}

// The 8 corners of a box (world): its frame's centre and axes as a transform, half extents
fn box_corners(to_world: impl Fn(Vec3) -> Vec3, half: Vec3) -> [Vec3; 8] {
    std::array::from_fn(|i| {
        let sign = Vec3::new(
            if i & 1 == 0 { -1.0 } else { 1.0 },
            if i & 2 == 0 { -1.0 } else { 1.0 },
            if i & 4 == 0 { -1.0 } else { 1.0 },
        );
        to_world(sign * half)
    })
}

// A sphere as the corners of the cube around it (world axes)
fn cube_corners(center: Vec3, radius: f32) -> [Vec3; 8] {
    box_corners(|offset| center + offset, Vec3::splat(radius))
}

// Grows a rectangle of tangents (directions in front of the view) by an angle: a direction within `theta`
// of one of the rectangle's lies inside the result, if it lies in the view. Of two directions at an
// angle theta whose azimuths (about the view's y axis, tangent x) differ by d, sin(theta) >= cos(e) sin(d),
// e the elevation of the one in the view, cos(e) >= 1 / sqrt(1 + ty^2) with ty the largest |tangent y| of
// the view; the same for y. `view_max`: the largest |tangent| of the view on each axis. None: too large
fn grow_by_angle(bounds: Bounds, theta: f32, view_max: Vec2) -> Option<Bounds> {
    if bounds.is_empty() || theta <= 0.0 {
        return Some(bounds);
    }
    if theta.is_nan() || theta >= MAX_GROWTH {
        return None;
    }
    let grow = |stretch: f32| {
        let sine = theta.sin() * (1.0 + stretch * stretch).sqrt();
        (sine < 0.99).then(|| sine.asin())
    };
    let grow_x = grow(view_max.y)?;
    let grow_y = grow(view_max.x)?;
    // An azimuth beyond 89.9 degrees has no useful tangent: the side is open
    let limit = 89.9_f32.to_radians();
    let side = |tangent: f32, delta: f32| (tangent.atan() + delta).clamp(-limit, limit).tan();

    Some(Bounds {
        min: Vec2::new(side(bounds.min.x, -grow_x), side(bounds.min.y, -grow_y)),
        max: Vec2::new(side(bounds.max.x, grow_x), side(bounds.max.y, grow_y)),
    })
}

// Tangents of the view to view uv (origin top-left)
fn tangents_to_uv(bounds: Bounds, fov: Fov) -> Bounds {
    if bounds.is_empty() {
        return bounds;
    }
    let (left, right) = (fov.left.tan(), fov.right.tan());
    let (down, up) = (fov.down.tan(), fov.up.tan());
    Bounds {
        min: Vec2::new(
            (bounds.min.x - left) / (right - left),
            (up - bounds.max.y) / (up - down),
        ),
        max: Vec2::new(
            (bounds.max.x - left) / (right - left),
            (up - bounds.min.y) / (up - down),
        ),
    }
}

// The view's frame of one eye: its rotation from world, the largest |tangent| per axis, the smallest
// |z| of a unit view direction
struct EyeFrame {
    eye: Vec3,
    to_view: Quat,
    view_max: Vec2,
    min_depth: f32,
}

impl EyeFrame {
    fn new(view: &ViewParams, eye: Vec3) -> Self {
        let view_max = Vec2::new(
            view.fov.left.tan().abs().max(view.fov.right.tan().abs()),
            view.fov.up.tan().abs().max(view.fov.down.tan().abs()),
        );
        Self {
            eye,
            to_view: view.pose.orientation.inverse(),
            view_max,
            min_depth: 1.0 / (1.0 + view_max.length_squared()).sqrt(),
        }
    }

    // The rays from the eye through a box (a slab test from the eye, as the lever windows and the green
    // zone): the box cut 2 cm in front of the eye, and the full view if the eye is that close to it
    fn box_from_eye(&self, window: &LeverWindow) -> Result<Bounds, RectFallback> {
        let half = window.region.half_extents + BOX_MARGIN;
        let to_world = window.region.volume_to_world;
        let local_eye = to_world.inverse().transform_point3(self.eye);
        let distance = (local_eye.abs() - half).max(Vec3::ZERO).length();
        if distance < NEAR_BOX {
            return Err(RectFallback::NearBox);
        }
        let corners = box_corners(|point| to_world.transform_point3(point), half);
        Ok(hull_tangents(
            self.eye,
            self.to_view,
            &corners,
            NEAR_BOX * self.min_depth,
        ))
    }
}

// The swept arm of a hand (where arm_displacement is non-zero), as the corners of the cubes around four
// spheres of radius ARM_RADIUS: at the wrist end and the elbow end of the weight, now and moved back by
// the motion. None: the arm is still (the shader skips it) or not given
fn arm_points(arm: Option<&depth_mask::ArmMotion>) -> Option<[Vec3; 32]> {
    let arm = arm?;
    // The shader skips it below 1e-6; half of that for the rounding of the dot product
    if arm.displacement.length_squared() < 0.5e-6 {
        return None;
    }
    let along_end = if arm.weight_len > 0.0 {
        arm.weight_len
    } else {
        SHADER_ARM_LENGTH
    };
    let direction = arm.direction;
    let motion = arm.displacement - direction * arm.displacement.dot(direction);
    let mut points = [Vec3::ZERO; 32];
    for (i, (along, back)) in [
        (SHADER_HAND_EDGE, 0.0),
        (along_end, 0.0),
        (SHADER_HAND_EDGE, 1.0),
        (along_end, 1.0),
    ]
    .into_iter()
    .enumerate()
    {
        let center = arm.wrist + direction * along - motion * back;
        points[8 * i..8 * i + 8]
            .copy_from_slice(&cube_corners(center, SHADER_ARM_RADIUS + BOX_MARGIN));
    }
    Some(points)
}

// The corners of the boxes of the region (world), each grown by its tallest dome and `extra`
fn region_boxes(
    area_to_world: alvr_common::glam::Mat4,
    area: &crate::PlayArea,
    extra: f32,
) -> impl Iterator<Item = [Vec3; 8]> + '_ {
    area.boxes().iter().map(move |area_box| {
        let half = area_box.half + area_box.tallest_dome() + extra;
        box_corners(
            |local| area_to_world.transform_point3(area_box.to_area(local)),
            half,
        )
    })
}

// The bound of one eye in view uv (before the pad), or why it is the full view
fn eye_bounds(rect: &RectInput, eye: usize) -> Result<Bounds, RectFallback> {
    let view = &rect.stream_views[eye];
    let input = rect.input;
    let frame = EyeFrame::new(view, input.eye_positions[eye]);
    let mut tangents = Bounds::EMPTY;
    let mut uv = Bounds::EMPTY;

    // 1. The green-screen zone, also without depth
    for region in input.green_windows.iter().flatten() {
        if let Some(window) = (LeverWindow {
            region: *region,
            open: 1.0,
        })
        .checked()
        {
            tangents = tangents.union(frame.box_from_eye(&window)?);
        }
    }

    // 2. Nothing else without depth
    let Some(depth) = input.frame else {
        return Ok(tangents_to_uv(tangents, view.fov));
    };

    let (area_to_world, area) = match effective_region(rect.config, input) {
        EffectiveRegion::Distance { .. } => return Err(RectFallback::Distance),
        EffectiveRegion::Volume {
            area_to_world,
            area,
            ..
        } => (area_to_world, area),
    };
    let feathering = rect.config.edge_feathering.max(0.0005);
    let near = if depth.near.is_finite() && depth.near > 0.0 {
        depth.near
    } else {
        0.0
    };

    // The swept arms (3., 4. with `cloud 2`): from 0.05 m along the view ray
    let arms = [0, 1].map(|hand| arm_points(input.arms[hand].as_ref()));
    let mut arm_tangents = Bounds::EMPTY;
    for points in arms.iter().flatten() {
        arm_tangents = arm_tangents.union(hull_tangents(
            frame.eye,
            frame.to_view,
            points,
            SHADER_MIN_T * frame.min_depth,
        ));
    }

    // 3. The depth: the cone of the depth camera over the grown boxes, grown by the angle
    let camera = depth.depth_views[eye].pose.position;
    let offset = camera.distance(frame.eye);
    if offset.is_nan() || offset >= SHADER_MIN_T {
        return Err(RectFallback::DepthCamera);
    }
    let after_refinement = near * (1.0 - (offset / SHADER_MIN_T).powi(2)).max(0.0).sqrt() - offset;
    let min_t = SHADER_MIN_T.max(after_refinement);
    // The unit direction of Q - C has a view z of at most -g; the surface is at least `near` from C
    let g = (min_t * frame.min_depth - offset) / (min_t + offset);
    if !(g > 0.0 && near > 0.0) {
        return Err(RectFallback::DepthCamera);
    }
    let mut region = Bounds::EMPTY;
    for corners in region_boxes(area_to_world, &area, feathering + BOX_MARGIN) {
        region = region.union(hull_tangents(
            camera,
            frame.to_view,
            &corners,
            0.9 * near * g,
        ));
    }
    let theta = (offset / min_t).asin();
    tangents = tangents
        .union(grow_by_angle(region, theta, frame.view_max).ok_or(RectFallback::Angle)?)
        .union(arm_tangents);

    // 4. The depth cloud's zone and trail
    if let Some(cloud) = &rect.cloud
        && depth_mask::cloud_shown(
            rect.config,
            input.cloud.as_ref(),
            depth_mask::cloud_runs(rect.config, input),
        )
        && let Some(field) = cloud.eyes[eye].filter(|field| field.open)
    {
        tangents = tangents.union(cloud_bounds(
            &frame,
            cloud,
            &field,
            &area_to_world,
            &area,
            near,
            &arms,
        )?);
    }

    // 5. The wrist gaps: the slot's wrist as the uniforms have it
    for hand in 0..2 {
        let Some(gap) = input.arm_gaps[hand].and_then(ArmGap::checked) else {
            continue;
        };
        let wrist = input.arms[hand].map_or(gap.wrist, |arm| arm.wrist);
        let mut points = [Vec3::ZERO; 16];
        points[..8].copy_from_slice(&cube_corners(wrist, SHADER_GAP_REACH + BOX_MARGIN));
        points[8..].copy_from_slice(&cube_corners(
            wrist + gap.axis * gap.length,
            SHADER_GAP_REACH + BOX_MARGIN,
        ));
        tangents = tangents.union(hull_tangents(
            frame.eye,
            frame.to_view,
            &points,
            SHADER_MIN_T * frame.min_depth,
        ));
    }

    // 6. Near the hands: the focus pass's texels and one around them (the mask's bilinear tap)
    if input.color_key.is_some()
        && rect.playzone
        && let Some(hand_rect) = rect.hand_rects[eye]
        && let Some((min, max)) = focus_scissor(hand_rect, rect.focus_size)
    {
        let size = rect.focus_size.as_vec2();
        uv = uv.union(Bounds {
            min: (min.as_vec2() - 1.0) / size,
            max: (max.as_vec2() + 1.0) / size,
        });
    }

    // 7. The lever windows that are drawn
    for window in input
        .lever_windows
        .iter()
        .filter_map(|window| window.and_then(LeverWindow::checked))
    {
        tangents = tangents.union(frame.box_from_eye(&window)?);
    }

    Ok(tangents_to_uv(tangents, view.fov).union(uv))
}

// 4. The zone of the cloud's field of one eye: the field camera's cone over the texels of M grown by the
// reach, seen from the eye, grown by the angle of the camera's offset at the lookup distance; with
// `cloud 2` also the swept arms
fn cloud_bounds(
    frame: &EyeFrame,
    cloud: &RectCloud,
    field: &RectField,
    area_to_world: &alvr_common::glam::Mat4,
    area: &crate::PlayArea,
    near: f32,
    arms: &[Option<[Vec3; 32]>; 2],
) -> Result<Bounds, RectFallback> {
    let params = &cloud.params;
    let finite = |value: f32| {
        if value.is_finite() {
            value.max(0.0)
        } else {
            0.0
        }
    };
    let reach = if finite(params.fixed) > 0.0 {
        finite(params.fixed)
    } else {
        finite(params.cap)
    };
    if reach > cloud_truncation(params.window) {
        return Err(RectFallback::CloudReach);
    }
    if !field.same_region {
        return Err(RectFallback::CloudRegion);
    }
    let fetch = finite(params.fetch_m);
    let camera = field.camera;
    let offset = camera.pose.position.distance(frame.eye);
    if offset.is_nan() || offset >= fetch {
        return Err(RectFallback::CloudCamera);
    }

    // The texels of M: rays of the field's camera through the boxes with their domes, at the depth's
    // near plane or farther (half of it here: a near plane of another frame)
    let to_field = camera.pose.orientation.inverse();
    let mut texels = Bounds::EMPTY;
    for corners in region_boxes(*area_to_world, area, BOX_MARGIN) {
        texels = texels.union(hull_tangents(
            camera.pose.position,
            to_field,
            &corners,
            (0.5 * near).max(1e-3),
        ));
    }
    let mut result = Bounds::EMPTY;
    if !texels.is_empty() {
        let low = Vec2::new(camera.fov.left.tan(), camera.fov.down.tan());
        let high = Vec2::new(camera.fov.right.tan(), camera.fov.up.tan());
        let per_texel = (high - low) / cloud.depth_size.max(UVec2::ONE).as_vec2();
        let grow = per_texel * (reach + CLOUD_TEXEL_MARGIN);
        // Outside the field's image the mask finds no zone
        let min = (texels.min - grow).max(low);
        let max = (texels.max + grow).min(high);
        if min.x <= max.x && min.y <= max.y {
            let mut cone = Bounds::EMPTY;
            for corner in [min, Vec2::new(max.x, min.y), Vec2::new(min.x, max.y), max] {
                let direction = frame.to_view * (camera.pose.orientation * corner.extend(-1.0));
                if direction.z.is_nan() || direction.z >= -1e-4 {
                    return Err(RectFallback::CloudCamera);
                }
                cone.add(direction.truncate() / -direction.z);
            }
            result = grow_by_angle(cone, (offset / fetch).asin(), frame.view_max)
                .ok_or(RectFallback::Angle)?;
        }
    }

    // `cloud 2`: the lookup point is moved by the arm shift where it lies in a swept arm
    if params.mode >= 2 {
        let near_arm = SHADER_MIN_T.min(fetch).max(1e-3) * frame.min_depth;
        for points in arms.iter().flatten() {
            result = result.union(hull_tangents(frame.eye, frame.to_view, points, near_arm));
        }
    }
    Ok(result)
}

/// The rectangles of both eyes for a frame (see the top of this file)
pub fn active_rects(rect: &RectInput) -> [EyeRect; 2] {
    let mask_size = rect.mask_size.max(UVec2::ONE);
    // The room through the game (`game_see`, only while the area editor is open) packs alphas that the
    // lower layer's crop would change
    let hold = rect.hold.or(match rect.config.debug_view {
        DepthPassthroughDebugView::ShowDepth => Some(RectFallback::ShowDepth),
        _ if rect.input.game_see > 0.0 => Some(RectFallback::Editor),
        _ => None,
    });

    [0, 1].map(|eye| {
        if let Some(reason) = hold {
            return EyeRect::full(reason, mask_size);
        }
        let bounds = match eye_bounds(rect, eye) {
            Ok(bounds) => bounds,
            Err(reason) => return EyeRect::full(reason, mask_size),
        };

        // The mask pass: the bound padded, in the view, rounded out to texels
        let size = mask_size.as_vec2();
        let scissor = if bounds.is_empty() {
            (UVec2::ZERO, UVec2::ZERO)
        } else {
            let pad = if rect.pad.is_finite() {
                rect.pad.max(0.0)
            } else {
                0.0
            };
            let min = (bounds.min - pad).clamp(Vec2::ZERO, Vec2::ONE);
            let max = (bounds.max + pad).clamp(Vec2::ZERO, Vec2::ONE);
            let min = (min * size).floor().as_uvec2().min(mask_size);
            let max = (max * size).ceil().as_uvec2().min(mask_size);
            if min.cmplt(max).all() {
                (min, max)
            } else {
                (UVec2::ZERO, UVec2::ZERO)
            }
        };
        // The main pass's bilinear tap reaches the texels next to its position: one more texel
        let mask_taps = if scissor.0.cmplt(scissor.1).all() {
            let min = (scissor.0.as_vec2() - 1.0) / size;
            let max = (scissor.1.as_vec2() + 1.0) / size;
            Vec4::new(min.x, min.y, max.x, max.y)
        } else {
            NO_TAPS
        };
        let focus_taps = rect.hand_rects[eye]
            .filter(|_| rect.input.color_key.is_some())
            .and_then(|hand_rect| focus_scissor(hand_rect, rect.focus_size))
            .map_or(NO_TAPS, |(min, max)| {
                let size = rect.focus_size.max(UVec2::ONE).as_vec2();
                let min = (min.as_vec2() - 1.0) / size;
                let max = (max.as_vec2() + 1.0) / size;
                Vec4::new(min.x, min.y, max.x, max.y)
            });
        let mut active = Bounds::EMPTY;
        for taps in [mask_taps, focus_taps] {
            if taps.x <= taps.z && taps.y <= taps.w {
                active = active.union(Bounds {
                    min: Vec2::new(taps.x, taps.y).max(Vec2::ZERO),
                    max: Vec2::new(taps.z, taps.w).min(Vec2::ONE),
                });
            }
        }

        EyeRect {
            full: None,
            scissor,
            mask_taps,
            focus_taps,
            active: (!active.is_empty()).then_some((active.min, active.max)),
        }
    })
}

/// The `gpu_rect` part of the uniform block (`depth_mask::gpu_rect_uniforms`)
pub(crate) fn write_uniforms(
    out: &mut [f32; depth_mask::GPU_RECT_FLOAT_COUNT],
    params: &GpuRectParams,
    eyes: &[EyeRect; 2],
) {
    out[0..4].copy_from_slice(&[1.0, if params.border { 2.0 } else { 1.0 }, 0.0, 0.0]);
    for (eye, rect) in eyes.iter().enumerate() {
        out[4 + 4 * eye..8 + 4 * eye].copy_from_slice(&rect.mask_taps.to_array());
        out[12 + 4 * eye..16 + 4 * eye].copy_from_slice(&rect.focus_taps.to_array());
    }
}

/// The lower key layer of an eye cropped to its active rectangle (the green layer's and `key_crop`'s
/// machinery): the pixels and fov of the crop in the output view, and the crop as stream uv for the
/// shader's `key_lower`. `stream_fov` and `output_fov` differ only with the edge fill (same pose)
pub fn lower_crop(
    rect: &EyeRect,
    stream_fov: Fov,
    output_fov: Fov,
    resolution: UVec2,
    edge_fill: bool,
) -> ((UVec2, UVec2, Fov), (Vec2, Vec2)) {
    if rect.full.is_some() {
        return (
            (UVec2::ZERO, resolution.max(UVec2::ONE), output_fov),
            (Vec2::ZERO, Vec2::ONE),
        );
    }
    // Nothing can be keyed: one pixel in the corner (the shader writes the same alpha there)
    let active = rect.active.unwrap_or((Vec2::ZERO, Vec2::ZERO));
    let active = if edge_fill {
        crate::stream_rect_to_output(stream_fov, output_fov, active)
    } else {
        active
    };
    let (offset, size, fov) = depth_mask::crop_view(output_fov, resolution, active, 0.0, false);
    let uv = depth_mask::crop_view_uv(resolution, offset, size, false);
    let uv = if edge_fill {
        crate::output_rect_to_stream(stream_fov, output_fov, uv)
    } else {
        uv
    };
    ((offset, size, fov), uv)
}

/// The rectangles of a frame as the renderer used them
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GpuRectFrame {
    pub eyes: [EyeRect; 2],
    pub mask_size: UVec2,
    /// The lower key layer's crop of each eye (pixels, size, fov), if it was asked for
    pub lower_crops: Option<[(UVec2, UVec2, Fov); 2]>,
}

/// What the client asks of the renderer for the next frames (`StreamRenderer::set_gpu_rect`)
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GpuRectRequest {
    pub params: GpuRectParams,
    /// Crop the lower key layer to the active rectangle (colour key with its extra layers, `key_crop 0`)
    pub crop_lower: bool,
    /// A reason of the client for the full view (the area editor, a burst)
    pub hold: Option<RectFallback>,
}

/// The `rect:` line: what the frames since the last line did
#[derive(Clone, Copy, Debug, Default)]
pub struct RectStats {
    frames: u32,
    share_sum: f64,
    share_max: f32,
    full_frames: u32,
    reasons: [u32; RectFallback::ALL.len()],
    cropped: u32,
    last: Option<GpuRectFrame>,
}

impl RectStats {
    /// One frame: its share is the mean of both eyes' mask pass (full view: 1)
    pub fn add(&mut self, frame: &GpuRectFrame) {
        let share =
            0.5 * (frame.eyes[0].share(frame.mask_size) + frame.eyes[1].share(frame.mask_size));
        self.frames += 1;
        self.share_sum += f64::from(share);
        self.share_max = self.share_max.max(share);
        let mut full = false;
        for rect in &frame.eyes {
            if let Some(reason) = rect.full {
                self.reasons[reason.index()] += 1;
                full = true;
            }
        }
        self.full_frames += u32::from(full);
        self.cropped += u32::from(frame.lower_crops.is_some());
        self.last = Some(*frame);
    }

    pub fn frames(&self) -> u32 {
        self.frames
    }

    /// The line; None without a frame
    pub fn text(&self) -> Option<String> {
        let last = self.last?;
        let mut text = String::from("rect:");
        for (eye, rect) in last.eyes.iter().enumerate() {
            let name = if eye == 0 { "L" } else { "R" };
            let sep = if eye == 0 { " " } else { ", " };
            match (rect.full, rect.scissor_uv(last.mask_size)) {
                (Some(reason), _) => {
                    write!(text, "{sep}{name} full ({})", reason.name()).ok();
                }
                (None, Some((min, max))) => {
                    write!(
                        text,
                        "{sep}{name} u {:.3}-{:.3} v {:.3}-{:.3}",
                        min.x, max.x, min.y, max.y
                    )
                    .ok();
                }
                (None, None) => {
                    write!(text, "{sep}{name} empty").ok();
                }
            }
        }
        let mean = if self.frames > 0 {
            self.share_sum / f64::from(self.frames)
        } else {
            0.0
        };
        write!(
            text,
            " (last frame, mask pass); share of the view mean {:.1} % max {:.1} % over {} frames; full \
             view {} frames",
            100.0 * mean,
            100.0 * self.share_max,
            self.frames,
            self.full_frames
        )
        .ok();
        if let Some((index, count)) = self
            .reasons
            .iter()
            .enumerate()
            .filter(|(_, count)| **count > 0)
            .max_by_key(|(_, count)| **count)
        {
            write!(
                text,
                " (most: {}, {count} eye frames)",
                RectFallback::ALL[index].name()
            )
            .ok();
        }
        write!(text, "; lower layer cropped {} frames", self.cropped).ok();
        Some(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        PlacedArea, PlayArea, RegionOverride,
        depth_mask::{DepthFrame, MAX_GREEN_WINDOWS},
    };
    use alvr_common::{
        Pose,
        glam::{Mat4, Quat},
    };
    use alvr_session::{DepthMaskRegion, PassthroughLightingConfig, settings_schema::Switch};

    fn config() -> DepthPassthroughConfig {
        DepthPassthroughConfig {
            region: DepthMaskRegion::Distance { max_distance: 1.0 },
            edge_feathering: 0.01,
            lighting: Switch::Disabled::<PassthroughLightingConfig>,
            debug_view: DepthPassthroughDebugView::Off,
            flip_depth_y: false,
        }
    }

    fn fov() -> Fov {
        Fov {
            left: -0.9,
            right: 0.7,
            up: 0.75,
            down: -0.95,
        }
    }

    fn view(position: Vec3, orientation: Quat) -> ViewParams {
        ViewParams {
            pose: Pose {
                orientation,
                position,
            },
            fov: fov(),
        }
    }

    fn area(center: Vec3, half: Vec3) -> PlacedArea {
        PlacedArea {
            area_to_world: Mat4::IDENTITY,
            area: PlayArea::single(crate::AreaBox::new(center, half)),
        }
    }

    fn input(region: Option<PlacedArea>, eye: Vec3, camera: Vec3) -> DepthRenderInput {
        DepthRenderInput {
            frame: Some(DepthFrame {
                swapchain_index: 0,
                near: 0.1,
                far: f32::INFINITY,
                depth_views: [view(camera, Quat::IDENTITY); 2],
            }),
            eye_positions: [eye; 2],
            color_key: None,
            arms: [None; 2],
            arm_gaps: [None; 2],
            edge_cut: 0.0,
            lever_windows: [None; depth_mask::MAX_LEVER_WINDOWS],
            green_windows: [None; MAX_GREEN_WINDOWS],
            capture_playzone: false,
            playzone_view: false,
            memory: None,
            cloud: None,
            key_lower_rects: None,
            region_override: region,
            game_dim: 0.0,
            game_see: 0.0,
        }
    }

    fn rects(input: &DepthRenderInput, hold: Option<RectFallback>) -> [EyeRect; 2] {
        let views = [view(Vec3::ZERO, Quat::IDENTITY); 2];
        active_rects(&RectInput {
            config: &config(),
            input,
            stream_views: &views,
            playzone: false,
            cloud: None,
            hand_rects: [None; 2],
            mask_size: UVec2::new(100, 80),
            focus_size: UVec2::new(100, 80),
            pad: 0.0,
            hold,
        })
    }

    // Brute force: the uv of the view ray towards a point
    fn uv_of(point: Vec3) -> Vec2 {
        let fov = fov();
        let tangent = point.truncate() / -point.z;
        Vec2::new(
            (tangent.x - fov.left.tan()) / (fov.right.tan() - fov.left.tan()),
            (fov.up.tan() - tangent.y) / (fov.up.tan() - fov.down.tan()),
        )
    }

    #[test]
    fn params_unset_are_off() {
        assert_eq!(gpu_rect_params(None, Some(30)), None);
        assert_eq!(gpu_rect_params(Some(0), None), None);
        assert_eq!(gpu_rect_params(Some(-1), None), None);
        let one = gpu_rect_params(Some(1), None).unwrap();
        assert!(!one.border);
        assert_eq!(one.pad, 0.02);
        assert!(gpu_rect_params(Some(2), Some(5)).unwrap().border);
        assert_eq!(gpu_rect_params(Some(1), Some(-4)).unwrap().pad, 0.0);
        assert_eq!(gpu_rect_params(Some(1), Some(9999)).unwrap().pad, 0.25);
        assert_eq!(
            gpu_work_text(gpu_rect_params(Some(2), None).as_ref(), true),
            "gpu_rect 2 (pad 20), fast_srgb 1"
        );
        assert_eq!(gpu_work_text(None, false), "gpu_rect 0, fast_srgb 0");
    }

    // The corners of a box in front of the view and the box cut by a plane through it: every point of the
    // hull projects inside
    #[test]
    fn hull_tangents_contain_the_hull() {
        let points = box_corners(
            |p| Quat::from_rotation_y(0.4) * p + Vec3::new(0.1, -0.2, -0.3),
            Vec3::new(0.2, 0.1, 0.35),
        );
        let near = 0.05;
        let bounds = hull_tangents(Vec3::ZERO, Quat::IDENTITY, &points, near);
        let mut seed = 1_u32;
        let mut random = || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) as f32 / (1 << 24) as f32
        };
        let mut inside = 0;
        for _ in 0..20_000 {
            let point = Quat::from_rotation_y(0.4)
                * Vec3::new(
                    (random() * 2.0 - 1.0) * 0.2,
                    (random() * 2.0 - 1.0) * 0.1,
                    (random() * 2.0 - 1.0) * 0.35,
                )
                + Vec3::new(0.1, -0.2, -0.3);
            if point.z > -near {
                continue;
            }
            inside += 1;
            let tangent = point.truncate() / -point.z;
            assert!(
                tangent.cmpge(bounds.min - 1e-5).all() && tangent.cmple(bounds.max + 1e-5).all(),
                "{tangent} {bounds:?}"
            );
        }
        assert!(inside > 1000);
        // Wholly behind: nothing
        let behind = box_corners(|p| p + Vec3::new(0.0, 0.0, 0.5), Vec3::splat(0.1));
        assert!(hull_tangents(Vec3::ZERO, Quat::IDENTITY, &behind, near).is_empty());
    }

    // A direction within the angle of a direction of the rectangle lies in the grown one
    #[test]
    fn growth_by_angle_contains_the_cone() {
        let bounds = Bounds {
            min: Vec2::new(-0.3, -0.8),
            max: Vec2::new(0.4, -0.2),
        };
        let view_max = Vec2::new(0.9, 0.95);
        let theta = 0.12;
        let grown = grow_by_angle(bounds, theta, view_max).unwrap();
        let mut seed = 7_u32;
        let mut random = || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) as f32 / (1 << 24) as f32
        };
        let mut checked = 0;
        for _ in 0..50_000 {
            let tangent = bounds.min + (bounds.max - bounds.min) * Vec2::new(random(), random());
            let u = tangent.extend(-1.0).normalize();
            // A direction at most theta away
            let axis = Vec3::new(random() - 0.5, random() - 0.5, random() - 0.5).normalize();
            let v = Quat::from_axis_angle(axis, theta * random()) * u;
            if v.z >= 0.0 {
                continue;
            }
            let t = v.truncate() / -v.z;
            if t.abs().cmpgt(view_max).any() {
                continue;
            }
            checked += 1;
            assert!(
                t.cmpge(grown.min - 1e-5).all() && t.cmple(grown.max + 1e-5).all(),
                "{t} {grown:?}"
            );
        }
        assert!(checked > 10_000);
        assert_eq!(grow_by_angle(bounds, 0.5, view_max), None);
    }

    // A box below and in front: the rectangle holds its projection from the eye, and not the top of the view
    #[test]
    fn box_ahead_is_bounded() {
        let region = area(Vec3::new(0.0, -0.4, -0.5), Vec3::new(0.2, 0.1, 0.15));
        let eye = Vec3::ZERO;
        let rects = rects(&input(Some(region), eye, eye), None);
        let rect = rects[0];
        assert_eq!(rect.full, None);
        let (min, max) = rect.scissor_uv(UVec2::new(100, 80)).unwrap();
        for corner in box_corners(
            |p| Vec3::new(0.0, -0.4, -0.5) + p,
            Vec3::new(0.2, 0.1, 0.15),
        ) {
            let uv = uv_of(corner).clamp(Vec2::ZERO, Vec2::ONE);
            assert!(uv.cmpge(min - 0.011).all() && uv.cmple(max + 0.011).all());
        }
        assert!(min.y > 0.3, "{min}");
        assert!(rect.share(UVec2::new(100, 80)) < 0.5);
        // The main pass taps one texel beyond the scissor
        assert_eq!(rect.mask_taps.x, (rect.scissor.0.x as f32 - 1.0) / 100.0);
        assert_eq!(rect.focus_taps, NO_TAPS);
    }

    // A box behind the eye shows nowhere; one the eye sits in is the full view only through a window
    #[test]
    fn box_behind_is_empty_and_fallbacks_are_full() {
        let region = area(Vec3::new(0.0, 0.0, 1.5), Vec3::splat(0.2));
        let rect = rects(&input(Some(region), Vec3::ZERO, Vec3::ZERO), None)[0];
        assert_eq!(rect.full, None);
        assert_eq!(rect.scissor_uv(UVec2::new(100, 80)), None);
        assert_eq!(rect.mask_taps, NO_TAPS);
        assert_eq!(rect.active, None);
        // A distance region, the depth camera 6 cm away, a held reason
        let far_camera = input(Some(region), Vec3::ZERO, Vec3::new(0.06, 0.0, 0.0));
        assert_eq!(
            rects(&far_camera, None)[0].full,
            Some(RectFallback::DepthCamera)
        );
        assert_eq!(
            rects(&input(None, Vec3::ZERO, Vec3::ZERO), None)[1].full,
            Some(RectFallback::Distance)
        );
        let held = rects(
            &input(Some(region), Vec3::ZERO, Vec3::ZERO),
            Some(RectFallback::Editor),
        );
        assert_eq!(held[0].full, Some(RectFallback::Editor));
        assert_eq!(held[0].mask_taps, ALL_TAPS);
        assert_eq!(held[0].share(UVec2::new(100, 80)), 1.0);
        // A green box around the eye
        let mut green = input(Some(region), Vec3::ZERO, Vec3::ZERO);
        green.green_windows[0] = Some(RegionOverride {
            volume_to_world: Mat4::IDENTITY,
            half_extents: Vec3::splat(0.3),
        });
        assert_eq!(rects(&green, None)[0].full, Some(RectFallback::NearBox));
    }

    // The farther the depth camera is from the eye, the larger the rectangle
    #[test]
    fn camera_offset_grows_the_rectangle() {
        let region = area(Vec3::new(0.0, -0.4, -0.5), Vec3::new(0.2, 0.1, 0.15));
        let share = |offset: f32| {
            rects(
                &input(Some(region), Vec3::ZERO, Vec3::new(offset, 0.0, 0.0)),
                None,
            )[0]
            .share(UVec2::new(100, 80))
        };
        assert!(share(0.0) < share(0.005));
        assert!(share(0.005) < share(0.015));
    }

    #[test]
    fn focus_scissor_is_the_one_of_the_focus_pass() {
        let size = UVec2::new(100, 80);
        assert_eq!(
            focus_scissor((Vec2::new(0.101, 0.5), Vec2::new(0.2, 0.75)), size),
            Some((UVec2::new(10, 40), UVec2::new(20, 60)))
        );
        assert_eq!(
            focus_scissor((Vec2::splat(0.3), Vec2::splat(0.3)), size),
            None
        );
    }

    // The lower layer's crop holds the active rectangle; full: the whole view, today's key_lower
    #[test]
    fn lower_crop_holds_the_active_rectangle() {
        let region = area(Vec3::new(0.0, -0.4, -0.5), Vec3::new(0.2, 0.1, 0.15));
        let rect = rects(&input(Some(region), Vec3::ZERO, Vec3::ZERO), None)[0];
        let resolution = UVec2::new(500, 400);
        let ((offset, size, _), uv) = lower_crop(&rect, fov(), fov(), resolution, false);
        let (min, max) = rect.active.unwrap();
        assert!(
            uv.0.cmple(min).all() && uv.1.cmpge(max).all(),
            "{uv:?} {min} {max}"
        );
        assert!(size.x < resolution.x || size.y < resolution.y);
        assert!((offset + size).cmple(resolution).all());
        let full = EyeRect::full(RectFallback::Dump, UVec2::new(100, 80));
        let ((offset, size, crop_fov), uv) = lower_crop(&full, fov(), fov(), resolution, false);
        assert_eq!((offset, size, crop_fov), (UVec2::ZERO, resolution, fov()));
        assert_eq!(uv, (Vec2::ZERO, Vec2::ONE));
    }

    #[test]
    fn stats_line() {
        let mut stats = RectStats::default();
        assert_eq!(stats.text(), None);
        let region = area(Vec3::new(0.0, -0.4, -0.5), Vec3::new(0.2, 0.1, 0.15));
        let eyes = rects(&input(Some(region), Vec3::ZERO, Vec3::ZERO), None);
        let mask_size = UVec2::new(100, 80);
        stats.add(&GpuRectFrame {
            eyes,
            mask_size,
            lower_crops: None,
        });
        stats.add(&GpuRectFrame {
            eyes: [EyeRect::full(RectFallback::Burst, mask_size); 2],
            mask_size,
            lower_crops: None,
        });
        let text = stats.text().unwrap();
        assert!(
            text.starts_with("rect: L full (burst), R full (burst)"),
            "{text}"
        );
        assert!(
            text.contains("max 100.0 %") && text.contains("over 2 frames"),
            "{text}"
        );
        assert!(
            text.contains("full view 1 frames (most: burst, 2 eye frames)"),
            "{text}"
        );
        assert_eq!(stats.frames(), 2);
    }

    // The cubic of `fast_srgb` against the exact sRGB decode above the threshold, measured after the
    // swapchain's own sRGB encoding in 8-bit steps: at most a third of a step, and never more than one step
    // apart after rounding
    #[test]
    fn fast_srgb_within_a_step() {
        let encode = |linear: f64| {
            let linear = linear.clamp(0.0, 1.0);
            if linear < 0.003_130_8 {
                12.92 * linear
            } else {
                1.055 * linear.powf(1.0 / 2.4) - 0.055
            }
        };
        let [c3, c2, c1, c0] = FAST_SRGB_COEFFICIENTS;
        let mut largest = 0.0_f64;
        let mut largest_rounded = 0.0_f64;
        let steps = 200_000;
        for i in 0..=steps {
            let x = i as f32 / steps as f32;
            let exact = if x < 0.04045 {
                f64::from(x) / 12.92
            } else {
                ((f64::from(x) + 0.055) / 1.055).powf(2.4)
            };
            // As the shader: the low branch unchanged, the cubic in f32 above it
            let fast = if x < 0.04045 {
                f64::from(x * (1.0 / 12.92))
            } else {
                f64::from(((c3 * x + c2) * x + c1) * x + c0)
            };
            let (a, b) = (255.0 * encode(fast), 255.0 * encode(exact));
            largest = largest.max((a - b).abs());
            largest_rounded = largest_rounded.max((a.round() - b.round()).abs());
        }
        eprintln!("fast_srgb: largest error {largest:.3} steps, after rounding {largest_rounded}");
        assert!(largest < 0.34, "{largest}");
        assert!(largest_rounded <= 1.0);
    }

    // The constants mirrored from the shaders are still there
    #[test]
    fn shader_constants_match() {
        let mask = crate::stream::depth_mask_shader_source();
        for text in [
            "max(dot(hit - origin, dir), 0.05)",
            "const ARM_LENGTH: f32 = 0.45;",
            "const ARM_RADIUS: f32 = 0.14;",
            "var hand_edge = vec2f(-0.2, -0.1);",
            "const GAP_REACH: f32 = 0.055;",
            "1.0 - smoothstep(reach - CLOUD_SOFT_EDGE, reach, abs(phi))",
        ] {
            assert!(mask.contains(text), "{text}");
        }
        assert_eq!(SHADER_MIN_T, 0.05);
        assert_eq!(SHADER_ARM_LENGTH, 0.45);
        assert_eq!(SHADER_ARM_RADIUS, 0.14);
        assert_eq!(SHADER_HAND_EDGE, -0.2);
        assert_eq!(SHADER_GAP_REACH, 0.055);
        let stream = crate::stream::stream_shader_source();
        for (name, value) in ["C3", "C2", "C1", "C0"].iter().zip(FAST_SRGB_COEFFICIENTS) {
            let line = stream
                .lines()
                .find(|line| line.contains(&format!("const FAST_SRGB_{name}: f32 = ")))
                .unwrap();
            let number: f32 = line
                .rsplit("= ")
                .next()
                .unwrap()
                .trim_end_matches(';')
                .parse()
                .unwrap();
            assert_eq!(number, value, "{name}");
        }
    }
}
