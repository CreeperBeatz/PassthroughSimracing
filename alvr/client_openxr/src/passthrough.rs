use crate::extra_extensions::{LutTest, PassthroughFB, PassthroughHTC};
use alvr_common::{
    anyhow::{Result, bail},
    error, warn,
};
use alvr_system_info::Platform;
use openxr::{self as xr};
use std::{marker::PhantomData, ops::Deref, ptr};

pub struct PassthroughLayer<'a> {
    handle_fb: Option<PassthroughFB>,
    handle_htc: Option<PassthroughHTC>,
    lut_test: LutTest,
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
            lut_test: LutTest::Off,
            _marker: PhantomData,
        })
    }

    pub fn set_lut_test(&mut self, test: LutTest) {
        if test == self.lut_test {
            return;
        }
        self.lut_test = test;

        if let Some(handle) = &mut self.handle_fb {
            match handle.set_lut_test(test) {
                Ok(()) => warn!("Passthrough LUT test: {test:?}"),
                Err(e) => error!("Passthrough LUT test {test:?} failed: {e}"),
            }
        }
    }

    pub fn is_on_top(&self) -> bool {
        self.lut_test.passthrough_on_top()
    }
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
