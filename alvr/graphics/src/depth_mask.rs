//! CPU side of the depth passthrough: per-frame uniforms consumed by `depth_mask.wgsl` and `stream.wgsl`
//! (`DepthUniforms`). All poses are expected in the same reference space (stage space).

use alvr_common::{
    Fov, Pose, ViewParams,
    glam::{Mat4, Quat, UVec2, Vec2, Vec3, Vec4},
};
use alvr_session::{DepthMaskRegion, DepthPassthroughConfig, DepthPassthroughDebugView};

/// One environment depth image, as returned by `xrAcquireEnvironmentDepthImageMETA`
#[derive(Clone, Copy)]
pub struct DepthFrame {
    pub swapchain_index: u32,
    pub near: f32,
    /// May be `f32::INFINITY`
    pub far: f32,
    /// Pose and fov of the virtual cameras the depth map was rendered from
    pub depth_views: [ViewParams; 2],
}

pub const HAND_SEGMENTS: usize = 28;
/// The first segments of a hand: around the wrist, and the whole palm
pub const WRIST_SEGMENTS: usize = 6;
pub const PALM_SEGMENTS: usize = 9;

/// OpenXR hand joints at the ends of each segment
pub const SEGMENT_JOINTS: [(usize, usize); HAND_SEGMENTS] = [
    // Palm
    (0, 0),
    (1, 2),
    (1, 6),
    (1, 11),
    (1, 16),
    (1, 21),
    (7, 12),
    (12, 17),
    (17, 22),
    // Thumb
    (2, 3),
    (3, 4),
    (4, 5),
    // Fingers
    (6, 7),
    (7, 8),
    (8, 9),
    (9, 10),
    (11, 12),
    (12, 13),
    (13, 14),
    (14, 15),
    (16, 17),
    (17, 18),
    (18, 19),
    (19, 20),
    (21, 22),
    (22, 23),
    (23, 24),
    (24, 25),
];

/// Start and end of the tapered capsules a hand is made of. xyz: position, w: radius
#[derive(Clone, Copy)]
pub struct HandShape {
    pub segments: [[Vec4; 2]; HAND_SEGMENTS],
}

/// Distance between a ray and the surface of a tapered capsule (xyz: position, w: radius), negative
/// inside. Same as in focus_area.wgsl
pub fn ray_capsule_distance(origin: Vec3, direction: Vec3, start: Vec4, end: Vec4) -> f32 {
    let axis = (end - start).truncate();
    let w = origin - start.truncate();
    let b = direction.dot(axis);
    let c = axis.dot(axis);
    let d = direction.dot(w);
    let e = axis.dot(w);

    let denominator = c - b * b;
    let s = if denominator > 1e-9 {
        ((e - b * d) / denominator).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let point = start.truncate() + axis * s;
    let t = (point - origin).dot(direction).max(0.0);

    (origin + direction * t).distance(point) - (start.w + (end.w - start.w) * s)
}

/// Same, for the capsule moved anywhere along the sweep. Seen along the ray, the ray is a point and the
/// moved axis covers a parallelogram: its closest point is found like the closest points of two
/// segments (the axis and the motion). Exact for capsules of constant radius in front of the eye; a
/// tapered capsule takes the radius at that point. Same as in focus_area.wgsl
pub fn ray_swept_capsule_distance(
    origin: Vec3,
    direction: Vec3,
    start: Vec4,
    end: Vec4,
    sweep: &HandSweep,
) -> f32 {
    // Per hand in the shader
    let motion = sweep.direction;
    let across = motion - direction * motion.dot(direction);
    let across_length2 = across.length_squared();
    let inv_across = if across_length2 > 1e-8 {
        1.0 / across_length2
    } else {
        0.0
    };

    let axis = (end - start).truncate();
    let w = origin - start.truncate();
    let b = direction.dot(axis);
    let d = direction.dot(w);
    // Across the ray: axis . axis, axis . w, axis . motion, motion . w
    let aa = axis.dot(axis) - b * b;
    let aw = axis.dot(w) - b * d;
    let am = axis.dot(across);
    let mw = w.dot(across);

    let inv_aa = if aa > 1e-9 { 1.0 / aa } else { 0.0 };
    // Where the axis crosses the line of the motion, the shift for that point, the point for the shift
    let det = aa - am * am * inv_across;
    let s = if det > 1e-7 * aa {
        (aw - am * mw * inv_across) / det
    } else {
        aw * inv_aa
    };
    let s = s.clamp(0.0, 1.0);
    let shift = ((mw - s * am) * inv_across).clamp(-sweep.backward, sweep.forward);
    let s = ((aw - shift * am) * inv_aa).clamp(0.0, 1.0);

    let point = start.truncate() + axis * s + motion * shift;
    let t = (point - origin).dot(direction).max(0.0);

    (origin + direction * t).distance(point) - (start.w + (end.w - start.w) * s)
}

impl HandShape {
    /// `joints`: position and radius of the OpenXR hand joints
    pub fn from_joints(joints: &[Vec4; 26]) -> Self {
        const TIPS: [usize; 5] = [5, 10, 15, 20, 25];

        // The tip joint is at the end of the finger: pull it back, so that the round end of the
        // capsule doesn't stick out
        let mut joints = *joints;
        for tip in TIPS {
            let offset = (joints[tip - 1] - joints[tip]).truncate();
            let shift = joints[tip].w.min(offset.length());
            joints[tip] += (offset.normalize_or_zero() * shift).extend(0.0);
        }

        let mut segments = [[Vec4::ZERO; 2]; HAND_SEGMENTS];
        for (segment, (start, end)) in segments.iter_mut().zip(SEGMENT_JOINTS) {
            *segment = [joints[start], joints[end]];
        }

        Self { segments }
    }

    /// Distance between a ray and the surface of the hand, negative inside. Same as in focus_area.wgsl
    pub fn ray_distance(&self, origin: Vec3, direction: Vec3) -> f32 {
        self.segments_ray_distance(HAND_SEGMENTS, origin, direction)
    }

    /// Same, for the palm only. It moves less and is tracked better than the fingers
    pub fn palm_ray_distance(&self, origin: Vec3, direction: Vec3) -> f32 {
        self.segments_ray_distance(PALM_SEGMENTS, origin, direction)
    }

    /// Same, for the hand moved anywhere along the sweep
    pub fn swept_ray_distance(&self, origin: Vec3, direction: Vec3, sweep: &HandSweep) -> f32 {
        self.segments
            .iter()
            .map(|[start, end]| ray_swept_capsule_distance(origin, direction, *start, *end, sweep))
            .fold(f32::MAX, f32::min)
    }

    fn segments_ray_distance(&self, count: usize, origin: Vec3, direction: Vec3) -> f32 {
        self.segments[..count]
            .iter()
            .map(|[start, end]| ray_capsule_distance(origin, direction, *start, *end))
            .fold(f32::MAX, f32::min)
    }

    /// Bounding sphere. xyz: position, w: radius
    pub fn bounds(&self) -> Vec4 {
        let points = self.segments.as_flattened();
        let center = points.iter().map(|p| p.truncate()).sum::<Vec3>() / points.len() as f32;
        let radius = points
            .iter()
            .map(|p| p.truncate().distance(center) + p.w)
            .fold(0.0, f32::max);

        center.extend(radius)
    }
}

/// The rim of a moving hand covers the hand moved anywhere from `backward` behind to `forward` ahead
/// along `direction`, in meters
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HandSweep {
    /// Unit vector, in the space of the hand shape
    pub direction: Vec3,
    pub forward: f32,
    pub backward: f32,
}

impl HandSweep {
    pub const NONE: Self = Self {
        direction: Vec3::ZERO,
        forward: 0.0,
        backward: 0.0,
    };
}

/// Growth of the rim along the motion of the hand instead of in all directions
/// (`debug.alvr.key_sweep*`)
#[derive(Clone, Copy, Debug)]
pub struct SweepParams {
    /// Limit of the sweep on each side, before the bias, meters
    pub max: f32,
    /// Share of the growth that stays in all directions (0..1), limited like the rim
    pub iso: f32,
    /// -1..1: positive sweeps further ahead than behind, 0: the same
    pub bias: f32,
}

/// Confidence zones of a hand, as distances from its surface in meters (negative inside): solid
/// passthrough up to `core`, fading out over `falloff`, and color keyed passthrough up to `rim`.
#[derive(Clone, Copy, Debug)]
pub struct HandZones {
    pub core: f32,
    pub falloff: f32,
    pub rim: f32,
    /// How solid the core of each segment is (0..1): only where the tracking is known to be right
    pub solid: [f32; HAND_SEGMENTS],
    /// The rim (not the solid fill) also covers the hand moved along its motion
    pub sweep: HandSweep,
}

impl HandZones {
    const MAX_CORE_SHRINK: f32 = 0.04;
    const UNTRACKED_CORE_SHRINK: f32 = 0.04;
    const MIN_SWEEP: f32 = 0.001;

    /// Hand tracking is late and predicted, so the faster the hand, the less certain its position:
    /// the solid core shrinks and the keyed rim grows.
    /// `tracked`: fraction of the joints that are seen by the cameras. The others are guessed, then the
    /// solid core would show a hand-shaped piece of the room.
    pub fn for_confidence(self, tracked: f32) -> Self {
        Self {
            core: self.core - (1.0 - tracked.clamp(0.0, 1.0)) * Self::UNTRACKED_CORE_SHRINK,
            ..self
        }
    }

    /// The errors are the assumed timing errors of the hand pose, in seconds. The rim must contain the
    /// hand even in the worst case, or the hand is cut off. With `sweep` the rim grows along the
    /// `velocity` only, except for a share in all directions (fingers and rotation).
    pub fn for_speed(
        self,
        velocity: Vec3,
        core_error: f32,
        rim_error: f32,
        max_growth: f32,
        sweep: Option<SweepParams>,
    ) -> Self {
        let speed = velocity.length();
        let growth = speed * rim_error;
        let (rim_growth, sweep) = match (sweep, velocity.try_normalize()) {
            // A hand that barely moves grows in all directions: the shader is cheaper without a sweep
            (Some(params), Some(direction)) if growth.min(params.max) >= Self::MIN_SWEEP => {
                let length = growth.min(params.max);
                let bias = params.bias.clamp(-1.0, 1.0);
                let sweep = HandSweep {
                    direction,
                    forward: length * (1.0 + bias),
                    backward: length * (1.0 - bias),
                };

                (growth * params.iso, sweep)
            }
            _ => (growth, self.sweep),
        };

        Self {
            core: self.core - (speed * core_error).clamp(0.0, Self::MAX_CORE_SHRINK),
            rim: self.rim + rim_growth.clamp(0.0, max_growth),
            sweep,
            ..self
        }
    }

    /// Where the background around the hand looks like the hand, the color key shows it: `leak` (0..1)
    /// narrows the rim down to `inset` inside the hand.
    pub fn for_leak(self, leak: f32, inset: f32) -> Self {
        if leak <= 0.0 {
            return self;
        }
        let leak = leak.min(1.0);
        Self {
            rim: self.rim * (1.0 - leak) - inset * leak,
            sweep: HandSweep {
                forward: self.sweep.forward * (1.0 - leak),
                backward: self.sweep.backward * (1.0 - leak),
                ..self.sweep
            },
            ..self
        }
    }
}

/// One more capsule of a hand, from the wrist joint towards the forearm (`debug.alvr.key_wrist_*`).
/// The hand capsules end at the wrist joint and the runtime removes the hand and a bit of the wrist from
/// the depth map, so without it there is a gap. Its direction is chosen with the camera.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WristBridge {
    /// The wrist joint
    pub start: Vec3,
    pub end: Vec3,
    pub radius: f32,
    /// How solid its core is (0..1)
    pub solid: f32,
}

impl WristBridge {
    /// `direction`: unit vector from the wrist to the forearm. None if the length or the radius is 0 (off)
    pub fn new(wrist: Vec3, direction: Vec3, length: f32, radius: f32, solid: f32) -> Option<Self> {
        (length > 0.0 && radius > 0.0).then(|| Self {
            start: wrist,
            end: wrist + direction * length,
            radius,
            solid: solid.clamp(0.0, 1.0),
        })
    }

    /// Solid only if the camera is sure enough of the direction (`confidence` at least `limit`), then
    /// faded with the speed like the verified segments (`stillness`, 0..1)
    pub fn solid_target(confidence: f32, limit: f32, stillness: f32) -> f32 {
        if confidence >= limit && confidence > 0.0 {
            stillness.clamp(0.0, 1.0)
        } else {
            0.0
        }
    }

    /// Start and end, xyz: position, w: radius
    pub fn capsule(&self) -> [Vec4; 2] {
        [self.start.extend(self.radius), self.end.extend(self.radius)]
    }
}

/// The hands are passthrough, the color key decides where the hand tracking is not certain
#[derive(Clone, Copy)]
pub struct ColorKeyInput {
    pub hands: [Option<(HandShape, HandZones)>; 2],
    /// Debug view: how much the rim of each hand is narrowed (0..1), shown in red with the tint
    pub leak: [f32; 2],
    /// Of each hand, only used with the hand. None: off
    pub bridges: [Option<WristBridge>; 2],
}

/// Motion of an arm since the depth image was captured
#[derive(Clone, Copy)]
pub struct ArmMotion {
    /// Now
    pub wrist: Vec3,
    /// From the wrist to the elbow
    pub direction: Vec3,
    pub displacement: Vec3,
}

/// Per-frame depth inputs for the stream renderer
#[derive(Clone, Copy)]
pub struct DepthRenderInput {
    /// None if the runtime has not produced a depth image yet
    pub frame: Option<DepthFrame>,
    /// Eye positions at display time
    pub eye_positions: [Vec3; 2],
    /// Set if the passthrough is color keyed around the hands
    pub color_key: Option<ColorKeyInput>,
    /// The depth map of the arms is shifted by their motion. None: plain depth
    pub arms: [Option<ArmMotion>; 2],
    /// Remember the current depth image as the playzone. There must be no hands in it
    pub capture_playzone: bool,
    /// Debug view (with "Tint mask" and the color key): what only the playzone shows is blue
    pub playzone_view: bool,
    /// With the color key: the part of each view (min and max view uv) where the lower game layer is
    /// shown. Outside of it the upper layer shows what both would. None: the whole view
    pub key_lower_rects: Option<[(Vec2, Vec2); 2]>,
}

const EYE_FLOAT_COUNT: usize = 16 + 4 + 16 + 16 + 4;
pub const DEPTH_UNIFORMS_FLOAT_COUNT: usize = 2 * EYE_FLOAT_COUNT
    + 16
    + 4 * 5
    + 4 * (2 + 2 * HAND_SEGMENTS / 4 + 2 + 2 * 2 * HAND_SEGMENTS + 6 + 2 * 9 + 1 + 2 + 2 + 2 * 2);
pub const DEPTH_UNIFORMS_SIZE: usize = DEPTH_UNIFORMS_FLOAT_COUNT * size_of::<f32>();

pub(crate) fn pose_to_mat4(pose: Pose) -> Mat4 {
    Mat4::from_rotation_translation(pose.orientation, pose.position)
}

/// Maps (u, v, 1, 0) with view-space uv (origin top-left) to a world-space ray direction
pub(crate) fn uv_to_world_dir(view: &ViewParams) -> Mat4 {
    let tanl = view.fov.left.tan();
    let tanr = view.fov.right.tan();
    let tanu = view.fov.up.tan();
    let tand = view.fov.down.tan();

    let uv_to_view_dir = Mat4::from_cols(
        Vec4::new(tanr - tanl, 0.0, 0.0, 0.0),
        Vec4::new(0.0, -(tanu - tand), 0.0, 0.0),
        Vec4::new(tanl, tanu, -1.0, 0.0),
        Vec4::ZERO,
    );

    Mat4::from_quat(view.pose.orientation) * uv_to_view_dir
}

/// Left, right, down, up
pub(crate) fn fov_tangents(fov: Fov) -> Vec4 {
    Vec4::new(
        fov.left.tan(),
        fov.right.tan(),
        fov.down.tan(),
        fov.up.tan(),
    )
}

/// Build the `DepthUniforms` buffer content.
///
/// `stream_views` are the poses/fovs the streamed image was rendered with (the projection layer
/// views). Only their orientation and fov are used, to convert a pixel into a ray direction.
pub fn depth_uniforms(
    config: &DepthPassthroughConfig,
    stream_views: &[ViewParams; 2],
    input: &DepthRenderInput,
    playzone: Option<&[ViewParams; 2]>,
    lighting_enabled: bool,
) -> [f32; DEPTH_UNIFORMS_FLOAT_COUNT] {
    let mut data = Vec::with_capacity(DEPTH_UNIFORMS_FLOAT_COUNT);

    for (view_idx, (stream_view, eye_position)) in
        stream_views.iter().zip(input.eye_positions).enumerate()
    {
        data.extend(uv_to_world_dir(stream_view).to_cols_array());
        data.extend(eye_position.extend(1.0).to_array());

        if let Some(frame) = &input.frame {
            let depth_view_to_world = pose_to_mat4(frame.depth_views[view_idx].pose);
            data.extend(depth_view_to_world.inverse().to_cols_array());
            data.extend(depth_view_to_world.to_cols_array());
            data.extend(fov_tangents(frame.depth_views[view_idx].fov).to_array());
        } else {
            data.extend(Mat4::IDENTITY.to_cols_array());
            data.extend(Mat4::IDENTITY.to_cols_array());
            data.extend(Vec4::new(-1.0, 1.0, -1.0, 1.0).to_array());
        }
    }

    let (region_mode, max_distance, world_to_volume, half_extents) = match &config.region {
        DepthMaskRegion::Distance { max_distance } => {
            (0.0, *max_distance, Mat4::IDENTITY, Vec3::ZERO)
        }
        DepthMaskRegion::Volume(volume) => {
            let volume_to_world = Mat4::from_rotation_translation(
                Quat::from_rotation_y(volume.yaw_deg.to_radians()),
                Vec3::new(volume.center_x, volume.center_y, volume.center_z),
            );
            (
                1.0,
                0.0,
                volume_to_world.inverse(),
                Vec3::new(volume.width, volume.height, volume.depth) * 0.5,
            )
        }
    };
    data.extend(world_to_volume.to_cols_array());
    data.extend(half_extents.extend(0.0).to_array());

    let debug_view = match config.debug_view {
        DepthPassthroughDebugView::Off => 0.0,
        DepthPassthroughDebugView::TintMask => 1.0,
        DepthPassthroughDebugView::ShowDepth => 2.0,
    };
    data.extend([
        region_mode,
        max_distance,
        config.edge_feathering,
        debug_view,
    ]);

    let (near, far, available) = if let Some(frame) = &input.frame {
        let far = if frame.far.is_finite() && frame.far > frame.near {
            frame.far
        } else {
            0.0 // infinite
        };
        (frame.near, far, 1.0)
    } else {
        (0.0, 0.0, 0.0)
    };
    data.extend([
        near,
        far,
        available,
        f32::from(u8::from(config.flip_depth_y)),
    ]);

    if let Some(lighting) = config.lighting.as_option().filter(|_| lighting_enabled) {
        data.extend([
            1.0,
            lighting.base_brightness,
            lighting.strength,
            lighting.reference_luminance,
        ]);
        data.extend([lighting.min_brightness, lighting.locality]);
    } else {
        data.extend([0.0; 6]);
    }
    let hands = input.color_key.map_or([None; 2], |key| key.hands);
    data.extend([f32::from(u8::from(input.color_key.is_some())), 0.0]);
    for hand in &hands {
        data.extend(hand.map_or([0.0; 4], |(_, zones)| {
            [zones.rim, zones.core, zones.falloff, zones.sweep.backward]
        }));
    }
    for hand in &hands {
        data.extend(hand.map_or([0.0; HAND_SEGMENTS], |(_, zones)| zones.solid));
    }
    for hand in &hands {
        data.extend(
            hand.map_or(Vec4::ZERO, |(shape, _)| shape.bounds())
                .to_array(),
        );
    }
    for hand in &hands {
        let segments = hand.map_or([[Vec4::ZERO; 2]; HAND_SEGMENTS], |(shape, _)| {
            shape.segments
        });
        for point in segments.as_flattened() {
            data.extend(point.to_array());
        }
    }
    for arm in &input.arms {
        let [wrist, direction, displacement] = arm.map_or([Vec3::ZERO; 3], |arm| {
            [arm.wrist, arm.direction, arm.displacement]
        });
        for v in [wrist, direction, displacement] {
            data.extend(v.extend(0.0).to_array());
        }
    }

    for view_idx in 0..2 {
        let (pose, tangents) = playzone.map_or((Mat4::IDENTITY, Vec4::ONE), |views| {
            (
                pose_to_mat4(views[view_idx].pose),
                fov_tangents(views[view_idx].fov),
            )
        });
        data.extend(pose.inverse().to_cols_array());
        data.extend(pose.to_cols_array());
        data.extend(tangents.to_array());
    }
    let leak = input.color_key.map_or([0.0; 2], |key| key.leak);
    data.extend([
        f32::from(u8::from(playzone.is_some())),
        leak[0],
        leak[1],
        f32::from(u8::from(input.playzone_view)),
    ]);
    for (min, max) in input
        .key_lower_rects
        .unwrap_or([(Vec2::ZERO, Vec2::ONE); 2])
    {
        data.extend([min.x, min.y, max.x, max.y]);
    }
    for hand in &hands {
        data.extend(hand.map_or([0.0; 4], |(_, zones)| {
            zones.sweep.direction.extend(zones.sweep.forward).to_array()
        }));
    }
    // Radius 0: no bridge
    let bridges = input.color_key.map_or([None; 2], |key| key.bridges);
    for (hand, bridge) in hands.iter().zip(bridges) {
        let [start, end] = bridge
            .filter(|_| hand.is_some())
            .map_or([Vec4::ZERO; 2], |bridge| {
                [
                    bridge.start.extend(bridge.radius),
                    bridge.end.extend(bridge.solid),
                ]
            });
        data.extend(start.to_array());
        data.extend(end.to_array());
    }

    data.try_into().unwrap()
}

// Distance of a point (in the space of the view) outside the side planes of the view (through its
// origin), in meters. Tangents: left, down and right, up
fn outside_view_sides(tan_min: Vec2, tan_max: Vec2, local: Vec3) -> f32 {
    let depth = -local.z;
    [
        (tan_min.x * depth - local.x) / tan_min.x.hypot(1.0),
        (local.x - tan_max.x * depth) / tan_max.x.hypot(1.0),
        (tan_min.y * depth - local.y) / tan_min.y.hypot(1.0),
        (local.y - tan_max.y * depth) / tan_max.y.hypot(1.0),
    ]
    .into_iter()
    .fold(f32::MIN, f32::max)
}

/// Whether a sphere is at least partly inside the view (pose and fov), in front of it. Conservative
/// near the corners of the view
pub fn sphere_in_view(view: &ViewParams, center: Vec3, radius: f32) -> bool {
    let local = view.pose.orientation.inverse() * (center - view.pose.position);
    let tan_min = Vec2::new(view.fov.left.tan(), view.fov.down.tan());
    let tan_max = Vec2::new(view.fov.right.tan(), view.fov.up.tan());

    -local.z + radius > 0.0 && outside_view_sides(tan_min, tan_max, local) <= radius
}

/// Part of the view that contains the hands with their rim: min and max view uv. None if there is nothing.
pub fn hands_view_rect(
    view: &ViewParams,
    eye_position: Vec3,
    color_key: &ColorKeyInput,
) -> Option<(Vec2, Vec2)> {
    const MARGIN: f32 = 0.01;
    // See focus_area.wgsl
    const PLAYZONE_RANGE: f32 = 0.06;
    const NEAR: f32 = 0.05;

    let tan_min = Vec2::new(view.fov.left.tan(), view.fov.down.tan());
    let tan_max = Vec2::new(view.fov.right.tan(), view.fov.up.tan());
    let outside_sides = |local: Vec3| outside_view_sides(tan_min, tan_max, local);

    color_key
        .hands
        .iter()
        .zip(color_key.bridges)
        .filter_map(|(hand, bridge)| Some((hand.as_ref()?, bridge)))
        .flat_map(|((shape, zones), bridge)| {
            let rim = zones.rim.max(0.0) + MARGIN;
            // Both ends of the sweep: the moved capsules lie in between
            let shifts = [0.0, zones.sweep.forward, -zones.sweep.backward]
                .map(|shift| zones.sweep.direction * shift);
            // The bridge is one more capsule of the hand
            let bridge = bridge.map(|bridge| bridge.capsule());
            shape
                .segments
                .iter()
                .chain(&bridge)
                .flatten()
                .flat_map(move |point| {
                    shifts.map(|shift| (point.truncate() + shift, point.w + rim))
                })
                // Where the playzone is used
                .chain([(shape.bounds().truncate(), shape.bounds().w + PLAYZONE_RANGE)])
                .collect::<Vec<_>>()
        })
        .filter_map(|(position, radius)| {
            let local = view.pose.orientation.inverse() * (position - eye_position);
            // Behind the near plane or beside the view: not visible
            if -local.z + radius < NEAR || outside_sides(local) > radius {
                return None;
            }
            let depth = -local.z - radius;
            if depth < NEAR {
                // Conservative where it crosses the near plane
                return Some((Vec2::ZERO, Vec2::ONE));
            }

            let corners = [-radius, radius].map(|r| {
                [depth, -local.z + radius].map(|d| {
                    let uv = ((local.truncate() + r) / d - tan_min) / (tan_max - tan_min);
                    Vec2::new(uv.x, 1.0 - uv.y)
                })
            });
            let corners = corners.as_flattened();
            let min = corners.iter().copied().fold(Vec2::MAX, Vec2::min);
            let max = corners.iter().copied().fold(Vec2::MIN, Vec2::max);

            Some((
                min.clamp(Vec2::ZERO, Vec2::ONE),
                max.clamp(Vec2::ZERO, Vec2::ONE),
            ))
        })
        .filter(|(min, max)| min.cmplt(*max).all())
        .reduce(|a, b| (a.0.min(b.0), a.1.max(b.1)))
}

/// Part of a view's image that contains `rect` (view uv, origin top-left) grown by `margin`: pixel
/// offset and size, and the fov of exactly these pixels, for a projection layer with the same pose.
/// `top_origin`: pixel row 0 is the top of the view. OpenXR on OpenGL ES uses the bottom.
pub fn crop_view(
    fov: Fov,
    resolution: UVec2,
    rect: (Vec2, Vec2),
    margin: f32,
    top_origin: bool,
) -> (UVec2, UVec2, Fov) {
    let resolution = resolution.max(UVec2::ONE);
    let size = resolution.as_vec2();
    let min = (rect.0 - margin).clamp(Vec2::ZERO, Vec2::ONE);
    let max = (rect.1 + margin).clamp(Vec2::ZERO, Vec2::ONE);
    let (row_min, row_max) = if top_origin {
        (min.y, max.y)
    } else {
        (1.0 - max.y, 1.0 - min.y)
    };

    let start = (Vec2::new(min.x, row_min) * size)
        .floor()
        .as_uvec2()
        .min(resolution - 1);
    let end = (Vec2::new(max.x, row_max) * size)
        .ceil()
        .as_uvec2()
        .clamp(start + 1, resolution);

    // The image is linear in the tangents of the view
    let angle_x = |column: u32| {
        let f = column as f32 / size.x;
        (fov.left.tan() * (1.0 - f) + fov.right.tan() * f).atan()
    };
    let angle_y = |row: u32| {
        let f = row as f32 / size.y;
        let from_bottom = if top_origin { 1.0 - f } else { f };
        (fov.down.tan() * (1.0 - from_bottom) + fov.up.tan() * from_bottom).atan()
    };
    let (down, up) = if top_origin {
        (angle_y(end.y), angle_y(start.y))
    } else {
        (angle_y(start.y), angle_y(end.y))
    };

    (
        start,
        end - start,
        Fov {
            left: angle_x(start.x),
            right: angle_x(end.x),
            up,
            down,
        },
    )
}

/// The pixels of `crop_view` (offset and size) as min and max view uv (origin top-left). A pixel is
/// inside if its center is >= min and < max.
pub fn crop_view_uv(
    resolution: UVec2,
    offset: UVec2,
    size: UVec2,
    top_origin: bool,
) -> (Vec2, Vec2) {
    let resolution = resolution.max(UVec2::ONE).as_vec2();
    let start = offset.as_vec2() / resolution;
    let end = (offset + size).as_vec2() / resolution;
    if top_origin {
        (start, end)
    } else {
        (
            Vec2::new(start.x, 1.0 - end.y),
            Vec2::new(end.x, 1.0 - start.y),
        )
    }
}

/// Encode a positive view-space distance as a [0, 1] OpenGL depth buffer value (inverse of
/// `linearize_depth` in depth_mask.wgsl). Used for tests and synthetic data.
pub fn encode_depth(distance: f32, near: f32, far: f32) -> f32 {
    let ndc = if far.is_finite() && far > near {
        ((far + near) - 2.0 * near * far / distance) / (far - near)
    } else {
        1.0 - 2.0 * near / distance
    };
    ndc * 0.5 + 0.5
}

#[cfg(test)]
mod tests {
    use super::*;
    use alvr_session::settings_schema::Switch;
    use alvr_session::{CockpitVolumeConfig, PassthroughLightingConfig};

    fn config(region: DepthMaskRegion) -> DepthPassthroughConfig {
        DepthPassthroughConfig {
            region,
            edge_feathering: 0.02,
            lighting: Switch::Disabled,
            debug_view: DepthPassthroughDebugView::Off,
            flip_depth_y: false,
        }
    }

    fn view() -> ViewParams {
        ViewParams {
            pose: Pose::IDENTITY,
            fov: Fov {
                left: -0.8,
                right: 0.7,
                up: 0.75,
                down: -0.85,
            },
        }
    }

    // Deterministic random numbers (xorshift)
    struct Random(u32);

    impl Random {
        fn range(&mut self, min: f32, max: f32) -> f32 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 17;
            self.0 ^= self.0 << 5;
            min + (max - min) * (self.0 >> 8) as f32 / (1 << 24) as f32
        }

        fn vec3(&mut self, size: f32) -> Vec3 {
            Vec3::new(
                self.range(-size, size),
                self.range(-size, size),
                self.range(-size, size),
            )
        }
    }

    // The swept distance against the minimum over copies of the hand moved along the sweep, on random
    // rays near random hands. Constant radius: exact (never above the copies, at most half their
    // spacing below). Tapered: the radius is taken where the axis is closest, so where the motion is
    // (almost) along a capsule the thicker end may be missed: up to the difference of its radii above.
    #[test]
    fn swept_distance_matches_brute_force() {
        const COPIES: usize = 16;
        let mut random = Random(0x1234_5678);

        for taper in [0.0, 0.003] {
            // How much the swept distance is below the copies (in their spacing) / above them (m)
            let (mut below, mut above) = (0.0_f32, 0.0_f32);
            for _ in 0..300 {
                let center = Vec3::new(
                    random.range(-0.2, 0.2),
                    random.range(-0.2, 0.1),
                    -random.range(0.25, 0.6),
                );
                let segments = std::array::from_fn(|i| {
                    let start = center + random.vec3(0.06);
                    // Some are spheres, like the first segment of a real hand
                    let end = if i % 7 == 0 {
                        start
                    } else {
                        start + random.vec3(0.04)
                    };
                    let radius = random.range(0.005, 0.012);
                    let end_radius = radius + random.range(-taper, taper);
                    [start.extend(radius), end.extend(end_radius)]
                });
                let shape = HandShape { segments };
                let length = random.range(0.0, 0.04);
                let bias = random.range(-1.0, 1.0);
                let sweep = HandSweep {
                    direction: random.vec3(1.0).normalize_or(Vec3::X),
                    forward: length * (1.0 + bias),
                    backward: length * (1.0 - bias),
                };
                let spacing = (sweep.forward + sweep.backward) / (COPIES - 1) as f32;

                for _ in 0..50 {
                    let origin = random.vec3(0.03);
                    let direction = (center + random.vec3(0.1) - origin).normalize();
                    let swept = shape.swept_ray_distance(origin, direction, &sweep);
                    let copies = (0..COPIES)
                        .map(|i| {
                            let shift = sweep.direction * (spacing * i as f32 - sweep.backward);
                            let segments = shape
                                .segments
                                .map(|ends| ends.map(|p| p + shift.extend(0.0)));
                            HandShape { segments }.ray_distance(origin, direction)
                        })
                        .fold(f32::MAX, f32::min);

                    // Between copies the distance can only be lower, by at most half their spacing
                    assert!(swept >= copies - 0.5 * spacing - 1e-5, "{swept} {copies}");
                    assert!(swept <= copies + taper + 1e-5, "{swept} {copies}");
                    below = below.max((copies - swept) / spacing.max(1e-6));
                    above = above.max(swept - copies);
                }
            }
            eprintln!("taper {taper} m: below {below} x spacing, above {above} m");
        }
    }

    #[test]
    fn swept_distance_without_sweep_is_the_same() {
        let start = Vec4::new(-0.05, 0.02, -0.4, 0.01);
        let end = Vec4::new(0.03, -0.01, -0.45, 0.008);
        let mut random = Random(42);
        for _ in 0..1000 {
            let direction = (Vec3::new(0.0, 0.0, -0.42) + random.vec3(0.1)).normalize();
            let plain = ray_capsule_distance(Vec3::ZERO, direction, start, end);
            for sweep in [
                HandSweep::NONE,
                HandSweep {
                    direction: random.vec3(1.0).normalize(),
                    ..HandSweep::NONE
                },
            ] {
                let swept = ray_swept_capsule_distance(Vec3::ZERO, direction, start, end, &sweep);
                assert!((swept - plain).abs() < 1e-6, "{swept} {plain}");
            }
        }
    }

    #[test]
    fn layout_size() {
        // std140: must be a multiple of 16 bytes
        assert_eq!(DEPTH_UNIFORMS_SIZE % 16, 0);
        assert_eq!(
            DEPTH_UNIFORMS_SIZE,
            592 + 16
                * (2 + 2 * HAND_SEGMENTS / 4 + 2 + 2 * 2 * HAND_SEGMENTS + 6 + 2 * 9 + 1 + 2 + 2)
                // The bridges
                + 16 * 4
        );
        // 28 capsules per hand, and the bridges from the wrists
        assert_eq!(DEPTH_UNIFORMS_SIZE, 3200);
    }

    #[test]
    fn uv_corners_map_to_fov_edges() {
        let v = view();
        let m = uv_to_world_dir(&v);

        let top_left = m * Vec4::new(0.0, 0.0, 1.0, 0.0);
        assert!((top_left.x - v.fov.left.tan()).abs() < 1e-5);
        assert!((top_left.y - v.fov.up.tan()).abs() < 1e-5);
        assert!((top_left.z + 1.0).abs() < 1e-5);

        let bottom_right = m * Vec4::new(1.0, 1.0, 1.0, 0.0);
        assert!((bottom_right.x - v.fov.right.tan()).abs() < 1e-5);
        assert!((bottom_right.y - v.fov.down.tan()).abs() < 1e-5);
    }

    #[test]
    fn sphere_in_view_tests_the_frustum() {
        let mut v = view();
        v.pose.position = Vec3::new(0.1, 1.5, 0.0);
        // Turned 90 degrees to the left: forward is -X
        v.pose.orientation = Quat::from_rotation_y(std::f32::consts::FRAC_PI_2);
        let ahead = |x: f32, z: f32| v.pose.position + Vec3::new(-x, 0.0, z);

        assert!(sphere_in_view(&v, ahead(0.5, 0.0), 0.01));
        // Behind, unless it reaches the eye
        assert!(!sphere_in_view(&v, ahead(-0.3, 0.0), 0.1));
        assert!(sphere_in_view(&v, ahead(-0.3, 0.0), 0.4));
        // Beside: right of the view at 45 degrees, its right edge is at 0.7 rad
        let beside = ahead(0.5, -0.5);
        let tan = v.fov.right.tan();
        let outside = (0.5 - tan * 0.5) / tan.hypot(1.0);
        assert!(!sphere_in_view(&v, beside, outside - 0.01));
        assert!(sphere_in_view(&v, beside, outside + 0.01));
    }

    #[test]
    fn uv_dir_follows_view_rotation() {
        let mut v = view();
        // Turn 90° to the left: forward (-Z) becomes -X
        v.pose.orientation = Quat::from_rotation_y(std::f32::consts::FRAC_PI_2);
        let m = uv_to_world_dir(&v);

        let tanl = v.fov.left.tan();
        let tanr = v.fov.right.tan();
        let center_u = -tanl / (tanr - tanl);
        let tanu = v.fov.up.tan();
        let tand = v.fov.down.tan();
        let center_v = tanu / (tanu - tand);

        let dir = (m * Vec4::new(center_u, center_v, 1.0, 0.0)).truncate();
        assert!((dir - Vec3::NEG_X).length() < 1e-5, "{dir}");
    }

    #[test]
    fn depth_encoding_roundtrip() {
        // Same math as linearize_depth() in stream.wgsl
        fn linearize(depth: f32, near: f32, far: f32) -> f32 {
            let ndc = depth * 2.0 - 1.0;
            if far <= near {
                2.0 * near / (1.0 - ndc).max(1e-6)
            } else {
                2.0 * near * far / ((far + near) - ndc * (far - near))
            }
        }

        for distance in [0.15, 0.4, 1.0, 3.0, 10.0] {
            let d = encode_depth(distance, 0.1, f32::INFINITY);
            assert!((linearize(d, 0.1, 0.0) - distance).abs() / distance < 1e-4);

            let d = encode_depth(distance, 0.1, 20.0);
            assert!((linearize(d, 0.1, 20.0) - distance).abs() / distance < 1e-4);
        }
    }

    #[test]
    fn volume_transform() {
        let cfg = config(DepthMaskRegion::Volume(CockpitVolumeConfig {
            center_x: 1.0,
            center_y: 0.8,
            center_z: -2.0,
            yaw_deg: 90.0,
            width: 1.0,
            height: 1.0,
            depth: 2.0,
        }));
        let input = DepthRenderInput {
            frame: None,
            eye_positions: [Vec3::ZERO; 2],
            color_key: None,
            arms: [None; 2],
            capture_playzone: false,
            playzone_view: false,
            key_lower_rects: None,
        };
        let data = depth_uniforms(&cfg, &[view(), view()], &input, None, false);

        let base = 2 * EYE_FLOAT_COUNT;
        let world_to_volume = Mat4::from_cols_slice(&data[base..base + 16]);
        let local = world_to_volume.transform_point3(Vec3::new(1.0, 0.8, -2.0));
        assert!(local.length() < 1e-5);

        // With 90° yaw (counter-clockwise from above), local +Z points to world +X, so world -X
        // maps to local -Z (the box "depth" axis)
        let local = world_to_volume.transform_point3(Vec3::new(0.0, 0.8, -2.0));
        assert!(
            (local - Vec3::new(0.0, 0.0, -1.0)).length() < 1e-5,
            "{local}"
        );

        assert_eq!(&data[base + 16..base + 19], &[0.5, 0.5, 1.0]);
        // region mode = volume
        assert_eq!(data[base + 20], 1.0);
        // depth unavailable
        assert_eq!(data[base + 26], 0.0);
    }

    #[test]
    fn lighting_disabled_when_not_rendered() {
        let mut cfg = config(DepthMaskRegion::Distance { max_distance: 0.9 });
        cfg.lighting = Switch::Enabled(PassthroughLightingConfig {
            base_brightness: 0.8,
            strength: 0.6,
            reference_luminance: 0.15,
            min_brightness: 0.15,
            locality: 0.0,
            response_speed: 0.2,
        });
        let input = DepthRenderInput {
            frame: None,
            eye_positions: [Vec3::ZERO; 2],
            color_key: None,
            arms: [None; 2],
            capture_playzone: false,
            playzone_view: false,
            key_lower_rects: None,
        };
        let base = 2 * EYE_FLOAT_COUNT + 16 + 4 * 3;

        let data = depth_uniforms(&cfg, &[view(), view()], &input, None, true);
        assert_eq!(&data[base..base + 4], &[1.0, 0.8, 0.6, 0.15]);

        let data = depth_uniforms(&cfg, &[view(), view()], &input, None, false);
        assert_eq!(data[base], 0.0);
    }
}
