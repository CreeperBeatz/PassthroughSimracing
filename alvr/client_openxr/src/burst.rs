//! `debug.alvr.burst <n>`: records a few seconds of consecutive frames for offline tests (stacking
//! depth frames, moving the arm's depth with the wrist, the tiles around tracked hands, the camera
//! against the depth). A new value other than 0 starts a burst of `debug.alvr.burst_ms` (3000, 100 to
//! 6000; the value at the start of the app is not a request, like `key_dump`):
//! - every new depth frame as the runtime delivered it: both eyes read back from the GPU (16 bit
//!   values, written in meters like the depth dump), its capture time, the display time it came with,
//!   the pose and fov of each depth view, near / far, and when the app got it
//! - one record per display frame: display time, the eyes, the views of the stream, both hands (26
//!   joints with radii, located at the time the key uses), the capsules the mask used, the shift of the
//!   arms, which depth frame the mask used, the region (a record each time it changes)
//! - the camera images of the key learner (only with `auto_key` and the key on: one per pass of the
//!   learner, about every 60 ms), with their time, the camera pose and intrinsics
//! - one depth dump (`alvr_dump_<n>_depth*`) at the start, in the folder of the burst, and the learned
//!   colour table of the key last uploaded before the start (`alvr_dump_<n>_key.lut`, if there is one)
//!
//! Everything is kept in RAM (about 0.4 MB per depth frame, 0.23 MB per camera image) and written by a
//! thread after the burst to `<files>/alvr_burst_<n>/`: `alvr_burst_<n>.json` (the index, every
//! record in time order), `alvr_burst_<n>_depth_<i>_left.f32` / `_right`, `alvr_burst_<n>_camera_<i>.rgb`.
//! Log lines start with "burst:". Cost on the render thread: per depth frame two small passes and a
//! readback (800 KB copied from mapped memory once the GPU is done), per display frame the hand
//! joints located once more and a few small allocations.

use crate::passthrough::{self, debug_property};
use alvr_common::{
    glam::{UVec2, Vec3},
    parking_lot::Mutex,
};
use alvr_graphics::{
    ArmGap, ArmMotion, BURST_DEFAULT_MS, BURST_MAX_DEPTH, BURST_MAX_DISPLAY, BURST_MAX_MS,
    BURST_MIN_MS, Burst, BurstDepth, BurstDisplay, BurstHand, BurstLut, BurstRegion,
    CameraBurstSink, ColorKeyInput, DepthFrame, SharedCameraBurst,
};
use std::{
    path::PathBuf,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

// After its time the burst waits this long at most for readbacks still on their way
const FINISH_TIMEOUT: Duration = Duration::from_millis(1500);
// Failed readbacks logged one by one, the rest only counted
const LOGGED_FAILURES: u32 = 3;

/// What the render thread knows about a display frame
pub struct DisplayInput<'a> {
    pub display_ns: i64,
    pub now_ns: i64,
    pub views: [alvr_common::ViewParams; 2],
    pub stream_views: [alvr_common::ViewParams; 2],
    pub eye_positions: [Vec3; 2],
    /// The mask of this frame used a depth image
    pub has_depth: bool,
    pub hand_ns: i64,
    pub hands: [Option<BurstHand>; 2],
    pub color_key: Option<&'a ColorKeyInput>,
    pub arms: [Option<ArmMotion>; 2],
    /// The wrist gaps and the edge cut the mask used (`DepthRenderInput`)
    pub arm_gaps: [Option<ArmGap>; 2],
    /// The wrist gap each arm was set to (mm, 0: off), see `BurstDisplay::arm_gap_set_mm`
    pub arm_gap_set_mm: [f32; 2],
    pub edge_cut: f32,
    /// See `alvr_graphics::region_json`
    pub region: String,
}

struct Active {
    burst: Burst,
    start: Instant,
    duration: Duration,
    // Token of the first depth frame: a readback answer belongs to depth frame `token - first_token`
    first_token: u32,
    // Of the last depth frame, to tell a new one: capture time, swapchain index, position of the left
    // depth view (bits)
    last_depth: Option<(Option<i64>, u32, [u32; 3])>,
    // The depth limit was reached: the last depth frame is not recorded
    depth_full: bool,
    last_region: Option<String>,
    // Readbacks asked for and not answered yet
    waiting: usize,
    failures: u32,
    // The time is over: no new records, waiting for the readbacks (since when)
    closing: Option<Instant>,
}

pub struct BurstRecorder {
    last_value: Option<i64>,
    next_token: u32,
    active: Option<Active>,
    camera: SharedCameraBurst,
}

impl BurstRecorder {
    pub fn new() -> Self {
        Self {
            last_value: None,
            next_token: 1,
            active: None,
            camera: Arc::new(Mutex::new(None)),
        }
    }

    /// For the key learner: it fills it with its camera images while a burst runs
    #[cfg_attr(not(target_os = "android"), expect(dead_code))]
    pub fn camera_sink(&self) -> SharedCameraBurst {
        Arc::clone(&self.camera)
    }

    /// A burst runs or waits for its last readbacks
    pub fn is_active(&self) -> bool {
        self.active.is_some()
    }

    /// Records are taken (the time of the burst is not over)
    pub fn is_recording(&self) -> bool {
        self.active
            .as_ref()
            .is_some_and(|active| active.closing.is_none())
    }

    /// Reads `debug.alvr.burst` (the caller calls it about twice a second). Some(n) when burst n
    /// starts: the caller asks for a depth dump with the same n (written into the folder of the burst)
    pub fn poll(&mut self, now_ns: i64, depth_size: UVec2, flip_y: bool) -> Option<i64> {
        let value = debug_property("debug.alvr.burst").unwrap_or(0);
        let is_new = self.last_value.is_some_and(|last| last != value) && value != 0;
        self.last_value = Some(value);
        if !is_new {
            return None;
        }

        if let Some(active) = &self.active {
            crate::stream_log(&format!(
                "burst: {} ended early by burst {value}",
                active.burst.n
            ));
            self.finish(now_ns);
        }

        let duration_ms = debug_property("debug.alvr.burst_ms")
            .unwrap_or(BURST_DEFAULT_MS)
            .clamp(BURST_MIN_MS, BURST_MAX_MS);
        let duration = Duration::from_millis(duration_ms as u64);
        *self.camera.lock() = Some(CameraBurstSink::new(duration));
        self.active = Some(Active {
            burst: Burst {
                n: value,
                duration_ms,
                start_ns: now_ns,
                end_ns: now_ns,
                depth_size,
                flip_y,
                dump: None,
                key_lut: None,
                depth: Vec::new(),
                depth_skipped: 0,
                display: Vec::new(),
                camera: Vec::new(),
                regions: Vec::new(),
            },
            start: Instant::now(),
            duration,
            first_token: self.next_token,
            last_depth: None,
            depth_full: false,
            last_region: None,
            waiting: 0,
            failures: 0,
            closing: None,
        });
        crate::stream_log(&format!(
            "burst: {value} started, {duration_ms} ms: every new depth frame, every display frame, the \
             camera images of the key learner if it runs"
        ));

        Some(value)
    }

    /// The depth dump of the start of the burst was asked for (its JSON name)
    pub fn set_dump(&mut self, json_name: String) {
        if let Some(active) = &mut self.active {
            active.burst.dump = Some(json_name);
        }
    }

    /// The learned colour table of the key at the start (None: none), written next to the depth dump
    pub fn set_key_lut(&mut self, lut: Option<BurstLut>) {
        if let Some(active) = &mut self.active {
            active.burst.key_lut = lut;
        }
    }

    /// The depth frame of this display frame. Returns the token of the readback to ask for if it is a
    /// new one
    pub fn depth_frame(
        &mut self,
        frame: &DepthFrame,
        capture_ns: Option<i64>,
        display_ns: i64,
        now_ns: i64,
    ) -> Option<u32> {
        let active = self.active.as_mut().filter(|a| a.closing.is_none())?;
        let key = (
            capture_ns,
            frame.swapchain_index,
            frame.depth_views[0]
                .pose
                .position
                .to_array()
                .map(f32::to_bits),
        );
        if active.last_depth == Some(key) {
            return None;
        }
        active.last_depth = Some(key);

        let index = active.burst.depth.len();
        if index >= BURST_MAX_DEPTH {
            active.burst.depth_skipped += 1;
            active.depth_full = true;
            return None;
        }
        let token = active.first_token + index as u32;
        self.next_token = token + 1;
        active.burst.depth.push(BurstDepth {
            frame: *frame,
            capture_ns,
            display_ns,
            now_ns,
            raw: None,
            error: None,
        });
        active.waiting += 1;

        Some(token)
    }

    /// One display frame, after `depth_frame`
    pub fn display_frame(&mut self, input: DisplayInput) {
        let Some(active) = self.active.as_mut().filter(|a| a.closing.is_none()) else {
            return;
        };
        if active.burst.display.len() >= BURST_MAX_DISPLAY {
            return;
        }
        if active.last_region.as_ref() != Some(&input.region) {
            active.burst.regions.push(BurstRegion {
                now_ns: input.now_ns,
                json: input.region.clone(),
            });
            active.last_region = Some(input.region);
        }
        // The depth frame of this display frame is the last one recorded (None before the first, in a
        // frame without a depth image, and after the depth limit: the frame the mask used is not kept)
        let depth = active
            .last_depth
            .filter(|_| input.has_depth && !active.depth_full)
            .and_then(|_| active.burst.depth.len().checked_sub(1));
        active.burst.display.push(BurstDisplay {
            display_ns: input.display_ns,
            now_ns: input.now_ns,
            depth,
            region: active.burst.regions.len().checked_sub(1),
            views: input.views,
            stream_views: input.stream_views,
            eye_positions: input.eye_positions,
            hand_ns: input.hand_ns,
            hands: input.hands,
            capsules: input.color_key.map_or([None; 2], |key| key.hands),
            arms: input.arms,
            arm_gaps: input.arm_gaps,
            arm_gap_set_mm: input.arm_gap_set_mm,
            edge_cut: input.edge_cut,
        });
    }

    /// Answers of the renderer (`StreamRenderer::poll_burst_depth`)
    pub fn depth_images(&mut self, results: Vec<(u32, Result<Vec<u16>, String>)>) {
        let Some(active) = &mut self.active else {
            return;
        };
        for (token, result) in results {
            let Some(depth) = token
                .checked_sub(active.first_token)
                .and_then(|index| active.burst.depth.get_mut(index as usize))
            else {
                // Of an earlier burst
                continue;
            };
            if depth.raw.is_some() || depth.error.is_some() {
                continue;
            }
            active.waiting = active.waiting.saturating_sub(1);
            match result {
                Ok(raw) => depth.raw = Some(raw),
                Err(e) => {
                    active.failures += 1;
                    if active.failures <= LOGGED_FAILURES {
                        crate::stream_log(&format!(
                            "burst: {} depth frame {}: {e}",
                            active.burst.n,
                            token - active.first_token
                        ));
                    }
                    depth.error = Some(e);
                }
            }
        }
    }

    /// Call every frame: ends the burst after its time once every readback is answered (or after a
    /// timeout) and writes it on a thread
    pub fn update(&mut self, now_ns: i64) {
        let Some(active) = &mut self.active else {
            return;
        };
        if active.closing.is_none() && active.start.elapsed() >= active.duration {
            active.closing = Some(Instant::now());
            active.burst.end_ns = now_ns;
        }
        if let Some(closing) = active.closing
            && (active.waiting == 0 || closing.elapsed() > FINISH_TIMEOUT)
        {
            self.finish(now_ns);
        }
    }

    // Takes the camera images and writes the burst on a thread
    fn finish(&mut self, now_ns: i64) {
        let Some(mut active) = self.active.take() else {
            return;
        };
        if active.closing.is_none() {
            active.burst.end_ns = now_ns;
        }
        // Readbacks that never came
        for depth in &mut active.burst.depth {
            if depth.raw.is_none() && depth.error.is_none() {
                depth.error = Some("no answer from the GPU before the end of the burst".into());
            }
        }
        if let Some(sink) = self.camera.lock().take() {
            active.burst.camera = sink.into_records();
        }
        let burst = active.burst;
        let dir = burst_dir(burst.n);
        crate::stream_log(&format!(
            "burst: {} ended after {:.2} s: {} depth frames ({} read back, {} not recorded), {} \
             display frames, {} camera images, {} regions, {} failed readbacks; writing {:.1} MB to {}",
            burst.n,
            active.start.elapsed().as_secs_f32(),
            burst.depth.len(),
            burst.depth_read_count(),
            burst.depth_skipped,
            burst.display.len(),
            burst.camera.len(),
            burst.regions.len(),
            active.failures,
            burst.image_bytes() as f32 / 1e6,
            dir.display(),
        ));
        thread::spawn(move || {
            let start = Instant::now();
            match burst.write_to(&dir) {
                Ok((files, bytes)) => crate::stream_log(&format!(
                    "burst: {} written: {files} files, {:.1} MB in {} ({} ms)",
                    burst.n,
                    bytes as f32 / 1e6,
                    dir.display(),
                    start.elapsed().as_millis()
                )),
                Err(e) => crate::stream_log(&format!(
                    "burst: {} writing to {} failed: {e}",
                    burst.n,
                    dir.display()
                )),
            }
        });
    }
}

impl Drop for BurstRecorder {
    // The stream stopped during a burst (PC gone, pause, end of the session): what was recorded is
    // written all the same
    fn drop(&mut self) {
        let Some(active) = &self.active else {
            return;
        };
        crate::stream_log(&format!(
            "burst: {} cut short: the stream stopped",
            active.burst.n
        ));
        // No session left to ask for the time: the start plus the time since
        let now_ns = active.burst.start_ns + active.start.elapsed().as_nanos() as i64;
        self.finish(now_ns);
    }
}

/// The folder of burst `n`
pub fn burst_dir(n: i64) -> PathBuf {
    PathBuf::from(passthrough::FILES_DIR).join(alvr_graphics::burst_dir_name(n))
}
