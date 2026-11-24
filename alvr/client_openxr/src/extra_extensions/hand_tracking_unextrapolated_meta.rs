// XR_META_hand_tracking_unextrapolated_poses: the joints of the newest hand sample as the tracker made it
// (not predicted to the asked time) and when the tracking data behind it was captured. The extension has
// no functions and `openxr` 0.21 has no bindings for it: the two structures are chained here, by their
// numbers in the registry (extension 694). The request goes into the locate info, the capture time comes
// back in the joint locations; the asked time then only places the base space

use openxr::{self as xr, sys};
use std::{ffi::c_void, mem::MaybeUninit, ptr};

pub const META_HAND_TRACKING_UNEXTRAPOLATED_POSES_EXTENSION_NAME: &str =
    "XR_META_hand_tracking_unextrapolated_poses";

// XR_TYPE_HAND_TRACKING_UNEXTRAPOLATED_POSES_REQUEST_META and XR_TYPE_HAND_TRACKING_UNEXTRAPOLATED_POSES_META:
// 1000000000 + (694 - 1) * 1000 + offset 0 / 1
const TYPE_REQUEST: i32 = 1_000_693_000;
const TYPE_POSES: i32 = 1_000_693_001;

#[repr(C)]
struct HandTrackingUnextrapolatedPosesRequestMETA {
    ty: sys::StructureType,
    next: *const c_void,
}

#[repr(C)]
struct HandTrackingUnextrapolatedPosesMETA {
    ty: sys::StructureType,
    next: *mut c_void,
    capture_time: sys::Time,
}

/// The joints of a hand in `base` at `time` (one call of xrLocateHandJointsEXT). With `unextrapolated`
/// (only if the extension is enabled) the newest sample as it is, and the time its tracking data was
/// captured (None: the runtime wrote 0). None: the hand is not active
pub fn locate_hand_sample<G>(
    session: &xr::Session<G>,
    tracker: &xr::HandTracker,
    base: &xr::Space,
    time: xr::Time,
    unextrapolated: bool,
) -> xr::Result<Option<(xr::HandJointLocations, Option<xr::Time>)>> {
    let fns = session
        .instance()
        .exts()
        .ext_hand_tracking
        .ok_or(sys::Result::ERROR_EXTENSION_NOT_PRESENT)?;

    let request = HandTrackingUnextrapolatedPosesRequestMETA {
        ty: sys::StructureType::from_raw(TYPE_REQUEST),
        next: ptr::null(),
    };
    let mut poses = HandTrackingUnextrapolatedPosesMETA {
        ty: sys::StructureType::from_raw(TYPE_POSES),
        next: ptr::null_mut(),
        capture_time: sys::Time::from_nanos(0),
    };
    let info = sys::HandJointsLocateInfoEXT {
        ty: sys::HandJointsLocateInfoEXT::TYPE,
        next: if unextrapolated {
            (&raw const request).cast::<c_void>()
        } else {
            ptr::null()
        },
        base_space: base.as_raw(),
        time,
    };
    let mut joints = MaybeUninit::<xr::HandJointLocations>::uninit();
    let mut locations = sys::HandJointLocationsEXT {
        ty: sys::HandJointLocationsEXT::TYPE,
        next: if unextrapolated {
            (&raw mut poses).cast::<c_void>()
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
    let capture_time =
        (unextrapolated && poses.capture_time.as_nanos() > 0).then_some(poses.capture_time);

    Ok(Some((joints, capture_time)))
}
