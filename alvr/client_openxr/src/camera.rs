//! Access to the passthrough camera through the Android NDK (Camera2).
//! Images are only used for the colors of the color key: they are neither stored nor sent anywhere.

use std::{
    ffi::{CStr, CString, c_char, c_int, c_void},
    ptr, slice,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

// Vendor tags, see the Passthrough Camera API documentation
const TAG_CAMERA_SOURCE: u32 = 0x80004d00;
const TAG_CAMERA_POSITION: u32 = 0x80004d01;
const TAG_STREAM_CONFIGURATIONS: u32 = 0xD000A;
const TAG_ACTIVE_ARRAY: u32 = 0xF0000;
const TAG_LENS_POSE_ROTATION: u32 = 0x80006;
const TAG_LENS_POSE_TRANSLATION: u32 = 0x80007;
const TAG_LENS_INTRINSICS: u32 = 0x8000A;

const TYPE_BYTE: u8 = 0;
const TYPE_INT32: u8 = 1;
const TYPE_FLOAT: u8 = 2;

const FORMAT_YUV_420_888: i32 = 0x23;
const TEMPLATE_PREVIEW: c_int = 1;

#[repr(C)]
struct CameraIdList {
    count: c_int,
    ids: *const *const c_char,
}

#[repr(C)]
struct MetadataEntry {
    tag: u32,
    ty: u8,
    count: u32,
    data: *const c_void,
}

#[repr(C)]
struct DeviceCallbacks {
    context: *mut c_void,
    on_disconnected: extern "C" fn(*mut c_void, *mut c_void),
    on_error: extern "C" fn(*mut c_void, *mut c_void, c_int),
}

#[repr(C)]
struct SessionCallbacks {
    context: *mut c_void,
    on_closed: extern "C" fn(*mut c_void, *mut c_void),
    on_ready: extern "C" fn(*mut c_void, *mut c_void),
    on_active: extern "C" fn(*mut c_void, *mut c_void),
}

#[link(name = "camera2ndk")]
unsafe extern "C" {
    fn ACameraManager_create() -> *mut c_void;
    fn ACameraManager_delete(manager: *mut c_void);
    fn ACameraManager_getCameraIdList(manager: *mut c_void, list: *mut *mut CameraIdList) -> c_int;
    fn ACameraManager_deleteCameraIdList(list: *mut CameraIdList);
    fn ACameraManager_getCameraCharacteristics(
        manager: *mut c_void,
        id: *const c_char,
        metadata: *mut *mut c_void,
    ) -> c_int;
    fn ACameraManager_openCamera(
        manager: *mut c_void,
        id: *const c_char,
        callbacks: *mut DeviceCallbacks,
        device: *mut *mut c_void,
    ) -> c_int;
    fn ACameraMetadata_getConstEntry(
        metadata: *const c_void,
        tag: u32,
        entry: *mut MetadataEntry,
    ) -> c_int;
    fn ACameraMetadata_free(metadata: *mut c_void);
    fn ACameraDevice_close(device: *mut c_void) -> c_int;
    fn ACameraDevice_createCaptureRequest(
        device: *const c_void,
        template: c_int,
        request: *mut *mut c_void,
    ) -> c_int;
    fn ACameraDevice_createCaptureSession(
        device: *mut c_void,
        outputs: *const c_void,
        callbacks: *const SessionCallbacks,
        session: *mut *mut c_void,
    ) -> c_int;
    fn ACaptureSessionOutputContainer_create(container: *mut *mut c_void) -> c_int;
    fn ACaptureSessionOutputContainer_free(container: *mut c_void);
    fn ACaptureSessionOutputContainer_add(container: *mut c_void, output: *const c_void) -> c_int;
    fn ACaptureSessionOutput_create(window: *mut c_void, output: *mut *mut c_void) -> c_int;
    fn ACaptureSessionOutput_free(output: *mut c_void);
    fn ACameraOutputTarget_create(window: *mut c_void, target: *mut *mut c_void) -> c_int;
    fn ACameraOutputTarget_free(target: *mut c_void);
    fn ACaptureRequest_addTarget(request: *mut c_void, target: *const c_void) -> c_int;
    fn ACaptureRequest_free(request: *mut c_void);
    fn ACameraCaptureSession_setRepeatingRequest(
        session: *mut c_void,
        callbacks: *mut c_void,
        request_count: c_int,
        requests: *mut *mut c_void,
        sequence_id: *mut c_int,
    ) -> c_int;
    fn ACameraCaptureSession_close(session: *mut c_void);
}

#[link(name = "mediandk")]
unsafe extern "C" {
    fn AImageReader_new(
        width: i32,
        height: i32,
        format: i32,
        max_images: i32,
        reader: *mut *mut c_void,
    ) -> c_int;
    fn AImageReader_delete(reader: *mut c_void);
    fn AImageReader_getWindow(reader: *mut c_void, window: *mut *mut c_void) -> c_int;
    fn AImageReader_acquireLatestImage(reader: *mut c_void, image: *mut *mut c_void) -> c_int;
    fn AImage_delete(image: *mut c_void);
    fn AImage_getTimestamp(image: *const c_void, timestamp_ns: *mut i64) -> c_int;
    fn AImage_getPlaneData(
        image: *const c_void,
        plane: c_int,
        data: *mut *mut u8,
        length: *mut c_int,
    ) -> c_int;
    fn AImage_getPlaneRowStride(image: *const c_void, plane: c_int, stride: *mut i32) -> c_int;
    fn AImage_getPlanePixelStride(image: *const c_void, plane: c_int, stride: *mut i32) -> c_int;
}

#[link(name = "log")]
unsafe extern "C" {
    fn __android_log_write(priority: c_int, tag: *const c_char, text: *const c_char) -> c_int;
}

// The logger of the client only forwards errors while streaming
pub fn log(text: &str) {
    const PRIORITY_INFO: c_int = 4;

    if let Ok(text) = CString::new(text) {
        unsafe { __android_log_write(PRIORITY_INFO, c"ALVR_CAM".as_ptr(), text.as_ptr()) };
    }
}

extern "C" fn on_disconnected(context: *mut c_void, _: *mut c_void) {
    unsafe { &*context.cast::<AtomicBool>() }.store(true, Ordering::Relaxed);
}

extern "C" fn on_error(context: *mut c_void, _: *mut c_void, error: c_int) {
    log(&format!("device error {error}"));
    unsafe { &*context.cast::<AtomicBool>() }.store(true, Ordering::Relaxed);
}

extern "C" fn on_session_event(_: *mut c_void, _: *mut c_void) {}

fn check(name: &str, status: c_int) -> Result<(), String> {
    if status == 0 {
        Ok(())
    } else {
        Err(format!("{name}: {status}"))
    }
}

// Runs the closures in reverse order
#[derive(Default)]
struct Cleanup(Vec<Box<dyn FnOnce()>>);

impl Cleanup {
    fn add(&mut self, f: impl FnOnce() + 'static) {
        self.0.push(Box::new(f));
    }
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        while let Some(f) = self.0.pop() {
            f();
        }
    }
}

struct Characteristics(*mut c_void);

impl Characteristics {
    fn entry(&self, tag: u32) -> Option<MetadataEntry> {
        let mut entry = MetadataEntry {
            tag: 0,
            ty: 0,
            count: 0,
            data: ptr::null(),
        };
        let status = unsafe { ACameraMetadata_getConstEntry(self.0, tag, &mut entry) };

        (status == 0 && entry.count > 0 && !entry.data.is_null()).then_some(entry)
    }

    fn ints(&self, tag: u32) -> Vec<i32> {
        match self.entry(tag) {
            Some(entry) if entry.ty == TYPE_INT32 => unsafe {
                slice::from_raw_parts(entry.data.cast::<i32>(), entry.count as usize).to_vec()
            },
            Some(entry) if entry.ty == TYPE_BYTE => unsafe {
                slice::from_raw_parts(entry.data.cast::<u8>(), entry.count as usize)
                    .iter()
                    .map(|v| i32::from(*v))
                    .collect()
            },
            _ => vec![],
        }
    }

    fn floats(&self, tag: u32) -> Vec<f32> {
        match self.entry(tag) {
            Some(entry) if entry.ty == TYPE_FLOAT => unsafe {
                slice::from_raw_parts(entry.data.cast::<f32>(), entry.count as usize).to_vec()
            },
            _ => vec![],
        }
    }
}

impl Drop for Characteristics {
    fn drop(&mut self) {
        unsafe { ACameraMetadata_free(self.0) };
    }
}

/// Calibration of the camera, for the delivered images
#[derive(Clone, Copy, Debug)]
pub struct CameraCalibration {
    pub width: u32,
    pub height: u32,
    /// In pixels: fx, fy, cx, cy
    pub intrinsics: [f32; 4],
    /// Quaternion (xyzw) from the head to the camera (+z forward, +y down)
    pub rotation: [f32; 4],
    /// Position relative to the head
    pub translation: [f32; 3],
}

struct Selected {
    id: CString,
    calibration: CameraCalibration,
}

// The left passthrough camera, at a low resolution: colors don't need more
fn find_camera(manager: *mut c_void) -> Result<Selected, String> {
    let mut list = ptr::null_mut();
    unsafe {
        check(
            "getCameraIdList",
            ACameraManager_getCameraIdList(manager, &mut list),
        )?;
    }
    let ids = unsafe {
        let ids = if (*list).count > 0 {
            slice::from_raw_parts((*list).ids, (*list).count as usize)
        } else {
            &[]
        };
        let ids = ids
            .iter()
            .map(|id| CStr::from_ptr(*id).to_owned())
            .collect::<Vec<_>>();
        ACameraManager_deleteCameraIdList(list);
        ids
    };

    for id in ids {
        let mut metadata = ptr::null_mut();
        let status =
            unsafe { ACameraManager_getCameraCharacteristics(manager, id.as_ptr(), &mut metadata) };
        if status != 0 {
            continue;
        }
        let characteristics = Characteristics(metadata);
        if characteristics.ints(TAG_CAMERA_SOURCE) != [0]
            || characteristics.ints(TAG_CAMERA_POSITION) != [0]
        {
            continue;
        }

        // Format, width, height, is input
        let Some((width, height)) = characteristics
            .ints(TAG_STREAM_CONFIGURATIONS)
            .chunks_exact(4)
            .filter(|c| c[0] == FORMAT_YUV_420_888 && c[3] == 0 && c[1] >= 320 && c[2] >= 240)
            .map(|c| (c[1], c[2]))
            .min_by_key(|(w, h)| w * h)
        else {
            continue;
        };

        // Left, top, width, height
        let array = characteristics.ints(TAG_ACTIVE_ARRAY);
        let intrinsics = characteristics.floats(TAG_LENS_INTRINSICS);
        let rotation = characteristics.floats(TAG_LENS_POSE_ROTATION);
        let translation = characteristics.floats(TAG_LENS_POSE_TRANSLATION);
        if array.len() < 4 || intrinsics.len() < 4 || rotation.len() < 4 || translation.len() < 3 {
            return Err(format!(
                "incomplete calibration: {array:?} {intrinsics:?} {rotation:?} {translation:?}"
            ));
        }

        // The image is the biggest centered crop of the sensor with the same aspect ratio
        let (array_width, array_height) = (array[2] as f32, array[3] as f32);
        let scale = f32::max(width as f32 / array_width, height as f32 / array_height);
        let crop_x = (array_width * scale - width as f32) / 2.0;
        let crop_y = (array_height * scale - height as f32) / 2.0;

        return Ok(Selected {
            id,
            calibration: CameraCalibration {
                width: width as u32,
                height: height as u32,
                intrinsics: [
                    intrinsics[0] * scale,
                    intrinsics[1] * scale,
                    intrinsics[2] * scale - crop_x,
                    intrinsics[3] * scale - crop_y,
                ],
                rotation: [rotation[0], rotation[1], rotation[2], rotation[3]],
                translation: [translation[0], translation[1], translation[2]],
            },
        });
    }

    Err("passthrough camera not found".into())
}

struct Plane {
    data: Vec<u8>,
    row_stride: usize,
    pixel_stride: usize,
}

pub struct CameraFrame {
    /// CLOCK_MONOTONIC
    pub timestamp_ns: i64,
    planes: [Plane; 3],
}

impl CameraFrame {
    fn value(&self, plane: usize, x: u32, y: u32) -> f32 {
        let plane = &self.planes[plane];
        let index = y as usize * plane.row_stride + x as usize * plane.pixel_stride;

        plane.data.get(index).copied().unwrap_or(128).into()
    }

    pub fn rgb(&self, x: u32, y: u32) -> [u8; 3] {
        let luma = self.value(0, x, y);
        let u = self.value(1, x / 2, y / 2) - 128.0;
        let v = self.value(2, x / 2, y / 2) - 128.0;

        [
            luma + 1.402 * v,
            luma - 0.344_136 * u - 0.714_136 * v,
            luma + 1.772 * u,
        ]
        .map(|value| value.round().clamp(0.0, 255.0) as u8)
    }
}

pub struct PassthroughCamera {
    reader: *mut c_void,
    lost: Arc<AtomicBool>,
    pub calibration: CameraCalibration,
    _cleanup: Cleanup,
}

impl PassthroughCamera {
    /// Requires the camera permission
    pub fn open() -> Result<Self, String> {
        let mut cleanup = Cleanup::default();

        let manager = unsafe { ACameraManager_create() };
        cleanup.add(move || unsafe { ACameraManager_delete(manager) });

        let selected = find_camera(manager)?;
        let calibration = selected.calibration;

        let mut reader = ptr::null_mut();
        let mut window = ptr::null_mut();
        unsafe {
            check(
                "AImageReader_new",
                AImageReader_new(
                    calibration.width as i32,
                    calibration.height as i32,
                    FORMAT_YUV_420_888,
                    4,
                    &mut reader,
                ),
            )?;
            cleanup.add(move || AImageReader_delete(reader));
            check(
                "AImageReader_getWindow",
                AImageReader_getWindow(reader, &mut window),
            )?;
        }

        // Late callbacks may arrive after the device is closed: the context and the callbacks are
        // leaked. The camera is opened rarely.
        let lost = Arc::new(AtomicBool::new(false));
        let device_callbacks = Box::leak(Box::new(DeviceCallbacks {
            context: Arc::into_raw(Arc::clone(&lost)).cast_mut().cast(),
            on_disconnected,
            on_error,
        }));
        let session_callbacks = Box::leak(Box::new(SessionCallbacks {
            context: ptr::null_mut(),
            on_closed: on_session_event,
            on_ready: on_session_event,
            on_active: on_session_event,
        }));

        let mut device = ptr::null_mut();
        let mut container = ptr::null_mut();
        let mut output = ptr::null_mut();
        let mut target = ptr::null_mut();
        let mut request = ptr::null_mut();
        let mut session = ptr::null_mut();
        unsafe {
            check(
                "openCamera",
                ACameraManager_openCamera(
                    manager,
                    selected.id.as_ptr(),
                    device_callbacks,
                    &mut device,
                ),
            )?;
            cleanup.add(move || {
                ACameraDevice_close(device);
            });

            check(
                "OutputContainer_create",
                ACaptureSessionOutputContainer_create(&mut container),
            )?;
            cleanup.add(move || ACaptureSessionOutputContainer_free(container));
            check(
                "Output_create",
                ACaptureSessionOutput_create(window, &mut output),
            )?;
            cleanup.add(move || ACaptureSessionOutput_free(output));
            check(
                "OutputContainer_add",
                ACaptureSessionOutputContainer_add(container, output),
            )?;

            check(
                "OutputTarget_create",
                ACameraOutputTarget_create(window, &mut target),
            )?;
            cleanup.add(move || ACameraOutputTarget_free(target));
            check(
                "createCaptureRequest",
                ACameraDevice_createCaptureRequest(device, TEMPLATE_PREVIEW, &mut request),
            )?;
            cleanup.add(move || ACaptureRequest_free(request));
            check("addTarget", ACaptureRequest_addTarget(request, target))?;

            check(
                "createCaptureSession",
                ACameraDevice_createCaptureSession(
                    device,
                    container,
                    session_callbacks,
                    &mut session,
                ),
            )?;
            cleanup.add(move || ACameraCaptureSession_close(session));
            check(
                "setRepeatingRequest",
                ACameraCaptureSession_setRepeatingRequest(
                    session,
                    ptr::null_mut(),
                    1,
                    &mut request,
                    ptr::null_mut(),
                ),
            )?;
        }

        Ok(Self {
            reader,
            lost,
            calibration,
            _cleanup: cleanup,
        })
    }

    /// Whether the camera must be opened again
    pub fn is_lost(&self) -> bool {
        self.lost.load(Ordering::Relaxed)
    }

    /// The newest image, or None if there is no new one
    pub fn latest_frame(&self) -> Option<CameraFrame> {
        let mut image = ptr::null_mut();
        let status = unsafe { AImageReader_acquireLatestImage(self.reader, &mut image) };
        if status != 0 || image.is_null() {
            return None;
        }

        let frame = read_image(image);
        unsafe { AImage_delete(image) };

        frame
    }
}

fn read_image(image: *const c_void) -> Option<CameraFrame> {
    let mut timestamp_ns = 0;
    unsafe { AImage_getTimestamp(image, &mut timestamp_ns) };

    let plane = |index| {
        let mut data = ptr::null_mut();
        let mut length = 0;
        let mut row_stride = 0;
        let mut pixel_stride = 0;
        unsafe {
            if AImage_getPlaneData(image, index, &mut data, &mut length) != 0
                || AImage_getPlaneRowStride(image, index, &mut row_stride) != 0
                || AImage_getPlanePixelStride(image, index, &mut pixel_stride) != 0
                || data.is_null()
            {
                return None;
            }

            Some(Plane {
                data: slice::from_raw_parts(data, length as usize).to_vec(),
                row_stride: row_stride as usize,
                pixel_stride: pixel_stride as usize,
            })
        }
    };

    Some(CameraFrame {
        timestamp_ns,
        planes: [plane(0)?, plane(1)?, plane(2)?],
    })
}
