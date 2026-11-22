use crate::extra_extensions::{COLOR_KEY_SIZE, PassthroughFB, PassthroughHTC};
use alvr_common::{
    anyhow::{Result, bail},
    error,
};
use alvr_graphics::HandZones;
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
/// - `setprop debug.alvr.auto_key 1` before starting the app: learn the colors with the camera
/// - `setprop debug.alvr.key_scale <percent>`: thickness of the hands
/// - `setprop debug.alvr.key_delay <ms>`: how late (may be negative) the passthrough image is, to locate the hands at
///   the moment it shows
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
    pub radius_scale: f32,
    /// Negative: predict ahead
    pub passthrough_delay_ms: i64,
}

fn millimeters(property: &str, default: i64) -> f32 {
    debug_property(property).unwrap_or(default).clamp(0, 300) as f32 / 1000.0
}

impl ColorKeyParams {
    fn load() -> Self {
        Self {
            zones: HandZones {
                core: -millimeters("debug.alvr.key_inset", 4),
                falloff: millimeters("debug.alvr.key_falloff", 8),
                rim: millimeters("debug.alvr.key_rim", 6),
            },
            // Milliseconds
            core_error_s: millimeters("debug.alvr.key_error", 10),
            rim_error_s: millimeters("debug.alvr.key_rim_error", 45),
            radius_scale: debug_property("debug.alvr.key_scale")
                .unwrap_or(100)
                .clamp(0, 300) as f32
                / 100.0,
            passthrough_delay_ms: debug_property("debug.alvr.key_delay")
                .unwrap_or(20)
                .clamp(-100, 200),
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
