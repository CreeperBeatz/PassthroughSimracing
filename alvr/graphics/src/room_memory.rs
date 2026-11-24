//! The room memory (`debug.alvr.mem_roll`, U3 of `design/universal/final.md`; step 3 of
//! `docs/mask-v2-design.md`, section 4.3, with the corrections of `docs/mask-v2-check.md`). CPU side:
//! the camera, when it is anchored again, the uniforms of room_memory.wgsl, the shares of the `memory:`
//! line. The GPU side is in stream.rs (`RoomMemory`).
//!
//! Today's playzone is one copy of the depth, taken only when no hand was near the depth view for 1.5 s.
//! While the user drives it is frozen everywhere, and the old places of moved things (the bottle used as
//! a shifter, legs, the chair) are drawn from it near the hands: the user's "transparent ghost" [measured,
//! logs/analysis-20261124-bottle]. The memory instead is written from the live depth on every new depth
//! image, except near the hands at the depth's capture time, where it is kept (frozen) because the
//! runtime removes the hands and what they hold. The mask reads only frozen texels.
//!
//! - Camera: fixed in stage space, 120 x 120 degrees, `mem_size` square (512: 0.0068 per texel in tangent
//!   units, the depth's 0.0069). At the middle of the two depth cameras when anchored. Aim: the head's yaw
//!   at that moment, pitched `mem_pitch_deg` (35) below the horizon, no roll. The design's "towards the
//!   centre of the play area" turned 90-180 degrees away from the hands for this play area, which holds
//!   the seat [measured, mask-v2-check.md L4].
//! - Anchored again: with no hand near the depth view for 1.5 s, cleanly (cleared, everything from the
//!   live depth) when the head is 3 cm from the anchor or its yaw 40 degrees from the aim; otherwise (a
//!   hand near) when the head is 12 cm away or turned 40 degrees, by one re-projection that keeps the
//!   frozen texels. A changed `mem_pitch_deg` counts as turned.
//! - Near: the segment from the eye's depth camera to the remembered point (else the live one) passes
//!   within `mem_margin_mm` (60) of a hand capsule at the depth's capture time, taken from the arm's own
//!   sample history (the runtime answers past-time lookups with its newest sample). Where that time is not
//!   in the history, the hand's last known capsules grown by 3 cm (`FALLBACK_GROWTH`): a failed lookup
//!   must never let the runtime's fill be written over the memory.
//!
//! Pure maths, so it can be tested on the PC.

use crate::{HAND_SEGMENTS, HandShape};
use alvr_common::{
    Fov, Pose, ViewParams,
    glam::{Quat, Vec3, Vec4},
};
use wgpu::TextureFormat;

pub const MEMORY_FORMAT: TextureFormat = TextureFormat::Rgba16Float;
/// Half of the field of view of the memory camera, both ways
pub const MEMORY_HALF_FOV_DEG: f32 = 60.0;
/// `debug.alvr.mem_size`: 512 (default) or 384
pub const MEMORY_SIZES: [u32; 2] = [512, 384];
pub const DEFAULT_MEMORY_SIZE: u32 = 512;
pub const DEFAULT_MARGIN_MM: i64 = 60;
pub const DEFAULT_PITCH_DEG: i64 = 35;
/// A live surface counts for a texel if it lies within this many texel widths of its ray
pub const LIVE_TOLERANCE_TEXELS: f32 = 1.5;
/// The last known capsules of a hand, used when the capture time is not in its history, are grown by
/// this (m)
pub const FALLBACK_GROWTH: f32 = 0.03;
/// With no hand near the depth view: anchored again cleanly when the head moved this far (m)
pub const CLEAN_MOVE: f32 = 0.03;
/// With a hand near: anchored again by a re-projection when the head moved this far (m)
pub const HANDS_MOVE: f32 = 0.12;
/// Either way when the head's yaw is this far from the aim (degrees)
pub const MAX_TURN_DEG: f32 = 40.0;
/// Cells per side of the stats pass (`stats_main`), each two texels wide
pub const MEMORY_STATS_CELLS: u32 = 16;
/// Seconds of the last update given to the shader at most (the age, the fall of `moving`)
const MAX_UPDATE_DT: f32 = 1.0;

/// `debug.alvr.mem_*` while `mem_roll` is 1
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MemoryParams {
    /// `mem_margin_mm` in meters
    pub margin: f32,
    /// `mem_outer_mm` in meters, 0: no outer ring (also when it is not beyond the margin)
    pub outer: f32,
    /// Texels per side
    pub size: u32,
    /// `mem_pitch_deg` in degrees below the horizon
    pub pitch_deg: f32,
    /// `mem_view`: 0 off, 1 what only the memory shows is tinted blue (the playzone view), 2 paler the
    /// older it is
    pub view: u8,
    /// `mem_moving 1`: texels marked moving are not read even when frozen
    pub skip_moving: bool,
}

impl Default for MemoryParams {
    fn default() -> Self {
        Self {
            margin: DEFAULT_MARGIN_MM as f32 / 1000.0,
            outer: 0.0,
            size: DEFAULT_MEMORY_SIZE,
            pitch_deg: DEFAULT_PITCH_DEG as f32,
            view: 0,
            skip_moving: false,
        }
    }
}

impl MemoryParams {
    /// How far beyond a hand capsule a texel can be frozen
    pub fn reach(&self) -> f32 {
        self.margin.max(self.outer)
    }

    fn outer_ring(&self) -> f32 {
        if self.outer > self.margin {
            self.outer
        } else {
            0.0
        }
    }
}

/// Where the capsules of a hand at the depth's capture time come from
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureHand {
    /// Not tracked (no history, no recent last known capsules): no hand near
    None,
    /// The arm's own sample history at the capture time
    Exact,
    /// The last known capsules, grown by `FALLBACK_GROWTH`
    Fallback,
}

impl CaptureHand {
    pub fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Exact => "exact",
            Self::Fallback => "fallback",
        }
    }
}

/// Per frame, from the client, while `debug.alvr.mem_roll` is 1 (`DepthRenderInput::memory`)
#[derive(Clone, Copy)]
pub struct MemoryInput {
    pub params: MemoryParams,
    /// When the depth image of this frame was captured (XrTime), to tell a new one
    pub capture_ns: Option<i64>,
    /// The capsules of each hand at that time
    pub hands: [Option<HandShape>; 2],
    pub hand_sources: [CaptureHand; 2],
    /// How much farther the zone near each hand reaches than today's 6 cm (m): the wrist's travel since
    /// the depth capture (`ArmMotion::displacement`, 0 without a motion) and the reach beyond 6 cm
    pub reach: [f32; 2],
    /// No hand was near the depth view for 1.5 s (today's trigger of the playzone copy)
    pub hands_away: bool,
}

/// The memory camera: fixed in stage space
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MemoryCamera {
    pub position: Vec3,
    /// About +y, 0: looking towards -z (counter-clockwise seen from above, like `RegionOverride::yaw`)
    pub yaw: f32,
    /// Below the horizon (radians)
    pub pitch: f32,
}

impl MemoryCamera {
    /// At the middle of the two depth cameras, aimed along the yaw of the head (their mean forward),
    /// pitched down by `pitch_deg`
    pub fn anchored(depth_views: &[ViewParams; 2], pitch_deg: f32) -> Self {
        let position = (depth_views[0].pose.position + depth_views[1].pose.position) * 0.5;
        let orientation = depth_views[0]
            .pose
            .orientation
            .slerp(depth_views[1].pose.orientation, 0.5);

        Self {
            position,
            yaw: head_yaw(orientation),
            pitch: pitch_deg.to_radians(),
        }
    }

    pub fn orientation(&self) -> Quat {
        Quat::from_rotation_y(self.yaw) * Quat::from_rotation_x(-self.pitch)
    }

    pub fn view_params(&self) -> ViewParams {
        let half = MEMORY_HALF_FOV_DEG.to_radians();
        ViewParams {
            pose: Pose {
                orientation: self.orientation(),
                position: self.position,
            },
            fov: Fov {
                left: -half,
                right: half,
                up: half,
                down: -half,
            },
        }
    }
}

/// The yaw of a head (or camera) orientation (radians, as `MemoryCamera::yaw`). Looking straight down or
/// up the forward has no direction on the floor: then its up vector gives it
pub fn head_yaw(orientation: Quat) -> f32 {
    let forward = orientation * Vec3::NEG_Z;
    let mut flat = Vec3::new(forward.x, 0.0, forward.z);
    if flat.length() < 0.2 {
        let up = orientation * Vec3::Y;
        let sign = if forward.y < 0.0 { 1.0 } else { -1.0 };
        flat = Vec3::new(up.x, 0.0, up.z) * sign;
    }
    f32::atan2(-flat.x, -flat.z)
}

/// `a - b` wrapped to -180..180 degrees (radians)
fn angle_between(a: f32, b: f32) -> f32 {
    let d = (a - b).rem_euclid(std::f32::consts::TAU);
    if d > std::f32::consts::PI {
        d - std::f32::consts::TAU
    } else {
        d
    }
}

/// What to do with the memory at a new depth image
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AnchorAction {
    Keep,
    /// The new camera, everything cleared: the update writes the live depth everywhere
    Clean,
    /// The new camera, the memory of the old one (given) re-projected into it, frozen texels kept
    Reproject(MemoryCamera),
}

/// The camera of the memory and how often it was anchored
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MemoryAnchor {
    pub camera: Option<MemoryCamera>,
    /// Since the memory was created: anchored cleanly (the first one too), and with hands
    pub clean: u32,
    pub with_hands: u32,
}

impl MemoryAnchor {
    /// At a new depth image. `hands_away`: no hand near the depth view for 1.5 s
    pub fn update(
        &mut self,
        depth_views: &[ViewParams; 2],
        pitch_deg: f32,
        hands_away: bool,
    ) -> AnchorAction {
        let next = MemoryCamera::anchored(depth_views, pitch_deg);
        if !next.position.is_finite() || !next.yaw.is_finite() {
            return AnchorAction::Keep;
        }
        let Some(camera) = self.camera else {
            self.camera = Some(next);
            self.clean += 1;
            return AnchorAction::Clean;
        };

        let moved = next.position.distance(camera.position);
        let turned = angle_between(next.yaw, camera.yaw).abs() > MAX_TURN_DEG.to_radians();
        let repitched = (next.pitch - camera.pitch).abs() > 1e-4;
        if hands_away {
            if moved > CLEAN_MOVE || turned || repitched {
                self.camera = Some(next);
                self.clean += 1;
                return AnchorAction::Clean;
            }
        } else if moved > HANDS_MOVE || turned || repitched {
            self.camera = Some(next);
            self.with_hands += 1;
            return AnchorAction::Reproject(camera);
        }

        AnchorAction::Keep
    }

    /// No camera: the next depth image anchors it cleanly (a recenter, the reference space changed)
    pub fn forget(&mut self) {
        self.camera = None;
    }
}

/// Floats of `MemoryUniforms` in room_memory.wgsl: two cameras (`PlayzoneEye`), params, the bounds of
/// the hands and their capsules
pub const MEMORY_UNIFORMS_FLOAT_COUNT: usize =
    2 * (16 + 16 + 4) + 4 + 2 * 4 + 2 * 2 * HAND_SEGMENTS * 4;
pub const MEMORY_UNIFORMS_SIZE: usize = MEMORY_UNIFORMS_FLOAT_COUNT * size_of::<f32>();

fn push_camera(data: &mut Vec<f32>, camera: &MemoryCamera) {
    let view = camera.view_params();
    let view_to_world = crate::depth_mask::pose_to_mat4(view.pose);
    data.extend(view_to_world.inverse().to_cols_array());
    data.extend(view_to_world.to_cols_array());
    data.extend(crate::depth_mask::fov_tangents(view.fov).to_array());
}

/// The live surface may lie this far from the ray of a texel (tangent units)
pub fn live_tolerance(size: u32) -> f32 {
    let span = 2.0 * MEMORY_HALF_FOV_DEG.to_radians().tan();
    LIVE_TOLERANCE_TEXELS * span / size.max(1) as f32
}

/// The uniforms of room_memory.wgsl. `source`: the camera re-projected from (None: the camera itself),
/// `dt`: seconds since the last update (clamped to 0..1 s)
pub fn memory_uniforms(
    camera: &MemoryCamera,
    source: Option<&MemoryCamera>,
    params: &MemoryParams,
    hands: &[Option<HandShape>; 2],
    dt: f32,
) -> [f32; MEMORY_UNIFORMS_FLOAT_COUNT] {
    let mut data = Vec::with_capacity(MEMORY_UNIFORMS_FLOAT_COUNT);
    push_camera(&mut data, camera);
    push_camera(&mut data, source.unwrap_or(camera));
    let dt = if dt.is_finite() {
        dt.clamp(0.0, MAX_UPDATE_DT)
    } else {
        0.0
    };
    data.extend([
        params.margin,
        params.outer_ring(),
        dt,
        live_tolerance(params.size),
    ]);
    for hand in hands {
        data.extend(hand.map_or(Vec4::ZERO, |shape| shape.bounds()).to_array());
    }
    for hand in hands {
        let segments = hand.map_or([[Vec4::ZERO; 2]; HAND_SEGMENTS], |shape| shape.segments);
        for point in segments.as_flattened() {
            data.extend(point.to_array());
        }
    }

    data.try_into().unwrap()
}

/// The `memory` vector of the depth uniforms: which playzone texels count, the debug view, the reach of
/// the zone near each hand. Zeros without the memory: today's
pub fn memory_read_uniform(memory: Option<&MemoryInput>) -> [f32; 4] {
    let Some(memory) = memory else {
        return [0.0; 4];
    };
    let reach = memory.reach.map(|reach| {
        if reach.is_finite() {
            reach.max(0.0)
        } else {
            0.0
        }
    });
    [
        if memory.params.skip_moving { 2.0 } else { 1.0 },
        f32::from(memory.params.view.min(2)),
        reach[0],
        reach[1],
    ]
}

/// The shares of the left eye's memory, from the stats pass
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MemoryStats {
    /// Of all texels (0..1): frozen, no live value at the last update (includes those never seen), nothing
    /// known, marked moving
    pub frozen: f32,
    pub no_live: f32,
    pub empty: f32,
    pub moving: f32,
    /// Age of the oldest frozen texel (s, None: none frozen; 60 means 60 s or more), and of any texel
    pub oldest_frozen_s: Option<f32>,
    pub oldest_s: f32,
}

impl MemoryStats {
    /// From the RGBA8 target of `stats_main` (2 * `MEMORY_STATS_CELLS` x `MEMORY_STATS_CELLS`, rows of
    /// `bytes_per_row`)
    pub fn decode(data: &[u8], bytes_per_row: usize) -> Option<Self> {
        let cells = MEMORY_STATS_CELLS as usize;
        if bytes_per_row < cells * 8 || data.len() < bytes_per_row * (cells - 1) + cells * 8 {
            return None;
        }
        let mut sums = [0.0_f32; 4];
        let mut oldest_frozen = 0.0_f32;
        let mut oldest = 0.0_f32;
        for row in 0..cells {
            for cell in 0..cells {
                let texel = &data[row * bytes_per_row + cell * 8..][..8];
                for (sum, byte) in sums.iter_mut().zip(&texel[..4]) {
                    *sum += f32::from(*byte) / 255.0;
                }
                oldest_frozen = oldest_frozen.max(f32::from(texel[4]) / 255.0 * 60.0);
                oldest = oldest.max(f32::from(texel[5]) / 255.0 * 60.0);
            }
        }
        let count = (cells * cells) as f32;
        let frozen = sums[0] / count;

        Some(Self {
            frozen,
            no_live: sums[1] / count,
            empty: sums[2] / count,
            moving: sums[3] / count,
            oldest_frozen_s: (frozen > 0.0).then_some(oldest_frozen),
            oldest_s: oldest,
        })
    }
}

/// What the renderer knows of the memory, for the `memory:` line, bursts and dumps
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MemoryStatus {
    pub anchor: MemoryAnchor,
    pub size: u32,
    /// Updates of each eye since the memory was created
    pub updates: [u32; 2],
    /// The last stats and how old they are (s)
    pub stats: Option<(MemoryStats, f32)>,
}

impl MemoryStatus {
    /// For the display records of bursts and the depth dump. `hands`: where the capsules of this frame
    /// came from (None: not known)
    pub fn to_json(
        &self,
        params: Option<&MemoryParams>,
        hands: Option<[CaptureHand; 2]>,
    ) -> String {
        use crate::key_dump::{array, number, numbers, object, string};

        let camera = self.anchor.camera.map_or("null".into(), |camera| {
            object([
                ("position", numbers(&camera.position.to_array())),
                ("yaw_deg", number(camera.yaw.to_degrees())),
                ("pitch_deg", number(camera.pitch.to_degrees())),
                ("view", crate::depth_dump::camera(&camera.view_params())),
            ])
        });
        let stats = self.stats.map_or("null".into(), |(stats, age)| {
            object([
                ("age_ms", number(age * 1000.0)),
                ("frozen", number(stats.frozen)),
                ("no_live", number(stats.no_live)),
                ("empty", number(stats.empty)),
                ("moving", number(stats.moving)),
                (
                    "oldest_frozen_s",
                    stats.oldest_frozen_s.map_or("null".into(), number),
                ),
                ("oldest_s", number(stats.oldest_s)),
            ])
        });
        let mut fields = vec![
            ("camera", camera),
            ("size", self.size.to_string()),
            ("anchors_clean", self.anchor.clean.to_string()),
            ("anchors_with_hands", self.anchor.with_hands.to_string()),
            ("updates", numbers(&self.updates.map(|n| n as f32))),
            ("stats", stats),
        ];
        if let Some(params) = params {
            fields.extend([
                ("margin_mm", number(params.margin * 1000.0)),
                ("outer_mm", number(params.outer_ring() * 1000.0)),
                ("skip_moving", params.skip_moving.to_string()),
            ]);
        }
        if let Some(hands) = hands {
            fields.push(("hands", array(hands.map(|hand| string(hand.name())))));
        }

        object(fields)
    }
}

/// The room memory in a display record of a burst: the renderer's state after the frame, the
/// properties, where this frame's capsules came from
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MemoryRecord {
    pub status: MemoryStatus,
    pub params: MemoryParams,
    pub hands: [CaptureHand; 2],
}

impl MemoryRecord {
    pub fn to_json(&self) -> String {
        self.status.to_json(Some(&self.params), Some(self.hands))
    }
}

/// Estimated work of one update of one eye: texels, and texture reads per texel away from the hands
/// (one memory read, two depth reads). The capsule loop (2 x 28 segment tests) runs only for texels whose
/// depth ray passes within the reach of a hand's bounding sphere
pub fn update_cost(size: u32) -> (u32, u32) {
    (size * size, 3)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::depth_mask::fov_tangents;

    fn depth_views(position: Vec3, yaw_deg: f32, pitch_deg: f32) -> [ViewParams; 2] {
        let orientation = Quat::from_rotation_y(yaw_deg.to_radians())
            * Quat::from_rotation_x(pitch_deg.to_radians());
        let half = 0.8;
        [-0.032, 0.032].map(|x| ViewParams {
            pose: Pose {
                orientation,
                position: position + orientation * Vec3::new(x, 0.0, 0.0),
            },
            fov: Fov {
                left: -half,
                right: half,
                up: half,
                down: -half,
            },
        })
    }

    #[test]
    fn the_camera_is_aimed_by_the_head_yaw_and_the_pitch() {
        // The head looks 30 degrees left and 50 degrees down, with some roll: the aim keeps the yaw only
        let head = Vec3::new(0.1, 1.2, 0.3);
        let views = depth_views(head, 30.0, -50.0);
        let camera = MemoryCamera::anchored(&views, 35.0);
        assert!(camera.position.distance(head) < 1e-5);
        assert!(
            (camera.yaw.to_degrees() - 30.0).abs() < 1e-3,
            "{}",
            camera.yaw
        );
        let forward = camera.orientation() * Vec3::NEG_Z;
        // 35 degrees below the horizon, towards the yaw
        assert!(
            (forward.y + 35_f32.to_radians().sin()).abs() < 1e-5,
            "{forward}"
        );
        let flat = Vec3::new(forward.x, 0.0, forward.z).normalize();
        let expected = Quat::from_rotation_y(30_f32.to_radians()) * Vec3::NEG_Z;
        assert!(flat.distance(expected) < 1e-5, "{flat}");
        // No roll: the camera's x axis stays level
        assert!((camera.orientation() * Vec3::X).y.abs() < 1e-6);

        let view = camera.view_params();
        let tangents = fov_tangents(view.fov);
        assert!((tangents.y - 3_f32.sqrt()).abs() < 1e-5, "{tangents}");
        // One texel of 512 is 0.0068 in tangent units, the depth's is 0.0069
        assert!((live_tolerance(512) / 1.5 - 0.00677).abs() < 1e-4);
    }

    #[test]
    fn looking_straight_down_keeps_the_yaw() {
        for yaw in [-120.0_f32, 0.0, 75.0] {
            let views = depth_views(Vec3::ZERO, yaw, -89.9);
            let camera = MemoryCamera::anchored(&views, 35.0);
            let error = angle_between(camera.yaw, yaw.to_radians()).abs();
            assert!(error < 0.01, "{yaw}: {}", camera.yaw.to_degrees());
            let views = depth_views(Vec3::ZERO, yaw, 89.9);
            let camera = MemoryCamera::anchored(&views, 35.0);
            let error = angle_between(camera.yaw, yaw.to_radians()).abs();
            assert!(error < 0.01, "{yaw} up: {}", camera.yaw.to_degrees());
        }
    }

    #[test]
    fn anchored_again_by_the_rules() {
        let mut anchor = MemoryAnchor::default();
        let head = Vec3::new(0.0, 1.2, 0.0);
        // The first depth image anchors it cleanly, with or without hands
        assert_eq!(
            anchor.update(&depth_views(head, 0.0, -20.0), 35.0, false),
            AnchorAction::Clean
        );
        let first = anchor.camera.unwrap();

        // Hands near: 10 cm and 35 degrees are kept, 13 cm re-projects from the old camera
        let moved = head + Vec3::new(0.1, 0.0, 0.0);
        assert_eq!(
            anchor.update(&depth_views(moved, 35.0, -20.0), 35.0, false),
            AnchorAction::Keep
        );
        let moved = head + Vec3::new(0.13, 0.0, 0.0);
        assert_eq!(
            anchor.update(&depth_views(moved, 0.0, -20.0), 35.0, false),
            AnchorAction::Reproject(first)
        );
        let second = anchor.camera.unwrap();
        assert!(second.position.distance(moved) < 1e-5);
        // A turn past 40 degrees with hands near: re-projected too
        assert_eq!(
            anchor.update(&depth_views(moved, -41.0, -20.0), 35.0, false),
            AnchorAction::Reproject(second)
        );
        assert_eq!((anchor.clean, anchor.with_hands), (1, 2));

        // No hand near for 1.5 s: 2 cm is kept, 4 cm anchors cleanly
        let camera = anchor.camera.unwrap();
        let still = camera.position + Vec3::new(0.0, 0.0, 0.02);
        assert_eq!(
            anchor.update(&depth_views(still, -41.0, 0.0), 35.0, true),
            AnchorAction::Keep
        );
        let still = camera.position + Vec3::new(0.0, 0.0, 0.04);
        assert_eq!(
            anchor.update(&depth_views(still, -41.0, 0.0), 35.0, true),
            AnchorAction::Clean
        );
        // Another pitch is a new aim
        assert_eq!(
            anchor.update(&depth_views(still, -41.0, 0.0), 45.0, true),
            AnchorAction::Clean
        );
        assert_eq!(anchor.camera.unwrap().pitch, 45_f32.to_radians());

        // Yaw wraps: 170 and -170 degrees are 20 apart
        let mut anchor = MemoryAnchor::default();
        anchor.update(&depth_views(head, 170.0, 0.0), 35.0, false);
        assert_eq!(
            anchor.update(&depth_views(head, -170.0, 0.0), 35.0, false),
            AnchorAction::Keep
        );

        // Forgotten: the next one is clean again, the counts stay
        anchor.forget();
        assert_eq!(
            anchor.update(&depth_views(head, 0.0, 0.0), 35.0, false),
            AnchorAction::Clean
        );
        assert_eq!(anchor.clean, 2);
    }

    #[test]
    fn uniforms_hold_the_cameras_params_and_capsules() {
        let camera = MemoryCamera {
            position: Vec3::new(0.0, 1.2, 0.0),
            yaw: 0.3,
            pitch: 0.6,
        };
        let source = MemoryCamera {
            position: Vec3::new(0.1, 1.2, 0.0),
            ..camera
        };
        let segment = [
            Vec4::new(0.0, 1.0, -0.4, 0.01),
            Vec4::new(0.0, 1.0, -0.5, 0.01),
        ];
        let shape = HandShape {
            segments: [segment; HAND_SEGMENTS],
        };
        let params = MemoryParams {
            outer: 0.12,
            ..Default::default()
        };
        let data = memory_uniforms(&camera, Some(&source), &params, &[None, Some(shape)], 5.0);
        assert_eq!(data.len() * 4, MEMORY_UNIFORMS_SIZE);
        // world_to_view of the camera moves its position to the origin
        let world_to_view = alvr_common::glam::Mat4::from_cols_slice(&data[..16]);
        assert!(world_to_view.transform_point3(camera.position).length() < 1e-5);
        let source_view_to_world =
            alvr_common::glam::Mat4::from_cols_slice(&data[36 + 16..36 + 32]);
        assert!(
            source_view_to_world
                .w_axis
                .truncate()
                .distance(source.position)
                < 1e-6
        );
        // Margin, outer ring, dt clamped to 1 s, tolerance
        assert_eq!(&data[72..75], &[0.06, 0.12, 1.0]);
        assert!((data[75] - live_tolerance(512)).abs() < 1e-9);
        // No left hand, the right one's bounds and first capsule
        assert_eq!(&data[76..80], &[0.0; 4]);
        assert!(data[83] > 0.05);
        let first_right = 84 + HAND_SEGMENTS * 2 * 4;
        assert_eq!(&data[first_right..first_right + 4], &segment[0].to_array());

        // An outer ring not beyond the margin is off
        let params = MemoryParams {
            outer: 0.05,
            ..Default::default()
        };
        let data = memory_uniforms(&camera, None, &params, &[None; 2], f32::NAN);
        assert_eq!(&data[72..75], &[0.06, 0.0, 0.0]);
        // The source is the camera itself
        assert_eq!(&data[..36], &data[36..72]);
    }

    #[test]
    fn read_uniform_is_zero_when_off() {
        assert_eq!(memory_read_uniform(None), [0.0; 4]);
        let input = MemoryInput {
            params: MemoryParams {
                view: 2,
                ..Default::default()
            },
            capture_ns: None,
            hands: [None; 2],
            hand_sources: [CaptureHand::None; 2],
            reach: [0.02, f32::NAN],
            hands_away: true,
        };
        assert_eq!(memory_read_uniform(Some(&input)), [1.0, 2.0, 0.02, 0.0]);
        let input = MemoryInput {
            params: MemoryParams {
                skip_moving: true,
                ..Default::default()
            },
            ..input
        };
        assert_eq!(memory_read_uniform(Some(&input))[0], 2.0);
    }

    #[test]
    fn stats_are_averaged_over_the_cells() {
        let cells = MEMORY_STATS_CELLS as usize;
        let bytes_per_row = 256;
        let mut data = vec![0_u8; bytes_per_row * cells];
        // One cell all frozen with a texel 30 s old, every cell half without a live value
        for row in 0..cells {
            for cell in 0..cells {
                let texel = &mut data[row * bytes_per_row + cell * 8..][..8];
                texel[1] = 128;
                texel[5] = 51;
            }
        }
        data[0] = 255;
        data[4] = 128;
        let stats = MemoryStats::decode(&data, bytes_per_row).unwrap();
        assert!((stats.frozen - 1.0 / 256.0).abs() < 1e-6);
        assert!((stats.no_live - 128.0 / 255.0).abs() < 1e-6);
        assert!((stats.oldest_frozen_s.unwrap() - 30.1).abs() < 0.1);
        assert!((stats.oldest_s - 12.0).abs() < 0.01);

        // Nothing frozen: no age
        data[0] = 0;
        assert_eq!(
            MemoryStats::decode(&data, bytes_per_row)
                .unwrap()
                .oldest_frozen_s,
            None
        );
        // Too short
        assert_eq!(MemoryStats::decode(&data[..100], bytes_per_row), None);
    }

    #[test]
    fn status_json_is_valid() {
        let status = MemoryStatus {
            anchor: MemoryAnchor {
                camera: Some(MemoryCamera {
                    position: Vec3::new(0.0, 1.2, 0.0),
                    yaw: 0.0,
                    pitch: 35_f32.to_radians(),
                }),
                clean: 1,
                with_hands: 0,
            },
            size: 512,
            updates: [10, 9],
            stats: Some((
                MemoryStats {
                    frozen: 0.01,
                    oldest_frozen_s: None,
                    ..Default::default()
                },
                0.5,
            )),
        };
        let json = status.to_json(
            Some(&MemoryParams::default()),
            Some([CaptureHand::Exact, CaptureHand::None]),
        );
        assert!(crate::key_dump::tests::is_valid_json(&json), "{json}");
        assert!(json.contains(r#""pitch_deg":35"#), "{json}");
        assert!(json.contains(r#""hands":["exact","none"]"#), "{json}");
        let json = MemoryStatus {
            anchor: MemoryAnchor::default(),
            stats: None,
            ..status
        }
        .to_json(None, None);
        assert!(crate::key_dump::tests::is_valid_json(&json), "{json}");
        assert!(json.starts_with(r#"{"camera":null"#), "{json}");
    }
}
