use crate::extra_extensions::{COLOR_KEY_SIZE, PassthroughFB, PassthroughHTC};
use alvr_common::{
    anyhow::{Result, bail},
    error,
};
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
                Ok(()) => self.color_key_version = version,
                Err(e) => error!("Passthrough color key: {e}"),
            }
        }

        self.color_key_version != 0
    }
}

/// Experimental passthrough color key, controlled with adb:
/// - push a LUT (see `COLOR_KEY_SIZE`) to one of `COLOR_KEY_PATHS`
/// - `setprop debug.alvr.color_key <n>`: 0 disables the key, another value (re)loads the LUT
/// - `setprop debug.alvr.key_growth <n>`: size of the focus area border, 1/1000 of the view
pub struct ColorKey {
    version: u32,
    lut: Vec<u8>,
    pub growth: f32,
}

const COLOR_KEY_PATHS: [&str; 2] = [
    "/data/local/tmp/alvr_color_key.lut",
    "/sdcard/Android/data/alvr.client.dev/files/alvr_color_key.lut",
];
const DEFAULT_KEY_GROWTH: u32 = 60;

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

            let version = debug_property("debug.alvr.color_key").unwrap_or(0);
            if version != self.last_version {
                self.last_version = version;
                self.key = (version != 0).then(|| load_color_key(version)).flatten();
            }
            if let Some(key) = &mut self.key {
                let growth = debug_property("debug.alvr.key_growth").unwrap_or(DEFAULT_KEY_GROWTH);
                key.growth = growth.min(500) as f32 / 1000.0;
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
                    growth: DEFAULT_KEY_GROWTH as f32 / 1000.0,
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
fn debug_property(name: &str) -> Option<u32> {
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
