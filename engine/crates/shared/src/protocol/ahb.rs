//! Owned `AHardwareBuffer` handle and helpers — the cross-crate
//! protocol type for "decoded image lives in a GPU-importable buffer
//! instead of a `Vec<u8>`".
//!
//! On Android (NDK API 26+) `OwnedAhb` is a thin RAII wrapper over
//! `AHardwareBuffer*`: `allocate` creates a fresh owned handle,
//! `from_raw_acquire` adopts a borrowed raw pointer by bumping the
//! refcount, `from_raw_owned` adopts an already-owned raw pointer,
//! and `Drop` releases it. The pointer is opaque to safe Rust;
//! consumers (graphics' `eglCreateImageKHR(EGL_NATIVE_BUFFER_ANDROID, ahb)`,
//! Java's `Bitmap.wrapHardwareBuffer`) reach in via [`OwnedAhb::raw`]
//! and trust the RAII to outlive their borrow.
//!
//! On non-Android targets the type is a **mock** backed by a `Vec<u8>`.
//! The mock exists for two reasons:
//!
//! 1. **TDD**. The Rust-side decode-→-AHB→-upload pipeline is the
//!    bulk of the M2 change set; we want to drive it from unit tests
//!    without requiring an emulator round-trip.
//! 2. **Desktop dev builds** (where `cfg!(not(target_os="android"))`
//!    keeps the Rust image decoders) can still produce a uniform
//!    `DecodedImage::HardwareBuffer` so downstream code paths are
//!    exercised on the host.
//!
//! # Thread safety
//!
//! `AHardwareBuffer_acquire` / `_release` / `_lock` / `_unlock` are
//! all documented thread-safe (the NDK uses an atomic refcount).
//! [`OwnedAhb`] is therefore `Send + Sync`. The mock holds a `Vec<u8>`
//! behind an `Arc<Mutex>` so it is `Send + Sync` for the same
//! contract.
//!
//! # What this module is NOT
//!
//! * Not a Vulkan importer — that lives in `graphics/`.
//! * Not a Java `HardwareBuffer` bridge — `platform/android/jni`
//!   wraps `_acquire` and hands the raw pointer through `jlong`.
//! * Not a general fence/sync primitive. CPU locks are synchronously
//!   released before a buffer is published to another subsystem.

use std::fmt;

#[cfg(target_os = "android")]
mod sys {
    //! Hand-rolled FFI for the small slice of the AHB ABI we use.
    //!
    //! Why not `ndk-sys`? It would pull a much larger NDK Rust
    //! surface for the sake of half a dozen function signatures.
    //! Linking against `libandroid` (which AHB lives in) is already
    //! handled elsewhere in the platform crate; this module just
    //! declares the C entry points.

    use std::os::raw::{c_int, c_void};

    /// Opaque NDK type. We never construct it directly.
    #[repr(C)]
    pub struct AHardwareBuffer {
        _private: [u8; 0],
    }

    #[repr(C)]
    pub struct AHardwareBuffer_Desc {
        pub width: u32,
        pub height: u32,
        pub layers: u32,
        pub format: u32,
        pub usage: u64,
        pub stride: u32,
        pub rfu0: u32,
        pub rfu1: u64,
    }

    #[repr(C)]
    pub struct ARect {
        pub left: i32,
        pub top: i32,
        pub right: i32,
        pub bottom: i32,
    }

    // libandroid is already linked transitively via `tracing-android`
    // and the AHB call sites in `crates/graphics`; a duplicate
    // `#[link(name = "android")]` is harmless thanks to the linker
    // dedup, but we omit it to avoid pulling the attribute into
    // `shared` (a "neutral" crate). The functions are still resolved
    // because every Android binary links libandroid.
    //
    // Edition 2024 requires `extern` blocks be spelled `unsafe extern`.
    unsafe extern "C" {
        pub fn AHardwareBuffer_allocate(
            desc: *const AHardwareBuffer_Desc,
            out: *mut *mut AHardwareBuffer,
        ) -> c_int;
        pub fn AHardwareBuffer_acquire(buffer: *mut AHardwareBuffer);
        pub fn AHardwareBuffer_release(buffer: *mut AHardwareBuffer);
        pub fn AHardwareBuffer_describe(
            buffer: *const AHardwareBuffer,
            out_desc: *mut AHardwareBuffer_Desc,
        );
        pub fn AHardwareBuffer_lock(
            buffer: *mut AHardwareBuffer,
            usage: u64,
            fence: c_int,
            rect: *const ARect,
            out_addr: *mut *mut c_void,
        ) -> c_int;
        pub fn AHardwareBuffer_unlock(buffer: *mut AHardwareBuffer, out_fence: *mut c_int)
        -> c_int;
    }
}

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Pixel formats we currently produce or consume. The integer
/// representation matches `AHARDWAREBUFFER_FORMAT_*` exactly so it
/// can be passed directly to `AHardwareBuffer_allocate` on Android.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum AhbFormat {
    /// 32-bit RGBA, byte order R, G, B, A. The single format the
    /// image pipeline produces today; mirrors
    /// `AHARDWAREBUFFER_FORMAT_R8G8B8A8_UNORM = 1`.
    R8g8b8a8Unorm = 1,
}

impl AhbFormat {
    /// Bytes per pixel for the format.  Used to size CPU-side row
    /// pitches and validate `desc.stride * height` against the
    /// expected payload length.
    #[inline]
    pub const fn bytes_per_pixel(self) -> u32 {
        match self {
            AhbFormat::R8g8b8a8Unorm => 4,
        }
    }
}

bitflags::bitflags! {
    /// `AHARDWAREBUFFER_USAGE_*` bit flags. The values mirror the
    /// NDK header so we can pass through to `_allocate` unchanged.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct AhbUsage: u64 {
        /// `AHARDWAREBUFFER_USAGE_CPU_READ_OFTEN`
        const CPU_READ_OFTEN = 0x3;
        /// `AHARDWAREBUFFER_USAGE_CPU_READ_RARELY`
        const CPU_READ_RARELY = 0x2;
        /// `AHARDWAREBUFFER_USAGE_CPU_WRITE_OFTEN`
        const CPU_WRITE_OFTEN = 0x30;
        /// `AHARDWAREBUFFER_USAGE_CPU_WRITE_RARELY`
        const CPU_WRITE_RARELY = 0x20;
        /// `AHARDWAREBUFFER_USAGE_GPU_SAMPLED_IMAGE`
        const GPU_SAMPLED_IMAGE = 0x100;
        /// `AHARDWAREBUFFER_USAGE_GPU_COLOR_OUTPUT`
        const GPU_COLOR_OUTPUT = 0x200;
    }
}

/// Description of an AHB. Mirrors the NDK `AHardwareBuffer_Desc`.
///
/// `stride_pixels` is **output-only**: callers should pass `0` to
/// [`OwnedAhb::allocate`] and read the actual stride from the
/// descriptor on the returned handle. (The driver may pad rows for
/// alignment; the actual stride is rarely equal to `width`.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AhbDesc {
    pub width: u32,
    pub height: u32,
    pub layers: u32,
    pub format: AhbFormat,
    pub usage: AhbUsage,
    /// Row stride in **pixels**. Set to 0 when calling `allocate`;
    /// populated by the driver and visible via [`OwnedAhb::desc`].
    pub stride_pixels: u32,
}

impl AhbDesc {
    /// Convenience for the canonical "decoded image, GPU-sampled,
    /// occasionally CPU-read for `getImageData`" usage.
    pub fn rgba_sampled(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            layers: 1,
            format: AhbFormat::R8g8b8a8Unorm,
            usage: AhbUsage::GPU_SAMPLED_IMAGE | AhbUsage::CPU_READ_RARELY,
            stride_pixels: 0,
        }
    }

    /// Descriptor for a static image decoded once on the CPU, sampled by the
    /// GPU, and occasionally read back if EGLImage import is unavailable.
    pub fn rgba_sampled_cpu_decode(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            layers: 1,
            format: AhbFormat::R8g8b8a8Unorm,
            usage: AhbUsage::GPU_SAMPLED_IMAGE
                | AhbUsage::CPU_WRITE_RARELY
                | AhbUsage::CPU_READ_RARELY,
            stride_pixels: 0,
        }
    }
}

/// Result type for AHB operations. Errors are coarse on purpose —
/// the NDK only ever returns `int` status codes; this enum keeps
/// the meaningful failure modes distinct from one another.
#[derive(Debug)]
pub enum AhbError {
    /// `AHardwareBuffer_allocate` returned non-zero.
    AllocateFailed { status: i32 },
    /// `AHardwareBuffer_lock` returned non-zero.
    LockFailed { status: i32 },
    /// `AHardwareBuffer_unlock` returned non-zero.
    UnlockFailed { status: i32 },
    /// Operation requires Android; called on a non-Android build.
    NotAndroid,
    /// Caller passed a null pointer to `from_raw_acquire` /
    /// `from_raw_owned`.
    NullHandle,
    /// CPU lock requested with usage flags that exclude both
    /// `CPU_READ_*` and `CPU_WRITE_*`.
    InvalidLockUsage,
    /// The requested or driver-returned descriptor cannot be represented by
    /// this single-plane RGBA contract.
    InvalidDescriptor { reason: &'static str },
}

impl fmt::Display for AhbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AhbError::AllocateFailed { status } => write!(f, "AHB allocate failed: {status}"),
            AhbError::LockFailed { status } => write!(f, "AHB lock failed: {status}"),
            AhbError::UnlockFailed { status } => write!(f, "AHB unlock failed: {status}"),
            AhbError::NotAndroid => write!(f, "AHB not available on this target"),
            AhbError::NullHandle => write!(f, "AHB null handle"),
            AhbError::InvalidLockUsage => write!(f, "AHB lock usage missing CPU_READ/WRITE bit"),
            AhbError::InvalidDescriptor { reason } => {
                write!(f, "AHB invalid descriptor: {reason}")
            }
        }
    }
}

impl std::error::Error for AhbError {}

const CPU_READ_MASK: u64 = 0x0f;
const CPU_WRITE_MASK: u64 = 0xf0;
const CPU_USAGE_MASK: u64 = CPU_READ_MASK | CPU_WRITE_MASK;

fn checked_layout(desc: &AhbDesc) -> Result<(usize, usize), AhbError> {
    if desc.width == 0 || desc.height == 0 {
        return Err(AhbError::InvalidDescriptor {
            reason: "zero width or height",
        });
    }
    if desc.width > i32::MAX as u32 || desc.height > i32::MAX as u32 {
        return Err(AhbError::InvalidDescriptor {
            reason: "dimensions exceed signed graphics limits",
        });
    }
    if desc.layers != 1 {
        return Err(AhbError::InvalidDescriptor {
            reason: "only one-layer RGBA buffers are supported",
        });
    }

    let stride_pixels = if desc.stride_pixels == 0 {
        desc.width
    } else {
        desc.stride_pixels
    };
    if stride_pixels < desc.width {
        return Err(AhbError::InvalidDescriptor {
            reason: "row stride is smaller than width",
        });
    }
    let stride_bytes = (stride_pixels as usize)
        .checked_mul(desc.format.bytes_per_pixel() as usize)
        .ok_or(AhbError::InvalidDescriptor {
            reason: "row byte size overflows usize",
        })?;
    let len_bytes =
        stride_bytes
            .checked_mul(desc.height as usize)
            .ok_or(AhbError::InvalidDescriptor {
                reason: "buffer byte size overflows usize",
            })?;
    Ok((stride_bytes, len_bytes))
}

fn validate_lock_usage(desc: &AhbDesc, usage: AhbUsage) -> Result<(), AhbError> {
    let requested = usage.bits();
    let requested_read = requested & CPU_READ_MASK;
    let requested_write = requested & CPU_WRITE_MASK;
    let valid_read = requested_read == 0 || requested_read == 0x2 || requested_read == 0x3;
    let valid_write = requested_write == 0 || requested_write == 0x20 || requested_write == 0x30;

    if requested == 0
        || requested & !CPU_USAGE_MASK != 0
        || !valid_read
        || !valid_write
        || (requested_read != 0 && desc.usage.bits() & CPU_READ_MASK == 0)
        || (requested_write != 0 && desc.usage.bits() & CPU_WRITE_MASK == 0)
    {
        return Err(AhbError::InvalidLockUsage);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// OwnedAhb — Android backend
// ---------------------------------------------------------------------------

#[cfg(target_os = "android")]
mod imp {
    use super::*;
    use std::os::raw::c_void;
    use std::ptr;
    use std::sync::Arc;

    /// Refcount-owned AHB. Cheap to clone (one Rust `Arc` increment).
    pub struct OwnedAhb {
        // `Arc` makes Clone one atomic increment. `AhbBox` owns exactly one
        // native AHB reference, released after the final Rust clone drops.
        inner: Arc<AhbBox>,
        desc: AhbDesc,
    }

    struct AhbBox {
        ptr: *mut sys::AHardwareBuffer,
        // NDK permits concurrent read locks but declares every simultaneous
        // write/read-write lock undefined. Serialize all CPU access so the
        // safe mutable-slice API can never create aliased `&mut [u8]` across
        // cloned `OwnedAhb` handles, even if a vendor driver accepts both.
        cpu_lock: parking_lot::Mutex<()>,
    }

    // SAFETY: `AHardwareBuffer_*` calls are documented thread-safe.
    unsafe impl Send for AhbBox {}
    unsafe impl Sync for AhbBox {}

    impl Drop for AhbBox {
        fn drop(&mut self) {
            // SAFETY: we own one strong refcount handed to us by
            // either `_allocate` or our own `_acquire`. Releasing
            // matches that count exactly.
            unsafe { sys::AHardwareBuffer_release(self.ptr) }
        }
    }

    impl OwnedAhb {
        pub fn allocate(mut desc: AhbDesc) -> Result<Self, AhbError> {
            checked_layout(&desc)?;
            // The NDK ignores `stride` on input but writes it on the
            // descriptor it stores; we still zero it for clarity.
            desc.stride_pixels = 0;
            let c_desc = sys::AHardwareBuffer_Desc {
                width: desc.width,
                height: desc.height,
                layers: desc.layers,
                format: desc.format as u32,
                usage: desc.usage.bits(),
                stride: 0,
                rfu0: 0,
                rfu1: 0,
            };
            let mut out = ptr::null_mut();
            // SAFETY: `c_desc` and `out` are valid pointers for the duration
            // of the allocation call, and the returned handle is checked below.
            let status = unsafe { sys::AHardwareBuffer_allocate(&c_desc, &mut out) };
            if status != 0 || out.is_null() {
                return Err(AhbError::AllocateFailed { status });
            }
            let inner = Arc::new(AhbBox {
                ptr: out,
                cpu_lock: parking_lot::Mutex::new(()),
            });
            // Read back the *actual* stride (driver may have padded).
            let mut described = sys::AHardwareBuffer_Desc {
                width: 0,
                height: 0,
                layers: 0,
                format: 0,
                usage: 0,
                stride: 0,
                rfu0: 0,
                rfu1: 0,
            };
            // SAFETY: `out` is the live handle returned by the successful
            // allocation above, and `described` is writable local storage.
            unsafe { sys::AHardwareBuffer_describe(out, &mut described) };
            if described.width != desc.width
                || described.height != desc.height
                || described.layers != desc.layers
                || described.format != desc.format as u32
                || described.usage & desc.usage.bits() != desc.usage.bits()
            {
                return Err(AhbError::InvalidDescriptor {
                    reason: "driver description differs from allocation request",
                });
            }
            desc.stride_pixels = described.stride;
            checked_layout(&desc)?;
            Ok(Self { inner, desc })
        }

        /// Adopt an externally-allocated AHB pointer. Typically used
        /// when Java passed `Bitmap.getHardwareBuffer()` back through
        /// JNI as a `jlong` — the Java side held a reference, so we
        /// `_acquire` to gain our own.
        ///
        /// # Safety
        ///
        /// `ptr` must identify a live AHB matching `desc`. Callers must not
        /// create another independently synchronized `OwnedAhb` for the same
        /// pointer and then perform overlapping CPU access; use `Clone` after
        /// the first adoption so all safe locks share one mutex.
        pub unsafe fn from_raw_acquire(ptr: *mut c_void, desc: AhbDesc) -> Result<Self, AhbError> {
            if ptr.is_null() {
                return Err(AhbError::NullHandle);
            }
            let ahb = ptr as *mut sys::AHardwareBuffer;
            // SAFETY: the caller's contract guarantees `ptr` identifies a
            // live AHB; acquire adds this wrapper's owned reference.
            unsafe { sys::AHardwareBuffer_acquire(ahb) };
            Ok(Self {
                inner: Arc::new(AhbBox {
                    ptr: ahb,
                    cpu_lock: parking_lot::Mutex::new(()),
                }),
                desc,
            })
        }

        /// Adopt an externally-allocated AHB pointer that already owns one
        /// strong refcount.
        ///
        /// This is the transfer-ownership counterpart to
        /// [`Self::from_raw_acquire`]: use it when the producer has
        /// already called `AHardwareBuffer_acquire` (or equivalent) before
        /// handing the pointer across an FFI boundary.
        ///
        /// # Safety
        ///
        /// `ptr` must identify a live AHB matching `desc`, and the caller must
        /// transfer exactly one native reference. As with
        /// [`Self::from_raw_acquire`], create subsequent owners with `Clone`
        /// so safe CPU locks share the same serialization guard.
        pub unsafe fn from_raw_owned(ptr: *mut c_void, desc: AhbDesc) -> Result<Self, AhbError> {
            if ptr.is_null() {
                return Err(AhbError::NullHandle);
            }
            Ok(Self {
                inner: Arc::new(AhbBox {
                    ptr: ptr as *mut sys::AHardwareBuffer,
                    cpu_lock: parking_lot::Mutex::new(()),
                }),
                desc,
            })
        }

        /// Raw pointer for FFI use (e.g. EGL / Vulkan import).
        /// Borrows from `self`; never outlive the [`OwnedAhb`].
        pub fn raw(&self) -> *mut c_void {
            self.inner.ptr as *mut c_void
        }

        pub fn desc(&self) -> &AhbDesc {
            &self.desc
        }

        /// Convert into a raw pointer **without** decrementing the
        /// refcount — the caller is responsible for matching with
        /// `AHardwareBuffer_release`.  For tightly-controlled
        /// hand-offs, e.g. when transferring across an unsafe ABI.
        pub fn into_raw(self) -> *mut c_void {
            let ptr = self.raw();
            // We can't move out of an Arc easily; clone-and-leak is
            // the standard pattern. The extra reference is transferred
            // to the raw-pointer caller before `self` is dropped.
            // SAFETY: `ptr` is a live AHB handle and this extra reference is
            // transferred to the raw-pointer caller before `self` is dropped.
            unsafe { sys::AHardwareBuffer_acquire(ptr as *mut sys::AHardwareBuffer) };
            drop(self);
            ptr
        }

        /// Lock the buffer for CPU access. `Drop` of the returned
        /// guard unlocks. The lock is plane-0 only (suitable for
        /// `R8G8B8A8_UNORM`); planar / YUV formats need a different
        /// API which we don't currently use.
        pub fn lock_cpu(&self, usage: AhbUsage) -> Result<AhbLock<'_>, AhbError> {
            validate_lock_usage(&self.desc, usage)?;
            let (stride_bytes, len_bytes) = checked_layout(&self.desc)?;
            let cpu_lock = self.inner.cpu_lock.lock();
            let mut addr: *mut c_void = ptr::null_mut();
            // SAFETY: `self.inner.ptr` is a live owned AHB reference, usage and
            // layout were validated above, and `addr` points to writable local
            // storage for the driver to fill.
            let status = unsafe {
                sys::AHardwareBuffer_lock(
                    self.inner.ptr,
                    usage.bits(),
                    -1,          // no input fence
                    ptr::null(), // entire buffer
                    &mut addr,
                )
            };
            if status != 0 {
                return Err(AhbError::LockFailed { status });
            }
            if addr.is_null() {
                // A successful lock owns a matching unlock even if a broken
                // driver failed to return the promised address.
                // SAFETY: the successful lock owns a matching unlock, and the
                // AHB pointer remains live through `self.inner`.
                unsafe {
                    let _ = sys::AHardwareBuffer_unlock(self.inner.ptr, ptr::null_mut());
                }
                return Err(AhbError::LockFailed { status: -1 });
            }
            Ok(AhbLock {
                ahb: self,
                _cpu_lock: cpu_lock,
                addr: addr as *mut u8,
                stride_bytes,
                len_bytes,
                writable: usage.bits() & CPU_WRITE_MASK != 0,
                locked: true,
            })
        }
    }

    impl Clone for OwnedAhb {
        fn clone(&self) -> Self {
            Self {
                inner: Arc::clone(&self.inner),
                desc: self.desc,
            }
        }
    }

    impl fmt::Debug for OwnedAhb {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("OwnedAhb")
                .field("ptr", &self.inner.ptr)
                .field("desc", &self.desc)
                .finish()
        }
    }

    pub struct AhbLock<'a> {
        ahb: &'a OwnedAhb,
        _cpu_lock: parking_lot::MutexGuard<'a, ()>,
        addr: *mut u8,
        stride_bytes: usize,
        len_bytes: usize,
        writable: bool,
        locked: bool,
    }

    impl<'a> AhbLock<'a> {
        /// Raw pointer to row 0, byte 0. Lifetime is tied to `self`.
        pub fn as_ptr(&self) -> *mut u8 {
            self.addr
        }
        pub fn stride_bytes(&self) -> usize {
            self.stride_bytes
        }

        /// Entire locked allocation, including any driver row padding.
        pub fn as_bytes_mut(&mut self) -> Result<&mut [u8], AhbError> {
            if !self.writable {
                return Err(AhbError::InvalidLockUsage);
            }
            // SAFETY: a successful exclusive CPU write lock owns this address
            // for `len_bytes`, which was checked from the described stride and
            // height. The mutable borrow cannot outlive this guard.
            Ok(unsafe { std::slice::from_raw_parts_mut(self.addr, self.len_bytes) })
        }

        /// Finish CPU access synchronously and surface an unlock failure.
        pub fn finish(mut self) -> Result<(), AhbError> {
            self.unlock()
        }

        fn unlock(&mut self) -> Result<(), AhbError> {
            if !self.locked {
                return Ok(());
            }
            // Mark consumed before entering the driver: retrying an unlock
            // after an error has undefined ownership semantics.
            self.locked = false;
            // SAFETY: the lock was acquired on this AHB and has not been
            // unlocked yet; the null fence is valid per the API 26 contract.
            let status = unsafe {
                // API 26 contract: a null fence makes unlock block until CPU
                // writes and cache maintenance are complete.
                sys::AHardwareBuffer_unlock(self.ahb.inner.ptr, ptr::null_mut())
            };
            if status == 0 {
                Ok(())
            } else {
                Err(AhbError::UnlockFailed { status })
            }
        }
    }

    impl<'a> Drop for AhbLock<'a> {
        fn drop(&mut self) {
            let _ = self.unlock();
        }
    }
}

// ---------------------------------------------------------------------------
// OwnedAhb — Mock backend (every other target)
// ---------------------------------------------------------------------------

#[cfg(not(target_os = "android"))]
mod imp {
    use super::*;
    use std::os::raw::c_void;
    use std::sync::Arc;

    /// Mock AHB: a heap-allocated pixel buffer with the same
    /// surface API. Used in unit tests and on dev hosts.
    #[derive(Clone)]
    pub struct OwnedAhb {
        inner: Arc<MockInner>,
        desc: AhbDesc,
    }

    struct MockInner {
        // `RwLock` would let multi-reader but we keep parity with
        // the Android lock semantics (exclusive access during a
        // CPU lock). `parking_lot::Mutex` for cheap unpoisoned
        // locking.
        pixels: parking_lot::Mutex<Vec<u8>>,
    }

    impl OwnedAhb {
        pub fn allocate(mut desc: AhbDesc) -> Result<Self, AhbError> {
            checked_layout(&desc)?;
            // Simulate driver-chosen stride: align to 64 px (a value
            // close to what real Mali / Adreno drivers tend to pick).
            let aligned = desc
                .width
                .checked_add(63)
                .ok_or(AhbError::InvalidDescriptor {
                    reason: "aligned row stride overflows u32",
                })?
                & !63;
            desc.stride_pixels = aligned.max(desc.width);
            let (_, bytes) = checked_layout(&desc)?;
            Ok(Self {
                inner: Arc::new(MockInner {
                    pixels: parking_lot::Mutex::new(vec![0u8; bytes]),
                }),
                desc,
            })
        }

        /// # Safety
        ///
        /// Mirrors the Android [`from_raw_acquire`] contract so the signature is
        /// identical on every target: `ptr` must identify a live AHB matching
        /// `desc`. This build has no AHB to adopt and returns
        /// [`AhbError::NotAndroid`] without dereferencing `ptr`.
        ///
        /// [`from_raw_acquire`]: #method.from_raw_acquire
        pub unsafe fn from_raw_acquire(
            _ptr: *mut c_void,
            _desc: AhbDesc,
        ) -> Result<Self, AhbError> {
            // The mock has no notion of an external pointer to adopt.
            Err(AhbError::NotAndroid)
        }

        /// # Safety
        ///
        /// Mirrors the Android [`from_raw_owned`] contract so the signature is
        /// identical on every target: `ptr` must identify a live AHB matching
        /// `desc` and transfer exactly one native reference. This build has no
        /// AHB to adopt and returns [`AhbError::NotAndroid`] without
        /// dereferencing `ptr`.
        ///
        /// [`from_raw_owned`]: #method.from_raw_owned
        pub unsafe fn from_raw_owned(_ptr: *mut c_void, _desc: AhbDesc) -> Result<Self, AhbError> {
            // The mock has no notion of an external pointer to adopt.
            Err(AhbError::NotAndroid)
        }

        pub fn raw(&self) -> *mut c_void {
            // The mock returns the address of the heap pixel buffer
            // so tests can `as_ptr` round-trip via FFI plumbing
            // without exercising AHB syscalls.
            let g = self.inner.pixels.lock();
            g.as_ptr() as *mut c_void
        }

        pub fn desc(&self) -> &AhbDesc {
            &self.desc
        }

        pub fn into_raw(self) -> *mut c_void {
            self.raw()
        }

        pub fn lock_cpu(&self, usage: AhbUsage) -> Result<AhbLock<'_>, AhbError> {
            validate_lock_usage(&self.desc, usage)?;
            let guard = self.inner.pixels.lock();
            let (stride_bytes, _) = checked_layout(&self.desc)?;
            Ok(AhbLock {
                _ahb: self,
                guard,
                stride_bytes,
                writable: usage.bits() & CPU_WRITE_MASK != 0,
            })
        }
    }

    impl fmt::Debug for OwnedAhb {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("OwnedAhb (mock)")
                .field("desc", &self.desc)
                .finish()
        }
    }

    pub struct AhbLock<'a> {
        _ahb: &'a OwnedAhb,
        guard: parking_lot::MutexGuard<'a, Vec<u8>>,
        stride_bytes: usize,
        writable: bool,
    }

    impl<'a> AhbLock<'a> {
        pub fn as_ptr(&self) -> *mut u8 {
            self.guard.as_ptr() as *mut u8
        }
        pub fn stride_bytes(&self) -> usize {
            self.stride_bytes
        }

        pub fn as_bytes_mut(&mut self) -> Result<&mut [u8], AhbError> {
            if !self.writable {
                return Err(AhbError::InvalidLockUsage);
            }
            Ok(self.guard.as_mut_slice())
        }

        pub fn finish(self) -> Result<(), AhbError> {
            Ok(())
        }
    }
}

pub use imp::{AhbLock, OwnedAhb};

// ---------------------------------------------------------------------------
// Helpers usable on every target (operate through the AhbLock surface)
// ---------------------------------------------------------------------------

/// Copy a tightly-packed RGBA buffer into the AHB, accounting for
/// driver-imposed row stride.  Panics if `rgba.len()` doesn't match
/// `width * height * bpp` exactly (this is a programming error, not
/// a runtime input).
pub fn write_rgba_into_ahb(ahb: &OwnedAhb, rgba: &[u8]) -> Result<(), AhbError> {
    let desc = *ahb.desc();
    let bpp = desc.format.bytes_per_pixel() as usize;
    let row_bytes_src = desc.width as usize * bpp;
    assert_eq!(
        rgba.len(),
        row_bytes_src * desc.height as usize,
        "write_rgba_into_ahb: source size mismatch (got {}, want {}x{}x{})",
        rgba.len(),
        desc.width,
        desc.height,
        bpp,
    );
    let lock = ahb.lock_cpu(AhbUsage::CPU_WRITE_RARELY)?;
    let stride_bytes = lock.stride_bytes();
    let dst_base = lock.as_ptr();
    // SAFETY: the lock guarantees exclusive access to `desc.height`
    // rows of `stride_bytes` each; we copy at most `row_bytes_src`
    // (≤ stride_bytes) per row from a slice we statically know is
    // `row_bytes_src * height` long.
    unsafe {
        for y in 0..desc.height as usize {
            let src = rgba.as_ptr().add(y * row_bytes_src);
            let dst = dst_base.add(y * stride_bytes);
            std::ptr::copy_nonoverlapping(src, dst, row_bytes_src);
        }
    }
    lock.finish()
}

/// Read the AHB pixels back into a tightly-packed RGBA `Vec<u8>`.
/// Used by `getImageData` / `readPixels` paths after an upload.
pub fn read_rgba_from_ahb(ahb: &OwnedAhb) -> Result<Vec<u8>, AhbError> {
    let desc = *ahb.desc();
    let bpp = desc.format.bytes_per_pixel() as usize;
    let row_bytes_src = desc.width as usize * bpp;
    let mut out = vec![0u8; row_bytes_src * desc.height as usize];
    let lock = ahb.lock_cpu(AhbUsage::CPU_READ_RARELY)?;
    let stride_bytes = lock.stride_bytes();
    let src_base = lock.as_ptr();
    // SAFETY: see `write_rgba_into_ahb`. Same row-stride caveat.
    unsafe {
        for y in 0..desc.height as usize {
            let src = src_base.add(y * stride_bytes);
            let dst = out.as_mut_ptr().add(y * row_bytes_src);
            std::ptr::copy_nonoverlapping(src, dst, row_bytes_src);
        }
    }
    lock.finish()?;
    Ok(out)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocate_returns_handle_with_described_dims() {
        let ahb = OwnedAhb::allocate(AhbDesc::rgba_sampled(64, 32)).expect("alloc");
        let d = ahb.desc();
        assert_eq!(d.width, 64);
        assert_eq!(d.height, 32);
        assert!(d.stride_pixels >= d.width, "stride must be >= width");
        assert_eq!(d.format, AhbFormat::R8g8b8a8Unorm);
    }

    #[test]
    fn decode_descriptor_declares_one_shot_cpu_access_and_gpu_sampling() {
        let desc = AhbDesc::rgba_sampled_cpu_decode(3, 2);
        assert!(desc.usage.contains(AhbUsage::GPU_SAMPLED_IMAGE));
        assert!(desc.usage.contains(AhbUsage::CPU_READ_RARELY));
        assert!(desc.usage.contains(AhbUsage::CPU_WRITE_RARELY));
        assert!(!desc.usage.contains(AhbUsage::CPU_WRITE_OFTEN));

        let adopted = AhbDesc::rgba_sampled(3, 2);
        assert!(!adopted.usage.contains(AhbUsage::CPU_WRITE_RARELY));
    }

    #[test]
    fn allocate_rejects_zero_and_overflowing_geometry() {
        assert!(matches!(
            OwnedAhb::allocate(AhbDesc::rgba_sampled_cpu_decode(0, 1)),
            Err(AhbError::InvalidDescriptor { .. })
        ));
        assert!(matches!(
            OwnedAhb::allocate(AhbDesc::rgba_sampled_cpu_decode(1, 0)),
            Err(AhbError::InvalidDescriptor { .. })
        ));
        assert!(matches!(
            OwnedAhb::allocate(AhbDesc::rgba_sampled_cpu_decode(u32::MAX, 1)),
            Err(AhbError::InvalidDescriptor { .. })
        ));
    }

    #[test]
    fn clone_is_cheap_and_independent_lifetime() {
        let a = OwnedAhb::allocate(AhbDesc::rgba_sampled(8, 8)).unwrap();
        let b = a.clone();
        // Drop `a`; `b` must still be valid.
        drop(a);
        assert_eq!(b.desc().width, 8);
    }

    #[test]
    fn lock_requires_cpu_usage_bit() {
        let ahb = OwnedAhb::allocate(AhbDesc::rgba_sampled(4, 4)).unwrap();
        let r = ahb.lock_cpu(AhbUsage::GPU_SAMPLED_IMAGE);
        assert!(matches!(r, Err(AhbError::InvalidLockUsage)));
    }

    #[test]
    fn lock_rejects_non_cpu_bits_and_access_not_declared_at_allocation() {
        let read_only = OwnedAhb::allocate(AhbDesc::rgba_sampled(4, 4)).unwrap();
        assert!(matches!(
            read_only.lock_cpu(AhbUsage::CPU_WRITE_RARELY),
            Err(AhbError::InvalidLockUsage)
        ));
        assert!(matches!(
            read_only.lock_cpu(AhbUsage::CPU_READ_RARELY | AhbUsage::GPU_SAMPLED_IMAGE),
            Err(AhbError::InvalidLockUsage)
        ));
    }

    #[test]
    fn locked_slice_covers_checked_padded_rows_and_finishes_explicitly() {
        let ahb = OwnedAhb::allocate(AhbDesc::rgba_sampled_cpu_decode(3, 2)).unwrap();
        let mut lock = ahb.lock_cpu(AhbUsage::CPU_WRITE_RARELY).unwrap();
        let expected = lock.stride_bytes() * ahb.desc().height as usize;
        assert!(lock.stride_bytes() >= 3 * 4);
        assert_eq!(lock.as_bytes_mut().unwrap().len(), expected);
        lock.finish().expect("synchronous unlock");
    }

    #[test]
    fn read_lock_does_not_expose_a_safe_mutable_slice() {
        let ahb = OwnedAhb::allocate(AhbDesc::rgba_sampled(3, 2)).unwrap();
        let mut lock = ahb.lock_cpu(AhbUsage::CPU_READ_RARELY).unwrap();
        assert!(matches!(
            lock.as_bytes_mut(),
            Err(AhbError::InvalidLockUsage)
        ));
        lock.finish().unwrap();
    }

    #[test]
    fn write_then_read_roundtrips_pixels() {
        let ahb = OwnedAhb::allocate(AhbDesc::rgba_sampled_cpu_decode(3, 2)).unwrap();
        // 3x2 image, RGBA bytes.
        #[rustfmt::skip]
        let pixels: Vec<u8> = vec![
            255,   0,   0, 255,    0, 255,   0, 255,    0,   0, 255, 255,
              0,   0,   0, 255,  255, 255, 255, 255,  128, 128, 128, 255,
        ];
        write_rgba_into_ahb(&ahb, &pixels).unwrap();
        let got = read_rgba_from_ahb(&ahb).unwrap();
        assert_eq!(got, pixels);
    }

    #[test]
    #[should_panic(expected = "source size mismatch")]
    fn write_panics_on_size_mismatch() {
        let ahb = OwnedAhb::allocate(AhbDesc::rgba_sampled_cpu_decode(2, 2)).unwrap();
        let _ = write_rgba_into_ahb(&ahb, &[0u8; 8]); // 2 rows of 8 bytes != 2*2*4
    }

    #[test]
    fn from_raw_acquire_rejects_null_on_android() {
        // On non-Android the mock always returns NotAndroid; on
        // Android the same call with a null pointer must error too.
        // SAFETY: null is passed deliberately to exercise validation; the
        // implementation rejects it before touching the pointer.
        let r = unsafe {
            OwnedAhb::from_raw_acquire(std::ptr::null_mut(), AhbDesc::rgba_sampled(1, 1))
        };
        assert!(matches!(
            r,
            Err(AhbError::NullHandle) | Err(AhbError::NotAndroid)
        ));
    }

    #[test]
    fn from_raw_owned_rejects_null_on_android() {
        // Mirrors the transfer-ownership path used by the Android
        // Java bridge after it acquires its own native refcount.
        // SAFETY: null is passed deliberately to exercise validation; the
        // implementation rejects it before taking ownership.
        let r =
            unsafe { OwnedAhb::from_raw_owned(std::ptr::null_mut(), AhbDesc::rgba_sampled(1, 1)) };
        assert!(matches!(
            r,
            Err(AhbError::NullHandle) | Err(AhbError::NotAndroid)
        ));
    }

    #[test]
    fn ahb_is_send_and_sync() {
        // Compile-time assertion via trait bounds.
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<OwnedAhb>();
        assert_send_sync::<AhbDesc>();
        assert_send_sync::<AhbFormat>();
    }
}
