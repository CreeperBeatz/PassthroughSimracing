#[cfg(target_os = "android")]
use crate::auto_color_key::AutoColorKey;
use crate::{
    extra_extensions::EnvironmentDepthMETA,
    graphics::{self, ProjectionLayerAlphaConfig, ProjectionLayerBuilder},
    interaction::{self, InteractionContext, InteractionSourcesConfig},
    passthrough::ColorKeyParams,
};
use alvr_client_core::{
    ClientCoreContext, VideoFrameMetadata,
    video_decoder::{self, VideoDecoderConfig, VideoDecoderSource},
};
use alvr_common::{
    AlvrFoveatedEncodingParams, DETACHED_CONTROLLER_LEFT_ID, DETACHED_CONTROLLER_RIGHT_ID,
    HAND_LEFT_ID, HAND_RIGHT_ID, HEAD_ID, Pose, RelaxedAtomic, ViewParams,
    anyhow::Result,
    error,
    glam::{Quat, UVec2, Vec2, Vec3},
    info,
    parking_lot::RwLock,
};
use alvr_graphics::{
    ArmMotion, ColorKeyInput, DepthRenderInput, GraphicsContext, HAND_SEGMENTS, HandShape,
    LeakGains, SEGMENT_JOINTS, StreamRenderer, StreamViewParams,
};
use alvr_packets::{ClientStreamConfig, RealTimeConfig, TrackingData};
use alvr_session::{
    ClientsideFoveationConfig, ClientsideFoveationMode, ClientsidePostProcessingConfig, CodecType,
    MediacodecProperty, PassthroughMode, UpscalingConfig,
};
use alvr_system_info::Platform;
use openxr as xr;
use std::{
    cell::Cell,
    ptr,
    rc::Rc,
    sync::Arc,
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const DECODER_MAX_TIMEOUT_MULTIPLIER: f32 = 0.8;

// The solid fill of the hands fades out between these speeds (m/s)
const MIN_FILL_SPEED: f32 = 0.08;
const MAX_FILL_SPEED: f32 = 0.25;

// How often `debug.alvr.arm_shift` is read
const ARM_SHIFT_POLL_INTERVAL: Duration = Duration::from_millis(500);

// The playzone is remembered when no hands were tracked for a while
const PLAYZONE_HANDS_TIMEOUT: Duration = Duration::from_millis(1500);
const PLAYZONE_INTERVAL: Duration = Duration::from_millis(500);

// The layers of the color key stay a moment after the hands left the view, so they don't flicker
const KEY_LAYERS_HOLD: Duration = Duration::from_millis(300);
// Of the view, around the hands, so that the edge of the cropped layer is not on a hand
const KEY_CROP_MARGIN: f32 = 0.03;

// Per hand for the log: ring, map, known share of the map, leak, rim
type LeakLog = Option<(f32, Option<f32>, f32, f32, f32)>;

// Spheres of `debug.alvr.key_leak_view 2`, meters
const LEAK_PROBE_RADIUS: f32 = 0.0125;
// The leak map values are logged this often while it is used
const LEAK_LOG_INTERVAL: Duration = Duration::from_secs(2);

/// What the color key needs in this frame
#[derive(Clone, Copy, Default)]
pub struct ColorKeyFrame {
    /// Hands with a pose
    pub tracked_hands: [bool; 2],
    /// Part of each view with the hands, see `hands_view_rect`
    pub hand_rects: [Option<(Vec2, Vec2)>; 2],
    /// The hands are in view, or were a moment ago: the layers of the color key are needed
    pub hands_in_view: bool,
}

pub struct ParsedStreamConfig {
    pub view_resolution: UVec2,
    pub refresh_rate_hint: f32,
    pub encoding_gamma: f32,
    pub enable_hdr: bool,
    pub passthrough: Option<PassthroughMode>,
    pub foveated_encoding_config: Option<AlvrFoveatedEncodingParams>,
    pub clientside_foveation_config: Option<ClientsideFoveationConfig>,
    pub clientside_post_processing: Option<ClientsidePostProcessingConfig>,
    pub upscaling: Option<UpscalingConfig>,
    pub force_software_decoder: bool,
    pub max_buffering_frames: f32,
    pub buffering_history_weight: f32,
    pub decoder_options: Vec<(String, MediacodecProperty)>,
    pub interaction_sources: InteractionSourcesConfig,
}

impl ParsedStreamConfig {
    pub fn new(config: &ClientStreamConfig) -> Self {
        Self {
            view_resolution: config.negotiated_config.view_resolution,
            refresh_rate_hint: config.negotiated_config.refresh_rate_hint,
            encoding_gamma: config.negotiated_config.encoding_gamma,
            enable_hdr: config.negotiated_config.enable_hdr,
            passthrough: config.settings.video.passthrough.as_option().cloned(),
            foveated_encoding_config: config.negotiated_config.foveated_encoding,
            clientside_foveation_config: config
                .settings
                .video
                .clientside_foveation
                .as_option()
                .cloned(),
            clientside_post_processing: config
                .settings
                .video
                .clientside_post_processing
                .as_option()
                .cloned(),
            upscaling: config.settings.video.upscaling.as_option().cloned(),
            force_software_decoder: config.settings.video.force_software_decoder,
            max_buffering_frames: config.settings.video.max_buffering_frames,
            buffering_history_weight: config.settings.video.buffering_history_weight,
            decoder_options: config.settings.video.mediacodec_extra_options.clone(),
            interaction_sources: InteractionSourcesConfig::new(config),
        }
    }
}

pub struct StreamContext {
    core_context: Arc<ClientCoreContext>,
    xr_session: xr::Session<xr::OpenGlEs>,
    gfx_ctx: Rc<GraphicsContext>,
    interaction_context: Arc<RwLock<InteractionContext>>,
    stage_reference_space: Arc<xr::Space>,
    view_reference_space: Arc<xr::Space>,
    swapchains: [xr::Swapchain<xr::OpenGlEs>; 2],
    last_good_video_frame_metadata: VideoFrameMetadata,
    input_thread: Option<JoinHandle<()>>,
    input_thread_running: Arc<RelaxedAtomic>,
    config: ParsedStreamConfig,
    target_view_resolution: UVec2,
    renderer: StreamRenderer,
    decoder: Option<(VideoDecoderConfig, VideoDecoderSource)>,
    use_custom_reprojection: bool,
    // Created on demand when the depth passthrough mode is selected
    environment_depth: Option<EnvironmentDepthMETA>,
    environment_depth_failed: bool,
    // Set if the passthrough is color keyed around the hands
    color_key: Option<ColorKeyParams>,
    // Learns the colors of the key, if enabled
    #[cfg(target_os = "android")]
    auto_color_key: Option<AutoColorKey>,
    // The depth of the arms is shifted by the motion of the wrist since the depth image was captured.
    // `setprop debug.alvr.arm_shift 0`: plain depth
    arm_shift: bool,
    last_arm_shift_poll: Instant,
    tracked_hands: Cell<[bool; 2]>,
    // Last part of each view with the hands, and when there was one
    last_hand_rects: [Option<(Vec2, Vec2)>; 2],
    last_hand_rect_time: Option<Instant>,
    last_hands_seen: Instant,
    last_playzone_capture: Instant,
    last_playzone_request: i64,
    // Smoothed, meters per second
    hand_speeds: [Cell<f32>; 2],
    hand_solid: [Cell<[f32; HAND_SEGMENTS]>; 2],
    // How much the rim of each hand is narrowed over backgrounds that look like it (0..1), smoothed
    hand_leak: [Cell<f32>; 2],
    // Log of the leak map: last time, and the lookup time since then
    last_leak_log: Cell<Instant>,
    leak_lookup_time: Cell<(Duration, u32)>,
}

impl StreamContext {
    pub fn new(
        core_ctx: Arc<ClientCoreContext>,
        xr_session: xr::Session<xr::OpenGlEs>,
        gfx_ctx: Rc<GraphicsContext>,
        interaction_ctx: Arc<RwLock<InteractionContext>>,
        config: ParsedStreamConfig,
    ) -> StreamContext {
        interaction_ctx
            .write()
            .select_sources(&config.interaction_sources);

        let xr_exts = xr_session.instance().exts();

        if xr_exts.fb_display_refresh_rate.is_some() {
            xr_session
                .request_display_refresh_rate(config.refresh_rate_hint)
                .unwrap();
        }

        let foveation_profile = if let Some(config) = &config.clientside_foveation_config
            && xr_exts.fb_swapchain_update_state.is_some()
            && xr_exts.fb_foveation.is_some()
            && xr_exts.fb_foveation_configuration.is_some()
        {
            let level;
            let dynamic;
            match config.mode {
                ClientsideFoveationMode::Static { level: lvl } => {
                    level = lvl;
                    dynamic = false;
                }
                ClientsideFoveationMode::Dynamic { max_level } => {
                    level = max_level;
                    dynamic = true;
                }
            };

            xr_session
                .create_foveation_profile(Some(xr::FoveationLevelProfile {
                    level: xr::FoveationLevelFB::from_raw(level as i32),
                    vertical_offset: config.vertical_offset_deg,
                    dynamic: xr::FoveationDynamicFB::from_raw(dynamic as i32),
                }))
                .ok()
        } else {
            None
        };

        let target_view_resolution = alvr_graphics::compute_target_view_resolution(
            config.view_resolution,
            &config.upscaling,
        );
        let format = graphics::swapchain_format(&gfx_ctx, &xr_session, config.enable_hdr);

        let swapchains = [
            graphics::create_swapchain(
                &xr_session,
                &gfx_ctx,
                target_view_resolution,
                format,
                foveation_profile.as_ref(),
            ),
            graphics::create_swapchain(
                &xr_session,
                &gfx_ctx,
                target_view_resolution,
                format,
                foveation_profile.as_ref(),
            ),
        ];

        let renderer = StreamRenderer::new(
            Rc::clone(&gfx_ctx),
            config.view_resolution,
            target_view_resolution,
            [
                swapchains[0]
                    .enumerate_images()
                    .unwrap()
                    .iter()
                    .map(|i| *i as _)
                    .collect(),
                swapchains[1]
                    .enumerate_images()
                    .unwrap()
                    .iter()
                    .map(|i| *i as _)
                    .collect(),
            ],
            format,
            config.foveated_encoding_config,
            !((core_ctx.platform().is_pico()
                || (core_ctx.platform() == Platform::SamsungGalaxyXR))
                && config.enable_hdr),
            // TODO: Find a driver heuristic for the limited range bug instead?
            core_ctx.platform() != Platform::SamsungGalaxyXR && !config.enable_hdr,
            config.encoding_gamma,
            config.upscaling.clone(),
        );

        {
            let int_ctx = interaction_ctx.read();
            core_ctx.send_active_interaction_profile(
                *HAND_LEFT_ID,
                int_ctx.hands_interaction[0].controllers_profile_id,
                int_ctx.hands_interaction[0].input_ids.clone(),
            );
            core_ctx.send_active_interaction_profile(
                *HAND_RIGHT_ID,
                int_ctx.hands_interaction[1].controllers_profile_id,
                int_ctx.hands_interaction[1].input_ids.clone(),
            );
        }

        let input_thread_running = Arc::new(RelaxedAtomic::new(false));

        let stage_reference_space = Arc::new(interaction::get_reference_space(
            &xr_session,
            xr::ReferenceSpaceType::STAGE,
        ));
        let view_reference_space = Arc::new(interaction::get_reference_space(
            &xr_session,
            xr::ReferenceSpaceType::VIEW,
        ));

        let mut this = StreamContext {
            use_custom_reprojection: core_ctx.platform().is_yvr(),
            core_context: core_ctx,
            xr_session,
            gfx_ctx,
            interaction_context: interaction_ctx,
            stage_reference_space,
            view_reference_space,
            swapchains,
            last_good_video_frame_metadata: VideoFrameMetadata {
                view_params: [ViewParams::DUMMY; 2],
                foveation_center_shifts: None,
            },
            input_thread: None,
            input_thread_running,
            config,
            target_view_resolution,
            renderer,
            decoder: None,
            environment_depth: None,
            environment_depth_failed: false,
            color_key: None,
            #[cfg(target_os = "android")]
            auto_color_key: None,
            arm_shift: true,
            last_arm_shift_poll: Instant::now(),
            tracked_hands: Cell::new([false; 2]),
            last_hand_rects: [None; 2],
            last_hand_rect_time: None,
            last_hands_seen: Instant::now(),
            last_playzone_capture: Instant::now(),
            last_playzone_request: 0,
            hand_speeds: [const { Cell::new(0.0) }; 2],
            hand_solid: [const { Cell::new([0.0; HAND_SEGMENTS]) }; 2],
            hand_leak: [const { Cell::new(0.0) }; 2],
            last_leak_log: Cell::new(Instant::now()),
            leak_lookup_time: Cell::new((Duration::ZERO, 0)),
        };

        this.update_reference_space();

        this
    }

    pub fn uses_depth_passthrough(&self) -> bool {
        alvr_graphics::depth_passthrough_config(self.config.passthrough.as_ref()).is_some()
    }

    pub fn set_color_key(&mut self, params: Option<ColorKeyParams>) {
        // The depth map shows the hands where they were a while ago. With the color key they are not
        // needed there.
        if self.color_key.is_some() != params.is_some()
            && let Some(depth) = &self.environment_depth
        {
            depth.set_hand_removal(params.is_some());
        }
        self.color_key = params;

        #[cfg(target_os = "android")]
        if self.color_key.is_none() {
            self.auto_color_key = None;
        } else if self.auto_color_key.is_none() && crate::auto_color_key::enabled() {
            self.auto_color_key = Some(AutoColorKey::new(
                Arc::clone(&self.stage_reference_space),
                Arc::clone(&self.view_reference_space),
                Arc::clone(&self.interaction_context),
            ));
        }
    }

    /// New colors for the key, learned from the camera
    pub fn take_color_key_lut(&self) -> Option<Vec<u8>> {
        #[cfg(target_os = "android")]
        return self.auto_color_key.as_ref()?.take_lut();
        #[cfg(not(target_os = "android"))]
        None
    }

    // Objects that are held are partly missing in the depth image, so remember how the playzone looks
    // without hands
    fn should_capture_playzone(&mut self) -> bool {
        if self.tracked_hands.get().contains(&true) {
            self.last_hands_seen = Instant::now();
        }

        let request = crate::passthrough::debug_property("debug.alvr.playzone").unwrap_or(0);
        let is_requested = request != self.last_playzone_request && request > 0;
        // A negative value forgets the playzone
        if request < 0 && request != self.last_playzone_request {
            self.renderer.clear_playzone();
        }
        self.last_playzone_request = request;

        let is_automatic = self.color_key.is_some()
            && self.last_hands_seen.elapsed() > PLAYZONE_HANDS_TIMEOUT
            && self.last_playzone_capture.elapsed() > PLAYZONE_INTERVAL
            && crate::passthrough::debug_property("debug.alvr.playzone_auto").unwrap_or(1) > 0;

        let capture = is_requested || is_automatic;
        if capture {
            self.last_playzone_capture = Instant::now();
        }

        capture
    }

    // How the arms moved since the depth image was captured. None if `debug.alvr.arm_shift` is 0
    fn arm_motions(
        &mut self,
        display_time: xr::Time,
        capture_time: xr::Time,
        views: &[xr::View],
    ) -> [Option<ArmMotion>; 2] {
        // Bigger displacements are tracking errors
        const MAX_DISPLACEMENT: f32 = 0.25;
        // Rough position of the elbows, relative to the head
        const ELBOW_OFFSET: Vec3 = Vec3::new(0.2, -0.5, 0.05);

        if self.last_arm_shift_poll.elapsed() > ARM_SHIFT_POLL_INTERVAL {
            self.last_arm_shift_poll = Instant::now();
            self.arm_shift =
                crate::passthrough::debug_property("debug.alvr.arm_shift").unwrap_or(1) != 0;
        }
        if !self.arm_shift {
            return [None; 2];
        }

        let delay = self
            .color_key
            .map_or(0, |params| params.passthrough_delay_ms * 1_000_000);
        let now = xr::Time::from_nanos(display_time.as_nanos() - delay);
        let int_ctx = self.interaction_context.read();

        let head_position = (crate::from_xr_vec3(views[0].pose.position)
            + crate::from_xr_vec3(views[1].pose.position))
            / 2.0;
        let forward = crate::from_xr_quat(views[0].pose.orientation) * Vec3::NEG_Z;
        let yaw = Quat::from_rotation_y(f32::atan2(-forward.x, -forward.z));

        [0, 1].map(|i| {
            let tracker = int_ctx.hands_interaction[i].skeleton_tracker.as_ref()?;
            let wrist = |time| {
                let joints = self
                    .stage_reference_space
                    .locate_hand_joints(tracker, time)
                    .ok()
                    .flatten()?;

                Some(crate::from_xr_vec3(
                    joints[xr::HandJointEXT::WRIST].pose.position,
                ))
            };
            let wrist_now = wrist(now)?;
            let displacement = wrist_now - wrist(capture_time)?;

            let side = if i == 0 { -1.0 } else { 1.0 };
            let elbow = head_position + yaw * (ELBOW_OFFSET * Vec3::new(side, 1.0, 1.0));

            Some(ArmMotion {
                wrist: wrist_now,
                direction: (elbow - wrist_now).normalize_or(Vec3::NEG_Y),
                displacement: displacement.clamp_length_max(MAX_DISPLACEMENT),
            })
        })
    }

    // The hands, at the moment shown by the passthrough image. `eye_positions` (stage space) are needed
    // for the leak map.
    fn color_key_input(
        &self,
        display_time: xr::Time,
        eye_positions: Option<[Vec3; 2]>,
    ) -> Option<ColorKeyInput> {
        let params = self.color_key?;
        let delay = params.passthrough_delay_ms * 1_000_000;
        let time = xr::Time::from_nanos(display_time.as_nanos() - delay);
        let int_ctx = self.interaction_context.read();

        #[cfg(target_os = "android")]
        let verified = self
            .auto_color_key
            .as_ref()
            .map_or([[0.0; HAND_SEGMENTS]; 2], |key| key.verified_hands());
        #[cfg(not(target_os = "android"))]
        let verified = [[0.0; HAND_SEGMENTS]; 2];

        let uses_leak = params.leak_gain > 0.0 || params.leak_force > 0.0 || params.leak_map > 0.0;
        #[cfg(target_os = "android")]
        let ring_leak = self
            .auto_color_key
            .as_ref()
            .filter(|_| uses_leak)
            .map_or([0.0; 2], |key| key.ring_leak());
        #[cfg(not(target_os = "android"))]
        let ring_leak = [0.0; 2];

        // Where the room leaks through the key, learned only while it is used
        let shows_probes = params.leak_view == 2;
        #[cfg(target_os = "android")]
        let leak_map_state = self
            .auto_color_key
            .as_ref()
            .filter(|_| params.leak_map > 0.0 || shows_probes)
            .and_then(|key| key.leak_map());
        #[cfg(target_os = "android")]
        let leak_map = leak_map_state
            .as_deref()
            .map(|state| (&state.snapshot, state.probes.as_slice()));
        #[cfg(not(target_os = "android"))]
        let leak_map: Option<(&alvr_graphics::LeakSnapshot, &[Vec3])> = None;
        let map_eyes = eye_positions.filter(|_| params.leak_map > 0.0);
        let gains = LeakGains {
            force: params.leak_force,
            ring_gain: params.leak_gain,
            ring_lo: params.leak_lo,
            ring_hi: params.leak_hi,
            map_weight: params.leak_map,
            map_lo: params.leak_map_lo,
            map_hi: params.leak_map_hi,
        };
        let mut lookup_time = Duration::ZERO;
        let mut leak_log: [LeakLog; 2] = [None; 2];

        let hands = [0, 1].map(|i| {
            let tracker = int_ctx.hands_interaction[i].skeleton_tracker.as_ref()?;
            let (joints, velocities) = self
                .stage_reference_space
                .relate_hand_joints(tracker, time)
                .ok()
                .flatten()?;

            let shape = HandShape::from_joints(&joints.map(|joint| {
                crate::from_xr_vec3(joint.pose.position).extend(joint.radius * params.radius_scale)
            }));

            // Some joints may have no valid velocity
            let speed = velocities
                .iter()
                .filter(|v| {
                    v.velocity_flags
                        .contains(xr::SpaceVelocityFlags::LINEAR_VALID)
                })
                .map(|v| crate::from_xr_vec3(v.linear_velocity).length())
                .fold(0.0, f32::max);
            // Grow immediately, shrink slowly: velocities are noisy
            let speed = speed.max(self.hand_speeds[i].get() * 0.9);
            self.hand_speeds[i].set(speed);

            let tracked_count = joints
                .iter()
                .filter(|joint| {
                    joint
                        .location_flags
                        .contains(xr::SpaceLocationFlags::POSITION_TRACKED)
                })
                .count();
            // Verified segments are solid, as long as they don't move: the tracking is late
            let mut solid = self.hand_solid[i].get();
            for (segment, weight) in solid.iter_mut().enumerate() {
                let (start, end) = SEGMENT_JOINTS[segment];
                let is_tracked = [start, end].iter().all(|joint| {
                    joints[*joint]
                        .location_flags
                        .contains(xr::SpaceLocationFlags::POSITION_TRACKED)
                });
                let segment_speed = [start, end]
                    .map(|joint| crate::from_xr_vec3(velocities[joint].linear_velocity).length())
                    .into_iter()
                    .fold(0.0, f32::max);
                let stillness = 1.0
                    - ((segment_speed - MIN_FILL_SPEED) / (MAX_FILL_SPEED - MIN_FILL_SPEED))
                        .clamp(0.0, 1.0);
                let target = if is_tracked {
                    verified[i][segment] * stillness
                } else {
                    0.0
                };
                // No popping, but let go quickly
                *weight += (target - *weight) * if target < *weight { 0.35 } else { 0.1 };
            }
            self.hand_solid[i].set(solid);

            let mut zones = params.zones;
            for (zone, solid) in zones.solid.iter_mut().zip(solid) {
                // Some segments may always be solid
                *zone = f32::max(*zone, solid);
            }

            let zones = zones
                .for_confidence(tracked_count as f32 / joints.len() as f32)
                .for_speed(
                    speed,
                    params.core_error_s,
                    params.rim_error_s,
                    params.max_rim_growth_m,
                );

            // Where the background around the hand looks like it, the rim would show it
            let zones = if uses_leak {
                // The map: the cells behind the segments, seen from both eyes
                let (map_leak, known) = match (leak_map, map_eyes) {
                    (Some((snapshot, _)), Some(eyes)) => {
                        let lookup_start = Instant::now();
                        let points = shape
                            .segments
                            .map(|[start, end]| ((start + end) * 0.5).truncate());
                        let result = snapshot.hand_leak_known(&eyes, &points);
                        lookup_time += lookup_start.elapsed();
                        result
                    }
                    _ => (None, 0.0),
                };
                let target = gains.target(ring_leak[i], map_leak);
                let leak = self.hand_leak[i].get();
                // Narrow quickly, widen slowly
                let leak = leak + (target - leak) * if target > leak { 0.2 } else { 0.05 };
                self.hand_leak[i].set(leak);

                let zones = zones.for_leak(leak, params.leak_inset_m);
                leak_log[i] = Some((ring_leak[i], map_leak, known, leak, zones.rim));

                zones
            } else {
                self.hand_leak[i].set(0.0);
                zones
            };

            Some((shape, zones))
        });

        self.tracked_hands.set(hands.map(|hand| hand.is_some()));

        // To measure the cost of the hands on the GPU. The playzone still knows about them.
        let mut hands = if params.hands { hands } else { [None; 2] };
        let mut leak = if params.leak_view != 0 {
            self.hand_leak.each_ref().map(Cell::get)
        } else {
            [0.0; 2]
        };

        // Debug view of the map: the right hand is replaced by spheres where the room leaks. They are
        // in the hand zones, so they also keep the layers of the key.
        if shows_probes {
            hands[1] = leak_map.and_then(|(snapshot, probes)| {
                let shape = alvr_graphics::leak_probes(
                    snapshot.anchor,
                    probes,
                    params.leak_probe_m,
                    LEAK_PROBE_RADIUS,
                )?;
                let mut zones = params.zones;
                zones.solid = [0.0; HAND_SEGMENTS];

                Some((shape, zones))
            });
            leak[1] = 0.0;
        }

        if params.leak_map > 0.0 {
            self.log_leak_map(leak_log, lookup_time);
        }

        Some(ColorKeyInput { hands, leak })
    }

    // Every 2 s, to tune `debug.alvr.key_leak_map_lo` / `key_leak_map_hi`
    #[allow(unused_variables)]
    fn log_leak_map(&self, hands: [LeakLog; 2], lookup_time: Duration) {
        let (total, count) = self.leak_lookup_time.get();
        let (total, count) = (total + lookup_time, count + 1);
        if self.last_leak_log.get().elapsed() < LEAK_LOG_INTERVAL {
            self.leak_lookup_time.set((total, count));
            return;
        }
        self.last_leak_log.set(Instant::now());
        self.leak_lookup_time.set((Duration::ZERO, 0));

        let text = ["left", "right"]
            .iter()
            .zip(hands)
            .map(|(name, hand)| match hand {
                Some((ring, map, known, leak, rim)) => format!(
                    "{name} ring {ring:.2} map {} known {:.0}% L {leak:.2} rim {:.1} mm",
                    map.map_or("-".into(), |map| format!("{map:.2}")),
                    known * 100.0,
                    rim * 1000.0,
                ),
                None => format!("{name} -"),
            })
            .collect::<Vec<_>>()
            .join(", ");
        #[cfg(target_os = "android")]
        crate::camera::log(&format!(
            "leak map: {text}, lookup {:.0} us per frame",
            total.as_secs_f32() * 1e6 / count as f32
        ));
    }

    pub fn uses_passthrough(&self) -> bool {
        self.config.passthrough.is_some()
    }

    pub fn update_reference_space(&mut self) {
        self.renderer.clear_playzone();
        #[cfg(target_os = "android")]
        if let Some(key) = &self.auto_color_key {
            key.clear_leak_map();
        }
        self.input_thread_running.set(false);

        self.stage_reference_space = Arc::new(interaction::get_reference_space(
            &self.xr_session,
            xr::ReferenceSpaceType::STAGE,
        ));
        self.view_reference_space = Arc::new(interaction::get_reference_space(
            &self.xr_session,
            xr::ReferenceSpaceType::VIEW,
        ));

        self.core_context.send_playspace(
            self.xr_session
                .reference_space_bounds_rect(xr::ReferenceSpaceType::STAGE)
                .unwrap()
                .map(|a| Vec2::new(a.width, a.height)),
        );

        if let Some(running) = self.input_thread.take() {
            running.join().ok();
        }

        self.input_thread_running.set(true);

        self.input_thread = Some(thread::spawn({
            let core_ctx = Arc::clone(&self.core_context);
            let xr_session = self.xr_session.clone();
            let interaction_ctx = Arc::clone(&self.interaction_context);
            let stage_reference_space = Arc::clone(&self.stage_reference_space);
            let view_reference_space = Arc::clone(&self.view_reference_space);
            let refresh_rate = self.config.refresh_rate_hint;
            let running = Arc::clone(&self.input_thread_running);
            move || {
                stream_input_loop(
                    &core_ctx,
                    xr_session,
                    &interaction_ctx,
                    &stage_reference_space,
                    &view_reference_space,
                    refresh_rate,
                    running,
                )
            }
        }));
    }

    pub fn maybe_initialize_decoder(&mut self, codec: CodecType, config_nal: Vec<u8>) {
        let new_config = VideoDecoderConfig {
            codec,
            force_software_decoder: self.config.force_software_decoder,
            max_buffering_frames: self.config.max_buffering_frames,
            buffering_history_weight: self.config.buffering_history_weight,
            options: self.config.decoder_options.clone(),
            config_buffer: config_nal,
        };

        let maybe_config = if let Some((config, _)) = &self.decoder {
            (new_config != *config).then_some(new_config)
        } else {
            Some(new_config)
        };

        if let Some(config) = maybe_config {
            let (mut sink, source) = video_decoder::create_decoder(config.clone(), {
                let ctx = Arc::clone(&self.core_context);
                move |maybe_timestamp: Result<Duration>| match maybe_timestamp {
                    Ok(timestamp) => ctx.report_frame_decoded(timestamp),
                    Err(e) => ctx.report_fatal_decoder_error(&e.to_string()),
                }
            });
            self.decoder = Some((config, source));

            self.core_context.set_decoder_input_callback(Box::new(
                move |timestamp, buffer| -> bool { sink.push_nal(timestamp, buffer) },
            ));
        }
    }

    pub fn update_real_time_config(&mut self, config: &RealTimeConfig) {
        self.config.passthrough = config.passthrough.clone();
        self.config.clientside_post_processing = config.clientside_post_processing.clone();
    }

    // Start or stop the environment depth provider depending on the passthrough mode, and get the
    // depth image for this frame
    fn update_environment_depth(
        &mut self,
        display_time: xr::Time,
        current_headset_views: &[xr::View],
    ) -> Option<DepthRenderInput> {
        if alvr_graphics::depth_passthrough_config(self.config.passthrough.as_ref()).is_none() {
            if let Some(depth) = self.environment_depth.take() {
                self.renderer.clear_depth_swapchain();
                self.gfx_ctx.make_current();
                drop(depth);
                info!("Environment depth stopped");
            }
            // Retry next time the mode is selected (e.g. after granting the permission)
            self.environment_depth_failed = false;

            return None;
        }

        // The runtime imports the depth textures into the current GL context, and wgpu releases the
        // context after each call. Without this the texture names are 0.
        self.gfx_ctx.make_current();

        if self.environment_depth.is_none() && !self.environment_depth_failed {
            match EnvironmentDepthMETA::new(&self.xr_session, self.color_key.is_some()) {
                Ok(depth) => {
                    info!(
                        "Environment depth started: {} images, {}x{}",
                        depth.images().len(),
                        depth.resolution().x,
                        depth.resolution().y
                    );
                    self.renderer
                        .set_depth_swapchain(depth.images(), depth.resolution());
                    self.environment_depth = Some(depth);

                    // The runtime refuses to acquire depth images until the next xrBeginFrame
                    return Some(DepthRenderInput {
                        frame: None,
                        eye_positions: [Vec3::ZERO; 2],
                        color_key: self.color_key_input(display_time, None),
                        arms: [None; 2],
                        capture_playzone: false,
                    });
                }
                Err(e) => {
                    error!(
                        "Depth passthrough unavailable (requires Quest 3/3S and the spatial data permission): {e}"
                    );
                    self.environment_depth_failed = true;
                }
            }
        }

        let frame = self.environment_depth.as_ref().and_then(|depth| {
            depth
                .acquire(&self.stage_reference_space, display_time)
                .inspect_err(|e| error!("Acquire environment depth: {e}"))
                .ok()
                .flatten()
        });

        let (frame, capture_time) = frame.unzip();
        let arms = capture_time.flatten().map_or([None; 2], |time| {
            self.arm_motions(display_time, time, current_headset_views)
        });

        let eye_positions = [
            crate::from_xr_vec3(current_headset_views[0].pose.position),
            crate::from_xr_vec3(current_headset_views[1].pose.position),
        ];
        let color_key = self.color_key_input(display_time, Some(eye_positions));
        let capture_playzone = frame.is_some() && self.should_capture_playzone();

        Some(DepthRenderInput {
            frame,
            eye_positions,
            color_key,
            arms,
            capture_playzone,
        })
    }

    pub fn render(
        &mut self,
        frame_interval: Duration,
        vsync_time: Duration,
    ) -> (ProjectionLayerBuilder<'_>, Duration, ColorKeyFrame) {
        let xr_vsync_time = xr::Time::from_nanos(vsync_time.as_nanos() as _);
        let frame_poll_deadline = Instant::now()
            + Duration::from_secs_f32(
                frame_interval.as_secs_f32() * DECODER_MAX_TIMEOUT_MULTIPLIER,
            );
        let mut frame_result = None;
        if let Some((_, source)) = &mut self.decoder {
            while frame_result.is_none() && Instant::now() < frame_poll_deadline {
                frame_result = source.get_frame();
                thread::sleep(Duration::from_micros(500));
            }
        }

        let (timestamp, frame_metadata, buffer_ptr) =
            if let Some((timestamp, buffer_ptr)) = frame_result {
                if let Some(metadata) = self.core_context.report_compositor_start(timestamp) {
                    self.last_good_video_frame_metadata = VideoFrameMetadata {
                        foveation_center_shifts: metadata
                            .foveation_center_shifts
                            .or(self.last_good_video_frame_metadata.foveation_center_shifts),
                        ..metadata
                    };
                }

                // Keep displaying new images even when their metadata is unavailable.
                (timestamp, self.last_good_video_frame_metadata, buffer_ptr)
            } else {
                (
                    vsync_time,
                    self.last_good_video_frame_metadata,
                    ptr::null_mut(),
                )
            };
        let view_params = frame_metadata.view_params;

        let left_swapchain_idx = self.swapchains[0].acquire_image().unwrap();
        let right_swapchain_idx = self.swapchains[1].acquire_image().unwrap();

        self.swapchains[0]
            .wait_image(xr::Duration::INFINITE)
            .unwrap();
        self.swapchains[1]
            .wait_image(xr::Duration::INFINITE)
            .unwrap();

        let (flags, maybe_views) = self
            .xr_session
            .locate_views(
                xr::ViewConfigurationType::PRIMARY_STEREO,
                xr_vsync_time,
                &self.stage_reference_space,
            )
            .unwrap();

        let current_headset_views = if flags.contains(xr::ViewStateFlags::ORIENTATION_VALID) {
            maybe_views
        } else {
            vec![crate::default_view(), crate::default_view()]
        };

        // The poses and FoVs we received from the PC runtime, which may differ and/or include
        // altered FoVs based on settings and view conversions done for canting.
        let input_view_params = view_params;
        let mut output_view_params = input_view_params;
        // Avoid passing invalid timestamp to runtime.
        // `timestamp` is generally a current vsync time, but may be repeated if frames are
        // dropped. Some runtimes dislike it if the timestamp is repeated for too long, so after
        // one second we begin presenting a lagged vsync time instead.
        let mut openxr_display_time =
            Duration::max(timestamp, vsync_time.saturating_sub(Duration::from_secs(1)));

        // (shinyquagsire23) I don't entirely trust runtimes to implement CompositionLayerProjectionView
        // correctly, but if we do trust them, avoid doing rotation ourselves. Otherwise, rerender.
        // Ex: YVR/PFDMR has issues with aspect ratio mismatches and passthrough compositing.
        if self.use_custom_reprojection {
            output_view_params = [
                ViewParams {
                    pose: crate::from_xr_pose(current_headset_views[0].pose),
                    fov: crate::from_xr_fov(current_headset_views[0].fov),
                },
                ViewParams {
                    pose: crate::from_xr_pose(current_headset_views[1].pose),
                    fov: crate::from_xr_fov(current_headset_views[1].fov),
                },
            ];

            openxr_display_time = vsync_time;
        }

        let depth_input = self.update_environment_depth(xr_vsync_time, &current_headset_views);

        let hand_rects = self.renderer.render(
            buffer_ptr,
            [
                StreamViewParams {
                    swapchain_index: left_swapchain_idx,
                    input_view_params: input_view_params[0],
                    output_view_params: output_view_params[0],
                },
                StreamViewParams {
                    swapchain_index: right_swapchain_idx,
                    input_view_params: input_view_params[1],
                    output_view_params: output_view_params[1],
                },
            ],
            self.config.passthrough.as_ref(),
            frame_metadata
                .foveation_center_shifts
                .map(|centers| centers.map(Vec2::from_array)),
            depth_input.as_ref(),
        );

        let hand_rects = hand_rects.unwrap_or_default();
        if hand_rects.iter().any(Option::is_some) {
            self.last_hand_rect_time = Some(Instant::now());
        }
        for (last, rect) in self.last_hand_rects.iter_mut().zip(hand_rects) {
            if rect.is_some() {
                *last = rect;
            }
        }
        let key_frame = ColorKeyFrame {
            tracked_hands: self.tracked_hands.get(),
            hand_rects,
            hands_in_view: self
                .last_hand_rect_time
                .is_some_and(|time| time.elapsed() < KEY_LAYERS_HOLD),
        };

        self.swapchains[0].release_image().unwrap();
        self.swapchains[1].release_image().unwrap();

        if !buffer_ptr.is_null()
            && let Some(xr_now) = crate::xr_runtime_now(self.xr_session.instance())
        {
            self.core_context.report_submit(
                timestamp,
                vsync_time.saturating_sub(Duration::from_nanos(xr_now.as_nanos() as u64)),
            );
        }

        let rect = xr::Rect2Di {
            offset: xr::Offset2Di { x: 0, y: 0 },
            extent: xr::Extent2Di {
                width: self.target_view_resolution.x as _,
                height: self.target_view_resolution.y as _,
            },
        };

        let clientside_post_processing = self
            .xr_session
            .instance()
            .exts()
            .fb_composition_layer_settings
            .and(self.config.clientside_post_processing.clone());

        let layer = ProjectionLayerBuilder::new(
            &self.stage_reference_space,
            [
                xr::CompositionLayerProjectionView::new()
                    .pose(crate::to_xr_pose(output_view_params[0].pose))
                    .fov(crate::to_xr_fov(output_view_params[0].fov))
                    .sub_image(
                        xr::SwapchainSubImage::new()
                            .swapchain(&self.swapchains[0])
                            .image_array_index(0)
                            .image_rect(rect),
                    ),
                xr::CompositionLayerProjectionView::new()
                    .pose(crate::to_xr_pose(output_view_params[1].pose))
                    .fov(crate::to_xr_fov(output_view_params[1].fov))
                    .sub_image(
                        xr::SwapchainSubImage::new()
                            .swapchain(&self.swapchains[1])
                            .image_array_index(0)
                            .image_rect(rect),
                    ),
            ],
            self.config
                .passthrough
                .clone()
                .map(|mode| ProjectionLayerAlphaConfig {
                    premultiplied: self.color_key.is_none()
                        && alvr_graphics::passthrough_requires_premultiplied_alpha(&mode),
                }),
            clientside_post_processing,
        );

        // Outside the hands the lower color key layer is covered by the upper one, or shows the same
        // passthrough. For a view without hands the last rect is used, the content there is the same.
        let crop = self.color_key.map_or(0, |params| params.crop);
        let layer = if crop > 0 && !self.use_custom_reprojection {
            let swapchains = &self.swapchains;
            let resolution = self.target_view_resolution;
            let last_rects = self.last_hand_rects;
            layer.crop_lower_key_layer([0, 1].map(|i| {
                let fov = output_view_params[i].fov;
                let (offset, size, fov) = match last_rects[i] {
                    Some(rect) => {
                        alvr_graphics::crop_view(fov, resolution, rect, KEY_CROP_MARGIN, crop == 2)
                    }
                    None => (UVec2::ZERO, resolution, fov),
                };

                xr::CompositionLayerProjectionView::new()
                    .pose(crate::to_xr_pose(output_view_params[i].pose))
                    .fov(crate::to_xr_fov(fov))
                    .sub_image(
                        xr::SwapchainSubImage::new()
                            .swapchain(&swapchains[i])
                            .image_array_index(0)
                            .image_rect(xr::Rect2Di {
                                offset: xr::Offset2Di {
                                    x: offset.x as _,
                                    y: offset.y as _,
                                },
                                extent: xr::Extent2Di {
                                    width: size.x as _,
                                    height: size.y as _,
                                },
                            }),
                    )
            }))
        } else {
            layer
        };

        (layer, openxr_display_time, key_frame)
    }
}

impl Drop for StreamContext {
    fn drop(&mut self) {
        // Closes the camera
        #[cfg(target_os = "android")]
        {
            self.auto_color_key = None;
        }

        self.input_thread_running.set(false);
        self.input_thread.take().unwrap().join().ok();
    }
}

fn stream_input_loop(
    core_ctx: &ClientCoreContext,
    xr_session: xr::Session<xr::OpenGlEs>,
    interaction_ctx: &RwLock<InteractionContext>,
    stage_reference_space: &xr::Space,
    view_reference_space: &xr::Space,
    refresh_rate: f32,
    running: Arc<RelaxedAtomic>,
) {
    let mut last_controller_poses = [Pose::IDENTITY; 2];
    let mut last_palm_poses = [Pose::IDENTITY; 2];
    let mut last_view_params = [ViewParams::DUMMY; 2];

    let mut deadline = Instant::now();
    let frame_interval = Duration::from_secs_f32(1.0 / refresh_rate);
    while running.value() {
        let int_ctx_lock = interaction_ctx.read();
        let int_ctx = &*int_ctx_lock;
        // Streaming related inputs are updated here. Make sure every input poll is done in this
        // thread
        if let Err(e) = xr_session.sync_actions(&[(&int_ctx.action_set).into()]) {
            error!("{e}");
            return;
        }

        let Some(now) = crate::xr_runtime_now(xr_session.instance()).map(crate::from_xr_time)
        else {
            error!("Cannot poll tracking: invalid time");
            return;
        };

        let target_time = now + core_ctx.get_total_prediction_offset();

        let Some((head_motion, local_views)) = interaction::get_head_data(
            &xr_session,
            core_ctx.platform(),
            stage_reference_space,
            view_reference_space,
            now,
            target_time,
            &last_view_params,
        ) else {
            continue;
        };

        if let Some(views) = local_views {
            core_ctx.send_view_params(views);
            last_view_params = views;
        }

        let mut device_motions = Vec::with_capacity(3);

        device_motions.push((*HEAD_ID, head_motion));

        let left_hand_data = crate::interaction::get_hand_data(
            &xr_session,
            core_ctx.platform(),
            stage_reference_space,
            now,
            target_time,
            &int_ctx.hands_interaction[0],
            &mut last_controller_poses[0],
            &mut last_palm_poses[0],
        );
        let right_hand_data = crate::interaction::get_hand_data(
            &xr_session,
            core_ctx.platform(),
            stage_reference_space,
            now,
            target_time,
            &int_ctx.hands_interaction[1],
            &mut last_controller_poses[1],
            &mut last_palm_poses[1],
        );

        // Note: When multimodal input is enabled, we are sure that when free hands are used
        // (not holding controllers) the controller data is None.
        if (int_ctx.multimodal_hands_enabled || left_hand_data.skeleton_joints.is_none())
            && let Some(motion) = left_hand_data.grip_motion
        {
            device_motions.push((*HAND_LEFT_ID, motion));
        }
        if (int_ctx.multimodal_hands_enabled || right_hand_data.skeleton_joints.is_none())
            && let Some(motion) = right_hand_data.grip_motion
        {
            device_motions.push((*HAND_RIGHT_ID, motion));
        }

        if int_ctx.multimodal_hands_enabled
            && let Some(detached_controller) = left_hand_data.detached_grip_motion
        {
            device_motions.push((*DETACHED_CONTROLLER_LEFT_ID, detached_controller));
        }
        if int_ctx.multimodal_hands_enabled
            && let Some(detached_controller) = right_hand_data.detached_grip_motion
        {
            device_motions.push((*DETACHED_CONTROLLER_RIGHT_ID, detached_controller));
        }

        let face = interaction::get_face_data(
            &xr_session,
            core_ctx.platform(),
            &int_ctx.face_sources,
            stage_reference_space,
            view_reference_space,
            head_motion.pose.orientation,
            now,
        );

        let body = int_ctx
            .body_source
            .as_ref()
            .and_then(|source| interaction::get_body_skeleton(source, stage_reference_space, now));

        if let Some(source) = &int_ctx.body_source {
            device_motions.append(&mut interaction::get_bd_motion_trackers(source, now));
        }

        drop(int_ctx_lock);

        // Even though the server is already adding the motion-to-photon latency, here we use
        // target_time as the poll_timestamp to compensate for the fact that video frames are sent
        // with the poll timestamp instead of the vsync time. This is to ensure correctness when
        // submitting frames to OpenXR. This won't cause any desync with the server because no time
        // sync step is performed between client and server.
        core_ctx.send_tracking(TrackingData {
            poll_timestamp: target_time,
            device_motions,
            hand_skeletons: [
                left_hand_data.skeleton_joints,
                right_hand_data.skeleton_joints,
            ],
            face,
            body,
        });

        let button_entries =
            interaction::update_buttons(&xr_session, &interaction_ctx.read().button_actions);
        if !button_entries.is_empty() {
            core_ctx.send_buttons(button_entries);
        }

        deadline += frame_interval / 3;
        thread::sleep(deadline.saturating_duration_since(Instant::now()));
    }
}
