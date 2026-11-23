//! Learns the colors of the passthrough color key while it is used, from the passthrough camera and
//! the tracked hands.

use crate::{
    camera::{self, CameraCalibration, CameraFrame, PassthroughCamera},
    interaction::InteractionContext,
    passthrough::{WristParams, debug_property},
    stream::PALM_JOINTS,
};
use alvr_common::{
    Pose, RelaxedAtomic,
    glam::{Quat, Vec2, Vec3, Vec4},
    parking_lot::{Mutex, RwLock},
};
use alvr_graphics::{
    BridgeChoice, CameraView, ColorModel, DumpHand, DumpJoint, HAND_SEGMENTS, HandColorSamples,
    HandShape, KeyDump, LeakMap, LeakSnapshot, LocatedHand, SEGMENT_JOINTS, SampledHand, WristAxes,
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
// The direction of the bridge from the wrist is forgotten if the camera didn't see it for this long
const BRIDGE_HOLD: Duration = Duration::from_secs(1);
// The leak map sees every 8th pixel in x and y (1200 of 320 x 240), see `leak_sample_offset`
const LEAK_MAP_STEP: u32 = 8;
// Grid of the camera view for `debug.alvr.key_leak_dump`
const LEAK_DUMP_SIZE: [u32; 2] = [40, 30];
// For `debug.alvr.key_lut_dump` and `debug.alvr.key_dump`
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
    bridges: [BridgeChoice; 2],
}

/// Segments of one hand by why they were or were not verified, and joint velocities without
/// LINEAR_VALID, counted over the passes since the last log
#[derive(Clone, Copy, Default)]
struct GateCounts {
    fast: usize,
    untracked: usize,
    // Fewer than 8 points in the image
    outside: usize,
    // Share of kept points below 0.5
    voted_off: usize,
    confirmed: usize,
    no_valid_velocity: usize,
}

/// The leak map for the render thread
pub struct LeakMapState {
    time: Instant,
    pub snapshot: LeakSnapshot,
    /// Directions of the leakiest clusters from the anchor, only for `debug.alvr.key_leak_view 2`
    pub probes: Vec<Vec3>,
}

/// Shared between the render thread and the learner, one lock so that a clear can't be overwritten
#[derive(Default)]
struct SharedLeakMap {
    state: Option<Arc<LeakMapState>>,
    // Forget the map at the first camera image from this time on (ns, CLOCK_MONOTONIC like the images)
    clear_at: Option<i64>,
}

struct Context {
    stage_space: Arc<xr::Space>,
    view_space: Arc<xr::Space>,
    interaction_context: Arc<RwLock<InteractionContext>>,
    running: Arc<RelaxedAtomic>,
    lut: Arc<Mutex<Option<Vec<u8>>>>,
    verified: Arc<Mutex<Option<Verified>>>,
    leak_map: Arc<Mutex<SharedLeakMap>>,
}

pub struct AutoColorKey {
    running: Arc<RelaxedAtomic>,
    lut: Arc<Mutex<Option<Vec<u8>>>>,
    verified: Arc<Mutex<Option<Verified>>>,
    leak_map: Arc<Mutex<SharedLeakMap>>,
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
        let leak_map = Arc::new(Mutex::new(SharedLeakMap::default()));
        let context = Context {
            stage_space,
            view_space,
            interaction_context,
            running: Arc::clone(&running),
            lut: Arc::clone(&lut),
            verified: Arc::clone(&verified),
            leak_map: Arc::clone(&leak_map),
        };

        Self {
            running,
            lut,
            verified,
            leak_map,
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

    /// Direction of the bridge from each wrist to the forearm, in the space of the wrist joint, and how
    /// sure the camera is about it. The default direction with confidence 0 if it is off or not known.
    pub fn wrist_bridges(&self) -> [BridgeChoice; 2] {
        match &*self.verified.lock() {
            Some(verified) if verified.time.elapsed() < BRIDGE_HOLD => verified.bridges,
            _ => [BridgeChoice::default(); 2],
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
            .state
            .as_ref()
            .filter(|state| state.time.elapsed() < LEAK_HOLD)
            .cloned()
    }

    /// Forget the leak map when the reference space changes at `change_time` (camera images from before
    /// it are not used)
    pub fn clear_leak_map(&self, change_time: xr::Time) {
        let mut shared = self.leak_map.lock();
        let time = change_time.as_nanos();
        shared.clear_at = Some(shared.clear_at.map_or(time, |pending| pending.max(time)));
        shared.state = None;
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
    let mut bridges = [BridgeChoice::default(); 2];
    let mut last_bridges = [Instant::now(); 2];
    // Flexion and deviation of each bridge, for the log
    let mut bridge_angles = [(0.0, 0.0); 2];
    let mut gates = [GateCounts::default(); 2];
    let mut last_dump_value = None;
    let mut dump = None;

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

            if is_new_value(&mut last_dump_value, "debug.alvr.key_dump") {
                dump = last_dump_value.filter(|n| *n != 0).map(|n| DumpRequest {
                    n,
                    count: debug_property("debug.alvr.key_dump_burst")
                        .unwrap_or(1)
                        .clamp(1, 30) as u32,
                    index: 0,
                    burst_ns: 0,
                    key_table: None,
                    key_alpha_table: None,
                    confirm_table: None,
                    pass_start_ns: 0,
                });
            }
            if let Some(dump) = &mut dump {
                dump.pass_start_ns = monotonic_ns();
            }

            // Once the camera is sure where the forearm is, it is not background
            let wrist = WristParams::load();
            let arm_directions = bridges.map(|bridge| {
                bridge.direction.filter(|_| {
                    wrist.is_some_and(|wrist| bridge.confidence >= wrist.min_confidence)
                })
            });
            let steady = debug_property("debug.alvr.key_steady").unwrap_or(1) > 0;
            let Some(Sampled {
                samples,
                view,
                hands,
                wrists,
                exclusions,
                verify_hands,
                verify_rings,
                located,
            }) = sample(
                context,
                &camera,
                &frame,
                arm_directions,
                steady,
                &mut gates,
                dump.is_some(),
            )
            else {
                continue;
            };
            model.unknown_keep = percent("debug.alvr.key_unknown", 10);
            model.skin_bias = percent("debug.alvr.key_bias", 150);
            model.tightness = percent("debug.alvr.key_tight", 30);
            model.skin_smoothing = percent("debug.alvr.key_smooth", 100);
            model.confirm_tightness = percent("debug.alvr.key_confirm", 12);
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
            let background_kept = [0, 1].map(|i| {
                if use_hand_kept {
                    hand_kept[i].unwrap_or(background_kept)
                } else {
                    background_kept
                }
            });

            // The verification sees the backgrounds of all hands it checks, with the confirm table. The
            // bridge and the narrower rim keep the key table above
            let mut verify_background = samples.background.clone();
            verify_background.extend(verify_rings.iter().flatten());
            let verify_kept = if !verify_background.is_empty() {
                model.confirm_kept_fraction(&verify_background)
            } else if steady {
                // Nothing around the hands: nothing can be said
                1.0
            } else {
                0.0
            };
            let verify_kept = [0, 1].map(|i| {
                if hands[i].is_none() && verify_hands[i].is_some() {
                    // Checked but not learned (a finger is fast): only its own ring tells what is
                    // behind it, the ring of the other hand doesn't. Too few colors: nothing is known
                    return model.confirm_leak_fraction(&verify_rings[i]).unwrap_or(1.0);
                }
                match model.confirm_leak_fraction(samples.hand_background(i)) {
                    Some(kept) if use_hand_kept => kept,
                    _ => verify_kept,
                }
            });

            // For the dump
            let mut pass_shares = [None; 2];
            let mut reliabilities = [0.0; 2];
            for (i, (weights, hand)) in weights.iter_mut().zip(&verify_hands).enumerate() {
                let reliability = alvr_graphics::verification_reliability(verify_kept[i]);
                reliabilities[i] = reliability;
                let shares = hand.as_ref().and_then(|(hand, eligible)| {
                    model.segment_shares(
                        &view,
                        camera.calibration.width,
                        camera.calibration.height,
                        hand,
                        eligible,
                        |x, y| frame.rgb(x, y),
                    )
                });
                pass_shares[i] = shares;
                let targets = shares.map_or([0.0; HAND_SEGMENTS], |shares| {
                    shares.map(|share| alvr_graphics::verification_weight(share, reliability))
                });
                // Lose trust immediately, gain it slowly
                for (weight, target) in weights.iter_mut().zip(targets) {
                    *weight = if target < *weight {
                        target
                    } else {
                        *weight + (target - *weight) * 0.5
                    };
                }

                if let (Some((_, eligible)), Some(shares)) = (hand, shares) {
                    for (is_eligible, share) in eligible.iter().zip(shares) {
                        match share {
                            None if *is_eligible => gates[i].outside += 1,
                            Some(share) if share < 0.5 => gates[i].voted_off += 1,
                            _ => (),
                        }
                    }
                }
                gates[i].confirmed += weights.iter().filter(|weight| **weight > 0.0).count();
            }

            // Which way from the wrist the camera sees the forearm
            for i in 0..2 {
                let is_fresh = match (wrist, &hands[i], &wrists[i]) {
                    (Some(wrist), Some(hand), Some((orientation, axes))) => {
                        let scores = model.bridge_scores(
                            &view,
                            camera.calibration.width,
                            camera.calibration.height,
                            hand,
                            *orientation,
                            axes,
                            wrist.length_m,
                            |x, y| frame.rgb(x, y),
                        );
                        let reliability =
                            alvr_graphics::verification_reliability(background_kept[i]);
                        let is_fresh = bridges[i].update(axes, &scores, reliability, wrist.follow);
                        if is_fresh {
                            bridge_angles[i] = bridges[i].angles(axes);
                        }
                        is_fresh
                    }
                    _ => false,
                };
                if is_fresh {
                    last_bridges[i] = Instant::now();
                } else if wrist.is_none() || last_bridges[i].elapsed() > BRIDGE_HOLD {
                    // The default direction, not sure
                    bridges[i] = BridgeChoice::default();
                    bridge_angles[i] = (0.0, 0.0);
                }
            }

            *context.verified.lock() = Some(Verified {
                time: Instant::now(),
                weights,
                leak,
                bridges,
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
                // Flexion / deviation in degrees and the confidence of each bridge
                let wrist_log = if wrist.is_some() {
                    let [left, right] = [0, 1].map(|i| {
                        format!(
                            "{:.0}/{:.0} deg {:.2}",
                            bridge_angles[i].0, bridge_angles[i].1, bridges[i].confidence
                        )
                    });
                    format!(", wrist {left}, {right}")
                } else {
                    String::new()
                };
                // Segments: fast, untracked, outside of the image, voted off, confirmed; joint
                // velocities without LINEAR_VALID
                let [left, right] = gates.map(|gate| {
                    format!(
                        "f{} u{} o{} v{} c{} nv{}",
                        gate.fast,
                        gate.untracked,
                        gate.outside,
                        gate.voted_off,
                        gate.confirmed,
                        gate.no_valid_velocity
                    )
                });
                gates = [GateCounts::default(); 2];
                let [lost_floor, lost_background] = model.lost_fractions(&samples.skin);
                let confirm_log = if model.confirm_tightness > 0.0 {
                    format!(
                        ", confirm kept {:.2} {:.2}",
                        model.confirm_kept_fraction(&samples.skin),
                        model.confirm_kept_fraction(&verify_background),
                    )
                } else {
                    String::new()
                };
                camera::log(&format!(
                    "auto key: skin {} kept {:.2} mean {:?}, background {} kept {:.2} mean {:?}, \
                     verified {:.2} {:.2}, leak {:.2} {:.2}, scene {} kept {:.2}, \
                     update {:.1} ms{wrist_log}{leak_map}, lost floor {lost_floor:.2} \
                     bg {lost_background:.2}, gate L {left}, R {right}{confirm_log}",
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

            if let Some(request) = &mut dump
                && let Some(located) = located
            {
                let (head, located) = *located;
                let dump_hands = [0, 1].map(|i| DumpHand {
                    located: located[i],
                    learned: hands[i].is_some(),
                    verified: verify_hands[i].is_some(),
                    eligible: verify_hands[i]
                        .as_ref()
                        .map_or([false; HAND_SEGMENTS], |(_, eligible)| *eligible),
                    shares: pass_shares[i],
                    weights: weights[i],
                    reliability: reliabilities[i],
                });
                let calibration = &camera.calibration;
                let pass = KeyDump {
                    n: request.n,
                    index: request.index,
                    count: request.count,
                    burst_ns: 0,
                    image_ns: frame.timestamp_ns,
                    image_age_ms: (request.pass_start_ns - frame.timestamp_ns) as f64 * 1e-6,
                    pass_interval_ms: elapsed * 1000.0,
                    monotonic_ms: request.pass_start_ns as f64 * 1e-6,
                    width: calibration.width,
                    height: calibration.height,
                    intrinsics: calibration.intrinsics,
                    camera: Pose {
                        orientation: view.orientation,
                        position: view.position,
                    },
                    head,
                    params: vec![
                        ("key_tight", model.tightness),
                        ("key_confirm", model.confirm_tightness),
                        ("key_smooth", model.skin_smoothing),
                        ("key_bias", model.skin_bias),
                        ("key_unknown", model.unknown_keep),
                        ("key_steady", f32::from(u8::from(steady))),
                    ],
                    counts: vec![
                        ("skin", samples.skin.len() as f32),
                        ("background", samples.background.len() as f32),
                        ("scene", samples.scene.len() as f32),
                        ("skin_kept", model.kept_fraction(&samples.skin)),
                        (
                            "skin_kept_confirm",
                            model.confirm_kept_fraction(&samples.skin),
                        ),
                        ("background_kept", model.kept_fraction(&samples.background)),
                        (
                            "background_kept_confirm",
                            model.confirm_kept_fraction(&samples.background),
                        ),
                    ],
                    key_table: None,
                    key_alpha_table: None,
                    confirm_table: None,
                    hands: dump_hands,
                };
                write_dump(request, pass, &frame, &last_lut, &model);
            }
            if dump
                .as_ref()
                .is_some_and(|request| request.index >= request.count)
            {
                dump = None;
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
        context.leak_map.lock().state = None;
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
        // The images before a change of the reference space are in the old one
        let clear_at = context.leak_map.lock().clear_at;
        let is_old_space = clear_at.is_some_and(|time| frame.timestamp_ns < time);
        if (clear_at.is_some() && !is_old_space) || is_clear_requested {
            if !is_old_space {
                let mut shared = context.leak_map.lock();
                if shared.clear_at == clear_at {
                    shared.clear_at = None;
                }
            }
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

        if is_old_space {
            return;
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
        if let Some(snapshot) = snapshot {
            let probes = if view_mode == 2 {
                snapshot.top_clusters(HAND_SEGMENTS)
            } else {
                Vec::new()
            };
            let mut shared = context.leak_map.lock();
            // Not if a clear came in the meantime
            if shared.clear_at.is_none() {
                shared.state = Some(Arc::new(LeakMapState {
                    time: Instant::now(),
                    snapshot,
                    probes,
                }));
            }
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

/// A burst of `debug.alvr.key_dump`
struct DumpRequest {
    n: i64,
    count: u32,
    // Of the next pass
    index: u32,
    // Image time of the first pass
    burst_ns: i64,
    // Names of the table files of the first pass, if they were written
    key_table: Option<String>,
    key_alpha_table: Option<String>,
    confirm_table: Option<String>,
    // CLOCK_MONOTONIC
    pass_start_ns: i64,
}

fn monotonic_ns() -> i64 {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut time) };

    time.tv_sec * 1_000_000_000 + time.tv_nsec
}

// For offline tests: the camera image as the learner reads it, the hands and the tables (with the
// first pass of a burst only, they change slowly)
fn write_dump(
    request: &mut DumpRequest,
    mut pass: KeyDump,
    frame: &CameraFrame,
    lut: &[u8],
    model: &ColorModel,
) {
    let name = format!("alvr_dump_{}_{:02}", request.n, request.index);
    let label = format!("dump {}/{:02}", request.n, request.index);
    let is_first = request.index == 0;
    request.index += 1;

    let result = fs::create_dir_all(LEARNED_LUT_DIR).and_then(|()| {
        if is_first {
            request.burst_ns = pass.image_ns;
            if !lut.is_empty() {
                let file = format!("{name}_key.lut");
                fs::write(format!("{LEARNED_LUT_DIR}/{file}"), lut)?;
                request.key_table = Some(file);
            }
            if let Some(key) = model.key_table() {
                let file = format!("{name}_key_alpha.lut");
                fs::write(format!("{LEARNED_LUT_DIR}/{file}"), key)?;
                request.key_alpha_table = Some(file);
            }
            if let Some(confirm) = model.confirm_table() {
                let file = format!("{name}_confirm.lut");
                fs::write(format!("{LEARNED_LUT_DIR}/{file}"), confirm)?;
                request.confirm_table = Some(file);
            }
        }
        pass.burst_ns = request.burst_ns;
        pass.key_table.clone_from(&request.key_table);
        pass.key_alpha_table.clone_from(&request.key_alpha_table);
        pass.confirm_table.clone_from(&request.confirm_table);

        let mut rgb = Vec::with_capacity(pass.width as usize * pass.height as usize * 3);
        for y in 0..pass.height {
            for x in 0..pass.width {
                rgb.extend(frame.rgb(x, y));
            }
        }
        fs::write(format!("{LEARNED_LUT_DIR}/{name}.rgb"), rgb)?;
        let path = format!("{LEARNED_LUT_DIR}/{name}.json");
        fs::write(&path, pass.to_json())?;

        Ok(path)
    });
    match result {
        Ok(path) => camera::log(&format!("{label}: {path}")),
        Err(e) => camera::log(&format!("{label}: {e}")),
    }
}

fn percent(property: &str, default: i64) -> f32 {
    debug_property(property).unwrap_or(default).clamp(0, 1000) as f32 / 100.0
}

fn mean(colors: &[[u8; 3]]) -> [u32; 3] {
    let count = colors.len().max(1) as u32;
    [0, 1, 2].map(|c| colors.iter().map(|color| u32::from(color[c])).sum::<u32>() / count)
}

struct Sampled {
    samples: HandColorSamples,
    view: CameraView,
    /// The hands whose colors are learned
    hands: [Option<SampledHand>; 2],
    /// The orientation and the axes of their wrists
    wrists: [Option<(Quat, WristAxes)>; 2],
    /// Spheres around every located hand for the leak map
    exclusions: Vec<Vec4>,
    /// The hands that are verified, and which of their segments
    verify_hands: [Option<(SampledHand, [bool; HAND_SEGMENTS])>; 2],
    /// The background colors of verified hands whose colors are not learned. Not learned either
    verify_rings: [Vec<[u8; 3]>; 2],
    /// The head and the located hands, only for `debug.alvr.key_dump`
    located: Option<Box<(Pose, [Option<LocatedHand>; 2])>>,
}

// `arm_directions`: of the forearms where the camera is sure, in the space of the wrist joint. None: the
// default, opposite to the fingers. `steady`: see `debug.alvr.key_steady`. `gates`: counts why
// segments are not verified. `keep_located`: for `debug.alvr.key_dump`
fn sample(
    context: &Context,
    camera: &PassthroughCamera,
    frame: &CameraFrame,
    arm_directions: [Option<Vec3>; 2],
    steady: bool,
    gates: &mut [GateCounts; 2],
    keep_located: bool,
) -> Option<Sampled> {
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
    let mut exclusions = Vec::with_capacity(2 * (1 + alvr_graphics::ARM_EXCLUSION_SPHERES));
    let mut wrists = [None; 2];
    let mut verify_hands = [None; 2];
    let mut located = [None; 2];
    let hands = [0, 1].map(|i| {
        let tracker = interaction_context.hands_interaction[i]
            .skeleton_tracker
            .as_ref()?;
        let (joints, velocities) = context
            .stage_space
            .relate_hand_joints(tracker, time)
            .ok()
            .flatten()?;

        let tracked = joints.map(|joint| {
            joint
                .location_flags
                .contains(xr::SpaceLocationFlags::POSITION_TRACKED)
        });
        let speeds = velocities.map(|v| crate::from_xr_vec3(v.linear_velocity).length());
        let all_tracked = tracked.iter().all(|is_tracked| *is_tracked);
        let speed = speeds.iter().copied().fold(0.0, f32::max);
        gates[i].no_valid_velocity += velocities
            .iter()
            .filter(|v| {
                !v.velocity_flags
                    .contains(xr::SpaceVelocityFlags::LINEAR_VALID)
            })
            .count();
        let shape = HandShape::from_joints(
            &joints.map(|joint| crate::from_xr_vec3(joint.pose.position).extend(joint.radius)),
        );
        if keep_located {
            let joints = std::array::from_fn(|joint| DumpJoint {
                position: crate::from_xr_vec3(joints[joint].pose.position),
                radius: joints[joint].radius,
                tracked: tracked[joint],
                velocity: crate::from_xr_vec3(velocities[joint].linear_velocity),
                velocity_valid: velocities[joint]
                    .velocity_flags
                    .contains(xr::SpaceVelocityFlags::LINEAR_VALID),
            });
            located[i] = Some(LocatedHand { joints, shape });
        }
        // Also partly tracked and fast hands: the leak map must not learn them
        let valid_speed = velocities
            .iter()
            .filter(|v| {
                v.velocity_flags
                    .contains(xr::SpaceVelocityFlags::LINEAR_VALID)
            })
            .map(|v| crate::from_xr_vec3(v.linear_velocity).length())
            .fold(0.0, f32::max);
        let position = |joint: xr::HandJointEXT| crate::from_xr_vec3(joints[joint].pose.position);
        let wrist = position(xr::HandJointEXT::WRIST);
        // Opposite to the fingers: the skin of the forearm is not background
        let default_direction =
            (wrist - position(xr::HandJointEXT::MIDDLE_METACARPAL)).normalize_or_zero();
        let wrist_joint = joints[xr::HandJointEXT::WRIST];
        let wrist_orientation = wrist_joint
            .location_flags
            .contains(xr::SpaceLocationFlags::ORIENTATION_VALID)
            .then(|| crate::from_xr_quat(wrist_joint.pose.orientation).normalize());
        // Or where the camera saw it
        let arm_direction = wrist_orientation
            .zip(arm_directions[i])
            .map_or(default_direction, |(orientation, local)| {
                orientation * local
            });

        exclusions.push(alvr_graphics::hand_exclusion(shape.bounds(), valid_speed));
        exclusions.extend(alvr_graphics::arm_exclusions(
            wrist,
            arm_direction,
            valid_speed,
        ));
        let hand = SampledHand {
            shape,
            wrist,
            arm_direction,
            is_still: speed < STILL_HAND_SPEED,
        };

        // Verified are the segments whose joints and the palm are tracked and slow (`steady`), or all
        // of them if the whole hand is. A joint that is not tracked has no meaningful speed
        let palm_untracked = PALM_JOINTS.iter().any(|joint| !tracked[*joint]);
        let palm_fast = PALM_JOINTS
            .iter()
            .any(|joint| speeds[*joint] > MAX_HAND_SPEED);
        let mut eligible = [false; HAND_SEGMENTS];
        for (segment, (start, end)) in SEGMENT_JOINTS.into_iter().enumerate() {
            let (is_untracked, is_fast) = if steady {
                (
                    palm_untracked || !tracked[start] || !tracked[end],
                    palm_fast || speeds[start] > MAX_HAND_SPEED || speeds[end] > MAX_HAND_SPEED,
                )
            } else {
                (!all_tracked, speed > MAX_HAND_SPEED)
            };
            if is_untracked {
                gates[i].untracked += 1;
            } else if is_fast {
                gates[i].fast += 1;
            } else {
                eligible[segment] = true;
            }
        }
        verify_hands[i] = eligible.contains(&true).then_some((hand, eligible));

        // The colors only of the whole hand
        if !all_tracked || speed > MAX_HAND_SPEED {
            return None;
        }

        // The joint spaces have +y out of the back of the hand
        wrists[i] = wrist_orientation.and_then(|orientation| {
            let to_local = orientation.inverse();
            let thumb = position(xr::HandJointEXT::INDEX_PROXIMAL)
                - position(xr::HandJointEXT::LITTLE_PROXIMAL);
            let axes = WristAxes::new(to_local * default_direction, Vec3::NEG_Y, to_local * thumb)?;

            Some((orientation, axes))
        });

        Some(hand)
    });
    drop(interaction_context);

    let samples = alvr_graphics::sample_hand_colors(
        &view,
        calibration.width,
        calibration.height,
        &hands,
        |x, y| frame.rgb(x, y),
    );
    // Verified hands whose colors are not learned: only their backgrounds, for the reliability
    let verify_rings = [0, 1].map(|i| match (&hands[i], &verify_hands[i]) {
        (None, Some((hand, _))) => alvr_graphics::sample_ring_colors(
            &view,
            calibration.width,
            calibration.height,
            hand,
            verify_hands[1 - i].as_ref().map(|(other, _)| other),
            |x, y| frame.rgb(x, y),
        ),
        _ => Vec::new(),
    });

    Some(Sampled {
        samples,
        view,
        hands,
        wrists,
        exclusions,
        verify_hands,
        verify_rings,
        located: keep_located.then(|| Box::new((head, located))),
    })
}
