//! Learns the colors of the passthrough color key while it is used, from the passthrough camera and
//! the tracked hands.

use crate::{
    camera::{self, CameraCalibration, CameraFrame, PassthroughCamera},
    interaction::InteractionContext,
    passthrough::debug_property,
};
use alvr_common::{
    RelaxedAtomic,
    glam::{Quat, Vec2, Vec3, Vec4},
    parking_lot::{Mutex, RwLock},
};
use alvr_graphics::{
    CameraView, ColorModel, HAND_SEGMENTS, HandColorSamples, HandShape, LeakMap, LeakSnapshot,
    SampledHand,
};
use openxr as xr;
use std::{
    fs,
    sync::Arc,
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

// The hands are verified at each update, the LUT is updated less often
const UPDATE_INTERVAL: Duration = Duration::from_millis(60);
const LUT_UPDATE_INTERVAL: Duration = Duration::from_millis(180);
// Verified hands are not used if they are older
const MAX_VERIFICATION_AGE: Duration = Duration::from_millis(300);
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
// The leak map sees every 8th pixel in x and y (1200 of 320 x 240), see `leak_sample_offset`
const LEAK_MAP_STEP: u32 = 8;
// Grid of the camera view for `debug.alvr.key_leak_dump`
const LEAK_DUMP_SIZE: [u32; 2] = [40, 30];
// For `debug.alvr.key_lut_dump`
const LEARNED_LUT_DIR: &str = "/sdcard/Android/data/alvr.client.dev/files";

pub fn enabled() -> bool {
    debug_property("debug.alvr.auto_key").unwrap_or(0) > 0
}

pub type HandWeights = [[f32; HAND_SEGMENTS]; 2];

#[derive(Clone, Copy)]
struct Verified {
    time: Instant,
    weights: HandWeights,
    leak: [f32; 2],
}

/// The leak map for the render thread
pub struct LeakMapState {
    time: Instant,
    pub snapshot: LeakSnapshot,
    /// Directions of the leakiest clusters from the anchor, only for `debug.alvr.key_leak_view 2`
    pub probes: Vec<Vec3>,
}

struct Context {
    stage_space: Arc<xr::Space>,
    view_space: Arc<xr::Space>,
    interaction_context: Arc<RwLock<InteractionContext>>,
    running: Arc<RelaxedAtomic>,
    lut: Arc<Mutex<Option<Vec<u8>>>>,
    verified: Arc<Mutex<Option<Verified>>>,
    leak_map: Arc<Mutex<Option<Arc<LeakMapState>>>>,
    leak_map_clear: Arc<RelaxedAtomic>,
}

pub struct AutoColorKey {
    running: Arc<RelaxedAtomic>,
    lut: Arc<Mutex<Option<Vec<u8>>>>,
    verified: Arc<Mutex<Option<Verified>>>,
    leak_map: Arc<Mutex<Option<Arc<LeakMapState>>>>,
    leak_map_clear: Arc<RelaxedAtomic>,
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
        let verified = Arc::new(Mutex::new(None));
        let leak_map = Arc::new(Mutex::new(None));
        let leak_map_clear = Arc::new(RelaxedAtomic::new(false));
        let context = Context {
            stage_space,
            view_space,
            interaction_context,
            running: Arc::clone(&running),
            lut: Arc::clone(&lut),
            verified: Arc::clone(&verified),
            leak_map: Arc::clone(&leak_map),
            leak_map_clear: Arc::clone(&leak_map_clear),
        };

        Self {
            running,
            lut,
            verified,
            leak_map,
            leak_map_clear,
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

    /// Where the room leaks through the key, if the map is learned (see `debug.alvr.key_leak_map`)
    pub fn leak_map(&self) -> Option<Arc<LeakMapState>> {
        self.leak_map
            .lock()
            .as_ref()
            .filter(|state| state.time.elapsed() < LEAK_HOLD)
            .cloned()
    }

    /// Forget the leak map, e.g. when the reference space changed
    pub fn clear_leak_map(&self) {
        self.leak_map_clear.set(true);
        *self.leak_map.lock() = None;
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
    let mut leak = [0.0; 2];
    let mut last_leak = [Instant::now(); 2];
    let mut last_lut = vec![];
    let mut leak_learner = LeakLearner::new();

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

            let Some((samples, view, hands, exclusions)) = sample(context, &camera, &frame) else {
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
                // Lose trust immediately, gain it slowly
                for (weight, target) in weights.iter_mut().zip(targets) {
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
                leak,
            });

            if last_log.elapsed() > LOG_INTERVAL {
                last_log = Instant::now();
                let map = &leak_learner.map;
                let leak_map = if leak_learner.is_on {
                    format!(
                        ", leak map {:.1} ms known {:.0}% anchor moves {} light resets {}",
                        leak_learner.update_ms,
                        map.known_fraction() * 100.0,
                        map.anchor_moves(),
                        map.light_resets(),
                    )
                } else {
                    String::new()
                };
                camera::log(&format!(
                    "auto key: skin {} kept {:.2} mean {:?}, background {} kept {:.2} mean {:?}, \
                     verified {:.2} {:.2}, leak {:.2} {:.2}, scene {} kept {:.2}, \
                     update {:.1} ms{leak_map}",
                    samples.skin.len(),
                    model.kept_fraction(&samples.skin),
                    mean(&samples.skin),
                    samples.background.len(),
                    model.kept_fraction(&samples.background),
                    mean(&samples.background),
                    weights[0].iter().sum::<f32>() / HAND_SEGMENTS as f32,
                    weights[1].iter().sum::<f32>() / HAND_SEGMENTS as f32,
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
                    leak_learner.alpha_changed = true;
                }
            }

            leak_learner.update(
                context,
                &view,
                &camera.calibration,
                &frame,
                &exclusions,
                &last_lut,
            );
        }
    }
}

/// Learns the leak map from the camera, see `debug.alvr.key_leak_map`
struct LeakLearner {
    map: LeakMap,
    is_on: bool,
    // The map doesn't use the last published LUT yet
    alpha_changed: bool,
    sample_index: u32,
    samples: Vec<(Vec3, [u8; 3])>,
    last_clear: Option<i64>,
    last_dump: Option<i64>,
    last_lut_dump: Option<i64>,
    // Duration of the last update, for the log
    update_ms: f32,
}

impl LeakLearner {
    fn new() -> Self {
        Self {
            map: LeakMap::new(),
            is_on: false,
            alpha_changed: true,
            sample_index: 0,
            samples: Vec::new(),
            last_clear: None,
            last_dump: None,
            last_lut_dump: None,
            update_ms: 0.0,
        }
    }

    fn clear(&mut self, context: &Context) {
        self.map.clear();
        *context.leak_map.lock() = None;
    }

    // Observe the camera image, then publish the map against the published LUT
    fn update(
        &mut self,
        context: &Context,
        view: &CameraView,
        calibration: &CameraCalibration,
        frame: &CameraFrame,
        exclusions: &[Vec4],
        lut: &[u8],
    ) {
        let start = Instant::now();

        if is_new_value(&mut self.last_lut_dump, "debug.alvr.key_lut_dump") {
            dump_lut(lut);
        }
        let is_clear_requested = is_new_value(&mut self.last_clear, "debug.alvr.key_leak_clear");
        if context.leak_map_clear.value() || is_clear_requested {
            context.leak_map_clear.set(false);
            self.clear(context);
            camera::log("leak map: cleared");
        }
        let is_dump_requested = is_new_value(&mut self.last_dump, "debug.alvr.key_leak_dump");

        let view_mode = debug_property("debug.alvr.key_leak_view").unwrap_or(0);
        let is_on = debug_property("debug.alvr.key_leak_map").unwrap_or(0) > 0
            || view_mode == 2
            || debug_property("debug.alvr.key_leak_dump").unwrap_or(0) != 0;
        if !is_on {
            // Hands are not excluded while it is off, so start again
            if self.is_on {
                self.clear(context);
                self.is_on = false;
            }
            return;
        }
        self.is_on = true;
        self.map.memory = debug_property("debug.alvr.key_leak_memory")
            .unwrap_or(120)
            .clamp(1, 3600) as f32;
        if self.alpha_changed && !lut.is_empty() {
            self.map.set_alpha_from_lut(lut);
            self.alpha_changed = false;
        }

        // Scene pixels as stage space rays, other pixels at each update
        let (offset_x, offset_y) =
            alvr_graphics::leak_sample_offset(self.sample_index, LEAK_MAP_STEP);
        self.sample_index = self.sample_index.wrapping_add(1);
        self.samples.clear();
        for y in (offset_y..calibration.height).step_by(LEAK_MAP_STEP as usize) {
            for x in (offset_x..calibration.width).step_by(LEAK_MAP_STEP as usize) {
                let ray = view.ray(Vec2::new(x as f32 + 0.5, y as f32 + 0.5));
                self.samples.push((ray, frame.rgb(x, y)));
            }
        }
        self.map.observe(
            view.position,
            &self.samples,
            exclusions,
            frame.timestamp_ns as f64 * 1e-9,
        );

        // None until the first learned LUT
        let snapshot = self.map.snapshot();
        if is_dump_requested {
            dump_leak_map(snapshot.as_ref(), view, calibration, exclusions);
        }
        if let Some(snapshot) = snapshot
            && !context.leak_map_clear.value()
        {
            let probes = if view_mode == 2 {
                snapshot.top_clusters(HAND_SEGMENTS)
            } else {
                Vec::new()
            };
            *context.leak_map.lock() = Some(Arc::new(LeakMapState {
                time: Instant::now(),
                snapshot,
                probes,
            }));
        }

        self.update_ms = start.elapsed().as_secs_f32() * 1000.0;
    }
}

// Whether the property has a new value. Not at the first call: an old value does nothing.
fn is_new_value(last: &mut Option<i64>, property: &str) -> bool {
    let value = debug_property(property).unwrap_or(0);
    let is_new = last.is_some_and(|last| last != value);
    *last = Some(value);

    is_new
}

// The leak map in the camera view: '#' leaks, '.' doesn't, '?' unknown, 'H' a hand is excluded
fn dump_leak_map(
    snapshot: Option<&LeakSnapshot>,
    view: &CameraView,
    calibration: &CameraCalibration,
    exclusions: &[Vec4],
) {
    let Some(snapshot) = snapshot else {
        camera::log("leak map: nothing to show yet (no learned colors)");
        return;
    };
    let [width, height] = LEAK_DUMP_SIZE;
    let rays = (0..height)
        .flat_map(|y| (0..width).map(move |x| (x, y)))
        .map(|(x, y)| {
            view.ray(Vec2::new(
                (x as f32 + 0.5) * calibration.width as f32 / width as f32,
                (y as f32 + 0.5) * calibration.height as f32 / height as f32,
            ))
        })
        .collect::<Vec<_>>();
    let text = snapshot
        .ascii_dump(&rays, width as usize)
        .lines()
        .zip(rays.chunks(width as usize))
        .map(|(line, rays)| {
            line.chars()
                .zip(rays)
                .map(|(c, ray)| {
                    let is_hand = exclusions
                        .iter()
                        .any(|sphere| alvr_graphics::ray_hits_sphere(view.position, *ray, *sphere));
                    if is_hand { 'H' } else { c }
                })
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    camera::log(&format!(
        "leak map of the camera view, known {:.0}%:\n{text}",
        snapshot.known_fraction() * 100.0
    ));
}

// For offline tests, a new file each time
fn dump_lut(lut: &[u8]) {
    if lut.is_empty() {
        camera::log("learned LUT: nothing learned yet");
        return;
    }
    let path = format!(
        "{LEARNED_LUT_DIR}/alvr_learned_key_{}.lut",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |time| time.as_secs())
    );
    match fs::create_dir_all(LEARNED_LUT_DIR).and_then(|()| fs::write(&path, lut)) {
        Ok(()) => camera::log(&format!("learned LUT written to {path}")),
        Err(e) => camera::log(&format!("learned LUT {path}: {e}")),
    }
}

fn percent(property: &str, default: i64) -> f32 {
    debug_property(property).unwrap_or(default).clamp(0, 1000) as f32 / 100.0
}

fn mean(colors: &[[u8; 3]]) -> [u32; 3] {
    let count = colors.len().max(1) as u32;
    [0, 1, 2].map(|c| colors.iter().map(|color| u32::from(color[c])).sum::<u32>() / count)
}

// Colors, camera, hands, and spheres around every located hand for the leak map
type Sampled = (
    HandColorSamples,
    CameraView,
    [Option<SampledHand>; 2],
    Vec<Vec4>,
);

fn sample(context: &Context, camera: &PassthroughCamera, frame: &CameraFrame) -> Option<Sampled> {
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
    let mut exclusions = Vec::with_capacity(2);
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
        let shape = HandShape::from_joints(
            &joints.map(|joint| crate::from_xr_vec3(joint.pose.position).extend(joint.radius)),
        );
        // Also partly tracked and fast hands: the leak map must not learn them
        let valid_speed = velocities
            .iter()
            .filter(|v| {
                v.velocity_flags
                    .contains(xr::SpaceVelocityFlags::LINEAR_VALID)
            })
            .map(|v| crate::from_xr_vec3(v.linear_velocity).length())
            .fold(0.0, f32::max);
        exclusions.push(alvr_graphics::hand_exclusion(shape.bounds(), valid_speed));
        if !all_tracked || speed > MAX_HAND_SPEED {
            return None;
        }

        let position = |joint: xr::HandJointEXT| crate::from_xr_vec3(joints[joint].pose.position);
        let wrist = position(xr::HandJointEXT::WRIST);

        Some(SampledHand {
            shape,
            wrist,
            // Opposite to the fingers: the skin of the forearm is not background
            arm_direction: (wrist - position(xr::HandJointEXT::MIDDLE_METACARPAL))
                .normalize_or_zero(),
            is_still: speed < STILL_HAND_SPEED,
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

    Some((samples, view, hands, exclusions))
}
