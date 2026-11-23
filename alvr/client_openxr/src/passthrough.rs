use crate::extra_extensions::{COLOR_KEY_SIZE, PassthroughFB, PassthroughHTC};
use alvr_common::{
    anyhow::{Result, bail},
    error,
};
use alvr_graphics::{HandSweep, HandZones, SweepParams};
use alvr_system_info::Platform;
use openxr::{self as xr};
use std::{
    fs,
    marker::PhantomData,
    ops::Deref,
    ptr,
    time::{Duration, Instant},
};

pub struct PassthroughLayer<'a> {
    handle_fb: Option<PassthroughFB>,
    handle_htc: Option<PassthroughHTC>,
    // Version of the applied color key, 0: none
    color_key_version: u32,
    _marker: PhantomData<&'a ()>,
}

impl PassthroughLayer<'_> {
    pub fn new(session: &xr::Session<xr::OpenGlEs>, platform: Platform) -> Result<Self> {
        let mut handle_fb = None;
        let mut handle_htc = None;

        let exts = session.instance().exts();
        if exts.fb_passthrough.is_some() {
            handle_fb = Some(PassthroughFB::new(session, platform)?);
        } else if exts.htc_passthrough.is_some() {
            handle_htc = Some(PassthroughHTC::new(session)?);
        } else {
            bail!("No passthrough extension available");
        };

        Ok(Self {
            handle_fb,
            handle_htc,
            color_key_version: 0,
            _marker: PhantomData,
        })
    }

    /// Apply or remove the color key. Returns whether the passthrough is color keyed.
    pub fn update_color_key(&mut self, key: Option<&ColorKey>) -> bool {
        let Some(handle) = &mut self.handle_fb else {
            return false;
        };

        let version = key.map_or(0, |key| key.version);
        if version != self.color_key_version {
            match handle.set_color_key(key.map(|key| key.lut.as_slice())) {
                Ok(()) => (),
                Err(e) => error!("Passthrough color key: {e}"),
            }
            // Don't retry a failed key every frame
            self.color_key_version = version;
        }

        handle.color_key_layer().is_some()
    }

    /// Replace the colors of the key, until the next change of its version
    pub fn set_color_key_lut(&mut self, lut: &[u8]) {
        if let Some(handle) = &mut self.handle_fb
            && let Err(e) = handle.update_color_key(lut)
        {
            error!("Passthrough color key update: {e}");
        }
    }

    /// The color keyed layer, if `update_color_key` returned true
    pub fn color_key_layer(&self) -> Option<&xr::CompositionLayerBase<'_, xr::OpenGlEs>> {
        let layer = self.handle_fb.as_ref()?.color_key_layer()?;

        Some(unsafe { &*ptr::from_ref(layer).cast() })
    }
}

/// Experimental passthrough color key, controlled with adb:
/// - push a LUT (see `COLOR_KEY_SIZE`) to one of `COLOR_KEY_PATHS`
/// - `setprop debug.alvr.color_key <n>`: 0 disables the key, another value (re)loads the LUT
/// - `setprop debug.alvr.key_rim <mm>`: width of the color keyed rim around the hands
/// - `setprop debug.alvr.key_inset <mm>`: how much the solid inside of the hands is shrunk
/// - `setprop debug.alvr.key_falloff <mm>`: how gradually the solid inside fades out
/// - `setprop debug.alvr.key_error <ms>`: assumed timing error, the solid core shrinks with the speed
/// - `setprop debug.alvr.key_rim_error <ms>`: worst timing error, the rim grows with the speed
/// - `setprop debug.alvr.key_rim_max <mm>`: limit of that growth
/// - `setprop debug.alvr.key_speed <n>`: that speed is of the wrist (2), of the palm, the wrist and the
///   knuckles (1: a hand that turns in place grows the rim) or of the fastest joint (0: also moving fingers)
/// - `setprop debug.alvr.key_sweep 0`: the rim grows in all directions, not along the motion of the hand
/// - `setprop debug.alvr.key_sweep_max <mm>`: limit of the growth along the motion, ahead and behind
/// - `setprop debug.alvr.key_sweep_iso <percent>`: share of the growth that stays in all directions
/// - `setprop debug.alvr.key_sweep_bias <percent>`: -100..100, positive grows further ahead than behind
/// - `setprop debug.alvr.key_unknown <percent>`: how much to keep colors that were never seen
/// - `setprop debug.alvr.key_bias <percent>`: preference for skin if the background looks the same
/// - `setprop debug.alvr.key_tight <percent>`: how similar to the skin a color must be
/// - `setprop debug.alvr.key_smooth <percent>`: width of the smoothing of the skin colors, of a LUT texel
///   (70..100). `key_tight` is scaled by its cube: 70 keeps rarer skin colors, not more around common ones
/// - `setprop debug.alvr.key_confirm <percent>`: `key_tight` of a second table (never uploaded) for the points
///   on the axis of each segment when hand segments are verified, and for its reliability. At most
///   `key_tight`. 0: off, the key table
/// - `setprop debug.alvr.key_steady 0`: a hand is verified only if all its joints are tracked and slow, as
///   before. 1: if its palm is, each segment whose two joints are. Its reliability then comes from its own
///   background only
/// - `setprop debug.alvr.playzone <n>`: another value remembers the playzone now (keep the hands away)
/// - `setprop debug.alvr.playzone_auto 0`: don't remember the playzone when no hands are tracked
/// - `setprop debug.alvr.auto_key 1` before starting the app: learn the colors with the camera
/// - `setprop debug.alvr.key_solid <n>`: capsules that are always solid, 0: none, 6: wrist, 9: palm, 28: whole hand
/// - `setprop debug.alvr.key_scale <percent>`: thickness of the hands
/// - `setprop debug.alvr.key_delay <ms>`: how late (may be negative) the passthrough image is, to locate the hands at
///   the moment it shows
/// - `setprop debug.alvr.key_layers <n>`: 4: the extra layers only with hands in view, 2: never, 5: always,
///   30 / 31: always, three layers for measurements (without the upper game layer / the keyed passthrough)
/// - `setprop debug.alvr.key_crop 0`: don't crop the lower game layer to the hands, 2: crop with the y origin
///   at the top of the image
/// - `setprop debug.alvr.key_hands 0`: no hands in the GPU passes (to measure them)
/// - `setprop debug.alvr.arm_shift 0`: plain depth for the arms, not shifted by the motion of the wrist (also
///   without the key, see `StreamContext`)
/// - `setprop debug.alvr.playzone -1`: forget the remembered playzone
/// - `setprop debug.alvr.key_leak <percent>`: narrow the rim where the background around a hand looks like
///   the hand (needs `auto_key`), 0: off, 100: on
/// - `setprop debug.alvr.key_leak_lo <percent>` / `key_leak_hi`: share of the background around the hand
///   that is kept by the key, from where the rim is narrowed to where it is narrowest
/// - `setprop debug.alvr.key_leak_inset <mm>`: the narrowest rim, inside of the hand
/// - `setprop debug.alvr.key_leak_force <percent>`: narrow the rim of both hands this much, without the camera
/// - `setprop debug.alvr.key_leak_view 1`: with the "Tint mask" debug view the narrowed rim is red. 2: the
///   right hand is replaced by spheres where the leak map says the room leaks (and the map is learned)
/// - `setprop debug.alvr.key_leak_map <percent>`: weight of the leak map (directions around the head where the
///   key showed the room), 0: off, the map is not learned
/// - `setprop debug.alvr.key_leak_map_lo <percent>` / `key_leak_map_hi`: leak of the cells behind the hand,
///   from where the rim is narrowed to where it is narrowest
/// - `setprop debug.alvr.key_leak_memory <s>`: how long the leak map remembers a direction
/// - `setprop debug.alvr.key_leak_clear <n>`: another value forgets the leak map
/// - `setprop debug.alvr.key_leak_dump <n>`: another value logs the leak map of the camera view (`ALVR_CAM`),
///   a value other than 0 also learns the map
/// - `setprop debug.alvr.key_leak_probe <cm>`: distance of the spheres of `key_leak_view 2`
/// - `setprop debug.alvr.key_lut_dump <n>`: another value writes the learned LUT to the files folder of the app
/// - `setprop debug.alvr.key_wrist_len <mm>`: length of the key zone from the wrist towards the forearm, its
///   direction is chosen with the camera (`auto_key`), 0: off
/// - `setprop debug.alvr.key_wrist_scale <percent>`: its radius, of the radius of the wrist joint
/// - `setprop debug.alvr.key_wrist_conf <percent>`: how sure the camera must be of its direction for the
///   solid fill
/// - `setprop debug.alvr.key_wrist_follow <percent>`: below this score the camera doesn't see the forearm,
///   the bridge goes back to the default direction. 0: always the best candidate
pub struct ColorKey {
    version: u32,
    lut: Vec<u8>,
    pub params: ColorKeyParams,
}

#[derive(Clone, Copy)]
pub struct ColorKeyParams {
    /// Of a hand that doesn't move
    pub zones: HandZones,
    /// How wrong the timing of the hand pose may be: for the solid core, and in the worst case
    pub core_error_s: f32,
    pub rim_error_s: f32,
    pub max_rim_growth_m: f32,
    /// Joints whose speed grows the rim, see `debug.alvr.key_speed`
    pub speed_joints: i64,
    /// The rim grows along the motion of the hand. None: in all directions
    pub sweep: Option<SweepParams>,
    pub radius_scale: f32,
    /// Negative: predict ahead
    pub passthrough_delay_ms: i64,
    /// Composition layers of the key, see `debug.alvr.key_layers`
    pub layers: i64,
    /// 0: the lower game layer is not cropped, 1: cropped, 2: cropped, y origin at the top
    pub crop: i64,
    /// Whether the GPU passes get the hands
    pub hands: bool,
    /// Narrower rim over backgrounds that look like the hand: gain (0..1), the shares of the background
    /// that is kept where it starts and where it is complete, the narrowest rim (inside of the hand), a
    /// fixed amount (0..1), and how it is shown (`debug.alvr.key_leak_view`)
    pub leak_gain: f32,
    pub leak_lo: f32,
    pub leak_hi: f32,
    pub leak_inset_m: f32,
    pub leak_force: f32,
    pub leak_view: i64,
    /// The leak map: weight (0..1, 0: not used), leak of the cells behind the hand where the narrowing
    /// starts and where it is complete, and the distance of the debug spheres
    pub leak_map: f32,
    pub leak_map_lo: f32,
    pub leak_map_hi: f32,
    pub leak_probe_m: f32,
    /// Key zone from the wrist towards the forearm. None: off
    pub wrist: Option<WristParams>,
}

/// Key zone from the wrist towards the forearm, its direction chosen by the camera
/// (`debug.alvr.key_wrist_*`)
#[derive(Clone, Copy)]
pub struct WristParams {
    pub length_m: f32,
    /// Of the radius of the wrist joint
    pub radius_scale: f32,
    /// Solid only if the camera is at least this sure of the direction (0..1)
    pub min_confidence: f32,
    /// Below this score of the best direction (0..1) the default direction is used
    pub follow: f32,
}

impl WristParams {
    /// None if it is off
    pub fn load() -> Option<Self> {
        let length_m = millimeters("debug.alvr.key_wrist_len", 80);

        (length_m > 0.0).then(|| Self {
            length_m,
            radius_scale: debug_property("debug.alvr.key_wrist_scale")
                .unwrap_or(100)
                .clamp(0, 300) as f32
                / 100.0,
            min_confidence: percent("debug.alvr.key_wrist_conf", 60),
            follow: percent("debug.alvr.key_wrist_follow", 30),
        })
    }
}

fn millimeters(property: &str, default: i64) -> f32 {
    debug_property(property).unwrap_or(default).clamp(0, 300) as f32 / 1000.0
}

// 0..1
fn percent(property: &str, default: i64) -> f32 {
    debug_property(property).unwrap_or(default).clamp(0, 100) as f32 / 100.0
}

impl ColorKeyParams {
    fn load() -> Self {
        let solid_count = debug_property("debug.alvr.key_solid")
            .unwrap_or(alvr_graphics::WRIST_SEGMENTS as i64)
            .clamp(0, alvr_graphics::HAND_SEGMENTS as i64) as usize;
        let mut solid = [0.0; alvr_graphics::HAND_SEGMENTS];
        solid[..solid_count].fill(1.0);

        Self {
            zones: HandZones {
                core: -millimeters("debug.alvr.key_inset", 4),
                falloff: millimeters("debug.alvr.key_falloff", 4),
                rim: millimeters("debug.alvr.key_rim", 0),
                solid,
                sweep: HandSweep::NONE,
            },
            // Milliseconds
            core_error_s: millimeters("debug.alvr.key_error", 10),
            rim_error_s: millimeters("debug.alvr.key_rim_error", 45),
            max_rim_growth_m: millimeters("debug.alvr.key_rim_max", 16),
            speed_joints: debug_property("debug.alvr.key_speed").unwrap_or(2),
            sweep: (debug_property("debug.alvr.key_sweep").unwrap_or(1) != 0).then(|| {
                SweepParams {
                    max: millimeters("debug.alvr.key_sweep_max", 40),
                    iso: percent("debug.alvr.key_sweep_iso", 25),
                    bias: debug_property("debug.alvr.key_sweep_bias")
                        .unwrap_or(0)
                        .clamp(-100, 100) as f32
                        / 100.0,
                }
            }),
            radius_scale: debug_property("debug.alvr.key_scale")
                .unwrap_or(100)
                .clamp(0, 300) as f32
                / 100.0,
            passthrough_delay_ms: debug_property("debug.alvr.key_delay")
                .unwrap_or(0)
                .clamp(-100, 200),
            layers: debug_property("debug.alvr.key_layers").unwrap_or(4),
            crop: debug_property("debug.alvr.key_crop").unwrap_or(1),
            hands: debug_property("debug.alvr.key_hands").unwrap_or(1) != 0,
            leak_gain: percent("debug.alvr.key_leak", 0),
            leak_lo: percent("debug.alvr.key_leak_lo", 5),
            leak_hi: percent("debug.alvr.key_leak_hi", 20),
            leak_inset_m: millimeters("debug.alvr.key_leak_inset", 3),
            leak_force: percent("debug.alvr.key_leak_force", 0),
            leak_view: debug_property("debug.alvr.key_leak_view").unwrap_or(0),
            leak_map: percent("debug.alvr.key_leak_map", 0),
            leak_map_lo: percent("debug.alvr.key_leak_map_lo", 3),
            leak_map_hi: percent("debug.alvr.key_leak_map_hi", 15),
            leak_probe_m: debug_property("debug.alvr.key_leak_probe")
                .unwrap_or(60)
                .clamp(10, 500) as f32
                / 100.0,
            wrist: WristParams::load(),
        }
    }

    /// Whether the frame has the lower game layer and the keyed passthrough (`debug.alvr.key_layers`).
    /// Otherwise only one game layer: the lower one if it is not cropped, else the upper one.
    pub fn extra_layers(&self, hands_in_view: bool) -> bool {
        match self.layers {
            2 => false,
            5 | 30 | 31 => true,
            _ => hands_in_view,
        }
    }
}

const COLOR_KEY_PATHS: [&str; 2] = [
    "/data/local/tmp/alvr_color_key.lut",
    "/sdcard/Android/data/alvr.client.dev/files/alvr_color_key.lut",
];

pub struct ColorKeyLoader {
    key: Option<ColorKey>,
    last_version: u32,
    last_poll: Instant,
}

impl ColorKeyLoader {
    pub fn new() -> Self {
        Self {
            key: None,
            last_version: 0,
            last_poll: Instant::now(),
        }
    }

    pub fn poll(&mut self) -> Option<&ColorKey> {
        if self.last_poll.elapsed() > Duration::from_millis(500) {
            self.last_poll = Instant::now();

            let version = debug_property("debug.alvr.color_key")
                .and_then(|version| u32::try_from(version).ok())
                .unwrap_or(0);
            if version != self.last_version {
                self.last_version = version;
                self.key = (version != 0).then(|| load_color_key(version)).flatten();
            }
            if let Some(key) = &mut self.key {
                key.params = ColorKeyParams::load();
            }
        }

        self.key.as_ref()
    }
}

fn load_color_key(version: u32) -> Option<ColorKey> {
    for path in COLOR_KEY_PATHS {
        match fs::read(path) {
            Ok(lut) if lut.len() == COLOR_KEY_SIZE => {
                return Some(ColorKey {
                    version,
                    lut,
                    params: ColorKeyParams::load(),
                });
            }
            Ok(lut) => error!("Color key {path}: unexpected size {}", lut.len()),
            Err(e) => error!("Color key {path}: {e}"),
        }
    }

    None
}

/// Integer Android system property. `debug.*` properties can be set with adb.
#[allow(unused_variables)]
pub fn debug_property(name: &str) -> Option<i64> {
    #[cfg(target_os = "android")]
    {
        let name = std::ffi::CString::new(name).unwrap();
        let mut value = [0_u8; libc::PROP_VALUE_MAX as usize];
        let length =
            unsafe { libc::__system_property_get(name.as_ptr(), value.as_mut_ptr().cast()) };

        String::from_utf8_lossy(&value[..length.max(0) as usize])
            .trim()
            .parse()
            .ok()
    }
    #[cfg(not(target_os = "android"))]
    None
}

impl<'a> Deref for PassthroughLayer<'a> {
    type Target = xr::CompositionLayerBase<'a, xr::OpenGlEs>;

    fn deref(&self) -> &Self::Target {
        if let Some(handle) = &self.handle_fb {
            unsafe { &*ptr::from_ref(handle.layer()).cast() }
        } else if let Some(handle) = &self.handle_htc {
            unsafe { &*ptr::from_ref(handle.layer()).cast() }
        } else {
            panic!("No passthrough extension available");
        }
    }
}
