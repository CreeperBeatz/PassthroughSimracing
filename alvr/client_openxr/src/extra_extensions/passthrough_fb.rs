use alvr_system_info::Platform;
use openxr::{
    self as xr, raw,
    sys::{self, Handle},
};
use std::ptr;

/// Experiment: does the compositor use the alpha channel of passthrough color LUTs? If so, the
/// passthrough can be color keyed without any added latency. Selected at runtime with
/// `adb shell setprop debug.alvr.lut_test <n>`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LutTest {
    Off,
    /// 1: passthrough below the game, bright colors get alpha 0 and keep their RGB
    Underlay,
    /// 2: like `Underlay` but with the passthrough above the game
    Overlay,
    /// 3: control, bright colors are inverted and opaque: shows if the LUT is applied at all
    Control,
    /// 4: like `Overlay` but RGB is also 0 (premultiplied alpha)
    OverlayPremultiplied,
}

impl LutTest {
    pub fn from_index(index: u32) -> Self {
        match index {
            1 => Self::Underlay,
            2 => Self::Overlay,
            3 => Self::Control,
            4 => Self::OverlayPremultiplied,
            _ => Self::Off,
        }
    }

    pub fn passthrough_on_top(self) -> bool {
        matches!(self, Self::Overlay | Self::OverlayPremultiplied)
    }

    fn lut_data(self, resolution: u32) -> Vec<u8> {
        let mut data = Vec::with_capacity((resolution * resolution * resolution * 4) as usize);
        let value = |i: u32| (i * 255 / (resolution - 1)) as u8;
        for b in 0..resolution {
            for g in 0..resolution {
                for r in 0..resolution {
                    let rgb = [value(r), value(g), value(b)];
                    let bright = rgb.iter().map(|c| u32::from(*c)).sum::<u32>() > 3 * 110;
                    let texel = match self {
                        Self::Underlay | Self::Overlay if bright => [rgb[0], rgb[1], rgb[2], 0],
                        Self::OverlayPremultiplied if bright => [0, 0, 0, 0],
                        Self::Control if bright => [255 - rgb[0], 255 - rgb[1], 255 - rgb[2], 255],
                        _ => [rgb[0], rgb[1], rgb[2], 255],
                    };
                    data.extend(texel);
                }
            }
        }
        data
    }
}

pub struct PassthroughFB {
    handle: sys::PassthroughFB,
    layer_handle: sys::PassthroughLayerFB,
    layer: sys::CompositionLayerPassthroughFB,
    ext_fns: raw::PassthroughFB,
    lut_fns: Option<raw::PassthroughColorLutMETA>,
    lut: sys::PassthroughColorLutMETA,
}

impl PassthroughFB {
    pub fn new(session: &xr::Session<xr::OpenGlEs>, platform: Platform) -> xr::Result<Self> {
        let ext_fns = session
            .instance()
            .exts()
            .fb_passthrough
            .ok_or(sys::Result::ERROR_EXTENSION_NOT_PRESENT)?;

        let mut handle = sys::PassthroughFB::NULL;
        let info = sys::PassthroughCreateInfoFB {
            ty: sys::PassthroughCreateInfoFB::TYPE,
            next: ptr::null(),
            flags: sys::PassthroughFlagsFB::IS_RUNNING_AT_CREATION,
        };
        unsafe {
            super::xr_res((ext_fns.create_passthrough)(
                session.as_raw(),
                &info,
                &mut handle,
            ))?
        };

        let mut layer_handle = sys::PassthroughLayerFB::NULL;
        let info = sys::PassthroughLayerCreateInfoFB {
            ty: sys::PassthroughLayerCreateInfoFB::TYPE,
            next: ptr::null(),
            passthrough: handle,
            flags: sys::PassthroughFlagsFB::IS_RUNNING_AT_CREATION,
            purpose: sys::PassthroughLayerPurposeFB::RECONSTRUCTION,
        };
        unsafe {
            super::xr_res((ext_fns.create_passthrough_layer)(
                session.as_raw(),
                &info,
                &mut layer_handle,
            ))?
        };

        let layer = sys::CompositionLayerPassthroughFB {
            ty: sys::CompositionLayerPassthroughFB::TYPE,
            next: ptr::null(),
            flags: xr::CompositionLayerFlags::BLEND_TEXTURE_SOURCE_ALPHA,
            space: sys::Space::NULL,
            layer_handle,
        };

        // HACK: YVR runtime seems to ignore IS_RUNNING_AT_CREATION on versions <= 3.0.1
        if platform.is_yvr() {
            unsafe { super::xr_res((ext_fns.passthrough_start)(handle))? };
        }

        Ok(Self {
            handle,
            layer_handle,
            layer,
            ext_fns,
            lut_fns: session.instance().exts().meta_passthrough_color_lut,
            lut: sys::PassthroughColorLutMETA::NULL,
        })
    }

    pub fn set_lut_test(&mut self, test: LutTest) -> xr::Result<()> {
        const RESOLUTION: u32 = 32;

        let lut_fns = self
            .lut_fns
            .ok_or(sys::Result::ERROR_EXTENSION_NOT_PRESENT)?;

        let mut new_lut = sys::PassthroughColorLutMETA::NULL;
        if test != LutTest::Off {
            let data = test.lut_data(RESOLUTION);
            let info = sys::PassthroughColorLutCreateInfoMETA {
                ty: sys::PassthroughColorLutCreateInfoMETA::TYPE,
                next: ptr::null(),
                channels: sys::PassthroughColorLutChannelsMETA::RGBA,
                resolution: RESOLUTION,
                data: sys::PassthroughColorLutDataMETA {
                    buffer_size: data.len() as u32,
                    buffer: data.as_ptr(),
                },
            };
            unsafe {
                super::xr_res((lut_fns.create_passthrough_color_lut)(
                    self.handle,
                    &info,
                    &mut new_lut,
                ))?;
            }
        }

        let color_map = sys::PassthroughColorMapLutMETA {
            ty: sys::PassthroughColorMapLutMETA::TYPE,
            next: ptr::null(),
            color_lut: new_lut,
            weight: 1.0,
        };
        let style = sys::PassthroughStyleFB {
            ty: sys::PassthroughStyleFB::TYPE,
            next: if test == LutTest::Off {
                ptr::null()
            } else {
                (&raw const color_map).cast()
            },
            texture_opacity_factor: 1.0,
            edge_color: sys::Color4f::default(),
        };
        let result = unsafe {
            super::xr_res((self.ext_fns.passthrough_layer_set_style)(
                self.layer_handle,
                &style,
            ))
        };

        // The style doesn't reference the old LUT anymore, unless setting it failed
        let unused_lut = if result.is_ok() {
            std::mem::replace(&mut self.lut, new_lut)
        } else {
            new_lut
        };
        if unused_lut != sys::PassthroughColorLutMETA::NULL {
            unsafe { (lut_fns.destroy_passthrough_color_lut)(unused_lut) };
        }

        result
    }

    // return reference to make sure the passthrough handle is not dropped while the layer is in use
    pub fn layer(&self) -> &sys::CompositionLayerPassthroughFB {
        &self.layer
    }
}

impl Drop for PassthroughFB {
    fn drop(&mut self) {
        unsafe {
            if let Some(lut_fns) = &self.lut_fns
                && self.lut != sys::PassthroughColorLutMETA::NULL
            {
                (lut_fns.destroy_passthrough_color_lut)(self.lut);
            }
            (self.ext_fns.destroy_passthrough_layer)(self.layer_handle);
            (self.ext_fns.destroy_passthrough)(self.handle);
        }
    }
}
