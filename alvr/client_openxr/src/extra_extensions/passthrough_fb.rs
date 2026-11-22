use alvr_system_info::Platform;
use openxr::{
    self as xr, raw,
    sys::{self, Handle},
};
use std::ptr;

/// Size of the color key LUT: RGBA, indexed by the passthrough color as `[b][g][r]`
pub const COLOR_KEY_RESOLUTION: u32 = 32;
pub const COLOR_KEY_SIZE: usize = (COLOR_KEY_RESOLUTION.pow(3) * 4) as usize;

pub struct PassthroughFB {
    handle: sys::PassthroughFB,
    layer_handle: sys::PassthroughLayerFB,
    layer: sys::CompositionLayerPassthroughFB,
    ext_fns: raw::PassthroughFB,
    lut_fns: Option<raw::PassthroughColorLutMETA>,
    key: Option<KeyedLayer>,
    session: sys::Session,
}

struct KeyedLayer {
    layer_handle: sys::PassthroughLayerFB,
    layer: sys::CompositionLayerPassthroughFB,
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
            key: None,
            session: session.as_raw(),
        })
    }

    /// Create, replace or remove a second passthrough layer that is color keyed with the alpha
    /// channel of a LUT (`COLOR_KEY_SIZE` bytes). The key is applied by the compositor to the image
    /// it displays, so it doesn't lag behind it.
    pub fn set_color_key(&mut self, data: Option<&[u8]>) -> xr::Result<()> {
        let lut_fns = self
            .lut_fns
            .ok_or(sys::Result::ERROR_EXTENSION_NOT_PRESENT)?;

        self.destroy_color_key();

        let Some(data) = data else {
            return Ok(());
        };
        if data.len() != COLOR_KEY_SIZE {
            return Err(sys::Result::ERROR_VALIDATION_FAILURE);
        }

        let mut lut = sys::PassthroughColorLutMETA::NULL;
        let info = sys::PassthroughColorLutCreateInfoMETA {
            ty: sys::PassthroughColorLutCreateInfoMETA::TYPE,
            next: ptr::null(),
            channels: sys::PassthroughColorLutChannelsMETA::RGBA,
            resolution: COLOR_KEY_RESOLUTION,
            data: sys::PassthroughColorLutDataMETA {
                buffer_size: data.len() as u32,
                buffer: data.as_ptr(),
            },
        };
        unsafe {
            super::xr_res((lut_fns.create_passthrough_color_lut)(
                self.handle,
                &info,
                &mut lut,
            ))?;
        }

        let mut layer_handle = sys::PassthroughLayerFB::NULL;
        let info = sys::PassthroughLayerCreateInfoFB {
            ty: sys::PassthroughLayerCreateInfoFB::TYPE,
            next: ptr::null(),
            passthrough: self.handle,
            flags: sys::PassthroughFlagsFB::IS_RUNNING_AT_CREATION,
            purpose: sys::PassthroughLayerPurposeFB::RECONSTRUCTION,
        };
        let result = unsafe {
            super::xr_res((self.ext_fns.create_passthrough_layer)(
                self.session,
                &info,
                &mut layer_handle,
            ))
        };
        if let Err(e) = result {
            unsafe { (lut_fns.destroy_passthrough_color_lut)(lut) };
            return Err(e);
        }

        // From here on, destroy_color_key() takes care of the cleanup
        self.key = Some(KeyedLayer {
            layer_handle,
            layer: sys::CompositionLayerPassthroughFB {
                layer_handle,
                ..self.layer
            },
            lut,
        });

        let color_map = sys::PassthroughColorMapLutMETA {
            ty: sys::PassthroughColorMapLutMETA::TYPE,
            next: ptr::null(),
            color_lut: lut,
            weight: 1.0,
        };
        let style = sys::PassthroughStyleFB {
            ty: sys::PassthroughStyleFB::TYPE,
            next: (&raw const color_map).cast(),
            texture_opacity_factor: 1.0,
            edge_color: sys::Color4f::default(),
        };
        let result = unsafe {
            super::xr_res((self.ext_fns.passthrough_layer_set_style)(
                layer_handle,
                &style,
            ))
        };
        if result.is_err() {
            self.destroy_color_key();
        }

        result
    }

    fn destroy_color_key(&mut self) {
        if let Some(key) = self.key.take() {
            unsafe {
                (self.ext_fns.destroy_passthrough_layer)(key.layer_handle);
                if let Some(lut_fns) = &self.lut_fns {
                    (lut_fns.destroy_passthrough_color_lut)(key.lut);
                }
            }
        }
    }

    pub fn color_key_layer(&self) -> Option<&sys::CompositionLayerPassthroughFB> {
        self.key.as_ref().map(|key| &key.layer)
    }

    // return reference to make sure the passthrough handle is not dropped while the layer is in use
    pub fn layer(&self) -> &sys::CompositionLayerPassthroughFB {
        &self.layer
    }
}

impl Drop for PassthroughFB {
    fn drop(&mut self) {
        self.destroy_color_key();

        unsafe {
            (self.ext_fns.destroy_passthrough_layer)(self.layer_handle);
            (self.ext_fns.destroy_passthrough)(self.handle);
        }
    }
}
