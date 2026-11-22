//! XR_META_environment_depth: real-world depth map estimated by the headset (Quest 3 / 3S).
//! Requires the com.oculus.permission.USE_SCENE runtime permission.

use alvr_common::{ViewParams, glam::UVec2};
use alvr_graphics::DepthFrame;
use openxr::{
    self as xr, raw,
    sys::{self, Handle},
};
use std::{mem, ptr};

pub struct EnvironmentDepthMETA {
    ext_fns: raw::EnvironmentDepthMETA,
    provider: sys::EnvironmentDepthProviderMETA,
    swapchain: sys::EnvironmentDepthSwapchainMETA,
    resolution: UVec2,
    images: Vec<u32>,
}

impl EnvironmentDepthMETA {
    /// The GL context of the session must be current
    pub fn new(session: &xr::Session<xr::OpenGlEs>) -> xr::Result<Self> {
        let ext_fns = session
            .instance()
            .exts()
            .meta_environment_depth
            .ok_or(sys::Result::ERROR_EXTENSION_NOT_PRESENT)?;

        let mut provider = sys::EnvironmentDepthProviderMETA::NULL;
        let info = sys::EnvironmentDepthProviderCreateInfoMETA {
            ty: sys::EnvironmentDepthProviderCreateInfoMETA::TYPE,
            next: ptr::null(),
            create_flags: sys::EnvironmentDepthProviderCreateFlagsMETA::EMPTY,
        };
        unsafe {
            super::xr_res((ext_fns.create_environment_depth_provider)(
                session.as_raw(),
                &info,
                &mut provider,
            ))?;
        }

        // From here on, Drop takes care of the cleanup if something fails
        let mut this = Self {
            ext_fns,
            provider,
            swapchain: sys::EnvironmentDepthSwapchainMETA::NULL,
            resolution: UVec2::ZERO,
            images: vec![],
        };

        let info = sys::EnvironmentDepthSwapchainCreateInfoMETA {
            ty: sys::EnvironmentDepthSwapchainCreateInfoMETA::TYPE,
            next: ptr::null(),
            create_flags: sys::EnvironmentDepthSwapchainCreateFlagsMETA::EMPTY,
        };
        unsafe {
            super::xr_res((ext_fns.create_environment_depth_swapchain)(
                provider,
                &info,
                &mut this.swapchain,
            ))?;
        }

        let state = unsafe {
            let mut state = sys::EnvironmentDepthSwapchainStateMETA::out(ptr::null_mut());
            super::xr_res((ext_fns.get_environment_depth_swapchain_state)(
                this.swapchain,
                state.as_mut_ptr(),
            ))?;
            state.assume_init()
        };
        this.resolution = UVec2::new(state.width, state.height);

        let mut count = 0;
        unsafe {
            super::xr_res((ext_fns.enumerate_environment_depth_swapchain_images)(
                this.swapchain,
                0,
                &mut count,
                ptr::null_mut(),
            ))?;
        }
        let mut images = vec![
            sys::SwapchainImageOpenGLESKHR {
                ty: sys::SwapchainImageOpenGLESKHR::TYPE,
                next: ptr::null_mut(),
                image: 0,
            };
            count as usize
        ];
        unsafe {
            super::xr_res((ext_fns.enumerate_environment_depth_swapchain_images)(
                this.swapchain,
                count,
                &mut count,
                images.as_mut_ptr().cast(),
            ))?;
        }
        this.images = images
            .iter()
            .take(count as usize)
            .map(|i| i.image)
            .collect();
        if this.images.is_empty() || this.images.contains(&0) {
            alvr_common::error!("Invalid environment depth textures: {:?}", this.images);
            return Err(sys::Result::ERROR_RUNTIME_FAILURE);
        }

        // We want the hands in the depth map: they are what we need to see. This is the default,
        // but set it explicitly. Fails harmlessly on systems without hand removal support.
        let hand_removal = sys::EnvironmentDepthHandRemovalSetInfoMETA {
            ty: sys::EnvironmentDepthHandRemovalSetInfoMETA::TYPE,
            next: ptr::null(),
            enabled: sys::FALSE,
        };
        unsafe { (ext_fns.set_environment_depth_hand_removal)(provider, &hand_removal) };

        unsafe { super::xr_res((ext_fns.start_environment_depth_provider)(provider))? };

        Ok(this)
    }

    pub fn resolution(&self) -> UVec2 {
        self.resolution
    }

    /// GL texture names (GL_TEXTURE_2D_ARRAY, 2 layers)
    pub fn images(&self) -> &[u32] {
        &self.images
    }

    /// Returns `Ok(None)` while the runtime has not produced a depth image yet
    pub fn acquire(
        &self,
        space: &xr::Space,
        display_time: xr::Time,
    ) -> xr::Result<Option<DepthFrame>> {
        let info = sys::EnvironmentDepthImageAcquireInfoMETA {
            ty: sys::EnvironmentDepthImageAcquireInfoMETA::TYPE,
            next: ptr::null(),
            space: space.as_raw(),
            display_time,
        };

        let mut image: sys::EnvironmentDepthImageMETA = unsafe { mem::zeroed() };
        image.ty = sys::EnvironmentDepthImageMETA::TYPE;
        for view in &mut image.views {
            view.ty = sys::EnvironmentDepthImageViewMETA::TYPE;
        }

        let result = unsafe {
            (self.ext_fns.acquire_environment_depth_image)(self.provider, &info, &mut image)
        };
        if result == sys::Result::ENVIRONMENT_DEPTH_NOT_AVAILABLE_META {
            return Ok(None);
        }
        super::xr_res(result)?;

        Ok(Some(DepthFrame {
            swapchain_index: image.swapchain_index,
            near: image.near_z,
            far: image.far_z,
            depth_views: image.views.map(|view| ViewParams {
                pose: crate::from_xr_pose(view.pose),
                fov: crate::from_xr_fov(view.fov),
            }),
        }))
    }
}

impl Drop for EnvironmentDepthMETA {
    fn drop(&mut self) {
        unsafe {
            (self.ext_fns.stop_environment_depth_provider)(self.provider);
            if self.swapchain != sys::EnvironmentDepthSwapchainMETA::NULL {
                (self.ext_fns.destroy_environment_depth_swapchain)(self.swapchain);
            }
            (self.ext_fns.destroy_environment_depth_provider)(self.provider);
        }
    }
}
