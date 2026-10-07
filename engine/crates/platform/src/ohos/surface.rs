//! `OHNativeWindow` ownership for OpenHarmony hosts.
//!
//! The shape mirrors the Android wrapper because the underlying model is the
//! same: a producer/consumer buffer queue reached through an opaque native
//! window handle, reference counted by the platform. What differs is only the
//! spelling of the reference-count calls.

// Some native-window helpers are exported for OpenHarmony/C-ABI call sites
// that are compiled in a different target configuration than this module.
#![allow(dead_code)]

use shared::surface::Surface;
use std::{ffi::c_void, fmt, ptr::NonNull};

#[repr(C)]
pub struct OHNativeWindow(c_void);

// Both live in libnative_window.so, which the sysroot provides. They take a
// void* rather than a typed handle: OpenHarmony's reference counting is a
// property of its native-object base type, shared by several handle kinds, and
// the header declares them accordingly.
#[link(name = "native_window")]
unsafe extern "C" {
    pub fn OH_NativeWindow_NativeObjectReference(obj: *mut c_void) -> i32;
    pub fn OH_NativeWindow_NativeObjectUnreference(obj: *mut c_void) -> i32;
}

/// OpenHarmony Surface wrapper:
/// - Owns one `OHNativeWindow*` reference.
/// - Drops it in `Drop`.
/// - Stores the physical pixel size.
///
/// The C ABI promises exactly this ("Migo takes its own native-object
/// reference and releases it before `RELEASED`"), so the reference is acquired
/// here rather than borrowed from the host: the retiring and the incoming
/// render generation can both be live until the render thread fences the old
/// one, and sharing one reference across that window is what makes a surface
/// disappear underneath a GPU that is still using it.
pub struct OhosSurfaceWrapper {
    handle: NonNull<OHNativeWindow>,
    dimension: (u32, u32), // physical pixels
}

// The native window's reference counting and buffer APIs are internally
// synchronized. This token is never dereferenced as Rust memory; it is only
// handed back to OpenHarmony APIs.
unsafe impl Send for OhosSurfaceWrapper {}
unsafe impl Sync for OhosSurfaceWrapper {}

impl fmt::Debug for OhosSurfaceWrapper {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OhosSurfaceWrapper")
            .field("handle", &format_args!("{:p}", self.handle.as_ptr()))
            .field("dimension", &self.dimension)
            .finish()
    }
}

impl OhosSurfaceWrapper {
    /// Use when a reference is already owned and must not be taken again.
    ///
    /// # Safety
    /// `handle` must be a live `OHNativeWindow*` whose reference this wrapper
    /// takes over.
    pub unsafe fn from_owned(
        handle: *mut OHNativeWindow,
        width: u32,
        height: u32,
    ) -> Result<Self, &'static str> {
        let handle = NonNull::new(handle).ok_or("OHNativeWindow pointer is null")?;
        Ok(Self {
            handle,
            dimension: (width, height),
        })
    }

    /// Use when the pointer is only borrowed from the host and an independent
    /// reference has to be taken.
    ///
    /// # Safety
    /// `handle` must be a live `OHNativeWindow*` for the duration of the call.
    pub unsafe fn from_borrowed_reference(
        handle: *mut OHNativeWindow,
        width: u32,
        height: u32,
    ) -> Result<Self, &'static str> {
        let handle = NonNull::new(handle).ok_or("OHNativeWindow pointer is null")?;
        // A non-zero result means the reference was not taken. Proceeding would
        // leave this wrapper unreferencing something it never referenced, which
        // is an over-release of the host's own reference -- worse than failing.
        let taken = unsafe { OH_NativeWindow_NativeObjectReference(handle.as_ptr().cast()) };
        if taken != 0 {
            return Err("OH_NativeWindow_NativeObjectReference failed");
        }
        Ok(Self {
            handle,
            dimension: (width, height),
        })
    }

    #[inline]
    pub fn native_handle(&self) -> *mut OHNativeWindow {
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
}

impl Drop for OhosSurfaceWrapper {
    fn drop(&mut self) {
        unsafe { OH_NativeWindow_NativeObjectUnreference(self.handle.as_ptr().cast()) };
    }
}

impl Surface for OhosSurfaceWrapper {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn size(&self) -> (u32, u32) {
        self.dimension
    }
}
