//! Experiment: can the passthrough camera be used to learn the colors for the passthrough color key?
//!
//! Enabled with `adb shell setprop debug.alvr.camera_probe 1` before starting the app (which then
//! asks for the camera permission). Logs the timing of the camera images and their colors, averaged
//! over a coarse grid, with the tag "ALVR_CAM". Images are neither stored nor sent anywhere. Stops
//! by itself.

use crate::passthrough::debug_property;
use std::{
    ffi::{CStr, CString, c_char, c_int, c_void},
    ptr, slice,
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};

const PROPERTY: &str = "debug.alvr.camera_probe";
const MAX_DURATION: Duration = Duration::from_secs(120);
const GRID_WIDTH: usize = 16;
const GRID_HEIGHT: usize = 12;

// Vendor tags, see the Passthrough Camera API documentation
const TAG_CAMERA_SOURCE: u32 = 0x80004d00;
const TAG_CAMERA_POSITION: u32 = 0x80004d01;
const TAG_STREAM_CONFIGURATIONS: u32 = 0xD000A;
const TAG_TIMESTAMP_SOURCE: u32 = 0xF0008;
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
fn log(text: &str) {
    const PRIORITY_INFO: c_int = 4;

    if let Ok(text) = CString::new(text) {
        unsafe { __android_log_write(PRIORITY_INFO, c"ALVR_CAM".as_ptr(), text.as_ptr()) };
    }
}

static CAMERA_LOST: AtomicBool = AtomicBool::new(false);

extern "C" fn on_disconnected(_: *mut c_void, _: *mut c_void) {
    CAMERA_LOST.store(true, Ordering::Relaxed);
}

extern "C" fn on_error(_: *mut c_void, _: *mut c_void, error: c_int) {
    log(&format!("device error {error}"));
    CAMERA_LOST.store(true, Ordering::Relaxed);
}

extern "C" fn on_session_event(_: *mut c_void, _: *mut c_void) {}

fn clock_ns(clock: libc::clockid_t) -> i64 {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    unsafe { libc::clock_gettime(clock, &mut time) };

    time.tv_sec as i64 * 1_000_000_000 + time.tv_nsec as i64
}

fn check(name: &str, status: c_int) -> Result<(), String> {
    if status == 0 {
        Ok(())
    } else {
        Err(format!("{name}: {status}"))
    }
}

pub fn enabled() -> bool {
    debug_property(PROPERTY).unwrap_or(0) > 0
}

/// Runs the probe in the background if it is enabled
pub fn spawn() {
    if !enabled() {
        return;
    }

    thread::spawn(|| {
        // The permission may be granted after the start
        for attempt in 0..60 {
            if !enabled() {
                return;
            }
            match run() {
                Ok(()) => {
                    log("finished");
                    return;
                }
                Err(e) => log(&format!("attempt {attempt}: {e}")),
            }
            thread::sleep(Duration::from_secs(3));
        }
    });
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

// Returns the id and image size of the left passthrough camera
fn find_camera(manager: *mut c_void) -> Result<(CString, i32, i32), String> {
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

    let mut selected = None;
    for id in ids {
        let mut metadata = ptr::null_mut();
        let status =
            unsafe { ACameraManager_getCameraCharacteristics(manager, id.as_ptr(), &mut metadata) };
        if status != 0 {
            log(&format!("camera {id:?}: no characteristics ({status})"));
            continue;
        }
        let characteristics = Characteristics(metadata);

        let source = characteristics.ints(TAG_CAMERA_SOURCE);
        let position = characteristics.ints(TAG_CAMERA_POSITION);
        // Format, width, height, is input
        let sizes = characteristics
            .ints(TAG_STREAM_CONFIGURATIONS)
            .chunks_exact(4)
            .filter(|c| c[0] == FORMAT_YUV_420_888 && c[3] == 0)
            .map(|c| (c[1], c[2]))
            .collect::<Vec<_>>();
        log(&format!(
            "camera {id:?}: source {source:?} position {position:?} timestamp source {:?} \
             rotation {:?} translation {:?} intrinsics {:?} sizes {sizes:?}",
            characteristics.ints(TAG_TIMESTAMP_SOURCE),
            characteristics.floats(TAG_LENS_POSE_ROTATION),
            characteristics.floats(TAG_LENS_POSE_TRANSLATION),
            characteristics.floats(TAG_LENS_INTRINSICS),
        ));

        if source == [0] && position == [0] {
            // Colors don't need resolution
            let size = sizes
                .iter()
                .filter(|(w, h)| *w >= 320 && *h >= 240)
                .min_by_key(|(w, h)| w * h);
            if let Some((width, height)) = size {
                selected = Some((id, *width, *height));
            }
        }
    }

    selected.ok_or_else(|| "passthrough camera not found".into())
}

// Mean Y, U and V of each cell
fn average_grid(image: *const c_void, width: i32, height: i32) -> Option<Vec<[u8; 3]>> {
    let mut grid = vec![[0; 3]; GRID_WIDTH * GRID_HEIGHT];

    for plane in 0..3 {
        let mut data = ptr::null_mut();
        let mut length = 0;
        let mut row_stride = 0;
        let mut pixel_stride = 0;
        unsafe {
            if AImage_getPlaneData(image, plane, &mut data, &mut length) != 0
                || AImage_getPlaneRowStride(image, plane, &mut row_stride) != 0
                || AImage_getPlanePixelStride(image, plane, &mut pixel_stride) != 0
                || data.is_null()
            {
                return None;
            }
        }
        let data = unsafe { slice::from_raw_parts(data, length as usize) };
        let (plane_width, plane_height) = if plane == 0 {
            (width as usize, height as usize)
        } else {
            (width as usize / 2, height as usize / 2)
        };

        for (cell, value) in grid.iter_mut().enumerate() {
            let (cell_x, cell_y) = (cell % GRID_WIDTH, cell / GRID_WIDTH);
            let columns =
                cell_x * plane_width / GRID_WIDTH..(cell_x + 1) * plane_width / GRID_WIDTH;
            let rows =
                cell_y * plane_height / GRID_HEIGHT..(cell_y + 1) * plane_height / GRID_HEIGHT;

            let mut sum = 0_u32;
            let mut count = 0_u32;
            for y in rows.step_by(2) {
                for x in columns.clone().step_by(2) {
                    let index = y * row_stride as usize + x * pixel_stride as usize;
                    if let Some(v) = data.get(index) {
                        sum += u32::from(*v);
                        count += 1;
                    }
                }
            }
            value[plane as usize] = (sum / count.max(1)) as u8;
        }
    }

    Some(grid)
}

fn run() -> Result<(), String> {
    CAMERA_LOST.store(false, Ordering::Relaxed);
    let mut cleanup = Cleanup::default();

    let manager = unsafe { ACameraManager_create() };
    cleanup.add(move || unsafe { ACameraManager_delete(manager) });

    let (id, width, height) = find_camera(manager)?;
    log(&format!("using camera {id:?} at {width}x{height}"));

    let mut reader = ptr::null_mut();
    let mut window = ptr::null_mut();
    unsafe {
        check(
            "AImageReader_new",
            AImageReader_new(width, height, FORMAT_YUV_420_888, 4, &mut reader),
        )?;
        cleanup.add(move || AImageReader_delete(reader));
        check(
            "AImageReader_getWindow",
            AImageReader_getWindow(reader, &mut window),
        )?;
    }

    // These must outlive the device and the session
    let device_callbacks = Box::leak(Box::new(DeviceCallbacks {
        context: ptr::null_mut(),
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
            ACameraManager_openCamera(manager, id.as_ptr(), device_callbacks, &mut device),
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
            ACameraDevice_createCaptureSession(device, container, session_callbacks, &mut session),
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
    log("capturing");

    let start = Instant::now();
    let mut last_report = Instant::now();
    let mut reports = 0;
    let mut frames = 0;
    let mut last_timestamp = 0;
    // Age of the images when they arrive, relative to both clocks (the documentation doesn't say
    // which one is used)
    let mut monotonic_ages = vec![];
    let mut boottime_ages = vec![];
    while start.elapsed() < MAX_DURATION && enabled() {
        if CAMERA_LOST.load(Ordering::Relaxed) {
            return Err("camera lost".into());
        }

        let mut image = ptr::null_mut();
        let status = unsafe { AImageReader_acquireLatestImage(reader, &mut image) };
        if status != 0 || image.is_null() {
            thread::sleep(Duration::from_millis(1));
        } else {
            let mut timestamp = 0;
            unsafe { AImage_getTimestamp(image, &mut timestamp) };
            if timestamp != last_timestamp {
                last_timestamp = timestamp;
                frames += 1;
                monotonic_ages.push((clock_ns(libc::CLOCK_MONOTONIC) - timestamp) as f64 / 1e6);
                boottime_ages.push((clock_ns(libc::CLOCK_BOOTTIME) - timestamp) as f64 / 1e6);
            }

            if last_report.elapsed() > Duration::from_secs(1) && frames > 0 {
                let statistics = |ages: &mut Vec<f64>| {
                    ages.sort_by(f64::total_cmp);
                    let text = format!(
                        "{:.1}/{:.1}/{:.1}",
                        ages[0],
                        ages[ages.len() / 2],
                        ages[ages.len() - 1]
                    );
                    ages.clear();
                    text
                };
                log(&format!(
                    "fps {:.1} age ms min/median/max: monotonic {} boottime {}",
                    frames as f32 / last_report.elapsed().as_secs_f32(),
                    statistics(&mut monotonic_ages),
                    statistics(&mut boottime_ages),
                ));
                last_report = Instant::now();
                frames = 0;

                reports += 1;
                if reports % 2 == 0
                    && let Some(grid) = average_grid(image, width, height)
                {
                    let text = grid
                        .iter()
                        .map(|[y, u, v]| format!("{y:02x}{u:02x}{v:02x}"))
                        .collect::<String>();
                    log(&format!("grid {GRID_WIDTH}x{GRID_HEIGHT} yuv {text}"));
                }
            }

            unsafe { AImage_delete(image) };
        }
    }

    Ok(())
}
