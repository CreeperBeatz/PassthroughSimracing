//! CPU side of the depth passthrough: per-frame uniforms consumed by `depth_mask.wgsl` and `stream.wgsl`
//! (`DepthUniforms`). All poses are expected in the same reference space (stage space).

use alvr_common::{
    Fov, Pose, ViewParams,
    glam::{Mat4, Quat, Vec3, Vec4},
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

/// Start and end of the tapered capsules a hand is made of. xyz: position, w: radius
#[derive(Clone, Copy)]
pub struct HandShape {
    pub segments: [[Vec4; 2]; HAND_SEGMENTS],
}

impl HandShape {
    /// `joints`: position and radius of the OpenXR hand joints
    pub fn from_joints(joints: &[Vec4; 26]) -> Self {
        const TIPS: [usize; 5] = [5, 10, 15, 20, 25];
        const PAIRS: [(usize, usize); HAND_SEGMENTS] = [
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

        // The tip joint is at the end of the finger: pull it back, so that the round end of the
        // capsule doesn't stick out
        let mut joints = *joints;
        for tip in TIPS {
            let offset = (joints[tip - 1] - joints[tip]).truncate();
            let shift = joints[tip].w.min(offset.length());
            joints[tip] += (offset.normalize_or_zero() * shift).extend(0.0);
        }

        let mut segments = [[Vec4::ZERO; 2]; HAND_SEGMENTS];
        for (segment, (start, end)) in segments.iter_mut().zip(PAIRS) {
            *segment = [joints[start], joints[end]];
        }

        Self { segments }
    }

    /// Distance between a ray and the surface of the hand, negative inside. Same as in focus_area.wgsl
    pub fn ray_distance(&self, origin: Vec3, direction: Vec3) -> f32 {
        self.segments
            .iter()
            .map(|[start, end]| {
                let axis = (*end - *start).truncate();
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
            })
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

/// Confidence zones of a hand, as distances from its surface in meters (negative inside): solid
/// passthrough up to `core`, fading out over `falloff`, and color keyed passthrough up to `rim`.
#[derive(Clone, Copy, Debug)]
pub struct HandZones {
    pub core: f32,
    pub falloff: f32,
    pub rim: f32,
}

impl HandZones {
    const MAX_CORE_SHRINK: f32 = 0.04;
    const UNTRACKED_CORE_SHRINK: f32 = 0.04;
    const MAX_RIM_GROWTH: f32 = 0.12;

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
    /// hand even in the worst case, or the hand is cut off.
    pub fn for_speed(self, speed: f32, core_error: f32, rim_error: f32) -> Self {
        Self {
            core: self.core - (speed * core_error).clamp(0.0, Self::MAX_CORE_SHRINK),
            rim: self.rim + (speed * rim_error).clamp(0.0, Self::MAX_RIM_GROWTH),
            ..self
        }
    }
}

/// The hands are passthrough, the color key decides where the hand tracking is not certain
#[derive(Clone, Copy)]
pub struct ColorKeyInput {
    pub hands: [Option<(HandShape, HandZones)>; 2],
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
}

const EYE_FLOAT_COUNT: usize = 16 + 4 + 16 + 16 + 4;
pub const DEPTH_UNIFORMS_FLOAT_COUNT: usize =
    2 * EYE_FLOAT_COUNT + 16 + 4 * 5 + 4 * (2 + 2 + 2 * 2 * HAND_SEGMENTS);
pub const DEPTH_UNIFORMS_SIZE: usize = DEPTH_UNIFORMS_FLOAT_COUNT * size_of::<f32>();

fn pose_to_mat4(pose: Pose) -> Mat4 {
    Mat4::from_rotation_translation(pose.orientation, pose.position)
}

/// Maps (u, v, 1, 0) with view-space uv (origin top-left) to a world-space ray direction
fn uv_to_world_dir(view: &ViewParams) -> Mat4 {
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

fn fov_tangents(fov: Fov) -> Vec4 {
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
            [zones.rim, zones.core, zones.falloff, 0.0]
        }));
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

    data.try_into().unwrap()
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

    #[test]
    fn layout_size() {
        // std140: must be a multiple of 16 bytes
        assert_eq!(DEPTH_UNIFORMS_SIZE % 16, 0);
        assert_eq!(
            DEPTH_UNIFORMS_SIZE,
            592 + 16 * (2 + 2 + 2 * 2 * HAND_SEGMENTS)
        );
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
        };
        let data = depth_uniforms(&cfg, &[view(), view()], &input, false);

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
        };
        let base = 2 * EYE_FLOAT_COUNT + 16 + 4 * 3;

        let data = depth_uniforms(&cfg, &[view(), view()], &input, true);
        assert_eq!(&data[base..base + 4], &[1.0, 0.8, 0.6, 0.15]);

        let data = depth_uniforms(&cfg, &[view(), view()], &input, false);
        assert_eq!(data[base], 0.0);
    }
}
