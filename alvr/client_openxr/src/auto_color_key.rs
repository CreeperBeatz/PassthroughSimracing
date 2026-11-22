//! Learns the colors of the passthrough color key while it is used, from the passthrough camera and
//! the tracked hands.

use crate::{
    camera::{self, CameraFrame, PassthroughCamera},
    interaction::InteractionContext,
    passthrough::debug_property,
};
use alvr_common::{
    RelaxedAtomic,
    glam::{Quat, Vec2, Vec3},
    parking_lot::{Mutex, RwLock},
};
use alvr_graphics::{CameraView, ColorModel, HandColorSamples, HandShape, SampledHand};
use openxr as xr;
use std::{
    sync::Arc,
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const UPDATE_INTERVAL: Duration = Duration::from_millis(120);
const RETRY_INTERVAL: Duration = Duration::from_secs(3);
const LOG_INTERVAL: Duration = Duration::from_secs(2);
// The images and the hand poses don't match well enough for fast hands (m/s)
const MAX_HAND_SPEED: f32 = 0.5;

pub fn enabled() -> bool {
    debug_property("debug.alvr.auto_key").unwrap_or(0) > 0
}

struct Context {
    stage_space: Arc<xr::Space>,
    view_space: Arc<xr::Space>,
    interaction_context: Arc<RwLock<InteractionContext>>,
    running: Arc<RelaxedAtomic>,
    lut: Arc<Mutex<Option<Vec<u8>>>>,
}

pub struct AutoColorKey {
    running: Arc<RelaxedAtomic>,
    lut: Arc<Mutex<Option<Vec<u8>>>>,
    thread: Option<JoinHandle<()>>,
}

impl AutoColorKey {
    pub fn new(
        stage_space: Arc<xr::Space>,
        view_space: Arc<xr::Space>,
        interaction_context: Arc<RwLock<InteractionContext>>,
    ) -> Self {
        let running = Arc::new(RelaxedAtomic::new(true));
        let lut = Arc::new(Mutex::new(None));
        let context = Context {
            stage_space,
            view_space,
            interaction_context,
            running: Arc::clone(&running),
            lut: Arc::clone(&lut),
        };

        Self {
            running,
            lut,
            thread: Some(thread::spawn(move || learn_loop(&context))),
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

            let Some(samples) = sample(context, &camera, &frame) else {
                continue;
            };
            model.unknown_keep = debug_property("debug.alvr.key_unknown")
                .unwrap_or(30)
                .clamp(0, 100) as f32
                / 100.0;
            model.add_samples(&samples, elapsed);
            let new_lut = model.update_lut();

            if last_log.elapsed() > LOG_INTERVAL {
                last_log = Instant::now();
                camera::log(&format!(
                    "auto key: skin {} kept {:.2} mean {:?}, background {} kept {:.2} mean {:?}, \
                     learned {}, update {:.1} ms",
                    samples.skin.len(),
                    model.kept_fraction(&samples.skin),
                    mean(&samples.skin),
                    samples.background.len(),
                    model.kept_fraction(&samples.background),
                    mean(&samples.background),
                    new_lut.is_some(),
                    last_update.elapsed().as_secs_f32() * 1000.0,
                ));
            }

            if new_lut.is_some() {
                *context.lut.lock() = new_lut;
            }
        }
    }
}

fn mean(colors: &[[u8; 3]]) -> [u32; 3] {
    let count = colors.len().max(1) as u32;
    [0, 1, 2].map(|c| colors.iter().map(|color| u32::from(color[c])).sum::<u32>() / count)
}

fn sample(
    context: &Context,
    camera: &PassthroughCamera,
    frame: &CameraFrame,
) -> Option<HandColorSamples> {
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

        Some(SampledHand {
            shape: HandShape::from_joints(
                &joints.map(|joint| crate::from_xr_vec3(joint.pose.position).extend(joint.radius)),
            ),
            wrist,
            arm_direction: (wrist - position(xr::HandJointEXT::MIDDLE_METACARPAL))
                .normalize_or_zero(),
        })
    });
    drop(interaction_context);

    if hands.iter().all(Option::is_none) {
        return None;
    }

    Some(alvr_graphics::sample_hand_colors(
        &view,
        calibration.width,
        calibration.height,
        &hands,
        |x, y| frame.rgb(x, y),
    ))
}
