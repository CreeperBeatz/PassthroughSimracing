//! Learns the colors of the passthrough color key while it is used, from the passthrough camera and
//! the tracked hands.

use crate::{
    camera::{self, CameraFrame, PassthroughCamera},
    extra_extensions::BodyTrackerFB,
    interaction::InteractionContext,
    passthrough::debug_property,
};
use alvr_common::{
    RelaxedAtomic,
    glam::{Quat, Vec2, Vec3},
    parking_lot::{Mutex, RwLock},
};
use alvr_graphics::{
    ARM_SEGMENTS, CameraView, ColorModel, HAND_SEGMENTS, HandColorSamples, HandShape, SampledHand,
};
use openxr as xr;
use std::{
    sync::Arc,
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

// The hands are verified at each update, the LUT is updated less often
const UPDATE_INTERVAL: Duration = Duration::from_millis(60);
const LUT_UPDATE_INTERVAL: Duration = Duration::from_millis(180);
// Verified hands are not used if they are older
const MAX_VERIFICATION_AGE: Duration = Duration::from_millis(300);
const MAX_ARM_TRUST_AGE: Duration = Duration::from_secs(1);
// How long a forearm is trusted if it can't be checked, e.g. because it moves
const ARM_TRUST_HOLD: Duration = Duration::from_secs(3);
// Smaller changes of the LUT are not worth an update of the passthrough layer
const MIN_LUT_CHANGE: u8 = 10;
const RETRY_INTERVAL: Duration = Duration::from_secs(3);
const LOG_INTERVAL: Duration = Duration::from_secs(2);
// The images and the hand poses don't match well enough for fast hands (m/s)
const MAX_HAND_SPEED: f32 = 0.5;
// Fingers included (m/s)
const STILL_HAND_SPEED: f32 = 0.12;
// How long the leak of a hand is kept when it's not seen, and when it is not used any more
const LEAK_HOLD: Duration = Duration::from_secs(1);

pub fn enabled() -> bool {
    debug_property("debug.alvr.auto_key").unwrap_or(0) > 0
}

pub type HandWeights = [[f32; HAND_SEGMENTS]; 2];
pub type ArmTrust = [[f32; ARM_SEGMENTS]; 2];

#[derive(Clone, Copy)]
struct Verified {
    time: Instant,
    weights: HandWeights,
    arm_trust: ArmTrust,
    leak: [f32; 2],
}

struct Context {
    stage_space: Arc<xr::Space>,
    view_space: Arc<xr::Space>,
    interaction_context: Arc<RwLock<InteractionContext>>,
    body_tracker: Option<Arc<BodyTrackerFB>>,
    running: Arc<RelaxedAtomic>,
    lut: Arc<Mutex<Option<Vec<u8>>>>,
    verified: Arc<Mutex<Option<Verified>>>,
}

pub struct AutoColorKey {
    running: Arc<RelaxedAtomic>,
    lut: Arc<Mutex<Option<Vec<u8>>>>,
    verified: Arc<Mutex<Option<Verified>>>,
    thread: Option<JoinHandle<()>>,
}

impl AutoColorKey {
    pub fn new(
        stage_space: Arc<xr::Space>,
        view_space: Arc<xr::Space>,
        interaction_context: Arc<RwLock<InteractionContext>>,
        body_tracker: Option<Arc<BodyTrackerFB>>,
    ) -> Self {
        let running = Arc::new(RelaxedAtomic::new(true));
        let lut = Arc::new(Mutex::new(None));
        let verified = Arc::new(Mutex::new(None));
        let context = Context {
            stage_space,
            view_space,
            interaction_context,
            body_tracker,
            running: Arc::clone(&running),
            lut: Arc::clone(&lut),
            verified: Arc::clone(&verified),
        };

        Self {
            running,
            lut,
            verified,
            thread: Some(thread::spawn(move || learn_loop(&context))),
        }
    }

    /// How sure it is that each segment of the hands is where the hand tracking says
    pub fn verified_hands(&self) -> HandWeights {
        match &*self.verified.lock() {
            Some(verified) if verified.time.elapsed() < MAX_VERIFICATION_AGE => verified.weights,
            _ => [[0.0; HAND_SEGMENTS]; 2],
        }
    }

    /// Which parts of the forearms are tracked well. This doesn't depend on their motion
    pub fn arm_trust(&self) -> ArmTrust {
        match &*self.verified.lock() {
            Some(verified) if verified.time.elapsed() < MAX_ARM_TRUST_AGE => verified.arm_trust,
            _ => [[0.0; ARM_SEGMENTS]; 2],
        }
    }

    /// For each hand, the share of the colors around it that the key keeps (0..1): there the background
    /// shows through the rim
    pub fn ring_leak(&self) -> [f32; 2] {
        match &*self.verified.lock() {
            Some(verified) if verified.time.elapsed() < LEAK_HOLD => verified.leak,
            _ => [0.0; 2],
        }
    }

    /// New colors, if there are any
    pub fn take_lut(&self) -> Option<Vec<u8>> {
        self.lut.lock().take()
    }
}

impl Drop for AutoColorKey {
    fn drop(&mut self) {
        self.running.set(false);
        if let Some(thread) = self.thread.take() {
            thread.join().ok();
        }
    }
}

fn sleep_while_running(context: &Context, duration: Duration) {
    let start = Instant::now();
    while context.running.value() && start.elapsed() < duration {
        thread::sleep(Duration::from_millis(20));
    }
}

fn learn_loop(context: &Context) {
    let mut model = ColorModel::default();
    let mut weights: HandWeights = [[0.0; HAND_SEGMENTS]; 2];
    let mut arm_trust: ArmTrust = [[0.0; ARM_SEGMENTS]; 2];
    let mut last_arm_check = [Instant::now(); 2];
    let mut leak = [0.0; 2];
    let mut last_leak = [Instant::now(); 2];
    let mut last_lut = vec![];

    while context.running.value() {
        let camera = match PassthroughCamera::open() {
            Ok(camera) => camera,
            Err(e) => {
                camera::log(&format!("auto key: {e}"));
                sleep_while_running(context, RETRY_INTERVAL);
                continue;
            }
        };
        camera::log(&format!("auto key: {:?}", camera.calibration));

        let mut last_update = Instant::now();
        let mut last_lut_update = Instant::now();
        let mut last_log = Instant::now();
        while context.running.value() && !camera.is_lost() {
            let frame = camera.latest_frame();
            if last_update.elapsed() < UPDATE_INTERVAL {
                thread::sleep(Duration::from_millis(15));
                continue;
            }
            let Some(frame) = frame else {
                thread::sleep(Duration::from_millis(5));
                continue;
            };
            let elapsed = last_update.elapsed().as_secs_f32();
            last_update = Instant::now();

            let Some((samples, view, hands)) = sample(context, &camera, &frame, &arm_trust) else {
                continue;
            };
            model.unknown_keep = percent("debug.alvr.key_unknown", 10);
            model.skin_bias = percent("debug.alvr.key_bias", 150);
            model.tightness = percent("debug.alvr.key_tight", 30);
            model.add_samples(&samples, elapsed);
            let new_lut = if last_lut_update.elapsed() >= LUT_UPDATE_INTERVAL {
                last_lut_update = Instant::now();
                model.update_lut()
            } else {
                None
            };

            let background_kept = if samples.background.is_empty() {
                0.0
            } else {
                model.kept_fraction(&samples.background)
            };
            // Per hand: rise at once, fall slowly, hold it for a moment when the hand is not seen
            let hand_kept = [0, 1].map(|i| model.leak_fraction(samples.hand_background(i)));
            for ((leak, last_leak), kept) in leak.iter_mut().zip(&mut last_leak).zip(hand_kept) {
                if let Some(kept) = kept {
                    *leak = if kept > *leak {
                        kept
                    } else {
                        *leak + (kept - *leak) * 0.3
                    };
                    *last_leak = Instant::now();
                } else if last_leak.elapsed() > LEAK_HOLD {
                    *leak = 0.0;
                }
            }
            // With the narrower rim each hand is verified against its own background
            let use_hand_kept = debug_property("debug.alvr.key_leak").unwrap_or(0) > 0;

            for (i, (weights, hand)) in weights.iter_mut().zip(&hands).enumerate() {
                let background_kept = if use_hand_kept {
                    hand_kept[i].unwrap_or(background_kept)
                } else {
                    background_kept
                };
                let targets = hand.as_ref().map_or([0.0; HAND_SEGMENTS], |hand| {
                    model.verify_hand(
                        &view,
                        camera.calibration.width,
                        camera.calibration.height,
                        hand,
                        background_kept,
                        |x, y| frame.rgb(x, y),
                    )
                });
                // Slow, to avoid switching between the tracked forearm and the one of the depth map
                if hand.as_ref().is_some_and(|hand| hand.arm_trust.is_some()) {
                    last_arm_check[i] = Instant::now();
                    for (trust, target) in arm_trust[i]
                        .iter_mut()
                        .zip(&targets[HAND_SEGMENTS - ARM_SEGMENTS..])
                    {
                        *trust += (f32::max(*target, 0.0) - *trust) * 0.2;
                    }
                } else if last_arm_check[i].elapsed() > ARM_TRUST_HOLD {
                    for trust in &mut arm_trust[i] {
                        *trust *= 0.8;
                    }
                }

                // Lose trust immediately, gain it slowly
                for (weight, target) in weights.iter_mut().zip(targets) {
                    // Outside of the image
                    let target = f32::max(target, 0.0);
                    *weight = if target < *weight {
                        target
                    } else {
                        *weight + (target - *weight) * 0.5
                    };
                }
            }
            *context.verified.lock() = Some(Verified {
                time: Instant::now(),
                weights,
                arm_trust,
                leak,
            });

            if last_log.elapsed() > LOG_INTERVAL {
                last_log = Instant::now();
                camera::log(&format!(
                    "auto key: skin {} kept {:.2} mean {:?}, background {} kept {:.2} mean {:?}, \
                     verified {:.2} {:.2}, arms {:.1?} {:.1?}, leak {:.2} {:.2}, scene {} kept {:.2}, \
                     update {:.1} ms",
                    samples.skin.len(),
                    model.kept_fraction(&samples.skin),
                    mean(&samples.skin),
                    samples.background.len(),
                    model.kept_fraction(&samples.background),
                    mean(&samples.background),
                    weights[0].iter().sum::<f32>() / HAND_SEGMENTS as f32,
                    weights[1].iter().sum::<f32>() / HAND_SEGMENTS as f32,
                    arm_trust[0],
                    arm_trust[1],
                    leak[0],
                    leak[1],
                    samples.scene.len(),
                    model.kept_fraction(&samples.scene),
                    last_update.elapsed().as_secs_f32() * 1000.0,
                ));
            }

            if let Some(new_lut) = new_lut {
                let is_changed = last_lut.len() != new_lut.len()
                    || new_lut
                        .iter()
                        .zip(&last_lut)
                        .any(|(a, b): (&u8, &u8)| a.abs_diff(*b) > MIN_LUT_CHANGE);
                if is_changed {
                    last_lut.clone_from(&new_lut);
                    *context.lut.lock() = Some(new_lut);
                }
            }
        }
    }
}

fn percent(property: &str, default: i64) -> f32 {
    debug_property(property).unwrap_or(default).clamp(0, 1000) as f32 / 100.0
}

fn mean(colors: &[[u8; 3]]) -> [u32; 3] {
    let count = colors.len().max(1) as u32;
    [0, 1, 2].map(|c| colors.iter().map(|color| u32::from(color[c])).sum::<u32>() / count)
}

fn sample(
    context: &Context,
    camera: &PassthroughCamera,
    frame: &CameraFrame,
    arm_trust: &ArmTrust,
) -> Option<(HandColorSamples, CameraView, [Option<SampledHand>; 2])> {
    // The OpenXR time of the Quest runtime is CLOCK_MONOTONIC, like the timestamps of the camera
    let time = xr::Time::from_nanos(frame.timestamp_ns);

    let head = context
        .view_space
        .locate(&context.stage_space, time)
        .ok()
        .filter(|location| {
            location.location_flags.contains(
                xr::SpaceLocationFlags::POSITION_VALID | xr::SpaceLocationFlags::ORIENTATION_VALID,
            )
        })?;
    let head = crate::from_xr_pose(head.pose);

    let calibration = &camera.calibration;
    let [fx, fy, cx, cy] = calibration.intrinsics;
    let head_to_camera = Quat::from_array(calibration.rotation).normalize();
    let view = CameraView {
        position: head.position + head.orientation * Vec3::from_array(calibration.translation),
        orientation: head.orientation * head_to_camera.inverse(),
        focal_length: Vec2::new(fx, fy),
        center: Vec2::new(cx, cy),
    };

    let interaction_context = context.interaction_context.read();
    let hands = [0, 1].map(|i| {
        let tracker = interaction_context.hands_interaction[i]
            .skeleton_tracker
            .as_ref()?;
        let (joints, velocities) = context
            .stage_space
            .relate_hand_joints(tracker, time)
            .ok()
            .flatten()?;

        let all_tracked = joints.iter().all(|joint| {
            joint
                .location_flags
                .contains(xr::SpaceLocationFlags::POSITION_TRACKED)
        });
        let speed = velocities
            .iter()
            .map(|v| crate::from_xr_vec3(v.linear_velocity).length())
            .fold(0.0, f32::max);
        if !all_tracked || speed > MAX_HAND_SPEED {
            return None;
        }

        let position = |joint: xr::HandJointEXT| crate::from_xr_vec3(joints[joint].pose.position);
        let wrist = position(xr::HandJointEXT::WRIST);
        let forearm = context
            .body_tracker
            .as_ref()
            .and_then(|tracker| tracker.forearm(time, &context.stage_space, i == 1));

        Some(SampledHand {
            shape: HandShape::from_joints(
                &joints.map(|joint| crate::from_xr_vec3(joint.pose.position).extend(joint.radius)),
                forearm.map(|forearm| wrist + forearm),
            ),
            wrist,
            arm_direction: forearm
                .unwrap_or_else(|| wrist - position(xr::HandJointEXT::MIDDLE_METACARPAL))
                .normalize_or_zero(),
            is_still: speed < STILL_HAND_SPEED,
            arm_trust: forearm.map(|_| arm_trust[i]),
        })
    });
    drop(interaction_context);

    let samples = alvr_graphics::sample_hand_colors(
        &view,
        calibration.width,
        calibration.height,
        &hands,
        |x, y| frame.rgb(x, y),
    );

    Some((samples, view, hands))
}
