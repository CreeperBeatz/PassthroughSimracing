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
//! payload is known at the start), in the two folders of the layout files (area_editor.rs). Read at the
//! start and when the payload changes to another one (not while the menu is open; a payload that is lost
//! keeps the profile in use), applied to the override table; written when the menu closes, from a
//! thread.
//!
//! Last writer wins: a property set with setprop to a new value replaces the menu's value of it (logged
//! as `tune: setprop <prop>=<v> replaces menu value <z>`; the next profile no longer has it).
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
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const PROPERTY_POLL_INTERVAL: Duration = Duration::from_millis(500);
/// The menu closes after this long without a pinch or a drag (seconds)
pub const IDLE_TIMEOUT_S: f64 = 120.0;
// The stats footer is read this often (seconds)
const STATS_INTERVAL_S: f64 = 0.5;
// Stats older than this are not shown
const STATS_MAX_AGE: Duration = Duration::from_secs(5);
// The panel is drawn again at most this often while it changes (seconds), and while only the cursors
// move (they move every frame)
const REDRAW_INTERVAL_S: f64 = 0.05;
const CURSOR_REDRAW_INTERVAL_S: f64 = 0.1;
// The cursors are drawn on a grid of this many texels: the jitter of a ray does not draw the panel again
const CURSOR_GRID_TEXELS: f32 = 4.0;
const GREEN_BOXES: &str = "debug.alvr.green_boxes";

// The number of the last cap message sent to green_patch.py, for the whole run
static LAST_PATCH_SEQ: AtomicU64 = AtomicU64::new(0);

// The first number of a new cap link: the Unix time in milliseconds (newer than any of an earlier run),
// and after the last one of this run
fn patch_seq_start() -> u64 {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |time| time.as_millis() as u64);
    now_ms.max(LAST_PATCH_SEQ.load(Ordering::Relaxed))
}

// The green key suspended by the green page: the override of `green_boxes` before it. For the whole run,
// so that a new `TuningMode` (a new session) still puts it back
static GREEN_SUSPENDED: Mutex<Option<Option<String>>> = Mutex::new(None);

fn suspend_green() {
    let mut suspended = GREEN_SUSPENDED
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    if suspended.is_some() {
        return;
    }
    *suspended = Some(passthrough::tuning_override(GREEN_BOXES));
    set_tuning_override(GREEN_BOXES, "", Some("green page: the green key is off"));
}

fn restore_green(why: &str) {
    let Some(saved) = GREEN_SUSPENDED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take()
    else {
        return;
    };
    match saved {
        Some(value) => set_tuning_override(GREEN_BOXES, &value, Some(why)),
        None => clear_tuning_override(GREEN_BOXES, Some(why)),
    }
}

// The properties whose value set with setprop changed since `last` (a property read for the first time
// is no change)
fn changed_properties(
    last: &BTreeMap<&'static str, Option<i64>>,
    current: &BTreeMap<&'static str, Option<i64>>,
) -> Vec<&'static str> {
    current
        .iter()
        .filter(|(property, value)| last.get(*property).is_some_and(|last| last != *value))
        .map(|(property, _)| *property)
        .collect()
}

// The cursors on a grid of `CURSOR_GRID_TEXELS`
fn snap_cursors(content: &mut MenuContent) {
    let grid = alvr_common::glam::Vec2::new(
        CURSOR_GRID_TEXELS / MENU_TEXTURE_WIDTH as f32,
        CURSOR_GRID_TEXELS / MENU_TEXTURE_HEIGHT as f32,
    );
    for cursor in &mut content.cursors {
        *cursor = (*cursor / grid).round() * grid;
    }
}

// How soon the panel may be drawn again for `content`: None, it shows that already
fn redraw_interval(drawn: Option<&MenuContent>, content: &MenuContent) -> Option<f64> {
    match drawn {
        Some(drawn) if drawn == content => None,
        Some(drawn)
            if drawn.tabs == content.tabs
                && drawn.rows == content.rows
                && drawn.footer == content.footer =>
        {
            Some(CURSOR_REDRAW_INTERVAL_S)
        }
        _ => Some(REDRAW_INTERVAL_S),
    }
}

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

// The saver thread of the run (one: two saves never write the same file at once)
static SAVER: Mutex<Option<mpsc::Sender<(String, String)>>> = Mutex::new(None);

// Writes a profile to both folders, on the saver thread
fn save_profile(name: String, text: String) {
    let mut saver = SAVER.lock().unwrap_or_else(PoisonError::into_inner);
    if saver.is_none() {
        let (sender, receiver) = mpsc::channel::<(String, String)>();
        let spawned = thread::Builder::new()
            .name("tune saver".into())
            .spawn(move || {
                while let Ok((name, text)) = receiver.recv() {
                    for dir in area_editor::area_dirs() {
                        let path = format!("{dir}/{name}");
                        match area_editor::write_through_temporary(&dir, &path, &text) {
                            Ok(()) => crate::stream_log(&format!("tune: saved {path}")),
                            Err(e) => {
                                crate::stream_log(&format!("tune: could not save {path}: {e}"));
                            }
                        }
                    }
                }
            });
        match spawned {
            Ok(_) => *saver = Some(sender),
            Err(e) => {
                crate::stream_log(&format!("tune: not saved, no saver thread: {e}"));
                return;
            }
        }
    }
    if let Some(sender) = saver.as_ref()
        && sender.send((name, text)).is_err()
    {
        crate::stream_log("tune: not saved, the saver thread stopped");
        *saver = None;
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
    // A content could not be given to the worker (logged once)
    jobs_failed: bool,
    // The worker's newest image, rows from the bottom (GL), and a buffer it may draw into (uploaded
    // ones come back: no new 3 MB per drawing)
    done: Arc<Mutex<Option<Vec<u8>>>>,
    free: Arc<Mutex<Option<Vec<u8>>>>,
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
        let free = Arc::new(Mutex::new(None));
        let (worker_done, worker_free) = (Arc::clone(&done), Arc::clone(&free));
        thread::Builder::new()
            .name("tune panel".into())
            .spawn(move || {
                while let Ok(mut content) = receiver.recv() {
                    // Only the newest one matters
                    while let Ok(newer) = receiver.try_recv() {
                        content = newer;
                    }
                    let mut pixels: Vec<u8> = worker_free
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .take()
                        .unwrap_or_default();
                    alvr_graphics::render_menu_into(
                        &content,
                        MENU_TEXTURE_WIDTH,
                        MENU_TEXTURE_HEIGHT,
                        &mut pixels,
                    );
                    alvr_graphics::flip_rows_in_place(
                        &mut pixels,
                        MENU_TEXTURE_WIDTH,
                        MENU_TEXTURE_HEIGHT,
                    );
                    // One not uploaded yet is replaced; its buffer is drawn into next time
                    let replaced = worker_done
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .replace(pixels);
                    if let Some(replaced) = replaced {
                        *worker_free.lock().unwrap_or_else(PoisonError::into_inner) =
                            Some(replaced);
                    }
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
            jobs_failed: false,
            done,
            free,
            upload_failed: false,
        })
    }

    // A new menu: nothing of the last one is shown or uploaded
    fn reset(&mut self) {
        self.ready = false;
        let stale = self
            .done
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if stale.is_some() {
            *self.free.lock().unwrap_or_else(PoisonError::into_inner) = stale;
        }
    }

    fn draw(&mut self, content: MenuContent) -> bool {
        match self.jobs.send(content) {
            Ok(()) => true,
            Err(_) => {
                if !self.jobs_failed {
                    self.jobs_failed = true;
                    crate::stream_log(
                        "tune: the panel thread stopped, the panel is not drawn (logged once)",
                    );
                }
                false
            }
        }
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
        let uploaded = gfx.upload_rgba8(
            self.images.get(index).copied().unwrap_or(0),
            MENU_TEXTURE_WIDTH,
            MENU_TEXTURE_HEIGHT,
            &pixels,
        );
        if let Err(e) = &uploaded {
            self.log_failure(&format!("upload: {e}"));
        }
        match self.swapchain.release_image() {
            // Only an image that was written is shown
            Ok(()) => self.ready |= uploaded.is_ok(),
            Err(e) => self.log_failure(&format!("release: {e}")),
        }
        *self.free.lock().unwrap_or_else(PoisonError::into_inner) = Some(pixels);
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
    // One for the run (its numbers go on over the visits of the green page)
    patch_link: PatchLink,
    // The values set with setprop of the menu's properties, as last read (None: not yet)
    system_values: Option<BTreeMap<&'static str, Option<i64>>>,
    // `RigLink::hint`, for the footer of the green page
    rig_hint: Option<&'static str>,
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
        // A green key suspended by the menu of a session before
        restore_green("left from the last session: the green key is back");
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
            patch_link: PatchLink::starting_at(patch_seq_start()),
            system_values: None,
            rig_hint: None,
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

        // The properties as set with setprop: for the values without an override, and a new value set
        // with setprop replaces the menu's (last writer wins)
        let current = tuned_properties()
            .into_iter()
            .map(|property| (property, system_property(&full_property(property))))
            .collect::<BTreeMap<_, _>>();
        if let Some(last) = &self.system_values {
            for property in changed_properties(last, &current) {
                let name = full_property(property);
                let Some(menu_value) = passthrough::tuning_override(&name) else {
                    continue;
                };
                clear_tuning_override(&name, None);
                if let Some(menu) = &mut self.menu {
                    menu.forget_override(property);
                }
                crate::stream_log(&format!(
                    "tune: setprop {property}={} replaces menu value {}",
                    current[property].map_or("(unset)".into(), |value| value.to_string()),
                    menu_value.trim()
                ));
            }
        }
        if let Some(menu) = &mut self.menu {
            for (property, value) in &current {
                menu.set_prop(property, *value);
            }
        }
        self.system_values = Some(current);
    }

    /// One frame, before the frame is rendered
    pub fn update(&mut self, frame: &TuneInput<'_>, rig: &mut RigLink) {
        let now = self.start.elapsed().as_secs_f64();
        self.poll_properties();
        self.rig_hint = rig.hint();
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
                self.leave(reason);
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
        self.last_draw = f64::NEG_INFINITY;
        self.last_stats = f64::NEG_INFINITY;
        if let Some(panel) = &mut self.panel {
            panel.reset();
        }
        crate::stream_log(&format!(
            "tune: on ({why}), profile {}, {overrides} values set by the menu",
            profile_file_name(self.profile_payload(payload).as_deref())
        ));
    }

    // The payload of the profile in use (the one read last), else `payload`
    fn profile_payload(&self, payload: Option<&str>) -> Option<String> {
        match &self.profile_for {
            Some(loaded) => loaded.clone(),
            None => payload.map(str::to_owned),
        }
    }

    /// The session stops (or the stream goes with it): the menu closes as with Done
    pub fn stop(&mut self, why: &str) {
        self.leave(why);
        restore_green(why);
    }

    // Saved under the profile in use
    fn leave(&mut self, why: &str) {
        if self.menu.take().is_none() {
            return;
        }
        restore_green("menu closed: the green key is back");
        self.space = None;
        let payload = self.profile_payload(None);
        let payload = payload.as_deref();
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
        // A payload that is lost keeps the profile in use: tune-default.txt only at the start
        if payload.is_none() && self.profile_for.is_some() {
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
            self.handle(event, rig, now);
            if self.menu.is_none() {
                return;
            }
        }
    }

    fn handle(&mut self, event: TuneEvent, rig: &mut RigLink, now: f64) {
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
                    LAST_PATCH_SEQ.store(self.patch_link.seq(), Ordering::Relaxed);
                    rig.send(&message);
                }
            }
            TuneEvent::CapCommitted { from, to } => {
                self.cap = Some(to);
                if let Some(message) = self.patch_link.message(&to, now, true) {
                    LAST_PATCH_SEQ.store(self.patch_link.seq(), Ordering::Relaxed);
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
                    suspend_green();
                    self.awaiting_reply = true;
                    rig.send(PATCH_GET_MESSAGE);
                } else {
                    restore_green("green page left: the green key is back");
                }
            }
            TuneEvent::PatchMode(on) => crate::stream_log(if on {
                "tune: moving / sizing the patch by hand: on"
            } else {
                "tune: moving / sizing the patch by hand: off"
            }),
            TuneEvent::Action(TuneAction::Done) => self.leave("Done"),
            TuneEvent::Action(TuneAction::EditBoxes) => {
                self.leave("Edit boxes");
                self.edit_boxes = true;
            }
            TuneEvent::Action(action) => crate::stream_log(&format!("tune: {action:?}")),
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
        let mut content = menu.content(&self.stats);
        snap_cursors(&mut content);
        if menu.page() == TunePage::GreenScreen
            && let Some(hint) = self.rig_hint
        {
            content.footer = hint.to_owned();
        }
        if let Some(interval) = redraw_interval(self.drawn.as_ref(), &content)
            && now - self.last_draw >= interval
        {
            self.last_draw = now;
            if panel.draw(content.clone()) {
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

impl Drop for TuningMode {
    // The session loop ends: the menu's values saved, the green key back
    fn drop(&mut self) {
        self.stop("the session ended");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alvr_common::glam::Vec2;

    #[test]
    fn setprop_changes_are_found() {
        let last = BTreeMap::from([("key_bias", Some(150)), ("cloud_own", None)]);
        let same = last.clone();
        assert!(changed_properties(&last, &same).is_empty());
        let current = BTreeMap::from([
            ("key_bias", Some(200)),
            ("cloud_own", None),
            // Read for the first time: no change
            ("edge_cut", Some(40)),
        ]);
        assert_eq!(changed_properties(&last, &current), vec!["key_bias"]);
        // Unset again is a change too
        let unset = BTreeMap::from([("key_bias", None), ("cloud_own", Some(20))]);
        assert_eq!(
            changed_properties(&last, &unset),
            vec!["cloud_own", "key_bias"]
        );
    }

    #[test]
    fn cursors_snap_and_redraws_wait() {
        let mut content = MenuContent {
            footer: "x".into(),
            cursors: vec![Vec2::new(0.5001, 0.3002)],
            ..MenuContent::default()
        };
        snap_cursors(&mut content);
        let mut jittered = content.clone();
        jittered.cursors = vec![Vec2::new(0.5006, 0.3008)];
        snap_cursors(&mut jittered);
        assert_eq!(content, jittered);

        assert_eq!(redraw_interval(None, &content), Some(REDRAW_INTERVAL_S));
        assert_eq!(redraw_interval(Some(&content), &content), None);
        let mut moved = content.clone();
        moved.cursors = vec![Vec2::new(0.6, 0.3)];
        assert_eq!(
            redraw_interval(Some(&content), &moved),
            Some(CURSOR_REDRAW_INTERVAL_S)
        );
        moved.footer = "y".into();
        assert_eq!(
            redraw_interval(Some(&content), &moved),
            Some(REDRAW_INTERVAL_S)
        );
    }

    #[test]
    fn cap_numbers_start_after_now_and_the_last() {
        let before = patch_seq_start();
        assert!(before > 1_700_000_000_000);
        LAST_PATCH_SEQ.store(before + 1_000_000, Ordering::Relaxed);
        assert_eq!(patch_seq_start(), before + 1_000_000);
        LAST_PATCH_SEQ.store(0, Ordering::Relaxed);
    }

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
