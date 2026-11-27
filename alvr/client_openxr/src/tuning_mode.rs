//! The tuning menu in the headset (the pure parts: alvr_graphics tuning.rs). Praying hands held 1 s open
//! it (and close it again, after the hands were let go); it is a floating panel, a quad layer on top of
//! everything, placed on entry ahead of the eyes and a little above them. The ray of a hand points at a
//! row, a pinch picks a tab, a switch or a button, a pinch-drag along the panel changes a value. The
//! values are overrides of the `debug.alvr.*` properties (passthrough.rs `set_tuning_override`): they win
//! over setprop, "Reset" removes them. Each change is logged as `tune: <prop> <old> -> <new>`.
//!
//! It also ends with "Done", after `IDLE_TIMEOUT_S` without a pinch or a drag, when the stream with
//! Passthrough = Depth goes, and when the area editor starts editing ("Edit boxes" ends it and starts the
//! editor). It never starts while the area editor edits. While it is open the hands are not sent to the
//! PC (as while editing the area).
//!
//! The green page: the green key is suspended (`debug.alvr.green_boxes` overridden with nothing) so that
//! the real green cap on the rig monitor is seen, the play area's outline is shown, the cap is asked from
//! `green_patch.py` (`{"get":1}`, rig_link.rs) and its moves are sent to it. Leaving the page puts back
//! the override of `green_boxes` as it was.
//!
//! Profiles: `tune-<payload>-<hash>.txt` of the code the box is placed from (`tune-default.txt` while no
//! payload is known), in the two folders of the layout files (area_editor.rs). Read at the start and
//! when the payload changes (not while the menu is open), applied to the override table; written when
//! the menu closes, from a thread.
//!
//! Experimental, controlled with adb (polled about twice a second):
//! - `debug.alvr.tune`: a change to 1 opens the menu, a change to 0 closes it (the value at the start
//!   of the app is not a request: set 0, then 1)
//! - `debug.alvr.tune_gesture 0`: the praying hands do nothing (unset or 1: they open and close it)

use crate::{
    area_editor,
    passthrough::{self, clear_tuning_override, set_tuning_override, system_property},
    rig_link::RigLink,
};
use alvr_graphics::{
    EditorInput, GraphicsContext, MENU_TEXTURE_HEIGHT, MENU_TEXTURE_WIDTH, MenuContent,
    PANEL_HEIGHT, PANEL_WIDTH, PATCH_GET_MESSAGE, PROFILE_CAP_KEY, PanelPose, PatchCap, PatchLink,
    PrayingGesture, TuneAction, TuneEvent, TuneMenu, TunePage, TuneProfile, TuneStats, TuneValues,
    full_property, parse_patch_reply, parse_profile, profile_file_name, profile_text,
    property_range, tuned_properties,
};
use openxr as xr;
use std::{
    collections::BTreeMap,
    fs, io,
    sync::{Arc, Mutex, PoisonError, mpsc},
    thread,
    time::{Duration, Instant},
};

const PROPERTY_POLL_INTERVAL: Duration = Duration::from_millis(500);
/// The menu closes after this long without a pinch or a drag (seconds)
pub const IDLE_TIMEOUT_S: f64 = 120.0;
// The stats footer is read this often (seconds)
const STATS_INTERVAL_S: f64 = 0.5;
// Stats older than this are not shown
const STATS_MAX_AGE: Duration = Duration::from_secs(5);
// The panel is drawn again at most this often while it changes (seconds): the cursors move every frame
const REDRAW_INTERVAL_S: f64 = 0.05;
const GREEN_BOXES: &str = "debug.alvr.green_boxes";

// --- The stats of the footer ----------------------------------------------------------------------

struct SharedStats {
    // skin, background, scene kept, the auto bias
    key: Option<(Instant, [Option<f32>; 4])>,
    green: Option<(Instant, f32)>,
}

static STATS: Mutex<SharedStats> = Mutex::new(SharedStats {
    key: None,
    green: None,
});

/// From the colour learner (auto_color_key.rs) at its `auto key:` line: of the skin, background and
/// scene colours the share the key keeps, and the bias `key_auto` chose
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub fn publish_key_stats(
    skin: Option<f32>,
    background: Option<f32>,
    scene: Option<f32>,
    auto_bias: Option<f32>,
) {
    STATS.lock().unwrap_or_else(PoisonError::into_inner).key =
        Some((Instant::now(), [skin, background, scene, auto_bias]));
}

/// From the green-screen key (green_screen.rs) at its `green:` line: the share of the zone's camera
/// pixels that are green
pub fn publish_green_share(share: f32) {
    STATS.lock().unwrap_or_else(PoisonError::into_inner).green = Some((Instant::now(), share));
}

fn stats_snapshot() -> TuneStats {
    let stats = STATS.lock().unwrap_or_else(PoisonError::into_inner);
    let fresh = |time: &Instant| time.elapsed() <= STATS_MAX_AGE;
    let key = stats
        .key
        .filter(|(time, _)| fresh(time))
        .map_or([None; 4], |(_, key)| key);
    TuneStats {
        skin_kept: key[0],
        background_kept: key[1],
        scene_kept: key[2],
        auto_bias: key[3],
        green_share: stats
            .green
            .filter(|(time, _)| fresh(time))
            .map(|(_, share)| share),
    }
}

// --- Profiles -------------------------------------------------------------------------------------

/// The overrides of the menu's properties in the table (names without `debug.alvr.`)
fn current_overrides() -> BTreeMap<String, i64> {
    let table = passthrough::tuning_overrides();
    tuned_properties()
        .into_iter()
        .filter_map(|property| {
            let value = table.get(&full_property(property))?.trim().parse().ok()?;
            Some((property.to_owned(), value))
        })
        .collect()
}

// Whether a property is a switch (logged on each change; values are logged when their drag ends)
fn is_switch(property: &str) -> bool {
    property_range(property).is_some_and(|range| range.min == 0.0 && range.max == 1.0)
}

// The newest of the profile files `name` that reads, with its path
fn read_profile(name: &str) -> Option<(String, TuneProfile)> {
    let mut found = Vec::new();
    for dir in area_editor::area_dirs() {
        let path = format!("{dir}/{name}");
        match fs::read_to_string(&path) {
            Ok(text) => {
                let modified = fs::metadata(&path).and_then(|meta| meta.modified()).ok();
                found.push((modified, path, text));
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => (),
            Err(e) => crate::stream_log(&format!("tune: can't read {path}: {e}")),
        }
    }
    found.sort_by_key(|(modified, _, _)| std::cmp::Reverse(*modified));
    found
        .into_iter()
        .next()
        .map(|(_, path, text)| (path, parse_profile(&text)))
}

// Writes a profile to both folders from a thread of its own
fn save_profile(name: String, text: String) {
    let spawned = thread::Builder::new()
        .name("tune saver".into())
        .spawn(move || {
            for dir in area_editor::area_dirs() {
                let path = format!("{dir}/{name}");
                match area_editor::write_through_temporary(&dir, &path, &text) {
                    Ok(()) => crate::stream_log(&format!("tune: saved {path}")),
                    Err(e) => crate::stream_log(&format!("tune: could not save {path}: {e}")),
                }
            }
        });
    if let Err(e) = spawned {
        crate::stream_log(&format!("tune: not saved, no saver thread: {e}"));
    }
}

fn cap_text(cap: &PatchCap) -> String {
    format!("{:.3},{:.3},{:.3},{:.2}", cap.cx, cap.cy, cap.r, cap.cut)
}

// --- The panel (quad layer) -----------------------------------------------------------------------

struct MenuPanel {
    swapchain: xr::Swapchain<xr::OpenGlEs>,
    images: Vec<u32>,
    // An image was released: the layer can be submitted
    ready: bool,
    jobs: mpsc::Sender<MenuContent>,
    // The worker's newest image, rows from the bottom (GL)
    done: Arc<Mutex<Option<Vec<u8>>>>,
    upload_failed: bool,
}

impl MenuPanel {
    fn new(session: &xr::Session<xr::OpenGlEs>, gfx: &GraphicsContext) -> Result<Self, String> {
        gfx.make_current();
        // An 8-bit UNORM format: the texture is premultiplied on gamma values, as the lobby's
        let formats = session
            .enumerate_swapchain_formats()
            .map_err(|e| format!("formats: {e}"))?;
        let format = [
            alvr_graphics::SDR_FORMAT_GL,
            0x8C43, /* GL_SRGB8_ALPHA8 */
        ]
        .into_iter()
        .find(|format| formats.contains(format))
        .unwrap_or(alvr_graphics::SDR_FORMAT_GL);
        let info = |usage| xr::SwapchainCreateInfo {
            create_flags: xr::SwapchainCreateFlags::EMPTY,
            usage_flags: usage,
            format,
            sample_count: 1,
            width: MENU_TEXTURE_WIDTH,
            height: MENU_TEXTURE_HEIGHT,
            face_count: 1,
            array_size: 1,
            mip_count: 1,
        };
        let usage = xr::SwapchainUsageFlags::COLOR_ATTACHMENT | xr::SwapchainUsageFlags::SAMPLED;
        let swapchain = session
            .create_swapchain(&info(usage | xr::SwapchainUsageFlags::TRANSFER_DST))
            .or_else(|_| session.create_swapchain(&info(usage)))
            .map_err(|e| format!("xrCreateSwapchain: {e}"))?;
        let images = swapchain
            .enumerate_images()
            .map_err(|e| format!("images: {e}"))?;

        let (jobs, receiver) = mpsc::channel::<MenuContent>();
        let done = Arc::new(Mutex::new(None));
        let worker_done = Arc::clone(&done);
        thread::Builder::new()
            .name("tune panel".into())
            .spawn(move || {
                while let Ok(mut content) = receiver.recv() {
                    // Only the newest one matters
                    while let Ok(newer) = receiver.try_recv() {
                        content = newer;
                    }
                    let pixels = alvr_graphics::render_menu(
                        &content,
                        MENU_TEXTURE_WIDTH,
                        MENU_TEXTURE_HEIGHT,
                    );
                    let pixels =
                        alvr_graphics::flip_rows(&pixels, MENU_TEXTURE_WIDTH, MENU_TEXTURE_HEIGHT);
                    *worker_done.lock().unwrap_or_else(PoisonError::into_inner) = Some(pixels);
                }
            })
            .map_err(|e| format!("no panel thread: {e}"))?;
        crate::stream_log(&format!(
            "tune: panel swapchain {MENU_TEXTURE_WIDTH}x{MENU_TEXTURE_HEIGHT}, format 0x{format:x}, {} \
             images",
            images.len()
        ));

        Ok(Self {
            swapchain,
            images,
            ready: false,
            jobs,
            done,
            upload_failed: false,
        })
    }

    // The worker's newest image into the next swapchain image
    fn upload(&mut self, gfx: &GraphicsContext) {
        let Some(pixels) = self
            .done
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        else {
            return;
        };
        gfx.make_current();
        let index = match self.swapchain.acquire_image() {
            Ok(index) => index as usize,
            Err(e) => {
                self.log_failure(&format!("acquire: {e}"));
                return;
            }
        };
        if let Err(e) = self.swapchain.wait_image(xr::Duration::INFINITE) {
            self.log_failure(&format!("wait: {e}"));
            self.swapchain.release_image().ok();
            return;
        }
        if let Some(error) = gfx.upload_rgba8(
            self.images.get(index).copied().unwrap_or(0),
            MENU_TEXTURE_WIDTH,
            MENU_TEXTURE_HEIGHT,
            &pixels,
        ) {
            self.log_failure(&format!("GL error 0x{error:x} in the upload"));
        }
        match self.swapchain.release_image() {
            Ok(()) => self.ready = true,
            Err(e) => self.log_failure(&format!("release: {e}")),
        }
    }

    fn log_failure(&mut self, text: &str) {
        if !self.upload_failed {
            self.upload_failed = true;
            crate::stream_log(&format!("tune: panel upload failed: {text} (logged once)"));
        }
    }
}

// --- The mode -------------------------------------------------------------------------------------

/// What the frame gives the tuning mode
pub struct TuneInput<'a> {
    /// The eyes and the hands (stage space), None: not available
    pub input: Option<&'a EditorInput>,
    /// A stream with Passthrough = Depth
    pub stream: bool,
    /// The area editor edits
    pub area_editing: bool,
    /// The payload of the code the box is placed from
    pub payload: Option<&'a str>,
    /// The pinch thresholds of the area editor (meters)
    pub pinch: (f32, f32),
}

pub struct TuningMode {
    start: Instant,
    last_poll: Option<Instant>,
    // `debug.alvr.tune` as last read, and a change not handled yet (true: open)
    tune_property: Option<i64>,
    request: Option<bool>,
    gesture_enabled: bool,
    // One for the whole run: a new one on entry would fire again while the hands are still together
    gesture: PrayingGesture,
    menu: Option<TuneMenu>,
    entered_at: f64,
    // The payload whose profile was read (Some(None): none known, `tune-default.txt`)
    profile_for: Option<Option<String>>,
    // The cap of the green patch as last known (profile, green_patch.py, the menu)
    cap: Option<PatchCap>,
    // On the green page: the override of `green_boxes` before it was suspended
    green_saved: Option<Option<String>>,
    patch_link: PatchLink,
    // A `get` was sent, its reply not yet logged
    awaiting_reply: bool,
    reply_error_logged: bool,
    stats: TuneStats,
    last_stats: f64,
    // "Edit boxes": the area editor is to start editing
    edit_boxes: bool,
    panel: Option<MenuPanel>,
    panel_failed: bool,
    space: Option<Arc<xr::Space>>,
    // What the panel shows (sent to the worker), and when it was last drawn
    drawn: Option<MenuContent>,
    last_draw: f64,
}

impl TuningMode {
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
            last_poll: None,
            // A value left from before is not a request
            tune_property: passthrough::debug_property("debug.alvr.tune"),
            request: None,
            gesture_enabled: true,
            gesture: PrayingGesture::default(),
            menu: None,
            entered_at: 0.0,
            profile_for: None,
            cap: None,
            green_saved: None,
            patch_link: PatchLink::default(),
            awaiting_reply: false,
            reply_error_logged: false,
            stats: TuneStats::default(),
            last_stats: f64::NEG_INFINITY,
            edit_boxes: false,
            panel: None,
            panel_failed: false,
            space: None,
            drawn: None,
            last_draw: f64::NEG_INFINITY,
        }
    }

    /// The menu is open
    pub fn is_active(&self) -> bool {
        self.menu.is_some()
    }

    /// The hands are not sent to the PC while the menu is open
    pub fn withholds_hands(&self) -> bool {
        self.is_active()
    }

    /// The hands are wanted (the gesture is watched, or the menu is open)
    pub fn wants_input(&self) -> bool {
        self.gesture_enabled || self.is_active()
    }

    /// Why the play area's outline must be shown: on the green page
    pub fn forced_outline(&self) -> Option<&'static str> {
        self.menu
            .as_ref()
            .filter(|menu| menu.page() == TunePage::GreenScreen)
            .map(|_| "tuning, green page")
    }

    /// "Edit boxes" was picked: the area editor is to start editing (once)
    pub fn take_edit_boxes(&mut self) -> bool {
        std::mem::take(&mut self.edit_boxes)
    }

    fn poll_properties(&mut self) {
        if self
            .last_poll
            .is_some_and(|time| time.elapsed() < PROPERTY_POLL_INTERVAL)
        {
            return;
        }
        self.last_poll = Some(Instant::now());

        let tune = passthrough::debug_property("debug.alvr.tune");
        if tune != self.tune_property {
            self.tune_property = tune;
            match tune {
                Some(1) => self.request = Some(true),
                Some(0) => self.request = Some(false),
                _ => (),
            }
        }
        let gesture = passthrough::debug_property("debug.alvr.tune_gesture").unwrap_or(1) != 0;
        if gesture != self.gesture_enabled {
            self.gesture_enabled = gesture;
            crate::stream_log(if gesture {
                "tune: the praying hands open and close the menu (tune_gesture 1)"
            } else {
                "tune: the praying hands do nothing (tune_gesture 0)"
            });
        }

        // The properties as set with setprop, for the values without an override
        if let Some(menu) = &mut self.menu {
            for property in tuned_properties() {
                menu.set_prop(property, system_property(&full_property(property)));
            }
        }
    }

    /// One frame, before the frame is rendered
    pub fn update(&mut self, frame: &TuneInput<'_>, rig: &mut RigLink) {
        let now = self.start.elapsed().as_secs_f64();
        self.poll_properties();
        if self.menu.is_none() {
            self.follow_payload(frame.payload);
        }

        let (_, fired) = if self.gesture_enabled {
            self.gesture.update(frame.input, now)
        } else {
            (0.0, false)
        };
        let request = self.request.take();

        if self.menu.is_some() {
            let reason = if !frame.stream {
                Some("the stream with Passthrough = Depth stopped")
            } else if frame.area_editing {
                Some("the area editor started")
            } else if fired {
                Some("gesture")
            } else if request == Some(false) {
                Some("tune 0")
            } else if self.idle(now) {
                Some("nothing done for 120 s")
            } else {
                None
            };
            if let Some(reason) = reason {
                self.leave(reason, frame.payload);
                return;
            }
            self.update_menu(frame, rig, now);
        } else if fired || request == Some(true) {
            let why = if fired { "gesture" } else { "tune 1" };
            match frame.input {
                Some(input) if frame.stream && !frame.area_editing => {
                    self.enter(why, input, frame.payload, now);
                }
                _ => crate::stream_log(&format!(
                    "tune: can't open the menu ({why}): {}",
                    if !frame.stream {
                        "no stream with Passthrough = Depth"
                    } else if frame.area_editing {
                        "the area editor is editing"
                    } else {
                        "no head pose"
                    }
                )),
            }
        }
    }

    fn idle(&self, now: f64) -> bool {
        let last = self
            .menu
            .as_ref()
            .and_then(TuneMenu::last_activity)
            .unwrap_or(self.entered_at)
            .max(self.entered_at);
        now - last > IDLE_TIMEOUT_S
    }

    fn enter(&mut self, why: &str, input: &EditorInput, payload: Option<&str>, now: f64) {
        let values = TuneValues {
            props: tuned_properties()
                .into_iter()
                .filter_map(|property| {
                    system_property(&full_property(property)).map(|value| (property.into(), value))
                })
                .collect(),
            overrides: current_overrides(),
        };
        let overrides = values.overrides.len();
        self.menu = Some(TuneMenu::new(
            PanelPose::from_head(input.eye, input.forward),
            values,
            self.cap.unwrap_or_default(),
        ));
        self.entered_at = now;
        self.drawn = None;
        self.last_stats = f64::NEG_INFINITY;
        self.patch_link = PatchLink::default();
        crate::stream_log(&format!(
            "tune: on ({why}), profile {}, {overrides} values set by the menu",
            profile_file_name(payload)
        ));
    }

    fn leave(&mut self, why: &str, payload: Option<&str>) {
        if self.menu.take().is_none() {
            return;
        }
        self.restore_green();
        self.space = None;
        let profile = TuneProfile {
            overrides: current_overrides(),
            cap: self.cap,
        };
        let name = profile_file_name(payload);
        crate::stream_log(&format!(
            "tune: off ({why}), profile {name}, {} values set by the menu{}",
            profile.overrides.len(),
            profile
                .cap
                .map_or(String::new(), |cap| format!(", cap {}", cap_text(&cap)))
        ));
        save_profile(name, profile_text(&profile, payload));
        // The payload whose profile is in the table now
        self.profile_for = Some(payload.map(str::to_owned));
    }

    // The profile of the payload, when it changed (not while the menu is open)
    fn follow_payload(&mut self, payload: Option<&str>) {
        if self
            .profile_for
            .as_ref()
            .is_some_and(|loaded| loaded.as_deref() == payload)
        {
            return;
        }
        let first = self.profile_for.is_none();
        self.profile_for = Some(payload.map(str::to_owned));
        let name = profile_file_name(payload);
        let Some((path, profile)) = read_profile(&name) else {
            crate::stream_log(&format!(
                "tune: no profile {name} in {} or {}{}",
                area_editor::area_dirs()[0],
                area_editor::area_dirs()[1],
                if first {
                    ""
                } else {
                    ", the menu's values stay (saved under it when the menu closes)"
                }
            ));
            return;
        };
        for property in tuned_properties() {
            let name = full_property(property);
            match profile.overrides.get(property) {
                Some(value) => set_tuning_override(&name, &value.to_string(), None),
                None => clear_tuning_override(&name, None),
            }
        }
        if profile.cap.is_some() {
            self.cap = profile.cap;
        }
        crate::stream_log(&format!(
            "tune: profile {path} {} values{}: {}",
            profile.overrides.len(),
            profile.cap.map_or(String::new(), |cap| format!(
                ", {PROFILE_CAP_KEY} {}",
                cap_text(&cap)
            )),
            profile
                .overrides
                .iter()
                .map(|(property, value)| format!("{property}={value}"))
                .collect::<Vec<_>>()
                .join(" ")
        ));
    }

    fn update_menu(&mut self, frame: &TuneInput<'_>, rig: &mut RigLink, now: f64) {
        // Replies of green_patch.py
        for reply in rig.poll() {
            match parse_patch_reply(&reply) {
                Ok(reply) => {
                    if let Some(cap) = reply.cap {
                        if let Some(menu) = &mut self.menu {
                            menu.set_cap(cap);
                        }
                        self.cap = self.menu.as_ref().map(TuneMenu::cap).or(Some(cap));
                    }
                    if self.awaiting_reply {
                        self.awaiting_reply = false;
                        crate::stream_log(&format!(
                            "tune: green_patch.py replied: shape {}, cap {}, visible {}",
                            reply.shape.as_deref().unwrap_or("-"),
                            reply.cap.map_or("-".into(), |cap| cap_text(&cap)),
                            reply
                                .visible
                                .map_or("-".into(), |visible| visible.to_string())
                        ));
                    }
                }
                Err(e) if !self.reply_error_logged => {
                    self.reply_error_logged = true;
                    crate::stream_log(&format!(
                        "tune: a reply of green_patch.py does not read ({e}): {reply:?} (logged once)"
                    ));
                }
                Err(_) => (),
            }
        }

        if now - self.last_stats >= STATS_INTERVAL_S {
            self.last_stats = now;
            self.stats = stats_snapshot();
        }

        let Some(menu) = &mut self.menu else {
            return;
        };
        let events = menu.update(frame.input, now, frame.pinch.0, frame.pinch.1);
        for event in events {
            self.handle(event, rig, now, frame.payload);
            if self.menu.is_none() {
                return;
            }
        }
    }

    fn handle(&mut self, event: TuneEvent, rig: &mut RigLink, now: f64, payload: Option<&str>) {
        match event {
            TuneEvent::Set { property, new, .. } => set_tuning_override(
                &full_property(property),
                &new.to_string(),
                is_switch(property).then_some(""),
            ),
            TuneEvent::Reset { property, .. } => {
                clear_tuning_override(&full_property(property), Some("reset"));
            }
            TuneEvent::Committed { property, from, to } => passthrough::log_tuning_change(
                &full_property(property),
                &from.to_string(),
                &to.to_string(),
                "",
            ),
            TuneEvent::Cap { new, .. } => {
                self.cap = Some(new);
                if let Some(message) = self.patch_link.message(&new, now, false) {
                    rig.send(&message);
                }
            }
            TuneEvent::CapCommitted { from, to } => {
                self.cap = Some(to);
                if let Some(message) = self.patch_link.message(&to, now, true) {
                    rig.send(&message);
                }
                passthrough::log_tuning_change(
                    PROFILE_CAP_KEY,
                    &cap_text(&from),
                    &cap_text(&to),
                    &format!("seq {}", self.patch_link.seq()),
                );
            }
            TuneEvent::Page(page) => {
                crate::stream_log(&format!("tune: page {}", page.title()));
                if page == TunePage::GreenScreen {
                    self.suspend_green();
                    self.awaiting_reply = true;
                    rig.send(PATCH_GET_MESSAGE);
                } else {
                    self.restore_green();
                }
            }
            TuneEvent::PatchMode(on) => crate::stream_log(if on {
                "tune: moving / sizing the patch by hand: on"
            } else {
                "tune: moving / sizing the patch by hand: off"
            }),
            TuneEvent::Action(TuneAction::Done) => self.leave("Done", payload),
            TuneEvent::Action(TuneAction::EditBoxes) => {
                self.leave("Edit boxes", payload);
                self.edit_boxes = true;
            }
            TuneEvent::Action(action) => crate::stream_log(&format!("tune: {action:?}")),
        }
    }

    // The green page: the green key off, so that the real cap is seen
    fn suspend_green(&mut self) {
        if self.green_saved.is_some() {
            return;
        }
        self.green_saved = Some(passthrough::tuning_override(GREEN_BOXES));
        set_tuning_override(GREEN_BOXES, "", Some("green page: the green key is off"));
    }

    // Leaving the green page: the override of `green_boxes` as it was
    fn restore_green(&mut self) {
        let Some(saved) = self.green_saved.take() else {
            return;
        };
        let why = Some("green page left: the green key is back");
        match saved {
            Some(value) => set_tuning_override(GREEN_BOXES, &value, why),
            None => clear_tuning_override(GREEN_BOXES, why),
        }
    }

    /// While the menu is open: draws the panel when it changed and uploads it. `stage_space`: the
    /// space the panel is placed in (the stream's stage space). Between the frame's begin and end
    pub fn render(
        &mut self,
        session: &xr::Session<xr::OpenGlEs>,
        gfx: &GraphicsContext,
        stage_space: Option<Arc<xr::Space>>,
    ) {
        let Some(menu) = &self.menu else {
            return;
        };
        self.space = stage_space;
        if self.panel.is_none() && !self.panel_failed {
            match MenuPanel::new(session, gfx) {
                Ok(panel) => self.panel = Some(panel),
                Err(e) => {
                    self.panel_failed = true;
                    crate::stream_log(&format!("tune: no panel: {e}"));
                }
            }
        }
        let Some(panel) = &mut self.panel else {
            return;
        };

        let now = self.start.elapsed().as_secs_f64();
        let content = menu.content(&self.stats);
        if self.drawn.as_ref() != Some(&content) && now - self.last_draw >= REDRAW_INTERVAL_S {
            self.last_draw = now;
            if panel.jobs.send(content.clone()).is_ok() {
                self.drawn = Some(content);
            }
        }
        panel.upload(gfx);
    }

    /// The panel as a quad layer, the last of the frame: while the menu is open and drawn
    pub fn layer(&self) -> Option<xr::CompositionLayerQuad<'_, xr::OpenGlEs>> {
        let menu = self.menu.as_ref()?;
        let panel = self.panel.as_ref().filter(|panel| panel.ready)?;
        let space = self.space.as_deref()?;
        Some(
            xr::CompositionLayerQuad::new()
                // Premultiplied (no UNPREMULTIPLIED_ALPHA): render_menu's texture
                .layer_flags(xr::CompositionLayerFlags::BLEND_TEXTURE_SOURCE_ALPHA)
                .space(space)
                .eye_visibility(xr::EyeVisibility::BOTH)
                .sub_image(
                    xr::SwapchainSubImage::new()
                        .swapchain(&panel.swapchain)
                        .image_array_index(0)
                        .image_rect(xr::Rect2Di {
                            offset: xr::Offset2Di { x: 0, y: 0 },
                            extent: xr::Extent2Di {
                                width: MENU_TEXTURE_WIDTH as i32,
                                height: MENU_TEXTURE_HEIGHT as i32,
                            },
                        }),
                )
                .pose(xr::Posef {
                    orientation: crate::to_xr_quat(menu.panel.orientation()),
                    position: crate::to_xr_vec3(menu.panel.center),
                })
                .size(xr::Extent2Df {
                    width: PANEL_WIDTH,
                    height: PANEL_HEIGHT,
                }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switches_are_logged_at_once() {
        assert!(is_switch("key_auto"));
        assert!(!is_switch("key_bias"));
        assert!(!is_switch("cloud_trail"));
        assert!(!is_switch("unknown"));
    }

    #[test]
    fn stats_are_dropped_when_old() {
        publish_green_share(0.25);
        assert_eq!(stats_snapshot().green_share, Some(0.25));
        STATS.lock().unwrap().green = Some((Instant::now() - STATS_MAX_AGE * 2, 0.5));
        assert_eq!(stats_snapshot().green_share, None);
        publish_key_stats(Some(0.9), None, Some(0.3), Some(1.5));
        let stats = stats_snapshot();
        assert_eq!(
            (
                stats.skin_kept,
                stats.background_kept,
                stats.scene_kept,
                stats.auto_bias
            ),
            (Some(0.9), None, Some(0.3), Some(1.5))
        );
    }

    #[test]
    fn the_menu_overrides_are_read_from_the_table() {
        set_tuning_override("debug.alvr.cloud_own", "20", None);
        set_tuning_override("debug.alvr.green_boxes", "0", None);
        let overrides = current_overrides();
        assert_eq!(overrides.get("cloud_own"), Some(&20));
        // Not a property of the menu
        assert!(!overrides.contains_key("green_boxes"));
        clear_tuning_override("debug.alvr.cloud_own", None);
        clear_tuning_override("debug.alvr.green_boxes", None);
    }
}
