//! One Objective-C autorelease pool per iteration of a Migo-owned thread.
//!
//! # Why this exists
//!
//! Every thread that calls into Cocoa owes the runtime an autorelease pool, and
//! until this module existed no thread Migo spawns had one. That is not a style
//! point on Apple platforms: ANGLE's Metal backend is Objective-C++, and the
//! objects it returns per frame -- `CAMetalDrawable` from `nextDrawable`,
//! `MTLCommandBuffer` from `commandBuffer`, `NSError` from a failed call -- are
//! **autoreleased**, not owned. An autoreleased object is released when the
//! enclosing pool is popped.
//!
//! On a thread with no pool the modern runtime does not drop them and does not
//! complain: `AutoreleasePoolPage::autoreleaseNoPage` installs a hidden
//! top-level pool and only prints "autoreleased with no pool in place" when
//! `OBJC_DEBUG_MISSING_POOLS=YES` is set. That hidden pool is popped when the
//! thread is destroyed. So a render thread that lives for the whole session
//! accumulates every per-frame Metal object for the whole session, silently.
//!
//! A `CAMetalDrawable` retains the `CAMetalLayer` it came from, which is why
//! this is a correctness problem and not only a footprint one: a layer the host
//! handed us stays alive after Migo has published RELEASED for it, and RELEASED
//! is the point the C ABI entitles a host to free that layer.
//!
//! # Shape
//!
//! An RAII guard rather than a `with_pool(|| ...)` closure. Pools must be popped
//! on the thread that pushed them and in LIFO order, and the render loop returns
//! out of the middle of an iteration (shutdown) and can unwind through it (a
//! panic in host code). A guard held as a local satisfies all three by
//! construction; a closure would have to re-state each one.
//!
//! Off Apple this is a zero-sized no-op, so call sites are unconditional and the
//! one that matters -- the render loop -- carries no `cfg` of its own.

#[cfg(target_vendor = "apple")]
mod imp {
    use std::ffi::c_void;

    // Already linked by the Apple presenter's retain/release; naming it again
    // here keeps this module self-contained rather than dependent on which
    // other crate happened to be compiled into the same artifact.
    #[link(name = "objc")]
    unsafe extern "C" {
        fn objc_autoreleasePoolPush() -> *mut c_void;
        fn objc_autoreleasePoolPop(pool: *mut c_void);
    }

    /// A pushed pool, popped when this value is dropped.
    ///
    /// Not `Send`: the runtime requires the pop to run on the pushing thread,
    /// and the raw pointer field is what enforces that in the type system.
    pub struct AutoreleasePool {
        token: *mut c_void,
    }

    impl AutoreleasePool {
        /// Push a pool that covers everything until the returned guard drops.
        pub fn new() -> Self {
            // SAFETY: the runtime call takes no arguments and returns an opaque
            // token this type hands back exactly once, from Drop, on this thread.
            Self {
                token: unsafe { objc_autoreleasePoolPush() },
            }
        }
    }

    impl Drop for AutoreleasePool {
        fn drop(&mut self) {
            // SAFETY: `token` came from the push in `new` on this thread and is
            // popped exactly once. Guards are locals, so nesting is LIFO.
            unsafe { objc_autoreleasePoolPop(self.token) };
        }
    }
}

#[cfg(not(target_vendor = "apple"))]
mod imp {
    /// The no-op every other platform gets.
    pub struct AutoreleasePool;

    impl AutoreleasePool {
        pub fn new() -> Self {
            Self
        }
    }
}

pub use imp::AutoreleasePool;

impl Default for AutoreleasePool {
    fn default() -> Self {
        Self::new()
    }
}

/// Push a pool for the current scope.
///
/// ```
/// # use shared::objc_autorelease::autorelease_scope;
/// let _pool = autorelease_scope();
/// // Objective-C calls made here have somewhere to drain.
/// ```
///
/// Bind the result. `let _ = autorelease_scope();` drops it immediately and
/// covers nothing, which is the one way to misuse this and the reason the
/// doctest above binds a name.
#[must_use = "the pool is popped when this guard drops, so a discarded guard covers nothing"]
#[inline]
pub fn autorelease_scope() -> AutoreleasePool {
    AutoreleasePool::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pool_can_be_pushed_and_popped() {
        let pool = autorelease_scope();
        drop(pool);
    }

    #[test]
    fn pools_nest() {
        let outer = autorelease_scope();
        {
            let _inner = autorelease_scope();
        }
        drop(outer);
    }

    // The one that is worth anything, and it only exists on the platform the
    // module is for: the two above would pass against a `new()` that pushed
    // nothing.
    //
    // A CoreFoundation string is toll-free bridged to an Objective-C object, so
    // `objc_autorelease` accepts it and `CFGetRetainCount` reads the same
    // counter the pool decrements. The object is given a second reference first
    // precisely so the pop is observable: draining the pool must take the count
    // from 2 to 1, and reading the count of an object the pool had freed would
    // be a use-after-free rather than a measurement.
    #[cfg(target_vendor = "apple")]
    #[test]
    fn popping_a_pool_releases_what_was_autoreleased_into_it() {
        use std::ffi::{c_char, c_void};

        #[link(name = "CoreFoundation", kind = "framework")]
        unsafe extern "C" {
            fn CFStringCreateWithCString(
                allocator: *const c_void,
                cstr: *const c_char,
                encoding: u32,
            ) -> *mut c_void;
            fn CFGetRetainCount(cf: *const c_void) -> isize;
            fn CFRelease(cf: *const c_void);
        }
        #[link(name = "objc")]
        unsafe extern "C" {
            fn objc_retain(object: *mut c_void) -> *mut c_void;
            fn objc_autorelease(object: *mut c_void) -> *mut c_void;
        }

        const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;

        // SAFETY: a constant NUL-terminated literal, the default allocator, and
        // a documented encoding constant. Every pointer below is the one this
        // call produced, and it is released exactly once at the end.
        let string = unsafe {
            CFStringCreateWithCString(
                std::ptr::null(),
                c"migo autorelease probe".as_ptr(),
                K_CF_STRING_ENCODING_UTF8,
            )
        };
        assert!(!string.is_null(), "CFStringCreateWithCString returned null");

        // SAFETY: `string` is live and owned here for the whole block.
        unsafe {
            objc_retain(string);
            let before = CFGetRetainCount(string);

            {
                let _pool = autorelease_scope();
                objc_autorelease(string);
                assert_eq!(
                    CFGetRetainCount(string),
                    before,
                    "autorelease must not release before the pool is popped"
                );
            }

            assert_eq!(
                CFGetRetainCount(string),
                before - 1,
                "popping the pool must release what was autoreleased into it; if this is \
                 equal to `before`, the guard pushed no pool and every Objective-C object \
                 the render thread autoreleases lives until the thread exits"
            );
            CFRelease(string);
        }
    }
}
