// Some native-window helpers are exported for Android/C-ABI call sites that are
// compiled in a different target configuration than this module.
#![allow(dead_code)]

use crate::android_frame_rate::game_compatibility;
use jni::sys::jobject;
use shared::surface::Surface;
use std::{ffi::c_void, fmt, ptr::NonNull, sync::OnceLock};

#[repr(C)]
pub struct ANativeWindow(c_void);

#[link(name = "android")]
unsafe extern "C" {
    pub fn ANativeWindow_release(window: *mut ANativeWindow);
    pub fn ANativeWindow_acquire(window: *mut ANativeWindow);
    pub fn ANativeWindow_setBuffersGeometry(
        window: *mut ANativeWindow,
        width: i32,
        height: i32,
        format: i32,
    ) -> i32;

    pub(crate) fn ANativeWindow_fromSurface(
        env: *mut jni::sys::JNIEnv,
        surface: jobject,
    ) -> *mut ANativeWindow;

    pub(crate) fn ANativeWindow_getHeight(window: *mut ANativeWindow) -> i32;
    pub(crate) fn ANativeWindow_getWidth(window: *mut ANativeWindow) -> i32;
}

type SetFrameRateFn = unsafe extern "C" fn(*mut ANativeWindow, f32, i8) -> i32;

/// `ANativeWindow_setFrameRate` is API 30 and this library loads on API 26, so it
/// is resolved at runtime rather than linked: the NDK stub for the compiled API
/// level does not export it, and linking it anyway would make the whole `.so`
/// fail to load on every device below Android 11 -- trading the engine for a
/// frame-pacing hint.
///
/// `libandroid.so` is already a NEEDED dependency of this library (the externs
/// above link it), so this resolves against the live image rather than mapping
/// anything new.
fn native_window_set_frame_rate() -> Option<SetFrameRateFn> {
    static ENTRY_POINT: OnceLock<Option<SetFrameRateFn>> = OnceLock::new();
    *ENTRY_POINT.get_or_init(|| {
        let library = unsafe { libloading::Library::new("libandroid.so") }.ok()?;
        let symbol =
            unsafe { library.get::<SetFrameRateFn>(b"ANativeWindow_setFrameRate\0") }.ok()?;
        let entry_point = *symbol;
        // The resolved pointer outlives the borrow only because the library is
        // never unloaded. Leaking the handle is the honest way to say that: the
        // process needs libandroid.so for its whole life, so there is nothing to
        // unload it for and nowhere to keep the handle that would not amount to
        // the same leak with more moving parts.
        std::mem::forget(library);
        Some(entry_point)
    })
}

/// Ask the display for a mode that presents `fps` frames per second evenly.
///
/// The two-argument form is deliberate: it behaves as
/// `CHANGE_FRAME_RATE_ONLY_IF_SEAMLESS`, so a switch the user would see as a
/// flicker is declined instead of performed. When it is declined the frame
/// scheduler decimates whatever the panel keeps delivering, which is correct --
/// just not as even. Asking for the visible switch would be trading a permanent
/// improvement for a visible cost at every rate change, including a mid-game
/// `setPreferredFramesPerSecond`.
pub fn request_frame_rate(window: *mut ANativeWindow, fps: u32) {
    let Some(set_frame_rate) = native_window_set_frame_rate() else {
        return;
    };
    let status = unsafe { set_frame_rate(window, fps as f32, game_compatibility().as_abi()) };
    if status != 0 {
        tracing::debug!(
            "ANativeWindow_setFrameRate({fps}) declined: status={status} (the frame \
             scheduler keeps pacing against the delivered vsyncs)"
        );
    }
}

/// Android Surface wrapper:
/// - Owns an `ANativeWindow*` strong ref.
/// - Releases it in `Drop`.
/// - Stores physical pixel size.
pub struct AndroidSurfaceWrapper {
    handle: NonNull<ANativeWindow>,
    dimension: (u32, u32), // physical pixels
}

unsafe impl Send for AndroidSurfaceWrapper {}
unsafe impl Sync for AndroidSurfaceWrapper {}

impl fmt::Debug for AndroidSurfaceWrapper {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AndroidSurfaceWrapper")
            .field("handle", &format_args!("{:p}", self.handle.as_ptr()))
            .field("dimension", &self.dimension)
            .finish()
    }
}

impl AndroidSurfaceWrapper {
    /// Use this when you already own a strong ref (e.g. `ANativeWindow_fromSurface()`).
    /// Do NOT call `acquire()` again.
    pub unsafe fn from_surface_owned(
        handle: *mut ANativeWindow,
        width: u32,
        height: u32,
    ) -> Result<Self, &'static str> {
        let handle = NonNull::new(handle).ok_or("ANativeWindow pointer is null")?;
        Ok(Self {
            handle,
            dimension: (width, height),
        })
    }

    /// Use this when you only borrowed a pointer and must acquire a strong ref.
    pub unsafe fn from_borrowed_acquire(
        handle: *mut ANativeWindow,
        width: u32,
        height: u32,
    ) -> Result<Self, &'static str> {
        let handle = NonNull::new(handle).ok_or("ANativeWindow pointer is null")?;
        unsafe { ANativeWindow_acquire(handle.as_ptr()) };
        Ok(Self {
            handle,
            dimension: (width, height),
        })
    }

    #[inline]
    pub fn native_handle(&self) -> *mut ANativeWindow {
        self.handle.as_ptr()
    }

    #[inline]
    pub fn size_physical(&self) -> (u32, u32) {
        self.dimension
    }

    #[inline]
    pub fn set_size_physical(&mut self, width: u32, height: u32) {
        self.dimension = (width, height);
    }

    pub fn set_buffers_geometry(&self, width: i32, height: i32, format: i32) -> Result<(), String> {
        let r = unsafe {
            ANativeWindow_setBuffersGeometry(self.handle.as_ptr(), width, height, format)
        };
        if r == 0 {
            Ok(())
        } else {
            Err(format!("ANativeWindow_setBuffersGeometry failed: code={r}"))
        }
    }
}

impl Drop for AndroidSurfaceWrapper {
    fn drop(&mut self) {
        unsafe { ANativeWindow_release(self.handle.as_ptr()) };
    }
}

impl Surface for AndroidSurfaceWrapper {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn size(&self) -> (u32, u32) {
        self.dimension
    }
}
