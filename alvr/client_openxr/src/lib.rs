mod area_editor;
mod area_overlay;
mod arm_history;
#[cfg(target_os = "android")]
mod auto_color_key;
mod burst;
mod c_api;
#[cfg(target_os = "android")]
mod camera;
mod extra_extensions;
mod graphics;
mod interaction;
mod lobby;
mod marker_region;
mod passthrough;
mod stream;

use crate::stream::ParsedStreamConfig;
use alvr_client_core::{ClientCapabilities, ClientCoreContext, ClientCoreEvent};
use alvr_common::{
    Fov, HAND_LEFT_ID, Pose, debug, error,
    glam::{Quat, UVec2, Vec3},
    info,
    parking_lot::RwLock,
};
use alvr_graphics::GraphicsContext;
use alvr_session::{BodyTrackingBDConfig, BodyTrackingSourcesConfig, PerformanceLevel};
use alvr_system_info::Platform;
use area_editor::AreaEditor;
use area_overlay::AreaOverlay;
use extra_extensions::{
    BD_MOTION_TRACKING_EXTENSION_NAME, META_BODY_TRACKING_FIDELITY_EXTENSION_NAME,
    META_HAND_TRACKING_UNEXTRAPOLATED_POSES_EXTENSION_NAME, PICO_CONFIGURATION_EXTENSION_NAME,
    SPATIAL_MARKER_EXTENSION_NAMES, SpatialMarkerFns, SpatialMarkerTracker,
};
use interaction::{InteractionContext, InteractionSourcesConfig};
use lobby::Lobby;
use marker_region::MarkerRegion;
use openxr as xr;
use passthrough::{ColorKeyLoader, PassthroughLayer};
use std::{
    ffi::CStr,
    path::Path,
    rc::Rc,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};
use stream::StreamContext;

fn from_xr_vec3(v: xr::Vector3f) -> Vec3 {
    Vec3::new(v.x, v.y, v.z)
}

fn to_xr_vec3(v: Vec3) -> xr::Vector3f {
    xr::Vector3f {
        x: v.x,
        y: v.y,
        z: v.z,
    }
}

fn from_xr_quat(q: xr::Quaternionf) -> Quat {
    Quat::from_xyzw(q.x, q.y, q.z, q.w)
}

fn to_xr_quat(q: Quat) -> xr::Quaternionf {
    xr::Quaternionf {
        x: q.x,
        y: q.y,
        z: q.z,
        w: q.w,
    }
}

fn from_xr_pose(p: xr::Posef) -> Pose {
    Pose {
        orientation: from_xr_quat(p.orientation),
        position: from_xr_vec3(p.position),
    }
}

fn to_xr_pose(p: Pose) -> xr::Posef {
    xr::Posef {
        orientation: to_xr_quat(p.orientation),
        position: to_xr_vec3(p.position),
    }
}

fn from_xr_fov(f: xr::Fovf) -> Fov {
    Fov {
        left: f.angle_left,
        right: f.angle_right,
        up: f.angle_up,
        down: f.angle_down,
    }
}

fn to_xr_fov(f: Fov) -> xr::Fovf {
    xr::Fovf {
        angle_left: f.left,
        angle_right: f.right,
        angle_up: f.up,
        angle_down: f.down,
    }
}

fn from_xr_time(timestamp: xr::Time) -> Duration {
    Duration::from_nanos(timestamp.as_nanos() as _)
}

fn to_xr_time(timestamp: Duration) -> xr::Time {
    xr::Time::from_nanos(timestamp.as_nanos() as _)
}

fn to_perf_settings_level(level: PerformanceLevel) -> xr::PerfSettingsLevelEXT {
    match level {
        PerformanceLevel::PowerSavings => xr::PerfSettingsLevelEXT::POWER_SAVINGS,
        PerformanceLevel::SustainedLow => xr::PerfSettingsLevelEXT::SUSTAINED_LOW,
        PerformanceLevel::SustainedHigh => xr::PerfSettingsLevelEXT::SUSTAINED_HIGH,
        PerformanceLevel::Boost => xr::PerfSettingsLevelEXT::BOOST,
    }
}

fn set_performance_level(
    xr_instance: &xr::Instance,
    xr_session: &xr::Session<xr::OpenGlEs>,
    domain: xr::PerfSettingsDomainEXT,
    level: PerformanceLevel,
) {
    if let Some(performance_settings) = xr_instance.exts().ext_performance_settings {
        let level = to_perf_settings_level(level);
        let result = unsafe {
            (performance_settings.perf_settings_set_performance_level)(
                xr_session.as_raw(),
                domain,
                level,
            )
        };
        stream_log(&format!("Perf: set {domain:?} to {level:?}: {result:?}"));
    } else {
        stream_log("Perf: XR_EXT_performance_settings is not available");
    }
}

// The client logger drops info while streaming
fn stream_log(text: &str) {
    #[cfg(target_os = "android")]
    camera::log(text);
    #[cfg(not(target_os = "android"))]
    info!("{text}");
}

fn default_view() -> xr::View {
    xr::View {
        pose: xr::Posef {
            orientation: xr::Quaternionf {
                x: 0.0,
                y: 0.0,
                z: 0.0,
                w: 1.0,
            },
            position: xr::Vector3f::default(),
        },
        fov: xr::Fovf {
            angle_left: -1.0,
            angle_right: 1.0,
            angle_up: 1.0,
            angle_down: -1.0,
        },
    }
}

// This exists to circumvent dead-code analysis
fn create_session(
    xr_instance: &xr::Instance,
    xr_system: xr::SystemId,
    graphics_context: &GraphicsContext,
) -> (
    xr::Session<xr::OpenGlEs>,
    xr::FrameWaiter,
    xr::FrameStream<xr::OpenGlEs>,
) {
    #[allow(unreachable_code)]
    unsafe {
        xr_instance
            .create_session(xr_system, &graphics::session_create_info(graphics_context))
            .unwrap()
    }
}

pub fn entry_point() {
    alvr_client_core::init_logging();

    const LEGACY_OPENXR_VERSION: xr::Version = xr::Version::new(1, 0, 34);
    const CURRENT_OPENXR_VERSION: xr::Version = xr::Version::new(1, 1, 36);

    // Using a provisional platform, before we can get the runtime info
    let (loader_suffix, openxr_version) = match alvr_system_info::platform(None, None) {
        Platform::Quest1 => ("_quest1", LEGACY_OPENXR_VERSION),
        Platform::PicoNeo3
        | Platform::PicoG3
        | Platform::Pico4
        | Platform::Pico4Pro
        | Platform::Pico4Enterprise => ("_pico_old", LEGACY_OPENXR_VERSION),
        p if p.is_vive() => ("", LEGACY_OPENXR_VERSION),
        p if p.is_yvr() => ("_yvr", LEGACY_OPENXR_VERSION),
        _ => ("", CURRENT_OPENXR_VERSION),
    };
    let xr_entry = unsafe {
        xr::Entry::load_from(Path::new(&format!("libopenxr_loader{loader_suffix}.so"))).unwrap()
    };

    #[cfg(target_os = "android")]
    xr_entry.initialize_android_loader().unwrap();

    let available_exts = xr_entry.enumerate_extensions().unwrap();
    info!(
        "OpenXR available extensions: {:#?}",
        available_exts
            .names()
            .iter()
            .map(|s| CStr::from_bytes_with_nul(s).unwrap().to_str().unwrap())
            .collect::<Vec<_>>()
    );

    // todo: switch to vulkan
    assert!(available_exts.khr_opengl_es_enable);

    let mut selected_exts = xr::ExtensionSet::default();
    selected_exts.bd_body_tracking = true;
    selected_exts.bd_controller_interaction = true;
    selected_exts.bd_facial_simulation = true;
    selected_exts.ext_eye_gaze_interaction = true;
    selected_exts.ext_hand_tracking = true;
    selected_exts.ext_local_floor = true;
    selected_exts.ext_performance_settings = true;
    selected_exts.ext_user_presence = true;
    selected_exts.fb_body_tracking = true;
    selected_exts.fb_color_space = true;
    selected_exts.fb_composition_layer_settings = true;
    selected_exts.fb_display_refresh_rate = true;
    selected_exts.fb_eye_tracking_social = true;
    selected_exts.fb_face_tracking2 = true;
    selected_exts.fb_foveation = true;
    selected_exts.fb_foveation_configuration = true;
    // The ray of each hand for the play area editor (area_editor.rs), as the system pointer
    selected_exts.fb_hand_tracking_aim = true;
    selected_exts.fb_passthrough = true;
    selected_exts.fb_swapchain_update_state = true;
    selected_exts.htc_facial_tracking = true;
    selected_exts.htc_passthrough = true;
    selected_exts.htc_vive_focus3_controller_interaction = true;
    #[cfg(target_os = "android")]
    {
        selected_exts.khr_android_create_instance = true;
    }
    selected_exts.khr_composition_layer_color_scale_bias = true;
    selected_exts.khr_convert_timespec_time = true;
    selected_exts.khr_opengl_es_enable = true;
    selected_exts.meta_body_tracking_full_body = true;
    selected_exts.meta_simultaneous_hands_and_controllers = true;
    selected_exts.meta_detached_controllers = true;
    selected_exts.meta_environment_depth = true;
    selected_exts.meta_passthrough_color_lut = true;
    selected_exts.other = [
        META_BODY_TRACKING_FIDELITY_EXTENSION_NAME,
        BD_MOTION_TRACKING_EXTENSION_NAME,
        PICO_CONFIGURATION_EXTENSION_NAME,
    ]
    .into_iter()
    .map(|s| s.as_bytes().to_vec())
    .collect();

    selected_exts = selected_exts.intersection(&available_exts);

    // The capture time of each hand sample, for the arm's own sample history (`arm_history.rs`). By
    // name: `openxr` lists the extensions it doesn't know in `other`, with their nul
    let unextrapolated_name = [
        META_HAND_TRACKING_UNEXTRAPOLATED_POSES_EXTENSION_NAME.as_bytes(),
        b"\0",
    ]
    .concat();
    let unextrapolated_listed = available_exts.other.contains(&unextrapolated_name);
    if unextrapolated_listed {
        selected_exts.other.push(unextrapolated_name);
    }
    stream_log(&format!(
        "arm: the runtime lists {META_HAND_TRACKING_UNEXTRAPOLATED_POSES_EXTENSION_NAME}: {}",
        if unextrapolated_listed {
            "yes, enabled (hand sample times from captureTime)"
        } else {
            "no (hand sample times from debug.alvr.arm_sample_age_ms)"
        }
    ));

    // QR code markers (`debug.alvr.marker`, read at the start of the app, see `SpatialMarkerTracker`).
    // Enabled by name, so that the `openxr` crate doesn't load their functions in `create_instance`,
    // and only all three together: each depends on the one before
    let marker_listed = [
        ("XR_EXT_future", available_exts.ext_future),
        ("XR_EXT_spatial_entity", available_exts.ext_spatial_entity),
        (
            "XR_EXT_spatial_marker_tracking",
            available_exts.ext_spatial_marker_tracking,
        ),
        ("XR_EXT_spatial_anchor", available_exts.ext_spatial_anchor),
        (
            "XR_EXT_spatial_persistence",
            available_exts.ext_spatial_persistence,
        ),
    ];
    let marker_wanted = passthrough::debug_property("debug.alvr.marker").unwrap_or(1) != 0;
    let marker_exts_listed = marker_listed[..3].iter().all(|(_, listed)| *listed);
    let mut enable_marker_exts = marker_wanted && marker_exts_listed;
    stream_log(&format!(
        "marker: the runtime lists {}; enabled: {}",
        marker_listed
            .iter()
            .map(|(name, listed)| format!("{name} {}", if *listed { "yes" } else { "no" }))
            .collect::<Vec<_>>()
            .join(", "),
        if enable_marker_exts {
            "the first three (anchor and persistence are not used)"
        } else if !marker_wanted {
            "none (debug.alvr.marker 0)"
        } else {
            "none (the first three are needed together)"
        }
    ));
    let mut instance_exts = selected_exts.clone();
    if enable_marker_exts {
        instance_exts
            .other
            .extend(SPATIAL_MARKER_EXTENSION_NAMES.map(|name| name.to_vec()));
    }

    let available_layers = xr_entry.enumerate_layers().unwrap();
    info!("OpenXR available layers: {available_layers:#?}");

    let other_exts = selected_exts
        .other
        .iter()
        .map(|vec| {
            CStr::from_bytes_with_nul(vec)
                .unwrap()
                .to_str()
                .unwrap()
                .to_owned()
        })
        .collect::<Vec<_>>();

    let app_info = xr::ApplicationInfo {
        application_name: "ALVR Client",
        application_version: 0,
        engine_name: "ALVR",
        engine_version: 0,
        api_version: openxr_version,
    };
    // A runtime that lists the marker extensions may still refuse them: start without them
    let xr_instance = match xr_entry.create_instance(&app_info, &instance_exts, &[]) {
        Ok(instance) => instance,
        Err(e) if enable_marker_exts => {
            stream_log(&format!(
                "marker: off, the instance can't be created with the extensions: {e:?}"
            ));
            enable_marker_exts = false;
            xr_entry
                .create_instance(&app_info, &selected_exts, &[])
                .unwrap()
        }
        Err(e) => panic!("{e:?}"),
    };

    let marker_fns = enable_marker_exts
        .then(|| {
            SpatialMarkerFns::load(&xr_instance)
                .inspect_err(|e| stream_log(&format!("marker: off, functions not found: {e:?}")))
                .ok()
        })
        .flatten();

    let platform = alvr_system_info::platform(
        xr_instance
            .properties()
            .ok()
            .map(|s| s.runtime_name.to_owned()),
        xr_instance
            .properties()
            .ok()
            .map(|s| s.runtime_version.into_raw()),
    );

    let graphics_context = Rc::new(GraphicsContext::new_gl());

    let mut last_lobby_message = String::new();

    'session_loop: loop {
        let xr_system = xr_instance
            .system(xr::FormFactor::HEAD_MOUNTED_DISPLAY)
            .unwrap();

        // mandatory call
        let _ = xr_instance
            .graphics_requirements::<xr::OpenGlEs>(xr_system)
            .unwrap();

        let (xr_session, mut xr_frame_waiter, mut xr_frame_stream) =
            create_session(&xr_instance, xr_system, &graphics_context);

        let views_config = xr_instance
            .enumerate_view_configuration_views(
                xr_system,
                xr::ViewConfigurationType::PRIMARY_STEREO,
            )
            .unwrap();
        assert_eq!(views_config.len(), 2);

        let default_view_resolution = UVec2::new(
            views_config[0].recommended_image_rect_width,
            views_config[0].recommended_image_rect_height,
        );

        let max_view_resolution = UVec2::new(
            views_config[0].max_image_rect_width,
            views_config[0].max_image_rect_height,
        );

        let refresh_rates = if selected_exts.fb_display_refresh_rate {
            xr_session.enumerate_display_refresh_rates().unwrap()
        } else {
            vec![90.0]
        };

        if selected_exts.fb_color_space {
            xr_session
                .set_color_space(xr::ColorSpaceFB::REC709)
                .unwrap();
        }

        let capabilities = ClientCapabilities {
            platform,
            default_view_resolution,
            max_view_resolution,
            refresh_rates,
            foveated_encoding: platform != Platform::Unknown,
            encoder_high_profile: platform != Platform::Unknown,
            encoder_10_bits: platform != Platform::Unknown,
            encoder_av1: matches!(
                platform,
                Platform::Quest3 | Platform::Quest3S | Platform::Pico4Ultra
            ),
            prefer_10bit: false,
            preferred_encoding_gamma: 1.0,
            prefer_hdr: false,
        };

        let permissions = {
            #[cfg(not(target_os = "android"))]
            {
                vec![]
            }
            #[cfg(target_os = "android")]
            {
                let mut permissions = vec![];

                // Eye-tracking permission is needed for gaze input in InteractionContext.
                if extra_extensions::supports_eye_gaze_interaction(&xr_session, xr_system) {
                    match platform {
                        Platform::QuestPro => {
                            permissions.push("com.oculus.permission.EYE_TRACKING");
                        }
                        Platform::PicoNeo3 | Platform::Pico4Pro | Platform::Pico4Enterprise => {
                            permissions.push("com.picovr.permission.EYE_TRACKING");
                        }
                        _ => {}
                    }
                }

                // Spatial data permission, needed for the environment depth (depth passthrough)
                if selected_exts.meta_environment_depth
                    && matches!(platform, Platform::Quest3 | Platform::Quest3S)
                {
                    permissions.push("com.oculus.permission.USE_SCENE");
                }

                #[cfg(target_os = "android")]
                if auto_color_key::enabled() {
                    permissions.push("android.permission.CAMERA");
                    permissions.push("horizonos.permission.HEADSET_CAMERA");
                }

                permissions
            }
        };

        let core_context = Arc::new(ClientCoreContext::new(capabilities, permissions));

        let interaction_context = Arc::new(RwLock::new(InteractionContext::new(
            xr_session.clone(),
            other_exts.clone(),
            xr_system,
            platform,
        )));

        let mut lobby = Lobby::new(
            xr_session.clone(),
            Rc::clone(&graphics_context),
            Arc::clone(&interaction_context),
            platform,
            UVec2::min(default_view_resolution * 2, max_view_resolution),
            &last_lobby_message,
        );

        // For Meta/Quest enabling body tracking would disable multimodal input
        let lobby_body_tracking_config = if platform.is_pico() {
            Some(BodyTrackingSourcesConfig {
                bd: BodyTrackingBDConfig::BodyTracking {
                    high_accuracy: true,
                    prompt_calibration_on_start: false,
                },
                meta: Default::default(),
            })
        } else {
            None
        };
        let lobby_interaction_sources = InteractionSourcesConfig {
            face_tracking: None,
            body_tracking: lobby_body_tracking_config,
            prefers_multimodal_input: true,
        };
        interaction_context
            .write()
            .select_sources(&lobby_interaction_sources);

        let mut session_running = false;
        let mut stream_context = None::<StreamContext>;
        let mut passthrough_layer = None;

        // QR code markers, while the session runs and `debug.alvr.marker` is 1 (unset: while
        // `debug.alvr.marker_box` is 1)
        let mut marker_tracker = None::<SpatialMarkerTracker>;
        // Gave up until `debug.alvr.marker` is set to 0 and back
        let mut marker_failed = false;
        let mut marker_restart_logged = false;
        let mut last_marker_poll = None::<Instant>;
        let mut marker_region = MarkerRegion::new();
        // Whether the depth mask uses the box, last logged. None: no box
        let mut marker_box_used = None::<bool>;
        // Outline of the code and the box over the stream (`debug.alvr.area_show`)
        let mut area_overlay = AreaOverlay::new();
        // The play area edited with the hands, and its layout file
        let mut area_editor = AreaEditor::new();

        let mut event_storage = xr::EventDataBuffer::new();
        let mut headset_is_worn = true;
        let mut color_key_loader = ColorKeyLoader::new();
        let mut last_key_log = Instant::now();
        'render_loop: loop {
            while let Some(event) = xr_instance.poll_event(&mut event_storage).unwrap() {
                match event {
                    xr::Event::EventsLost(event) => {
                        error!("OpenXR: lost {} events!", event.lost_event_count());
                    }
                    xr::Event::InstanceLossPending(_) => break 'session_loop,
                    xr::Event::SessionStateChanged(event) => match event.state() {
                        xr::SessionState::READY => {
                            xr_session
                                .begin(xr::ViewConfigurationType::PRIMARY_STEREO)
                                .unwrap();

                            core_context.resume();

                            passthrough_layer = PassthroughLayer::new(&xr_session, platform).ok();

                            session_running = true;
                        }
                        xr::SessionState::STOPPING => {
                            session_running = false;

                            passthrough_layer = None;
                            // Its handles must go before the session ends
                            marker_tracker = None;

                            core_context.pause();

                            xr_session.end().ok();
                        }
                        xr::SessionState::EXITING | xr::SessionState::LOSS_PENDING => {
                            // Its handles must go before the session
                            drop(marker_tracker.take());

                            break 'render_loop;
                        }
                        _ => (),
                    },
                    xr::Event::ReferenceSpaceChangePending(event) => {
                        // With stream_log: the client logger drops info while streaming, and the
                        // marker checklist reads ALVR_CAM
                        let pose = event.pose_in_previous_space();
                        let position = from_xr_vec3(pose.position);
                        stream_log(&format!(
                            "ReferenceSpaceChangePending type: {:?}, change time {} ns, pose valid: {}, \
                             pose in previous space ({:.3}, {:.3}, {:.3}) m, yaw {:.1} deg",
                            event.reference_space_type(),
                            event.change_time().as_nanos(),
                            event.pose_valid(),
                            position.x,
                            position.y,
                            position.z,
                            alvr_graphics::yaw_log_deg(alvr_graphics::heading_yaw(
                                from_xr_quat(pose.orientation) * Vec3::NEG_Z
                            )),
                        ));

                        lobby.update_reference_space();

                        if let Some(stream) = &mut stream_context {
                            stream.update_reference_space(Some(event.change_time()));
                        }

                        // The markers are located in STAGE: a change of LOCAL (a recenter) or VIEW
                        // doesn't move them. With a valid pose the box keeps its coordinates
                        // (`debug.alvr.marker_carry 0`: on 2026-11-24 the code was seen at the same
                        // STAGE coordinates after such a change, the carried box was 83 cm off) or is
                        // carried into the new space by the pose (1); the tracker drops its poses
                        // until the markers are seen again
                        if event.reference_space_type() == xr::ReferenceSpaceType::STAGE {
                            if let Some(tracker) = &mut marker_tracker {
                                tracker.on_reference_space_change(event.change_time());
                            }
                            let change = event.pose_valid().then(|| alvr_graphics::StageChange {
                                position,
                                orientation: from_xr_quat(pose.orientation).normalize(),
                            });
                            let carry = passthrough::debug_property("debug.alvr.marker_carry")
                                .unwrap_or(0)
                                != 0;
                            let stage_event = alvr_graphics::StageEvent::new(change, carry);
                            if change.is_some() {
                                stream_log(&format!(
                                    "STAGE change with a valid pose: {} (marker_carry {})",
                                    if carry {
                                        "the box is carried by it"
                                    } else {
                                        "the box keeps its coordinates"
                                    },
                                    u8::from(carry)
                                ));
                            }
                            marker_region
                                .on_reference_space_change(event.change_time(), stage_event);
                            area_editor.on_stage_change(event.change_time(), stage_event);
                        }
                    }
                    xr::Event::SpatialDiscoveryRecommendedEXT(event) => {
                        if let Some(tracker) = &mut marker_tracker {
                            tracker.on_discovery_recommended(event.spatial_context());
                        }
                    }
                    xr::Event::PerfSettingsEXT(event) => {
                        stream_log(&format!(
                            "Perf: from {:?} to {:?}, domain: {:?}/{:?}",
                            event.from_level(),
                            event.to_level(),
                            event.domain(),
                            event.sub_domain(),
                        ));
                    }
                    xr::Event::InteractionProfileChanged(_)
                    | xr::Event::PassthroughStateChangedFB(_) => {
                        // todo
                    }
                    xr::Event::UserPresenceChangedEXT(event) => {
                        debug!("user present: {:?}", event.is_user_present());
                        headset_is_worn = event.is_user_present();

                        core_context.send_proximity_state(event.is_user_present());
                    }
                    _ => (),
                }
            }

            if !session_running {
                thread::sleep(Duration::from_millis(100));
                continue;
            }

            while let Some(event) = core_context.poll_event() {
                match event {
                    ClientCoreEvent::UpdateHudMessage(message) => {
                        last_lobby_message.clone_from(&message);
                        lobby.update_hud_message(&message);
                    }
                    ClientCoreEvent::StreamingStarted(config) => {
                        // Also sent with the real time config, but only when it changes
                        let levels = &config.settings.headset.performance_level;
                        for (domain, level) in [
                            (xr::PerfSettingsDomainEXT::CPU, &levels.cpu),
                            (xr::PerfSettingsDomainEXT::GPU, &levels.gpu),
                        ] {
                            if let Some(level) = level.as_option() {
                                set_performance_level(
                                    &xr_instance,
                                    &xr_session,
                                    domain,
                                    level.clone(),
                                );
                            }
                        }

                        let config = ParsedStreamConfig::new(&config);

                        let context = StreamContext::new(
                            Arc::clone(&core_context),
                            xr_session.clone(),
                            Rc::clone(&graphics_context),
                            Arc::clone(&interaction_context),
                            config,
                        );

                        if !context.uses_passthrough() {
                            passthrough_layer = None;
                        }

                        stream_context = Some(context);

                        core_context.send_proximity_state(headset_is_worn);
                    }
                    ClientCoreEvent::StreamingStopped => {
                        if passthrough_layer.is_none() {
                            passthrough_layer = PassthroughLayer::new(&xr_session, platform).ok();
                        }

                        interaction_context
                            .write()
                            .select_sources(&lobby_interaction_sources);

                        stream_context = None;
                    }
                    ClientCoreEvent::Haptics {
                        device_id,
                        duration,
                        frequency,
                        amplitude,
                    } => {
                        let idx = if device_id == *HAND_LEFT_ID { 0 } else { 1 };
                        let action =
                            &interaction_context.read().hands_interaction[idx].vibration_action;

                        action
                            .apply_feedback(
                                &xr_session,
                                xr::Path::NULL,
                                &xr::HapticVibration::new()
                                    .amplitude(amplitude.clamp(0.0, 1.0))
                                    .frequency(frequency.max(0.0))
                                    .duration(xr::Duration::from_nanos(duration.as_nanos() as _)),
                            )
                            .unwrap();
                    }
                    ClientCoreEvent::DecoderConfig { codec, config_nal } => {
                        if let Some(stream) = &mut stream_context {
                            stream.maybe_initialize_decoder(codec, config_nal);
                        }
                    }
                    ClientCoreEvent::RealTimeConfig(config) => {
                        if config.passthrough.is_some() && passthrough_layer.is_none() {
                            passthrough_layer = PassthroughLayer::new(&xr_session, platform).ok();
                        } else if config.passthrough.is_none() && passthrough_layer.is_some() {
                            passthrough_layer = None;
                        }

                        if let Some(cpu_performance_level) = &config.cpu_performance_level {
                            set_performance_level(
                                &xr_instance,
                                &xr_session,
                                xr::PerfSettingsDomainEXT::CPU,
                                cpu_performance_level.clone(),
                            );
                        }

                        if let Some(gpu_performance_level) = &config.gpu_performance_level {
                            set_performance_level(
                                &xr_instance,
                                &xr_session,
                                xr::PerfSettingsDomainEXT::GPU,
                                gpu_performance_level.clone(),
                            );
                        }

                        if let Some(stream) = &mut stream_context {
                            stream.update_real_time_config(&config);
                        }
                    }
                }
            }

            let frame_state = match xr_frame_waiter.wait() {
                Ok(state) => state,
                Err(e) => {
                    error!("{e}");
                    panic!();
                }
            };
            let frame_interval =
                Duration::from_nanos(frame_state.predicted_display_period.as_nanos() as _);
            let vsync_time =
                Duration::from_nanos(frame_state.predicted_display_time.as_nanos() as _);

            // QR code markers: `debug.alvr.marker` (polled), and the box placed from one
            if last_marker_poll.is_none_or(|time| time.elapsed() > Duration::from_millis(500)) {
                last_marker_poll = Some(Instant::now());
                let marker_property = passthrough::debug_property("debug.alvr.marker");
                // Unset: only while the box is on
                let wanted = marker_property.map_or_else(
                    || passthrough::debug_property("debug.alvr.marker_box").unwrap_or(0) != 0,
                    |value| value != 0,
                );
                if !wanted {
                    if marker_tracker.take().is_some() {
                        stream_log(if marker_property.is_some() {
                            "marker: stopped (debug.alvr.marker 0)"
                        } else {
                            "marker: stopped (marker_box 0 and debug.alvr.marker unset)"
                        });
                    }
                    // 0 turns the box off too. Without a tracker for other reasons it is kept
                    if marker_property.is_some() {
                        marker_region.stop();
                    }
                    marker_failed = false;
                } else if marker_tracker.is_none() && !marker_failed {
                    if let Some(fns) = marker_fns {
                        marker_tracker = SpatialMarkerTracker::new(&xr_session, xr_system, fns);
                        marker_failed = marker_tracker.is_none();
                    } else if marker_exts_listed && !marker_wanted && !marker_restart_logged {
                        marker_restart_logged = true;
                        stream_log(
                            "marker: the extensions are only enabled at the start of the app, restart it",
                        );
                    }
                }
            }
            if let Some(tracker) = &mut marker_tracker {
                tracker.update(frame_state.predicted_display_time);
                if tracker.is_off() {
                    marker_tracker = None;
                    marker_failed = true;
                }
            }
            let region_override =
                marker_region.update(marker_tracker.as_ref(), frame_state.predicted_display_time);
            let depth_passthrough = stream_context
                .as_ref()
                .is_some_and(|stream| stream.uses_depth_passthrough());
            // The layout file of the code and the edits replace the box of the properties
            let editor_input = stream_context
                .as_ref()
                .filter(|_| marker_region.is_enabled())
                .and_then(|stream| stream.editor_input(frame_state.predicted_display_time));
            let region_override = area_editor.update(
                &marker_region,
                region_override,
                frame_state.predicted_display_time,
                editor_input.as_ref(),
                depth_passthrough,
            );
            if let Some(stream) = &mut stream_context {
                // While editing the hands are not cut to the area: the handles are on and outside it
                stream.set_region_override(
                    region_override,
                    marker_region.limits_hands() && !area_editor.is_editing(),
                );
                stream.set_game_dim(area_editor.game_dim());
                stream.set_game_see(area_editor.game_see());
                // The pinches of the editor are not sent to the PC
                stream.withhold_hands(area_editor.withholds_hands());
            }
            // The box applies only to a stream with Passthrough = Depth
            let used = region_override.map(|_| {
                stream_context
                    .as_ref()
                    .is_some_and(|stream| stream.uses_depth_passthrough())
            });
            if used != marker_box_used {
                marker_box_used = used;
                match used {
                    Some(true) => stream_log("marker box: used by the depth mask"),
                    Some(false) => stream_log(
                        "marker box: placed but not used (no stream, or Passthrough is not Depth)",
                    ),
                    None => (),
                }
            }
            let outline = area_overlay.update(
                &marker_region,
                region_override,
                depth_passthrough,
                &area_editor,
            );
            if let Some(stream) = &mut stream_context {
                stream.set_area_overlay(outline);
            }

            // The color key needs the focus area, which comes from the depth passthrough
            let color_key = color_key_loader.poll().filter(|_| {
                let exts = xr_session.instance().exts();
                let supported = exts.meta_passthrough_color_lut.is_some()
                    && exts.khr_composition_layer_color_scale_bias.is_some();
                supported
                    && stream_context
                        .as_ref()
                        .is_some_and(|stream| stream.uses_depth_passthrough())
            });
            let use_color_key = passthrough_layer
                .as_mut()
                .is_some_and(|layer| layer.update_color_key(color_key));
            if let Some(stream) = &mut stream_context {
                let color_key = color_key.filter(|_| use_color_key);
                stream.set_color_key(
                    color_key.map(|key| key.params),
                    color_key.map_or(0, |key| key.version),
                );

                if use_color_key
                    && let Some(lut) = stream.take_color_key_lut()
                    && let Some(passthrough_layer) = &mut passthrough_layer
                {
                    passthrough_layer.set_color_key_lut(&lut);
                }
            }

            xr_frame_stream.begin().unwrap();

            if !frame_state.should_render {
                xr_frame_stream
                    .end(
                        frame_state.predicted_display_time,
                        xr::EnvironmentBlendMode::OPAQUE,
                        &[],
                    )
                    .unwrap();

                continue;
            }

            let key_params = color_key.map(|key| key.params);

            // todo: allow rendering lobby and stream layers at the same time and add cross fade
            let (layer, display_time, key_frame) = if let Some(stream) = &mut stream_context {
                stream.render(frame_interval, vsync_time)
            } else {
                (
                    lobby.render(vsync_time),
                    vsync_time,
                    stream::ColorKeyFrame::default(),
                )
            };

            let key_layers;
            let layers: &[&xr::CompositionLayerBase<_>] =
                if let Some(passthrough_layer) = &passthrough_layer {
                    if use_color_key
                        && let Some(params) = key_params
                        && let Some(color_key_layer) = passthrough_layer.color_key_layer()
                    {
                        // See the color key in stream.wgsl
                        key_layers = layer.build_color_key();
                        let [lower, upper] = &key_layers;
                        // Without the extra layers, look like the four layers away from the hands:
                        // there only the upper one is visible if the lower one is cropped
                        // (the shader writes the alpha for this, see `key_lower_rects`)
                        let single = if params.crop > 0 { upper } else { lower };
                        if !params.extra_layers(key_frame.hands_in_view) {
                            &[passthrough_layer, single]
                        } else {
                            match params.layers {
                                30 => &[passthrough_layer, lower, color_key_layer],
                                31 => &[passthrough_layer, lower, upper],
                                _ => &[passthrough_layer, lower, color_key_layer, upper],
                            }
                        }
                    } else {
                        &[passthrough_layer, &layer.build()]
                    }
                } else {
                    &[&layer.build()]
                };

            if use_color_key && last_key_log.elapsed() > Duration::from_secs(2) {
                last_key_log = Instant::now();
                // Rim in all directions, sweep ahead / behind the motion
                let zones = key_frame.hand_zones.map(|zones| {
                    zones.map_or("-".into(), |zones| {
                        format!(
                            "rim {:.0} sweep {:.0}/{:.0} mm",
                            zones.rim * 1000.0,
                            zones.sweep.forward * 1000.0,
                            zones.sweep.backward * 1000.0
                        )
                    })
                });
                let playzone = key_frame
                    .playzone_age
                    .map_or("none".into(), |age| format!("{:.1} s", age.as_secs_f32()));
                stream_log(&format!(
                    "key: tracked hands {:?}, rects {:.2?}, layers {}, zones {}, {}, playzone {playzone}",
                    key_frame.tracked_hands,
                    key_frame.hand_rects,
                    layers.len(),
                    zones[0],
                    zones[1]
                ));
            }

            graphics_context.make_current();
            let res = xr_frame_stream.end(
                to_xr_time(display_time),
                xr::EnvironmentBlendMode::OPAQUE,
                layers,
            );

            if let Err(e) = res {
                let time = to_xr_time(display_time);
                error!("End frame failed! {e}, timestamp: {display_time:?}, time: {time:?}");

                if !platform.is_vive() {
                    xr_frame_stream
                        .end(
                            frame_state.predicted_display_time,
                            xr::EnvironmentBlendMode::OPAQUE,
                            &[],
                        )
                        .unwrap();
                }
            }
        }
    }

    // grapics_context is dropped here
}

#[allow(unused)]
fn xr_runtime_now(xr_instance: &xr::Instance) -> Option<xr::Time> {
    xr_instance
        .now()
        .ok()
        .filter(|&time_nanos| time_nanos.as_nanos() > 0)
}

#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
fn android_main(app: android_activity::AndroidApp) {
    use android_activity::{InputStatus, MainEvent, PollEvent};

    let rendering_thread = thread::spawn(|| {
        // workaround for the Pico runtime
        let context = ndk_context::android_context();
        let vm = unsafe { jni::JavaVM::from_raw(context.vm().cast()) };
        vm.attach_current_thread(|_env| {
            entry_point();

            jni::errors::Result::Ok(())
        })
        .unwrap();
    });

    let mut should_quit = false;
    while !should_quit {
        app.poll_events(Some(Duration::from_millis(100)), |event| match event {
            PollEvent::Main(MainEvent::Destroy) => {
                should_quit = true;
            }
            PollEvent::Main(MainEvent::InputAvailable) => {
                if let Ok(mut iter) = app.input_events_iter() {
                    while iter.next(|_| InputStatus::Unhandled) {}
                }
            }
            _ => (),
        });
    }

    // Note: the quit event is sent from OpenXR too, this will return rather quicly.
    rendering_thread.join().unwrap();
}
