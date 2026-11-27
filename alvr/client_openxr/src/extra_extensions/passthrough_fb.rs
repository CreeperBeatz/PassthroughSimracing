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
    // `debug.alvr.green_boxes`: the base layer keyed by monitor green, see `set_green_key`
    green: Option<GreenLayer>,
    // `debug.alvr.lut_probe` (see `LutProbe`), and the alpha scale / bias chained to the hand-key layer
    // for values 3 and 4 (boxed: the layer's `next` points at it)
    probe: LutProbe,
    probe_alpha: Box<sys::CompositionLayerColorScaleBiasKHR>,
    session: sys::Session,
}

/// `debug.alvr.lut_probe` (design/green-hand-one-lut-20261127.md, stage 0): what the Quest does with
/// colour LUTs on more than one passthrough layer. Unset or 0: off (today's layers)
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum LutProbe {
    #[default]
    Off,
    /// 1: the base layer is styled with the hand-key layer's own LUT object (green off)
    Share,
    /// 2: the hand-key layer is submitted at the base position too, the plain base is left out (green off)
    Twin,
    /// 3: the hand-key layer gets alpha scale 0.5 (XR_KHR_composition_layer_color_scale_bias)
    Scale,
    /// 4: the hand-key layer gets alpha bias -0.5
    Bias,
    /// 5: the hand-key layer is styled again after the green-keyed base (green on, `green_layer` 1)
    Restyle,
}

impl LutProbe {
    pub fn from_property(value: Option<i64>) -> Self {
        match value {
            Some(1) => Self::Share,
            Some(2) => Self::Twin,
            Some(3) => Self::Scale,
            Some(4) => Self::Bias,
            Some(5) => Self::Restyle,
            _ => Self::Off,
        }
    }
}

struct KeyedLayer {
    layer_handle: sys::PassthroughLayerFB,
    layer: sys::CompositionLayerPassthroughFB,
    lut: sys::PassthroughColorLutMETA,
}

/// How the green-keyed base layer is made (`debug.alvr.green_layer`). Seen on the Quest 3
/// (2026-11-26): with a layer of its own (`Separate`) the green stays opaque while the colour keyed
/// layer of the hands is submitted too, and is keyed once that layer is left out
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum GreenLayerMode {
    /// The LUT is the style of the base layer itself: two passthrough layers, as without the zone
    #[default]
    InPlace,
    /// A third passthrough layer with the LUT, submitted instead of the plain base layer
    Separate,
    /// As `Separate`, and the plain base layer is paused while the third one exists
    SeparatePaused,
}

impl GreenLayerMode {
    /// `debug.alvr.green_layer`: unset or 1 in place, 0 a layer of its own, 2 that with the plain
    /// layer paused
    pub fn from_property(value: Option<i64>) -> Self {
        match value {
            Some(0) => Self::Separate,
            Some(2) => Self::SeparatePaused,
            _ => Self::InPlace,
        }
    }
}

enum GreenLayer {
    InPlace {
        lut: sys::PassthroughColorLutMETA,
    },
    Separate {
        keyed: KeyedLayer,
        base_paused: bool,
    },
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
            green: None,
            probe: LutProbe::Off,
            probe_alpha: Box::new(sys::CompositionLayerColorScaleBiasKHR {
                ty: sys::CompositionLayerColorScaleBiasKHR::TYPE,
                next: ptr::null(),
                color_scale: sys::Color4f {
                    r: 1.0,
                    g: 1.0,
                    b: 1.0,
                    a: 1.0,
                },
                color_bias: sys::Color4f::default(),
            }),
            session: session.as_raw(),
        })
    }

    /// Create, replace or remove a second passthrough layer that is color keyed with the alpha
    /// channel of a LUT (`COLOR_KEY_SIZE` bytes). The key is applied by the compositor to the image
    /// it displays, so it doesn't lag behind it.
    pub fn set_color_key(&mut self, data: Option<&[u8]>) -> xr::Result<()> {
        self.lut_fns
            .ok_or(sys::Result::ERROR_EXTENSION_NOT_PRESENT)?;

        // The base layer must not refer to the LUT about to be destroyed (probe 1)
        self.undo_probe();
        self.destroy_color_key();

        let Some(data) = data else {
            return Ok(());
        };
        self.key = Some(self.create_lut_layer(data)?);
        self.apply_probe();

        Ok(())
    }

    /// `debug.alvr.lut_probe`: switch to this probe (undoes the one before). Returns a text for the log
    pub fn set_lut_probe(&mut self, probe: LutProbe) -> String {
        self.undo_probe();
        self.probe = probe;
        self.apply_probe()
    }

    pub fn lut_probe(&self) -> LutProbe {
        self.probe
    }

    // Puts the probe's state on the layers; probes that need the hand-key layer wait for it
    fn apply_probe(&mut self) -> String {
        let Some(key) = &mut self.key else {
            return format!("{:?}: waiting for the hand-key layer", self.probe);
        };
        let (key_handle, key_lut) = (key.layer_handle, key.lut);
        let text = match self.probe {
            LutProbe::Off => "off".to_string(),
            LutProbe::Share if self.green.is_some() => "share: not with green on".into(),
            LutProbe::Share => match self.set_layer_style(self.layer_handle, Some(key_lut)) {
                Ok(()) => "share: base layer styled with the hand-key LUT".into(),
                Err(e) => format!("share: styling the base layer failed: {e}"),
            },
            LutProbe::Twin => "twin: the hand-key layer is submitted as the base too".into(),
            LutProbe::Scale | LutProbe::Bias => {
                let (scale, bias) = if self.probe == LutProbe::Scale {
                    (0.5, 0.0)
                } else {
                    (1.0, -0.5)
                };
                self.probe_alpha.color_scale.a = scale;
                self.probe_alpha.color_bias.a = bias;
                let key = self.key.as_mut().unwrap();
                key.layer.next = ptr::from_ref(&*self.probe_alpha).cast();
                format!("alpha scale {scale} bias {bias} on the hand-key layer")
            }
            LutProbe::Restyle => match self.set_layer_style(key_handle, Some(key_lut)) {
                Ok(()) => format!(
                    "restyle: hand-key layer styled again (green {})",
                    if self.green.is_some() {
                        "on"
                    } else {
                        "off: set it"
                    }
                ),
                Err(e) => format!("restyle failed: {e}"),
            },
        };
        text
    }

    fn undo_probe(&mut self) {
        match self.probe {
            LutProbe::Share if self.green.is_none() && self.key.is_some() => {
                self.set_layer_style(self.layer_handle, None).ok();
            }
            LutProbe::Scale | LutProbe::Bias => {
                if let Some(key) = &mut self.key {
                    key.layer.next = ptr::null();
                }
            }
            _ => (),
        }
    }

    /// Probe 2: the layer submitted at the base position (the hand-key layer), else None
    pub fn twin_layer(&self) -> Option<&sys::CompositionLayerPassthroughFB> {
        (self.probe == LutProbe::Twin && self.green.is_none())
            .then(|| self.color_key_layer())
            .flatten()
    }

    /// Replace the LUT of the color keyed layer, if it exists
    pub fn update_color_key(&mut self, data: &[u8]) -> xr::Result<()> {
        match &self.key {
            Some(key) => self.update_lut(key.lut, data),
            None => Ok(()),
        }
    }

    fn destroy_color_key(&mut self) {
        if let Some(key) = self.key.take() {
            self.destroy_lut_layer(key);
        }
    }

    /// `debug.alvr.green_boxes` (green_screen.rs): create, replace or remove the green-keyed base layer:
    /// the base passthrough with a colour LUT style (`COLOR_KEY_SIZE` bytes), made as `mode` says.
    /// Without it the base layer is plain, exactly as before the zone
    pub fn set_green_key(&mut self, data: Option<&[u8]>, mode: GreenLayerMode) -> xr::Result<()> {
        let lut_fns = self
            .lut_fns
            .ok_or(sys::Result::ERROR_EXTENSION_NOT_PRESENT)?;

        self.undo_probe();
        self.destroy_green_key();

        let Some(data) = data else {
            self.apply_probe();
            return Ok(());
        };
        self.green = Some(match mode {
            GreenLayerMode::InPlace => {
                let lut = self.create_lut(data)?;
                if let Err(e) = self.set_layer_style(self.layer_handle, Some(lut)) {
                    unsafe { (lut_fns.destroy_passthrough_color_lut)(lut) };
                    return Err(e);
                }
                GreenLayer::InPlace { lut }
            }
            GreenLayerMode::Separate | GreenLayerMode::SeparatePaused => {
                let keyed = self.create_lut_layer(data)?;
                // A failed pause leaves the plain layer running, as in `Separate`
                let base_paused = mode == GreenLayerMode::SeparatePaused
                    && unsafe {
                        super::xr_res((self.ext_fns.passthrough_layer_pause)(self.layer_handle))
                    }
                    .is_ok();
                GreenLayer::Separate { keyed, base_paused }
            }
        });
        self.apply_probe();

        Ok(())
    }

    /// Replace the LUT of the green-keyed base layer in place, if it exists
    pub fn update_green_key(&mut self, data: &[u8]) -> xr::Result<()> {
        match &self.green {
            Some(GreenLayer::InPlace { lut }) => self.update_lut(*lut, data),
            Some(GreenLayer::Separate { keyed, .. }) => self.update_lut(keyed.lut, data),
            None => Ok(()),
        }
    }

    fn destroy_green_key(&mut self) {
        match self.green.take() {
            Some(GreenLayer::InPlace { lut }) => {
                // The base layer goes back to no colour mapping. A runtime that kept applying the LUT
                // of a style it no longer has would keep keying green: the table is made opaque first
                self.update_lut(lut, &opaque_identity_lut()).ok();
                self.set_layer_style(self.layer_handle, None).ok();
                if let Some(lut_fns) = &self.lut_fns {
                    unsafe { (lut_fns.destroy_passthrough_color_lut)(lut) };
                }
            }
            Some(GreenLayer::Separate { keyed, base_paused }) => {
                if base_paused {
                    unsafe { (self.ext_fns.passthrough_layer_resume)(self.layer_handle) };
                }
                self.destroy_lut_layer(keyed);
            }
            None => (),
        }
    }

    /// The green-keyed base layer, while it exists
    pub fn green_layer(&self) -> Option<&sys::CompositionLayerPassthroughFB> {
        match &self.green {
            Some(GreenLayer::InPlace { .. }) => Some(&self.layer),
            Some(GreenLayer::Separate { keyed, .. }) => Some(&keyed.layer),
            None => None,
        }
    }

    fn create_lut(&self, data: &[u8]) -> xr::Result<sys::PassthroughColorLutMETA> {
        let lut_fns = self
            .lut_fns
            .ok_or(sys::Result::ERROR_EXTENSION_NOT_PRESENT)?;
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

        Ok(lut)
    }

    /// The style of a passthrough layer: mapped by this colour LUT, or (None) no colour mapping
    fn set_layer_style(
        &self,
        layer_handle: sys::PassthroughLayerFB,
        lut: Option<sys::PassthroughColorLutMETA>,
    ) -> xr::Result<()> {
        let color_map = lut.map(|color_lut| sys::PassthroughColorMapLutMETA {
            ty: sys::PassthroughColorMapLutMETA::TYPE,
            next: ptr::null(),
            color_lut,
            weight: 1.0,
        });
        let style = sys::PassthroughStyleFB {
            ty: sys::PassthroughStyleFB::TYPE,
            next: color_map
                .as_ref()
                .map_or(ptr::null(), |color_map| ptr::from_ref(color_map).cast()),
            texture_opacity_factor: 1.0,
            edge_color: sys::Color4f::default(),
        };

        unsafe {
            super::xr_res((self.ext_fns.passthrough_layer_set_style)(
                layer_handle,
                &style,
            ))
        }
    }

    /// A new running passthrough layer on the passthrough of this one, styled with a new colour LUT made
    /// from `data`, and its composition struct (the flags of the base layer). Nothing is left on error
    fn create_lut_layer(&self, data: &[u8]) -> xr::Result<KeyedLayer> {
        let lut_fns = self
            .lut_fns
            .ok_or(sys::Result::ERROR_EXTENSION_NOT_PRESENT)?;
        let lut = self.create_lut(data)?;

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

        // From here on, destroy_lut_layer() takes care of the cleanup
        let keyed = KeyedLayer {
            layer_handle,
            layer: sys::CompositionLayerPassthroughFB {
                layer_handle,
                ..self.layer
            },
            lut,
        };

        if let Err(e) = self.set_layer_style(layer_handle, Some(lut)) {
            self.destroy_lut_layer(keyed);
            return Err(e);
        }

        Ok(keyed)
    }

    fn update_lut(&self, lut: sys::PassthroughColorLutMETA, data: &[u8]) -> xr::Result<()> {
        let Some(lut_fns) = &self.lut_fns else {
            return Ok(());
        };
        if data.len() != COLOR_KEY_SIZE {
            return Err(sys::Result::ERROR_VALIDATION_FAILURE);
        }

        let info = sys::PassthroughColorLutUpdateInfoMETA {
            ty: sys::PassthroughColorLutUpdateInfoMETA::TYPE,
            next: ptr::null(),
            data: sys::PassthroughColorLutDataMETA {
                buffer_size: data.len() as u32,
                buffer: data.as_ptr(),
            },
        };

        unsafe { super::xr_res((lut_fns.update_passthrough_color_lut)(lut, &info)) }
    }

    // The layer first: its style refers to the LUT
    fn destroy_lut_layer(&self, layer: KeyedLayer) {
        unsafe {
            (self.ext_fns.destroy_passthrough_layer)(layer.layer_handle);
            if let Some(lut_fns) = &self.lut_fns {
                (lut_fns.destroy_passthrough_color_lut)(layer.lut);
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

/// A LUT that changes nothing: every colour itself, opaque (`[b][g][r]`, RGBA)
fn opaque_identity_lut() -> Vec<u8> {
    let n = COLOR_KEY_RESOLUTION as usize;
    let step = |index: usize| (index * 255 / (n - 1)) as u8;
    let mut lut = Vec::with_capacity(COLOR_KEY_SIZE);
    for cell in 0..n * n * n {
        lut.extend_from_slice(&[
            step(cell % n),
            step(cell / n % n),
            step(cell / (n * n)),
            255,
        ]);
    }

    lut
}

impl Drop for PassthroughFB {
    fn drop(&mut self) {
        self.destroy_color_key();
        self.destroy_green_key();

        unsafe {
            (self.ext_fns.destroy_passthrough_layer)(self.layer_handle);
            (self.ext_fns.destroy_passthrough)(self.handle);
        }
    }
}
