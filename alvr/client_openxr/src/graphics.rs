use alvr_common::glam::UVec2;
use alvr_graphics::GraphicsContext;
use alvr_session::ClientsidePostProcessingConfig;
use openxr as xr;
use std::ptr;

#[allow(unused)]
pub fn session_create_info(ctx: &GraphicsContext) -> xr::opengles::SessionCreateInfo {
    #[cfg(target_os = "android")]
    {
        xr::opengles::SessionCreateInfo::Android {
            display: ctx.egl_display.as_ptr(),
            config: ctx.egl_config.as_ptr(),
            context: ctx.egl_context.as_ptr(),
        }
    }
    #[cfg(not(target_os = "android"))]
    unimplemented!()
}

pub fn swapchain_format(
    gfx_ctx: &GraphicsContext,
    session: &xr::Session<xr::OpenGlEs>,
    enable_hdr: bool,
) -> u32 {
    gfx_ctx.make_current();

    let formats = session.enumerate_swapchain_formats().unwrap();
    alvr_graphics::choose_swapchain_format(&formats, enable_hdr)
}

#[allow(unused_variables)]
pub fn create_swapchain(
    session: &xr::Session<xr::OpenGlEs>,
    gfx_ctx: &GraphicsContext,
    resolution: UVec2,
    format: u32,
    foveation: Option<&xr::FoveationProfileFB>,
) -> xr::Swapchain<xr::OpenGlEs> {
    gfx_ctx.make_current();

    let swapchain_info = xr::SwapchainCreateInfo {
        create_flags: xr::SwapchainCreateFlags::EMPTY,
        usage_flags: xr::SwapchainUsageFlags::COLOR_ATTACHMENT | xr::SwapchainUsageFlags::SAMPLED,
        format,
        sample_count: 1,
        width: resolution.x,
        height: resolution.y,
        face_count: 1,
        array_size: 1,
        mip_count: 1,
    };

    session.create_swapchain(&swapchain_info).unwrap()
}

pub struct ProjectionLayerAlphaConfig {
    pub premultiplied: bool,
}

// This is needed to work around lifetime limitations. Deref cannot be implemented because there are
// nested references, and in a way or the other I would get `cannot return reference to temporary
// value`
pub struct ProjectionLayerBuilder<'a> {
    reference_space: &'a xr::Space,
    layers: [xr::CompositionLayerProjectionView<'a, xr::OpenGlEs>; 2],
    // The lower color key layer may only show the part with the hands
    lower_key_layers: Option<[xr::CompositionLayerProjectionView<'a, xr::OpenGlEs>; 2]>,
    alpha: Option<ProjectionLayerAlphaConfig>,
    composition_layer_settings: Option<xr::sys::CompositionLayerSettingsFB>,
    lower_key_alpha: xr::sys::CompositionLayerColorScaleBiasKHR,
    upper_key_alpha: xr::sys::CompositionLayerColorScaleBiasKHR,
}

impl<'a> ProjectionLayerBuilder<'a> {
    pub fn new(
        reference_space: &'a xr::Space,
        layers: [xr::CompositionLayerProjectionView<'a, xr::OpenGlEs>; 2],
        alpha: Option<ProjectionLayerAlphaConfig>,
        clientside_post_processing_config: Option<ClientsidePostProcessingConfig>,
    ) -> Self {
        let composition_layer_settings = clientside_post_processing_config
            .map(|post_processing| {
                xr::CompositionLayerSettingsFlagsFB::from_raw(
                    (post_processing.sharpening as u64) | (post_processing.super_sampling as u64),
                )
            })
            .filter(|&flags| flags != xr::CompositionLayerSettingsFlagsFB::EMPTY)
            .map(|flags| xr::sys::CompositionLayerSettingsFB {
                ty: xr::StructureType::COMPOSITION_LAYER_SETTINGS_FB,
                next: std::ptr::null(),
                layer_flags: flags,
            });
        Self {
            reference_space,
            layers,
            lower_key_layers: None,
            alpha,
            composition_layer_settings,
            lower_key_alpha: alpha_scale_bias(2.0, 0.0),
            upper_key_alpha: alpha_scale_bias(2.0, -1.0),
        }
    }

    pub fn build(&self) -> xr::CompositionLayerProjection<'_, xr::OpenGlEs> {
        let mut flags = xr::CompositionLayerFlags::EMPTY;

        if let Some(alpha) = &self.alpha {
            flags |= xr::CompositionLayerFlags::BLEND_TEXTURE_SOURCE_ALPHA;

            if !alpha.premultiplied {
                flags |= xr::CompositionLayerFlags::UNPREMULTIPLIED_ALPHA;
            }
        }

        let next = self
            .composition_layer_settings
            .as_ref()
            .map_or(ptr::null(), |settings| ptr::from_ref(settings).cast());

        self.build_with_next(flags, next, &self.layers)
    }

    /// Show the lower color key layer only in these views: same poses, smaller fov and image rect
    pub fn crop_lower_key_layer(
        mut self,
        layers: [xr::CompositionLayerProjectionView<'a, xr::OpenGlEs>; 2],
    ) -> Self {
        self.lower_key_layers = Some(layers);
        self
    }

    /// Layers below and above the color keyed passthrough. They show the same image with different
    /// alpha values, see the color key in stream.wgsl.
    pub fn build_color_key(&self) -> [xr::CompositionLayerProjection<'_, xr::OpenGlEs>; 2] {
        [
            (&self.lower_key_alpha, self.lower_key_layers.as_ref()),
            (&self.upper_key_alpha, None),
        ]
        .map(|(scale_bias, layers)| {
            self.build_with_next(
                xr::CompositionLayerFlags::BLEND_TEXTURE_SOURCE_ALPHA
                    | xr::CompositionLayerFlags::UNPREMULTIPLIED_ALPHA,
                ptr::from_ref(scale_bias).cast(),
                layers.unwrap_or(&self.layers),
            )
        })
    }

    fn build_with_next<'s>(
        &'s self,
        flags: xr::CompositionLayerFlags,
        next: *const std::ffi::c_void,
        layers: &'s [xr::CompositionLayerProjectionView<'a, xr::OpenGlEs>; 2],
    ) -> xr::CompositionLayerProjection<'s, xr::OpenGlEs> {
        let layer = xr::CompositionLayerProjection::new()
            .layer_flags(flags)
            .space(self.reference_space)
            .views(layers);

        if next.is_null() {
            layer
        } else {
            unsafe {
                xr::CompositionLayerProjection::from_raw(xr::sys::CompositionLayerProjection {
                    next,
                    ..layer.into_raw()
                })
            }
        }
    }
}

fn alpha_scale_bias(scale: f32, bias: f32) -> xr::sys::CompositionLayerColorScaleBiasKHR {
    xr::sys::CompositionLayerColorScaleBiasKHR {
        ty: xr::sys::CompositionLayerColorScaleBiasKHR::TYPE,
        next: ptr::null(),
        color_scale: xr::Color4f {
            r: 1.0,
            g: 1.0,
            b: 1.0,
            a: scale,
        },
        color_bias: xr::Color4f {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: bias,
        },
    }
}
