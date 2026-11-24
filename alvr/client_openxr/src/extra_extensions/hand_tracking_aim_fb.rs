// XR_FB_hand_tracking_aim: the aim pose of a tracked hand (the ray of the system pointer of the Quest;
// its pinch strengths are not used), chained to the joint locations of XR_EXT_hand_tracking. The
// extension has no functions of its own; `openxr` locates the joints without a chain, so the call is
// made here

use openxr::{self as xr, sys};
use std::{ffi::c_void, mem::MaybeUninit, ptr};

/// The aim state of a hand as the runtime reports it
#[derive(Clone, Copy, Debug)]
pub struct HandAim {
    /// Of the pointer ray: its origin and -Z its direction
    pub pose: xr::Posef,
    /// COMPUTED and VALID
    pub valid: bool,
}

/// The joints of a hand in `base` at `time`, and with XR_FB_hand_tracking_aim enabled its aim state
/// (one call of xrLocateHandJointsEXT). None: the hand is not active
pub fn locate_hand_joints_with_aim<G>(
    session: &xr::Session<G>,
    tracker: &xr::HandTracker,
    base: &xr::Space,
    time: xr::Time,
) -> xr::Result<Option<(xr::HandJointLocations, Option<HandAim>)>> {
    let instance = session.instance();
    let fns = instance
        .exts()
        .ext_hand_tracking
        .ok_or(sys::Result::ERROR_EXTENSION_NOT_PRESENT)?;
    let aim_enabled = instance.exts().fb_hand_tracking_aim.is_some();

    let info = sys::HandJointsLocateInfoEXT {
        ty: sys::HandJointsLocateInfoEXT::TYPE,
        next: ptr::null(),
        base_space: base.as_raw(),
        time,
    };
    let mut aim = sys::HandTrackingAimStateFB {
        ty: sys::HandTrackingAimStateFB::TYPE,
        next: ptr::null_mut(),
        status: sys::HandTrackingAimFlagsFB::EMPTY,
        aim_pose: xr::Posef::IDENTITY,
        pinch_strength_index: 0.0,
        pinch_strength_middle: 0.0,
        pinch_strength_ring: 0.0,
        pinch_strength_little: 0.0,
    };
    let mut joints = MaybeUninit::<xr::HandJointLocations>::uninit();
    let mut locations = sys::HandJointLocationsEXT {
        ty: sys::HandJointLocationsEXT::TYPE,
        next: if aim_enabled {
            (&raw mut aim).cast::<c_void>()
        } else {
            ptr::null_mut()
        },
        is_active: false.into(),
        joint_count: xr::HAND_JOINT_COUNT as u32,
        joint_locations: joints.as_mut_ptr().cast(),
    };
    // SAFETY: the structures live until the call returns, the array holds `joint_count` joints (the
    // tracker uses the default joint set, as `openxr` creates it)
    unsafe {
        super::xr_res((fns.locate_hand_joints)(
            tracker.as_raw(),
            &info,
            &mut locations,
        ))?;
    }
    if !bool::from(locations.is_active) {
        return Ok(None);
    }
    // SAFETY: written by the runtime for an active hand
    let joints = unsafe { joints.assume_init() };
    let aim = aim_enabled.then(|| HandAim {
        pose: aim.aim_pose,
        valid: aim
            .status
            .contains(sys::HandTrackingAimFlagsFB::COMPUTED | sys::HandTrackingAimFlagsFB::VALID),
    });

    Ok(Some((joints, aim)))
}
